//! The ZOS browser: shows HTML pages (and plain text files, and directory listings)
//! from the ZOS filesystem in a desktop window. Web addresses are recognized but
//! cannot be loaded until the system has a network stack.
//!
//! Keys: type in the address bar and press Enter. Tab switches between the address
//! bar and the page (Ctrl+L goes to the address bar). In the page: Tab selects the next
//! link, Enter follows it, Up/Down or j/k scroll a line, Space/b a page, Home/End the
//! ends, Backspace goes back. Links can also be clicked.

#![no_std]
#![no_main]

use zos_apps::{
    html::{Kind, Page, BOLD, CODE},
    nav::{self, Address, Target},
};
use zos_console::{
    gui::{self, Event, Window},
    keyboard::Key,
};
use zos_libos as os;
use zos_storage::{protocol as api, vfs::READ};

const WIDTH: u16 = 720;
const HEIGHT: u16 = 520;
const BAR: i16 = 28;
const STATUS: i16 = 20;
const MARGIN: i16 = 8;
const COLS: usize = (WIDTH as usize - 2 * MARGIN as usize) / 8;
const VISIBLE: usize = (HEIGHT as usize - BAR as usize - STATUS as usize - 4) / 16;

const BACKGROUND: u32 = 0xfdfdf8;
const INK: u32 = 0x202020;
const LINK: u32 = 0x0b3fb5;
const HEADING: u32 = 0x7a1a1a;
const CODE_INK: u32 = 0x1d5a2a;
const CHROME: u32 = 0xd8d8d8;

const FILE: usize = 24 * 1024;
const MARKUP: usize = 44 * 1024;
const HISTORY: usize = 8;

const HOME: &[u8] = b"<title>ZOS Browser</title>\
<h1>Welcome to the ZOS browser</h1>\
<p>This is a small browser for the ZOS desktop. It shows HTML pages, plain text \
files and directory listings from the ZOS filesystem.</p>\
<h2>Start here</h2>\
<ul>\
<li><a href=\"about:help\">How to use the browser</a></li>\
<li><a href=\"about:zos\">About ZOS</a></li>\
<li><a href=\"/\">Browse the filesystem</a></li>\
<li><a href=\"/www/index.html\">Your own page: /www/index.html</a> \
(create it from the terminal, for example with echo)</li>\
<li><a href=\"http://example.com/\">A web address (needs networking)</a></li>\
</ul>";

const HELP: &[u8] = b"<title>Browser help</title>\
<h1>Using the browser</h1>\
<h2>Moving around</h2>\
<ul>\
<li>Type an address in the bar and press <b>Enter</b>. An address is a path such as \
<code>/www/index.html</code>, or <code>about:home</code>.</li>\
<li><b>Tab</b> switches between the address bar and the page. <b>Ctrl+L</b> jumps to \
the address bar.</li>\
<li>In the page, <b>Tab</b> selects the next link and <b>Enter</b> follows it. You can \
also click a link.</li>\
<li><b>Up</b>/<b>Down</b> or <b>j</b>/<b>k</b> scroll a line, <b>Space</b>/<b>b</b> a \
page, <b>Home</b>/<b>End</b> jump to the ends.</li>\
<li><b>Backspace</b> goes back.</li>\
</ul>\
<h2>What it can show</h2>\
<p>Headings, paragraphs, lists, links, preformatted text, bold and italic text, and \
rules. Other markup is ignored. Files that are not HTML are shown as plain text.</p>\
<p><a href=\"about:home\">Back to the start page</a></p>";

const ABOUT: &[u8] = b"<title>About ZOS</title>\
<h1>About ZOS</h1>\
<p>ZOS is a small microkernel operating system written in Rust. The kernel only does \
memory, scheduling and message passing; drivers, the filesystem, the shell, the \
window manager and this browser all run as separate programs.</p>\
<p>This browser is the first version. It reads pages from the ZOS filesystem. Loading \
pages from the internet needs a network stack, which ZOS does not have yet.</p>\
<p><a href=\"about:home\">Back to the start page</a></p>";

const NO_NETWORK: &[u8] = b"<title>No network</title>\
<h1>Web addresses are not available yet</h1>\
<p>ZOS does not have a network stack yet, so this browser cannot load pages from the \
internet. It can show pages stored on this computer.</p>\
<p><a href=\"about:home\">Back to the start page</a></p>";

const MISSING: &[u8] = b"<title>Not found</title>\
<h1>That page could not be opened</h1>\
<p>The file does not exist or cannot be read.</p>\
<p><a href=\"about:home\">Back to the start page</a></p>";

struct Browser {
    page: &'static mut Page,
    file: &'static mut [u8; FILE],
    markup: &'static mut [u8; MARKUP],
    current: Address,
    typed: Address,
    editing: bool,
    /// The address bar was just entered: the first character typed replaces the whole
    /// address, as it does in other browsers (which select it all).
    replace_on_type: bool,
    scroll: usize,
    selected: Option<usize>,
    history: [Address; HISTORY],
    history_len: usize,
    message: &'static [u8],
}

// The large buffers are all zero, so they live in zero-filled memory and add nothing
// to the program file.
static mut PAGE: Page = Page::new();
static mut FILE_BUFFER: [u8; FILE] = [0; FILE];
static mut MARKUP_BUFFER: [u8; MARKUP] = [0; MARKUP];

/// Appends `text` to `out` at `at`, escaping `& < >` so it is shown as written.
fn escape(text: &[u8], out: &mut [u8], at: &mut usize) {
    for &byte in text {
        let piece: &[u8] = match byte {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            b => &[b][..],
        };
        if *at + piece.len() > out.len() {
            return;
        }
        out[*at..*at + piece.len()].copy_from_slice(piece);
        *at += piece.len();
    }
}

fn append(text: &[u8], out: &mut [u8], at: &mut usize) {
    let room = out.len() - *at;
    let n = text.len().min(room);
    out[*at..*at + n].copy_from_slice(&text[..n]);
    *at += n;
}

/// How the bytes of a file are shown.
enum Source {
    /// The file buffer holds HTML.
    Html(usize),
    /// The markup buffer holds generated HTML.
    Generated(usize),
    Static(&'static [u8]),
}

impl Browser {
    fn open_file(&mut self, path: &[u8]) -> Source {
        let Ok(entry) = api::stat(path) else {
            return Source::Static(MISSING);
        };
        if entry.kind == 1 {
            return self.directory(path);
        }
        let Ok(fd) = api::open(path, READ) else {
            return Source::Static(MISSING);
        };
        let mut length = 0;
        while length < FILE {
            let end = (length + zos_storage::PAYLOAD).min(FILE);
            match api::read(fd, &mut self.file[length..end]) {
                Ok(0) | Err(_) => break,
                Ok(count) => length += count,
            }
        }
        let _ = api::close(fd);
        let is_html = path.ends_with(b".html") || path.ends_with(b".htm");
        if is_html {
            return Source::Html(length);
        }
        // Anything else is shown as it is written.
        let mut at = 0;
        append(b"<pre>", &mut self.markup[..], &mut at);
        escape(&self.file[..length], &mut self.markup[..], &mut at);
        append(b"</pre>", &mut self.markup[..], &mut at);
        Source::Generated(at)
    }

    /// A page listing a directory, one link per entry.
    fn directory(&mut self, path: &[u8]) -> Source {
        let mut at = 0;
        append(b"<h1>Index of ", &mut self.markup[..], &mut at);
        escape(path, &mut self.markup[..], &mut at);
        append(b"</h1><ul>", &mut self.markup[..], &mut at);
        if path != b"/" {
            append(
                b"<li><a href=\"..\">..</a></li>",
                &mut self.markup[..],
                &mut at,
            );
        }
        let mut ordinal = 0;
        while let Ok(Some(entry)) = api::readdir(path, ordinal) {
            ordinal += 1;
            let name = &entry.name[..entry.length as usize];
            append(b"<li><a href=\"", &mut self.markup[..], &mut at);
            if path != b"/" {
                append(path, &mut self.markup[..], &mut at);
            }
            append(b"/", &mut self.markup[..], &mut at);
            append(name, &mut self.markup[..], &mut at);
            append(b"\">", &mut self.markup[..], &mut at);
            escape(name, &mut self.markup[..], &mut at);
            if entry.kind == 1 {
                append(b"/", &mut self.markup[..], &mut at);
            }
            append(b"</a></li>", &mut self.markup[..], &mut at);
            if ordinal > 60 {
                break;
            }
        }
        append(b"</ul>", &mut self.markup[..], &mut at);
        Source::Generated(at)
    }

    /// Loads `address` into the page.
    fn load(&mut self, address: Address) {
        let source = match address.target() {
            Target::About(b"home" | b"") => Source::Static(HOME),
            Target::About(b"help") => Source::Static(HELP),
            Target::About(b"zos") => Source::Static(ABOUT),
            Target::About(_) => Source::Static(MISSING),
            Target::Web(_) => Source::Static(NO_NETWORK),
            Target::File(path) => match nav::resolve(b"/", path) {
                Some(absolute) => {
                    let mut buffer = [0u8; nav::URL];
                    let length = absolute.as_bytes().len();
                    buffer[..length].copy_from_slice(absolute.as_bytes());
                    self.open_file(&buffer[..length])
                }
                None => Source::Static(MISSING),
            },
        };
        let (bytes, length): (&[u8], usize) = match source {
            Source::Html(length) => (&self.file[..], length),
            Source::Generated(length) => (&self.markup[..], length),
            Source::Static(text) => (text, text.len()),
        };
        self.page.layout(&bytes[..length], COLS);
        self.current = address;
        self.typed = address;
        self.scroll = 0;
        self.selected = None;
        let name = self.page.title();
        zos_apps::log(format_args!(
            "BROWSER loaded {} rows={} links={} title={}",
            core::str::from_utf8(address.as_bytes()).unwrap_or("?"),
            self.page.rows(),
            self.page.link_count(),
            core::str::from_utf8(name).unwrap_or("?")
        ));
        self.message = b"";
    }

    fn navigate(&mut self, address: Address) {
        if !self.current.is_empty() && self.history_len < HISTORY {
            self.history[self.history_len] = self.current;
            self.history_len += 1;
        } else if !self.current.is_empty() {
            self.history.copy_within(1.., 0);
            self.history[HISTORY - 1] = self.current;
        }
        self.load(address);
    }

    fn back(&mut self) {
        if self.history_len > 0 {
            self.history_len -= 1;
            let address = self.history[self.history_len];
            self.load(address);
        } else {
            self.message = b"Nothing to go back to";
        }
    }

    fn follow(&mut self, link: usize) {
        let Some(href) = self.page.href(link) else {
            return;
        };
        match nav::resolve(self.current.as_bytes(), href) {
            Some(address) => self.navigate(address),
            None => self.message = b"That link is not a valid address",
        }
    }

    fn max_scroll(&self) -> usize {
        self.page.rows().saturating_sub(VISIBLE)
    }

    fn scroll_to(&mut self, row: usize) {
        self.scroll = row.min(self.max_scroll());
    }

    fn select_next(&mut self) {
        let count = self.page.link_count();
        if count == 0 {
            self.message = b"This page has no links";
            return;
        }
        let next = self.selected.map_or(0, |i| (i + 1) % count);
        self.selected = Some(next);
        if let Some(row) = self.page.link_row(next) {
            if row < self.scroll || row >= self.scroll + VISIBLE {
                self.scroll_to(row.saturating_sub(2));
            }
        }
    }

    fn key(&mut self, key: Key) {
        self.message = b"";
        if self.editing {
            match key {
                Key::Character(byte) => {
                    if core::mem::take(&mut self.replace_on_type) {
                        self.typed.clear();
                    }
                    self.typed.push(byte);
                }
                Key::Backspace => {
                    self.replace_on_type = false;
                    self.typed.pop()
                }
                Key::Enter => {
                    let typed = self.typed;
                    self.editing = false;
                    self.navigate(typed);
                }
                Key::Tab => self.editing = false,
                _ => (),
            }
            return;
        }
        match key {
            Key::Tab => self.select_next(),
            Key::Enter => {
                if let Some(link) = self.selected {
                    self.follow(link);
                }
            }
            Key::Clear => {
                self.editing = true;
                self.typed = self.current;
                self.replace_on_type = true;
            }
            Key::Backspace => self.back(),
            Key::Up | Key::Character(b'k') => self.scroll_to(self.scroll.saturating_sub(1)),
            Key::Down | Key::Character(b'j') => self.scroll_to(self.scroll + 1),
            Key::Character(b' ') => self.scroll_to(self.scroll + VISIBLE - 1),
            Key::Character(b'b') => self.scroll_to(self.scroll.saturating_sub(VISIBLE - 1)),
            Key::Home => self.scroll_to(0),
            Key::End => self.scroll_to(usize::MAX),
            Key::Character(b'/') => {
                self.editing = true;
                self.typed.clear();
            }
            _ => (),
        }
    }

    fn click(&mut self, x: i16, y: i16) {
        self.message = b"";
        if y < BAR {
            if !self.editing {
                self.typed = self.current;
                self.replace_on_type = true;
            }
            self.editing = true;
            return;
        }
        self.editing = false;
        if x < MARGIN || y < BAR {
            return;
        }
        let col = (x - MARGIN) as usize / 8;
        let row = (y - BAR) as usize / 16 + self.scroll;
        if let Some(link) = self.page.link_at(col, row) {
            self.selected = Some(link);
            self.follow(link);
        }
    }

    fn redraw(&self, window: &Window) {
        let _ = self.draw(window);
    }

    fn draw(&self, window: &Window) -> gui::Result<()> {
        window.clear(BACKGROUND)?;
        // Address bar.
        window.rect(0, 0, WIDTH, BAR as u16, CHROME)?;
        window.text(8, 6, INK, b"Go:")?;
        window.rect(
            40,
            3,
            WIDTH - 48,
            22,
            if self.editing { 0x0b3fb5 } else { 0x808080 },
        )?;
        window.rect(41, 4, WIDTH - 50, 20, 0xffffff)?;
        let shown = if self.editing {
            self.typed
        } else {
            self.current
        };
        let text = shown.as_bytes();
        let text = &text[text.len().saturating_sub(82)..];
        window.text(46, 6, INK, text)?;
        if self.editing {
            window.rect(46 + 8 * text.len() as i16, 6, 2, 14, INK)?;
        }
        // The page.
        let first = self.scroll;
        let last = first + VISIBLE;
        for run in self.page.runs() {
            let row = run.row as usize;
            if row < first || row >= last {
                continue;
            }
            let x = MARGIN + 8 * run.col as i16;
            let y = BAR + 2 + 16 * (row - first) as i16;
            if run.kind == Kind::Rule {
                window.rect(x, y + 7, 8 * run.len, 2, 0x909090)?;
                continue;
            }
            let selected = run.link != 0 && self.selected == Some(run.link as usize - 1);
            let text = self.page.text(run);
            let mut ink = match run.kind {
                Kind::Heading1 | Kind::Heading2 | Kind::Heading3 => HEADING,
                _ if run.link != 0 => LINK,
                _ if run.flags & CODE != 0 => CODE_INK,
                _ => INK,
            };
            if selected {
                window.rect(x, y, 8 * run.len, 16, LINK)?;
                ink = 0xffffff;
            }
            window.text(x, y, ink, text)?;
            if run.flags & BOLD != 0 {
                window.text(x + 1, y, ink, text)?;
            }
            if run.link != 0 && !selected {
                window.rect(x, y + 14, 8 * run.len, 1, LINK)?;
            }
        }
        // Status line.
        let y = HEIGHT as i16 - STATUS;
        window.rect(0, y, WIDTH, STATUS as u16, CHROME)?;
        let status: &[u8] = match (self.message.is_empty(), self.selected) {
            (false, _) => self.message,
            (true, Some(link)) => self.page.href(link).unwrap_or(b""),
            (true, None) if self.page.truncated => b"Page too large: showing the beginning",
            (true, None) => self.page.title(),
        };
        let status = &status[..status.len().min(80)];
        window.text(8, y + 2, INK, status)?;
        if self.page.rows() > VISIBLE {
            let mut digits = [0u8; 24];
            let percent = (self.scroll * 100 / self.max_scroll().max(1)) as i64;
            let text = zos_apps::calc::format(percent, &mut digits);
            window.text(WIDTH as i16 - 48, y + 2, INK, text)?;
            window.text(WIDTH as i16 - 48 + 8 * text.len() as i16, y + 2, INK, b"%")?;
        }
        window.present()
    }
}

#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    let Ok(window) = Window::create(b"ZOS Browser", WIDTH, HEIGHT) else {
        zos_apps::log(format_args!("BROWSER: no window"));
        os::exit(1)
    };
    // SAFETY: this program is single-threaded and these are the only references.
    let mut browser = unsafe {
        Browser {
            page: &mut *core::ptr::addr_of_mut!(PAGE),
            file: &mut *core::ptr::addr_of_mut!(FILE_BUFFER),
            markup: &mut *core::ptr::addr_of_mut!(MARKUP_BUFFER),
            current: Address::EMPTY,
            typed: Address::EMPTY,
            editing: true,
            replace_on_type: false,
            scroll: 0,
            selected: None,
            history: [Address::EMPTY; HISTORY],
            history_len: 0,
            message: b"",
        }
    };
    browser.navigate(Address::new(b"about:home").unwrap());
    browser.editing = false;
    zos_apps::log(format_args!("BROWSER ready"));
    loop {
        browser.redraw(&window);
        match window.wait() {
            Ok(Event::Key(key)) => browser.key(key),
            Ok(Event::Click { x, y, .. }) => browser.click(x, y),
            // The window is gone (closed by the user, or never ours): stop.
            Ok(Event::Close) | Err(_) => os::exit(0),
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    zos_apps::fail(info)
}
