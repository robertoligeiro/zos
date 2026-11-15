//! The ZOS shell language: a bounded, allocation-free lexer and parser.
//!
//! Supported syntax: simple commands, `NAME=value` assignments, single and double
//! quotes, backslash escapes, `#` comments, `;`, `&` (background), `&&`, `||`, a
//! leading `!`, pipelines with `|`, and the redirections `>`, `>>`, `<`, `2>`,
//! `2>>` and `2>&1`. Quote context is kept in the stored words as marker bytes so
//! that variables and patterns can be expanded later, when each command runs
//! (see `env::expand`); the parser itself never expands anything.
//!
//! Marker bytes (all below 32, which printable input can never contain):
//! `LIT`..`LIT_END` enclose text that must stay literal (single quotes, escapes),
//! `DQ`..`DQ_END` enclose double-quoted text (variables expand, patterns do not).

use zos_storage::{Error, Result};

pub const WORDS: usize = 12;
pub const STAGES: usize = 4;
pub const ITEMS: usize = 8;
pub const ASSIGNS: usize = 4;
pub const LIMIT: usize = zos_console::editor::CAPACITY;
const ARENA: usize = 2 * LIMIT + 16;
const NAME: usize = 16;

pub const LIT: u8 = 1;
pub const LIT_END: u8 = 2;
pub const DQ: u8 = 3;
pub const DQ_END: u8 = 4;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Span {
    start: u16,
    length: u16,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Assign {
    pub name: Span,
    pub value: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub path: Span,
    pub append: bool,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Redirects {
    pub output: Option<Target>,
    pub input: Option<Span>,
    pub error: Option<Target>,
    pub error_to_output: bool,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Command {
    pub words: [Span; WORDS],
    pub count: usize,
    pub assigns: [Assign; ASSIGNS],
    pub assign_count: usize,
    pub redirects: Redirects,
}

impl Command {
    fn is_empty(&self) -> bool {
        self.count == 0 && self.assign_count == 0
    }
    /// Redirections with no command to apply them to (`> f`) are an error.
    fn only_redirects(&self) -> bool {
        let r = &self.redirects;
        self.is_empty()
            && (r.output.is_some() || r.input.is_some() || r.error.is_some() || r.error_to_output)
    }
}

/// How a pipeline is joined to the one after it.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum Connector {
    #[default]
    End,
    Seq,
    And,
    Or,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Pipeline {
    pub commands: [Command; STAGES],
    pub count: usize,
    pub negate: bool,
    pub background: bool,
    pub connector: Connector,
}

pub struct Parsed {
    arena: [u8; ARENA],
    pub pipelines: [Pipeline; ITEMS],
    pub count: usize,
}

impl Parsed {
    /// The stored (quote-marked, unexpanded) bytes of a word.
    pub fn word(&self, span: Span) -> &[u8] {
        &self.arena[span.start as usize..(span.start + span.length) as usize]
    }
    /// A readable form of a word for messages: markers removed.
    pub fn plain(&self, span: Span, out: &mut [u8]) -> usize {
        let mut length = 0;
        for &byte in self.word(span) {
            if byte >= 32 && length < out.len() {
                out[length] = byte;
                length += 1;
            }
        }
        length
    }
    pub fn pipelines(&self) -> &[Pipeline] {
        &self.pipelines[..self.count]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Semi,
    Amp,
    AndAnd,
    OrOr,
    Pipe,
    Less,
    Great,
    GreatGreat,
    ErrGreat,
    ErrGreatGreat,
    ErrToOut,
}

enum Token {
    Word(Span),
    Op(Op),
    End,
}

struct Lexer<'a> {
    input: &'a [u8],
    position: usize,
    arena: [u8; ARENA],
    length: usize,
}

impl Lexer<'_> {
    fn push(&mut self, byte: u8) -> Result<()> {
        if self.length == ARENA {
            return Err(Error::TooLarge);
        }
        self.arena[self.length] = byte;
        self.length += 1;
        Ok(())
    }
    fn literal(&mut self, byte: u8) -> Result<()> {
        self.push(LIT)?;
        self.push(byte)?;
        self.push(LIT_END)
    }
    fn operator(&mut self) -> Option<Op> {
        let rest = &self.input[self.position..];
        let (op, length) = if rest.starts_with(b"2>&1") && !rest.get(4).is_some_and(is_word_byte) {
            (Op::ErrToOut, 4)
        } else if rest.starts_with(b"2>>") {
            (Op::ErrGreatGreat, 3)
        } else if rest.starts_with(b"2>") {
            (Op::ErrGreat, 2)
        } else if rest.starts_with(b"&&") {
            (Op::AndAnd, 2)
        } else if rest.starts_with(b"||") {
            (Op::OrOr, 2)
        } else if rest.starts_with(b">>") {
            (Op::GreatGreat, 2)
        } else {
            match rest[0] {
                b';' => (Op::Semi, 1),
                b'&' => (Op::Amp, 1),
                b'|' => (Op::Pipe, 1),
                b'<' => (Op::Less, 1),
                b'>' => (Op::Great, 1),
                _ => return None,
            }
        };
        self.position += length;
        Some(op)
    }
    fn next(&mut self) -> Result<Token> {
        while self.input.get(self.position) == Some(&b' ') {
            self.position += 1;
        }
        let Some(&first) = self.input.get(self.position) else {
            return Ok(Token::End);
        };
        if first == b'#' {
            self.position = self.input.len();
            return Ok(Token::End);
        }
        if let Some(op) = self.operator() {
            return Ok(Token::Op(op));
        }
        let start = self.length;
        while let Some(&byte) = self.input.get(self.position) {
            match byte {
                b' ' | b';' | b'&' | b'|' | b'<' | b'>' => break,
                b'\'' => {
                    self.position += 1;
                    self.push(LIT)?;
                    loop {
                        match self.input.get(self.position) {
                            None => return Err(Error::Invalid),
                            Some(b'\'') => break,
                            Some(&c) => self.push(c)?,
                        }
                        self.position += 1;
                    }
                    self.position += 1;
                    self.push(LIT_END)?;
                }
                b'"' => {
                    self.position += 1;
                    self.push(DQ)?;
                    loop {
                        match self.input.get(self.position) {
                            None => return Err(Error::Invalid),
                            Some(b'"') => break,
                            Some(b'\\') => {
                                let next = self.input.get(self.position + 1).copied();
                                if let Some(c @ (b'"' | b'\\' | b'$')) = next {
                                    self.literal(c)?;
                                    self.position += 2;
                                } else {
                                    self.push(b'\\')?;
                                    self.position += 1;
                                }
                            }
                            Some(&c) => {
                                self.push(c)?;
                                self.position += 1;
                            }
                        }
                    }
                    self.position += 1;
                    self.push(DQ_END)?;
                }
                b'\\' => {
                    self.position += 1;
                    let escaped = *self.input.get(self.position).ok_or(Error::Invalid)?;
                    self.literal(escaped)?;
                    self.position += 1;
                }
                _ => {
                    self.push(byte)?;
                    self.position += 1;
                }
            }
        }
        Ok(Token::Word(Span {
            start: start as u16,
            length: (self.length - start) as u16,
        }))
    }
}

fn is_word_byte(byte: &u8) -> bool {
    !matches!(byte, b' ' | b';' | b'&' | b'|' | b'<' | b'>')
}

/// `NAME=value` with a plain (unquoted) name, otherwise `None`.
fn assignment(arena: &[u8], span: Span) -> Option<Assign> {
    let word = &arena[span.start as usize..(span.start + span.length) as usize];
    let first = *word.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let end = word
        .iter()
        .position(|&b| !(b.is_ascii_alphanumeric() || b == b'_'))?;
    if word[end] != b'=' || end > NAME {
        return None;
    }
    Some(Assign {
        name: Span {
            start: span.start,
            length: end as u16,
        },
        value: Span {
            start: span.start + end as u16 + 1,
            length: span.length - end as u16 - 1,
        },
    })
}

fn push_stage(pipeline: &mut Pipeline, command: &mut Command) -> Result<()> {
    if pipeline.count == STAGES {
        return Err(Error::TooLarge);
    }
    pipeline.commands[pipeline.count] = core::mem::take(command);
    pipeline.count += 1;
    Ok(())
}

pub fn parse(input: &[u8]) -> Result<Parsed> {
    if input.len() > LIMIT {
        return Err(Error::TooLarge);
    }
    if input.iter().any(|&b| !(32..=126).contains(&b)) {
        return Err(Error::Invalid);
    }
    let mut lexer = Lexer {
        input,
        position: 0,
        arena: [0; ARENA],
        length: 0,
    };
    let mut parsed = Parsed {
        arena: [0; ARENA],
        pipelines: [Pipeline::default(); ITEMS],
        count: 0,
    };
    let mut pipeline = Pipeline::default();
    let mut command = Command::default();
    // What the next word must be, when a redirection operator was just read.
    let mut pending: Option<Op> = None;
    let mut assigning = true;
    let mut last = Connector::Seq;
    loop {
        match lexer.next()? {
            Token::Word(span) => {
                if let Some(kind) = pending.take() {
                    if span.length == 0 {
                        return Err(Error::Invalid);
                    }
                    let redirects = &mut command.redirects;
                    let target = Target {
                        path: span,
                        append: matches!(kind, Op::GreatGreat | Op::ErrGreatGreat),
                    };
                    let taken = match kind {
                        Op::Less => redirects.input.replace(span).is_some(),
                        Op::Great | Op::GreatGreat => redirects.output.replace(target).is_some(),
                        _ => redirects.error.replace(target).is_some(),
                    };
                    if taken {
                        return Err(Error::Invalid);
                    }
                    continue;
                }
                let bytes = &lexer.arena[span.start as usize..(span.start + span.length) as usize];
                if bytes == b"!" && pipeline.count == 0 && command.is_empty() && !pipeline.negate {
                    pipeline.negate = true;
                    continue;
                }
                if assigning {
                    if let Some(assign) = assignment(&lexer.arena, span) {
                        if command.assign_count == ASSIGNS {
                            return Err(Error::TooLarge);
                        }
                        command.assigns[command.assign_count] = assign;
                        command.assign_count += 1;
                        continue;
                    }
                    assigning = false;
                }
                if command.count == WORDS {
                    return Err(Error::TooLarge);
                }
                command.words[command.count] = span;
                command.count += 1;
            }
            Token::Op(
                op @ (Op::Less | Op::Great | Op::GreatGreat | Op::ErrGreat | Op::ErrGreatGreat),
            ) => {
                if pending.is_some() {
                    return Err(Error::Invalid);
                }
                pending = Some(op);
            }
            Token::Op(Op::ErrToOut) => {
                if pending.is_some() || command.redirects.error_to_output {
                    return Err(Error::Invalid);
                }
                command.redirects.error_to_output = true;
            }
            Token::Op(Op::Pipe) => {
                if pending.is_some() || command.is_empty() || command.only_redirects() {
                    return Err(Error::Invalid);
                }
                push_stage(&mut pipeline, &mut command)?;
                assigning = true;
            }
            Token::Op(op @ (Op::Semi | Op::Amp | Op::AndAnd | Op::OrOr)) => {
                if pending.is_some() || command.is_empty() {
                    return Err(Error::Invalid);
                }
                push_stage(&mut pipeline, &mut command)?;
                pipeline.background = op == Op::Amp;
                pipeline.connector = match op {
                    Op::AndAnd => Connector::And,
                    Op::OrOr => Connector::Or,
                    _ => Connector::Seq,
                };
                last = pipeline.connector;
                if parsed.count == ITEMS {
                    return Err(Error::TooLarge);
                }
                parsed.pipelines[parsed.count] = core::mem::take(&mut pipeline);
                parsed.count += 1;
                assigning = true;
            }
            Token::End => {
                if pending.is_some() || command.only_redirects() {
                    return Err(Error::Invalid);
                }
                if !command.is_empty() {
                    push_stage(&mut pipeline, &mut command)?;
                    pipeline.connector = Connector::End;
                    if parsed.count == ITEMS {
                        return Err(Error::TooLarge);
                    }
                    parsed.pipelines[parsed.count] = pipeline;
                    parsed.count += 1;
                } else if pipeline.count != 0
                    || pipeline.negate
                    || matches!(last, Connector::And | Connector::Or)
                {
                    // A dangling `|`, `!`, `&&` or `||`: the command is incomplete.
                    return Err(Error::Invalid);
                } else if parsed.count != 0 {
                    // A trailing `;` or `&` ends the previous pipeline.
                    parsed.pipelines[parsed.count - 1].connector = Connector::End;
                }
                break;
            }
        }
    }
    parsed.arena = lexer.arena;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{string::String, vec::Vec};

    fn plain(parsed: &Parsed, span: Span) -> String {
        let mut bytes = [0; 128];
        let length = parsed.plain(span, &mut bytes);
        String::from_utf8(bytes[..length].to_vec()).unwrap()
    }
    fn words(parsed: &Parsed, command: &Command) -> Vec<String> {
        command.words[..command.count]
            .iter()
            .map(|&s| plain(parsed, s))
            .collect()
    }

    #[test]
    fn quotes_escapes_and_empty_arguments_keep_their_context() {
        let parsed = parse(br#"echo 'a; > b' "x\"y" '' z\ z"#).unwrap();
        let command = &parsed.pipelines[0].commands[0];
        assert_eq!(
            words(&parsed, command),
            ["echo", "a; > b", "x\"y", "", "z z"]
        );
        // The stored form keeps what must not be expanded inside LIT markers.
        assert_eq!(
            parsed.word(command.words[1]),
            [&[LIT][..], b"a; > b", &[LIT_END]].concat()
        );
        assert_eq!(parsed.word(command.words[3]), [LIT, LIT_END]);
        assert_eq!(
            parsed.word(command.words[4]),
            [&b"z"[..], &[LIT], b" ", &[LIT_END], b"z"].concat()
        );
        let dollar = parse(br#"echo "$A" '$A' \$A"#).unwrap();
        let c = &dollar.pipelines[0].commands[0];
        assert_eq!(
            dollar.word(c.words[1]),
            [&[DQ][..], b"$A", &[DQ_END]].concat()
        );
        assert_eq!(
            dollar.word(c.words[2]),
            [&[LIT][..], b"$A", &[LIT_END]].concat()
        );
        assert_eq!(
            dollar.word(c.words[3]),
            [&[LIT][..], b"$", &[LIT_END], b"A"].concat()
        );
    }

    #[test]
    fn sequences_connectors_background_negation_and_comments() {
        let parsed = parse(b"mkdir a; cd a; echo hi > f; cat f").unwrap();
        assert_eq!(parsed.count, 4);
        assert!(parsed
            .pipelines()
            .iter()
            .take(3)
            .all(|p| p.connector == Connector::Seq));
        assert_eq!(parsed.pipelines[3].connector, Connector::End);
        let target = parsed.pipelines[2].commands[0].redirects.output.unwrap();
        assert_eq!(plain(&parsed, target.path), "f");
        assert!(!target.append);

        let parsed = parse(b"a && b || c; d &").unwrap();
        let connectors: Vec<_> = parsed.pipelines().iter().map(|p| p.connector).collect();
        // A trailing `&` or `;` ends the list.
        assert_eq!(
            connectors,
            [
                Connector::And,
                Connector::Or,
                Connector::Seq,
                Connector::End
            ]
        );
        assert!(parsed.pipelines[3].background && !parsed.pipelines[2].background);

        let parsed = parse(b"! false # not run, a comment; echo no").unwrap();
        assert_eq!(parsed.count, 1);
        assert!(parsed.pipelines[0].negate);
        assert_eq!(words(&parsed, &parsed.pipelines[0].commands[0]), ["false"]);
        // A '#' inside a word is not a comment.
        let parsed = parse(b"echo a#b").unwrap();
        assert_eq!(
            words(&parsed, &parsed.pipelines[0].commands[0]),
            ["echo", "a#b"]
        );
        // A trailing separator is fine; a leading one is not.
        assert_eq!(
            parse(b"echo hi;").unwrap().pipelines[0].connector,
            Connector::End
        );
        assert!(parse(b"; ls").is_err());
        assert_eq!(parse(b"   ").unwrap().count, 0);
        assert_eq!(parse(b"# only a comment").unwrap().count, 0);
    }

    #[test]
    fn assignments_apply_only_before_the_command_word() {
        let parsed = parse(b"A=1 B='x y' env C=3").unwrap();
        let command = &parsed.pipelines[0].commands[0];
        assert_eq!(command.assign_count, 2);
        assert_eq!(plain(&parsed, command.assigns[0].name), "A");
        assert_eq!(plain(&parsed, command.assigns[1].value), "x y");
        assert_eq!(words(&parsed, command), ["env", "C=3"]);
        let alone = parse(b"A=1").unwrap();
        assert_eq!(alone.pipelines[0].commands[0].count, 0);
        // Quoted or invalid names are ordinary words.
        for text in [&b"'A'=1"[..], b"1A=1", b"A-B=1", b"=1"] {
            let p = parse(text).unwrap();
            assert_eq!(p.pipelines[0].commands[0].assign_count, 0, "{text:?}");
        }
        assert!(parse(b"A=1 B=2 C=3 D=4 E=5").is_err());
    }

    #[test]
    fn pipelines_and_every_redirection_form_parse() {
        let parsed = parse(b"cat < in | sort 2> err | uniq >> out 2>&1").unwrap();
        let p = &parsed.pipelines[0];
        assert_eq!(p.count, 3);
        assert_eq!(plain(&parsed, p.commands[0].redirects.input.unwrap()), "in");
        let error = p.commands[1].redirects.error.unwrap();
        assert!(!error.append);
        assert_eq!(plain(&parsed, error.path), "err");
        assert!(p.commands[2].redirects.output.unwrap().append);
        assert!(p.commands[2].redirects.error_to_output);
        assert!(
            parse(b"echo x 2>> log").unwrap().pipelines[0].commands[0]
                .redirects
                .error
                .unwrap()
                .append
        );
        // '2>' only counts at the start of a word; elsewhere it is text then '>'.
        let odd = parse(b"echo a2>f").unwrap();
        assert_eq!(words(&odd, &odd.pipelines[0].commands[0]), ["echo", "a2"]);
        assert!(odd.pipelines[0].commands[0].redirects.output.is_some());
    }

    #[test]
    fn malformed_input_and_limits_are_explicit() {
        for input in [
            b"echo 'bad".as_slice(),
            b"echo \"bad",
            b"echo \\",
            b"> f",
            b"echo >",
            b"echo > f > g",
            b"echo < a < b",
            b"echo >>> f",
            b"a &&",
            b"a ||",
            b"a |",
            b"| a",
            b"&& a",
            b"!",
            b"a | | b",
            b"a ;; b",
            b"echo 2>&1 2>&1",
        ] {
            assert!(
                parse(input).is_err(),
                "{:?}",
                String::from_utf8_lossy(input)
            );
        }
        assert!(parse(&[b'x'; LIMIT + 1]).is_err());
        assert!(parse(b"x x x x x x x x x x x x x").is_err());
        assert!(parse(b"x;x;x;x;x;x;x;x;x").is_err());
        assert!(parse(b"a|b|c|d|e").is_err());
        assert!(parse(b"echo \x01").is_err());
        // The worst case for stored bytes (every byte escaped) fits the arena.
        let mut text = std::vec![b'a'; 0];
        for _ in 0..LIMIT / 2 {
            text.extend_from_slice(b"\\a");
        }
        assert!(parse(&text).is_ok());
    }
}
