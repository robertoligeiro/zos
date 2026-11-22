//! A small HTML layout engine for the ZOS browser: a bounded subset of HTML turned
//! into runs of monospace text positioned on a character grid.
//!
//! Supported: headings (h1-h3), paragraphs and divs, line breaks, rules, bulleted and
//! numbered lists (nested to four levels), block quotes, preformatted text, links,
//! bold / italic / code text, image placeholders, the page title, comments, and the
//! entities `&amp; &lt; &gt; &quot; &apos; &nbsp;` and numeric ones. `script` and
//! `style` content is skipped. Everything else is ignored, not an error. Pages that
//! exceed the fixed limits are cut off (`truncated`), never overrun.

pub const ARENA: usize = 36 * 1024;
pub const MAX_RUNS: usize = 1536;
pub const MAX_LINKS: usize = 96;
pub const TITLE: usize = 48;
const MAX_LIST_DEPTH: usize = 4;
/// Longest link target kept.
pub const HREF: usize = 96;

pub const BOLD: u8 = 1;
pub const CODE: u8 = 2;
pub const ITALIC: u8 = 4;

/// What a run is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    #[default]
    Text,
    Heading1,
    Heading2,
    Heading3,
    Rule,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Run {
    start: u16,
    pub len: u16,
    pub row: u16,
    pub col: u8,
    pub kind: Kind,
    pub flags: u8,
    /// 0 for plain text, else 1 + the index of the link in the page.
    pub link: u8,
}

#[derive(Clone, Copy, Default)]
struct Link {
    start: u16,
    len: u8,
}

pub struct Page {
    arena: [u8; ARENA],
    used: usize,
    runs: [Run; MAX_RUNS],
    run_count: usize,
    links: [Link; MAX_LINKS],
    link_count: usize,
    title: [u8; TITLE],
    title_len: usize,
    rows: usize,
    pub truncated: bool,
}

impl Default for Page {
    fn default() -> Self {
        Self::new()
    }
}

impl Page {
    pub const fn new() -> Self {
        Self {
            arena: [0; ARENA],
            used: 0,
            runs: [Run {
                start: 0,
                len: 0,
                row: 0,
                col: 0,
                kind: Kind::Text,
                flags: 0,
                link: 0,
            }; MAX_RUNS],
            run_count: 0,
            links: [Link { start: 0, len: 0 }; MAX_LINKS],
            link_count: 0,
            title: [0; TITLE],
            title_len: 0,
            rows: 0,
            truncated: false,
        }
    }
    pub fn runs(&self) -> &[Run] {
        &self.runs[..self.run_count]
    }
    pub fn text(&self, run: &Run) -> &[u8] {
        &self.arena[run.start as usize..run.start as usize + run.len as usize]
    }
    pub fn link_count(&self) -> usize {
        self.link_count
    }
    /// The target of link `index` (0-based).
    pub fn href(&self, index: usize) -> Option<&[u8]> {
        let link = self.links[..self.link_count].get(index)?;
        Some(&self.arena[link.start as usize..link.start as usize + link.len as usize])
    }
    pub fn title(&self) -> &[u8] {
        &self.title[..self.title_len]
    }
    /// Number of text rows the page occupies.
    pub fn rows(&self) -> usize {
        self.rows
    }
    /// The link under character cell (`col`, `row`), as a 0-based index.
    pub fn link_at(&self, col: usize, row: usize) -> Option<usize> {
        self.runs().iter().find_map(|run| {
            (run.link != 0
                && run.row as usize == row
                && (run.col as usize..run.col as usize + run.len as usize).contains(&col))
            .then(|| run.link as usize - 1)
        })
    }
    /// The first row any run of link `index` is on.
    pub fn link_row(&self, index: usize) -> Option<usize> {
        self.runs()
            .iter()
            .find(|run| run.link as usize == index + 1)
            .map(|run| run.row as usize)
    }

    /// Lays out `source` for a grid `cols` characters wide (at most 255).
    pub fn layout(&mut self, source: &[u8], cols: usize) {
        *self = Self::new();
        let cols = cols.clamp(8, 255);
        {
            let mut layout = Layout {
                page: self,
                cols,
                row: 0,
                col: 0,
                indent: 0,
                heading: Kind::Text,
                bold: 0,
                code: 0,
                italic: 0,
                link: 0,
                pre: false,
                pending_space: false,
                last_blank: true,
                lists: [0; MAX_LIST_DEPTH],
                depth: 0,
                in_title: false,
                skipping: None,
            };
            layout.run(source);
        }
        self.rows = self
            .runs()
            .iter()
            .map(|run| run.row as usize + 1)
            .max()
            .unwrap_or(0);
    }
}

struct Layout<'a> {
    page: &'a mut Page,
    cols: usize,
    row: usize,
    col: usize,
    indent: usize,
    heading: Kind,
    bold: u8,
    code: u8,
    italic: u8,
    link: u8,
    pre: bool,
    pending_space: bool,
    /// The row above the cursor is empty (or there is none).
    last_blank: bool,
    /// Per level: 0 for a bullet list, else the next number.
    lists: [u16; MAX_LIST_DEPTH],
    depth: usize,
    in_title: bool,
    /// Inside `script` or `style`: ignore everything up to this closing tag.
    skipping: Option<&'static [u8]>,
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

fn lower(byte: u8) -> u8 {
    byte.to_ascii_lowercase()
}

/// Decodes the entity whose text (after the `&`, up to the `;`) is `name`.
fn entity(name: &[u8]) -> Option<u8> {
    Some(match name {
        b"amp" => b'&',
        b"lt" => b'<',
        b"gt" => b'>',
        b"quot" => b'"',
        b"apos" => b'\'',
        b"nbsp" => b' ',
        [b'#', digits @ ..] if !digits.is_empty() => {
            let value = if let [b'x' | b'X', hex @ ..] = digits {
                hex.iter().try_fold(0u32, |acc, &d| {
                    (d as char)
                        .to_digit(16)
                        .and_then(|v| acc.checked_mul(16)?.checked_add(v))
                })?
            } else {
                digits.iter().try_fold(0u32, |acc, &d| {
                    (d as char)
                        .to_digit(10)
                        .and_then(|v| acc.checked_mul(10)?.checked_add(v))
                })?
            };
            // Only printable ASCII can be drawn; anything else becomes a placeholder.
            if (32..=126).contains(&value) {
                value as u8
            } else {
                b'?'
            }
        }
        _ => return None,
    })
}

impl Layout<'_> {
    // ---- Cursor and blocks ------------------------------------------------
    fn at_line_start(&self) -> bool {
        self.col <= self.indent
    }
    /// Ends the current line (if it has anything on it).
    fn newline(&mut self) {
        if !self.at_line_start() {
            self.row += 1;
            self.last_blank = false;
        }
        self.col = self.indent;
        self.pending_space = false;
    }
    /// Ends the line and leaves one empty row before the next block.
    fn block_break(&mut self) {
        self.newline();
        if !self.last_blank && self.row > 0 {
            self.row += 1;
            self.last_blank = true;
        }
    }

    // ---- Text output ------------------------------------------------------
    fn style_flags(&self) -> u8 {
        (if self.bold > 0 || self.heading != Kind::Text {
            BOLD
        } else {
            0
        }) | if self.code > 0 || self.pre { CODE } else { 0 }
            | if self.italic > 0 { ITALIC } else { 0 }
    }

    /// Appends `text` at the cursor on the current row, merging with the previous run
    /// when it continues it.
    fn put(&mut self, text: &[u8], kind: Kind) {
        if text.is_empty() || self.page.truncated {
            return;
        }
        if self.page.used + text.len() > ARENA {
            self.page.truncated = true;
            return;
        }
        let start = self.page.used;
        self.page.arena[start..start + text.len()].copy_from_slice(text);
        self.page.used += text.len();
        let (flags, link) = (self.style_flags(), self.link);
        if let Some(last) = self.page.runs[..self.page.run_count].last_mut() {
            if last.row as usize == self.row
                && last.col as usize + last.len as usize == self.col
                && last.start as usize + last.len as usize == start
                && last.kind == kind
                && last.flags == flags
                && last.link == link
                && last.len as usize + text.len() <= u16::MAX as usize
            {
                last.len += text.len() as u16;
                self.col += text.len();
                self.last_blank = false;
                return;
            }
        }
        if self.page.run_count == MAX_RUNS {
            self.page.truncated = true;
            return;
        }
        self.page.runs[self.page.run_count] = Run {
            start: start as u16,
            len: text.len() as u16,
            row: self.row.min(u16::MAX as usize) as u16,
            col: self.col.min(255) as u8,
            kind,
            flags,
            link,
        };
        self.page.run_count += 1;
        self.col += text.len();
        self.last_blank = false;
    }

    fn kind(&self) -> Kind {
        self.heading
    }

    /// One word of running text: wraps before it if it does not fit.
    fn word(&mut self, word: &[u8]) {
        let mut word = word;
        while !word.is_empty() {
            let gap = usize::from(self.pending_space && !self.at_line_start());
            let room = self.cols.saturating_sub(self.col + gap);
            if word.len() > room && !self.at_line_start() {
                self.newline();
                continue;
            }
            if gap == 1 {
                self.put(b" ", self.kind());
            }
            self.pending_space = false;
            let room = self.cols.saturating_sub(self.col).max(1);
            let take = word.len().min(room);
            self.put(&word[..take], self.kind());
            word = &word[take..];
            if !word.is_empty() {
                self.newline();
            }
        }
    }

    // ---- Tags -------------------------------------------------------------
    fn open_list(&mut self, ordered: bool) {
        // A list inside a list item continues that item; only a top-level list is a
        // separate block with a gap around it.
        if self.depth == 0 {
            self.block_break();
        } else {
            self.newline();
        }
        if self.depth < MAX_LIST_DEPTH {
            self.lists[self.depth] = u16::from(ordered);
            self.depth += 1;
            self.indent = (self.indent + 3).min(self.cols / 2);
        }
        self.col = self.indent;
    }
    fn close_list(&mut self) {
        self.newline();
        if self.depth > 0 {
            self.depth -= 1;
            self.indent = self.indent.saturating_sub(3);
        }
        // The cursor must follow the shallower indent before the block break, or the
        // break would count the old column as text on the line.
        self.col = self.indent;
        if self.depth == 0 {
            self.block_break();
        }
        self.col = self.indent;
    }
    fn list_item(&mut self) {
        self.newline();
        let marker_col = self.indent.saturating_sub(3);
        self.col = marker_col;
        if self.depth > 0 && self.lists[self.depth - 1] != 0 {
            let number = self.lists[self.depth - 1];
            self.lists[self.depth - 1] = number.saturating_add(1);
            let mut digits = [0u8; 8];
            let mut at = digits.len();
            let mut n = number;
            loop {
                at -= 1;
                digits[at] = b'0' + (n % 10) as u8;
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            let mut marker = [0u8; 10];
            let length = digits.len() - at;
            marker[..length].copy_from_slice(&digits[at..]);
            marker[length] = b'.';
            self.put(&marker[..length + 1], Kind::Text);
        } else {
            self.put(b"*", Kind::Text);
        }
        self.col = self.indent;
        self.pending_space = false;
        self.last_blank = false;
    }

    /// Handles one tag: `name` is lowercase without the slash; `attrs` is the text
    /// after the name.
    fn tag(&mut self, name: &[u8], closing: bool, attrs: &[u8]) {
        match (name, closing) {
            (b"title", false) => self.in_title = true,
            (b"title", true) => self.in_title = false,
            (b"script", false) => self.skipping = Some(b"script"),
            (b"style", false) => self.skipping = Some(b"style"),
            (b"h1" | b"h2" | b"h3", false) => {
                self.block_break();
                self.heading = match name {
                    b"h1" => Kind::Heading1,
                    b"h2" => Kind::Heading2,
                    _ => Kind::Heading3,
                };
            }
            (b"h1" | b"h2" | b"h3", true) => {
                self.heading = Kind::Text;
                self.block_break();
            }
            (b"p" | b"div" | b"table" | b"section" | b"article" | b"header" | b"footer", _) => {
                self.block_break()
            }
            (b"blockquote", false) => {
                self.block_break();
                self.indent = (self.indent + 4).min(self.cols / 2);
                self.col = self.indent;
            }
            (b"blockquote", true) => {
                self.block_break();
                self.indent = self.indent.saturating_sub(4);
                self.col = self.indent;
            }
            (b"ul", false) => self.open_list(false),
            (b"ol", false) => self.open_list(true),
            (b"ul" | b"ol", true) => self.close_list(),
            (b"li", false) => self.list_item(),
            (b"br", _) | (b"tr", _) => self.newline(),
            (b"hr", _) => {
                self.block_break();
                let width = self.cols - self.indent;
                self.put_rule(width);
                self.block_break();
            }
            (b"pre", false) => {
                self.block_break();
                self.pre = true;
            }
            (b"pre", true) => {
                self.pre = false;
                self.block_break();
            }
            (b"b" | b"strong", false) => self.bold = self.bold.saturating_add(1),
            (b"b" | b"strong", true) => self.bold = self.bold.saturating_sub(1),
            (b"i" | b"em", false) => self.italic = self.italic.saturating_add(1),
            (b"i" | b"em", true) => self.italic = self.italic.saturating_sub(1),
            (b"code" | b"tt" | b"kbd", false) => self.code = self.code.saturating_add(1),
            (b"code" | b"tt" | b"kbd", true) => self.code = self.code.saturating_sub(1),
            (b"img", false) => self.word(b"[image]"),
            (b"a", false) => self.open_link(attrs),
            (b"a", true) => self.link = 0,
            _ => (),
        }
    }

    fn put_rule(&mut self, width: usize) {
        if self.page.truncated || self.page.run_count == MAX_RUNS {
            self.page.truncated = true;
            return;
        }
        self.page.runs[self.page.run_count] = Run {
            start: 0,
            len: width.min(u16::MAX as usize) as u16,
            row: self.row.min(u16::MAX as usize) as u16,
            col: self.indent.min(255) as u8,
            kind: Kind::Rule,
            flags: 0,
            link: 0,
        };
        self.page.run_count += 1;
        self.row += 1;
        self.col = self.indent;
        self.last_blank = false;
    }

    fn open_link(&mut self, attrs: &[u8]) {
        self.link = 0;
        let Some(href) = attribute(attrs, b"href") else {
            return;
        };
        if self.page.link_count == MAX_LINKS || self.page.used + href.len() > ARENA {
            return;
        }
        let href = &href[..href.len().min(HREF)];
        let start = self.page.used;
        self.page.arena[start..start + href.len()].copy_from_slice(href);
        self.page.used += href.len();
        self.page.links[self.page.link_count] = Link {
            start: start as u16,
            len: href.len() as u8,
        };
        self.page.link_count += 1;
        self.link = self.page.link_count as u8;
    }

    // ---- Driver -----------------------------------------------------------
    fn run(&mut self, source: &[u8]) {
        let mut at = 0;
        let mut word = [0u8; 64];
        let mut word_len = 0;
        while at < source.len() && !self.page.truncated {
            let byte = source[at];
            if let Some(name) = self.skipping {
                // Look for the matching closing tag.
                if byte == b'<' && source[at + 1..].starts_with(b"/") {
                    let rest = &source[at + 2..];
                    if rest.len() >= name.len()
                        && rest[..name.len()]
                            .iter()
                            .map(|b| lower(*b))
                            .eq(name.iter().copied())
                    {
                        self.skipping = None;
                        // Skip to the closing tag's `>` (the step below passes it).
                        at += 2 + rest.iter().position(|&b| b == b'>').unwrap_or(rest.len());
                    }
                }
                at += 1;
                continue;
            }
            if byte == b'<' {
                self.flush(&mut word, &mut word_len);
                at = self.markup(source, at);
                continue;
            }
            let mut value = byte;
            if byte == b'&' {
                // An entity, if it ends soon enough with a semicolon.
                let limit = (at + 12).min(source.len());
                if let Some(end) = source[at + 1..limit].iter().position(|&b| b == b';') {
                    if let Some(decoded) = entity(&source[at + 1..at + 1 + end]) {
                        value = decoded;
                        at += end + 1;
                    }
                }
            }
            at += 1;
            if self.in_title {
                if self.page.title_len < TITLE && (32..=126).contains(&value) {
                    let n = self.page.title_len;
                    self.page.title[n] = value;
                    self.page.title_len += 1;
                }
                continue;
            }
            if self.pre {
                self.preformatted(value);
            } else if is_space(value) && !(byte == b'&' && value == b' ') {
                self.flush(&mut word, &mut word_len);
                self.pending_space = true;
            } else if value == b' ' {
                // A non-breaking space joins the words around it.
                if word_len < word.len() {
                    word[word_len] = b' ';
                    word_len += 1;
                }
            } else if (32..=126).contains(&value) {
                if word_len == word.len() {
                    self.flush(&mut word, &mut word_len);
                    self.pending_space = false;
                }
                word[word_len] = value;
                word_len += 1;
            }
        }
        self.flush(&mut word, &mut word_len);
    }

    fn flush(&mut self, word: &mut [u8; 64], length: &mut usize) {
        if *length > 0 {
            self.word(&word[..*length]);
            *length = 0;
        }
    }

    fn preformatted(&mut self, value: u8) {
        match value {
            b'\n' => {
                self.row += usize::from(!self.at_line_start() || self.col == self.indent);
                self.col = self.indent;
                self.last_blank = false;
            }
            b'\r' => (),
            b'\t' => {
                for _ in 0..4 {
                    self.pre_char(b' ');
                }
            }
            value if (32..=126).contains(&value) => self.pre_char(value),
            _ => (),
        }
    }

    fn pre_char(&mut self, value: u8) {
        if self.col < self.cols {
            self.put(&[value], Kind::Text);
        }
    }

    /// Parses the tag or comment starting at `source[at]` (a `<`); returns the index
    /// after it. A `<` that does not start a tag is shown as text.
    fn markup(&mut self, source: &[u8], at: usize) -> usize {
        let rest = &source[at + 1..];
        if rest.starts_with(b"!--") {
            return match rest[3..].windows(3).position(|w| w == b"-->") {
                Some(end) => at + 1 + 3 + end + 3,
                None => source.len(),
            };
        }
        if rest.first() == Some(&b'!') || rest.first() == Some(&b'?') {
            return match rest.iter().position(|&b| b == b'>') {
                Some(end) => at + 1 + end + 1,
                None => source.len(),
            };
        }
        let closing = rest.first() == Some(&b'/');
        let body = &rest[usize::from(closing)..];
        if !body.first().is_some_and(|b| b.is_ascii_alphabetic()) {
            // Not a tag: a literal `<`.
            self.word(b"<");
            return at + 1;
        }
        // The end of the tag, skipping `>` inside quoted attribute values.
        let mut quote = 0u8;
        let mut end = None;
        for (index, &byte) in body.iter().enumerate() {
            match (quote, byte) {
                (0, b'"' | b'\'') => quote = byte,
                (q, b) if q != 0 && b == q => quote = 0,
                (0, b'>') => {
                    end = Some(index);
                    break;
                }
                _ => (),
            }
        }
        let Some(end) = end else {
            return source.len();
        };
        let inside = &body[..end];
        let name_len = inside
            .iter()
            .position(|&b| is_space(b) || b == b'/')
            .unwrap_or(inside.len());
        let mut name = [0u8; 12];
        let n = name_len.min(name.len());
        for (to, from) in name[..n].iter_mut().zip(&inside[..n]) {
            *to = lower(*from);
        }
        let attrs = &inside[name_len..];
        if name_len <= name.len() {
            self.tag(&name[..n], closing, attrs);
        }
        at + 1 + usize::from(closing) + end + 1
    }
}

/// The value of attribute `wanted` in the text of a tag after its name.
pub fn attribute<'a>(attrs: &'a [u8], wanted: &[u8]) -> Option<&'a [u8]> {
    let mut at = 0;
    while at < attrs.len() {
        while at < attrs.len() && (is_space(attrs[at]) || attrs[at] == b'/') {
            at += 1;
        }
        let name_start = at;
        while at < attrs.len() && !is_space(attrs[at]) && attrs[at] != b'=' && attrs[at] != b'/' {
            at += 1;
        }
        let name = &attrs[name_start..at];
        while at < attrs.len() && is_space(attrs[at]) {
            at += 1;
        }
        let mut value: &[u8] = b"";
        if at < attrs.len() && attrs[at] == b'=' {
            at += 1;
            while at < attrs.len() && is_space(attrs[at]) {
                at += 1;
            }
            if at < attrs.len() && (attrs[at] == b'"' || attrs[at] == b'\'') {
                let quote = attrs[at];
                at += 1;
                let start = at;
                while at < attrs.len() && attrs[at] != quote {
                    at += 1;
                }
                value = &attrs[start..at];
                at = (at + 1).min(attrs.len());
            } else {
                let start = at;
                while at < attrs.len() && !is_space(attrs[at]) {
                    at += 1;
                }
                value = &attrs[start..at];
            }
        }
        if name.len() == wanted.len() && name.iter().zip(wanted).all(|(a, b)| lower(*a) == *b) {
            return Some(value);
        }
        if name.is_empty() {
            at += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::string::{String, ToString};
    use std::vec::Vec;

    fn page(source: &str, cols: usize) -> Box<Page> {
        let mut page = Box::new(Page::new());
        page.layout(source.as_bytes(), cols);
        page
    }

    /// The page as text rows (runs on a row joined by their columns).
    fn rows(page: &Page) -> Vec<String> {
        let mut grid = std::vec![std::vec![b' '; 255]; page.rows()];
        for run in page.runs() {
            if run.kind == Kind::Rule {
                for i in 0..run.len as usize {
                    grid[run.row as usize][run.col as usize + i] = b'-';
                }
            } else {
                for (i, &b) in page.text(run).iter().enumerate() {
                    grid[run.row as usize][run.col as usize + i] = b;
                }
            }
        }
        grid.into_iter()
            .map(|row| String::from_utf8(row).unwrap().trim_end().into())
            .collect()
    }

    #[test]
    fn paragraphs_wrap_at_the_grid_width_and_collapse_whitespace() {
        let p = page("<p>the quick   brown\n fox jumps over the lazy dog</p>", 16);
        assert_eq!(
            rows(&p),
            ["the quick brown", "fox jumps over", "the lazy dog"]
        );
        assert!(!p.truncated);
        // A word longer than a row is split, not lost.
        let p = page("<p>abcdefghijklmnopqrstuvwxyz</p>", 10);
        assert_eq!(rows(&p), ["abcdefghij", "klmnopqrst", "uvwxyz"]);
    }

    #[test]
    fn headings_blocks_breaks_and_rules_leave_the_right_gaps() {
        let p = page(
            "<h1>Title</h1><p>one</p><p>two<br>three</p><hr><p>end</p>",
            20,
        );
        assert_eq!(
            rows(&p),
            [
                "Title",
                "",
                "one",
                "",
                "two",
                "three",
                "",
                "--------------------",
                "",
                "end"
            ]
        );
        assert_eq!(p.runs()[0].kind, Kind::Heading1);
        assert_ne!(p.runs()[0].flags & BOLD, 0, "headings are bold");
    }

    #[test]
    fn lists_are_indented_with_bullets_or_numbers() {
        let p = page("<ul><li>apple<li>pear</ul><ol><li>first<li>second</ol>", 30);
        assert_eq!(
            rows(&p),
            ["*  apple", "*  pear", "", "1. first", "2. second"]
        );
        // Nesting indents further.
        let p = page("<ul><li>a<ul><li>b</ul><li>c</ul>", 30);
        assert_eq!(rows(&p), ["*  a", "   *  b", "*  c"]);
    }

    #[test]
    fn links_are_collected_and_found_by_position() {
        let p = page(
            "<p>See <a href=\"/www/two.html\">the second page</a> or <A HREF=other.html>other</A>.</p>",
            40,
        );
        assert_eq!(rows(&p), ["See the second page or other."]);
        assert_eq!(p.link_count(), 2);
        assert_eq!(p.href(0), Some(&b"/www/two.html"[..]));
        assert_eq!(p.href(1), Some(&b"other.html"[..]));
        assert_eq!(p.link_at(5, 0), Some(0));
        assert_eq!(p.link_at(18, 0), Some(0));
        assert_eq!(
            p.link_at(19, 0),
            None,
            "the space after the link is plain text"
        );
        assert_eq!(p.link_at(1, 0), None, "plain text is not a link");
        assert_eq!(p.link_at(25, 0), Some(1));
        assert_eq!(p.link_row(1), Some(0));
        // An anchor without a target is plain text.
        let p = page("<a name=x>anchor</a>", 20);
        assert_eq!(p.link_count(), 0);
        assert_eq!(p.link_at(1, 0), None);
    }

    #[test]
    fn a_link_that_wraps_stays_one_link_on_both_rows() {
        let p = page("<a href=x>aaaa bbbb cccc dddd</a>", 10);
        assert_eq!(rows(&p), ["aaaa bbbb", "cccc dddd"]);
        assert_eq!(p.link_at(1, 0), Some(0));
        assert_eq!(p.link_at(1, 1), Some(0));
        assert_eq!(p.link_count(), 1);
    }

    #[test]
    fn preformatted_text_keeps_its_spacing_and_lines() {
        let p = page("<pre>a  b\n  c\n\td</pre><p>after</p>", 20);
        assert_eq!(rows(&p), ["a  b", "  c", "    d", "", "after"]);
        assert_ne!(p.runs()[0].flags & CODE, 0);
        // Long lines are cut at the grid width, not wrapped.
        let p = page("<pre>0123456789abcdefXXXX</pre>", 16);
        assert_eq!(rows(&p), ["0123456789abcdef"]);
    }

    #[test]
    fn entities_comments_scripts_and_the_title() {
        let p = page(
            "<title>My &amp; Page</title><!-- hidden --><script>var a = '<b>';</script>\
             <style>p { color: red }</style><p>5 &lt; 6 &amp;&amp; &#65;&#x42; &quot;q&quot; a&nbsp;b &bogus; &#999;</p>",
            60,
        );
        assert_eq!(p.title(), b"My & Page");
        assert_eq!(rows(&p), ["5 < 6 && AB \"q\" a b &bogus; ?"]);
    }

    #[test]
    fn malformed_markup_is_survived() {
        for source in [
            "<",
            "<<<>>>",
            "a < b > c",
            "<p",
            "<a href=\"unterminated>text",
            "</p></div></ul></li>",
            "<!-- never ends",
            "<!DOCTYPE html",
            "&",
            "&#;&#x;&;",
            "<ul><li><li><li></ul></ul></ul>",
        ] {
            let p = page(source, 20);
            assert!(p.rows() < 100, "{source:?}");
        }
        let p = page("a < b", 20);
        assert_eq!(rows(&p), ["a < b"]);
    }

    #[test]
    fn oversized_pages_are_cut_off_and_never_overrun() {
        // Far more text than the arena holds.
        let mut source = String::new();
        for i in 0..20_000 {
            source.push_str("word");
            source.push_str(&i.to_string());
            source.push(' ');
        }
        let p = page(&source, 80);
        assert!(p.truncated);
        assert!(p.runs().len() <= MAX_RUNS);
        // Far more links than the table holds.
        let mut source = String::new();
        for i in 0..(MAX_LINKS + 10) {
            source.push_str(&std::format!("<a href=\"/{i}\">l{i}</a> "));
        }
        let p = page(&source, 80);
        assert_eq!(p.link_count(), MAX_LINKS);
        // An empty page is fine.
        let p = page("", 80);
        assert_eq!((p.rows(), p.link_count()), (0, 0));
    }

    #[test]
    fn random_input_never_panics() {
        let mut seed = 0x9e37_79b9u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as u8
        };
        let alphabet = b"<>/=\"' &;#!-abphrefulolilitbrpre\n\tx9";
        for length in 0..400 {
            let source: Vec<u8> = (0..length)
                .map(|_| alphabet[next() as usize % alphabet.len()])
                .collect();
            let mut p = Box::new(Page::new());
            p.layout(&source, 8 + (next() as usize % 80));
            for run in p.runs() {
                let _ = p.text(run);
            }
        }
    }

    #[test]
    fn attributes_are_found_in_any_quoting_style() {
        assert_eq!(
            attribute(b" href=\"a b\" class=x", b"href"),
            Some(&b"a b"[..])
        );
        assert_eq!(attribute(b" class=x href='q'", b"href"), Some(&b"q"[..]));
        assert_eq!(
            attribute(b" HREF=bare/path ", b"href"),
            Some(&b"bare/path"[..])
        );
        assert_eq!(attribute(b" nohref=1", b"href"), None);
        assert_eq!(attribute(b"", b"href"), None);
        assert_eq!(attribute(b" href", b"href"), Some(&b""[..]));
    }
}
