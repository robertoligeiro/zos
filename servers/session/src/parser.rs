use zos_storage::{Error, Result};
const WORDS: usize = 12;
const COMMANDS: usize = 8;
pub const LIMIT: usize = zos_console::editor::CAPACITY;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Span {
    start: usize,
    length: usize,
}
#[derive(Clone, Copy, Default, Debug)]
pub struct Command {
    pub arguments: [Span; WORDS],
    pub count: usize,
    pub output: Option<Span>,
    pub append: bool,
    pub background: bool,
}
pub struct Parsed {
    bytes: [u8; LIMIT],
    length: usize,
    pub commands: [Command; COMMANDS],
    pub count: usize,
}
impl Parsed {
    pub fn word(&self, span: Span) -> &[u8] {
        &self.bytes[span.start..span.start + span.length]
    }
    fn add_word(&mut self, command: &mut Command, span: Span, redirect: &mut bool) -> Result<()> {
        if *redirect {
            if span.length == 0 {
                return Err(Error::Invalid);
            }
            command.output = Some(span);
            *redirect = false;
        } else {
            if command.count == WORDS {
                return Err(Error::TooLarge);
            }
            command.arguments[command.count] = span;
            command.count += 1;
        }
        Ok(())
    }
    fn add_command(&mut self, command: &mut Command, redirect: bool) -> Result<()> {
        if command.count == 0 || redirect {
            return Err(Error::Invalid);
        }
        if self.count == COMMANDS {
            return Err(Error::TooLarge);
        }
        self.commands[self.count] = *command;
        self.count += 1;
        *command = Command::default();
        Ok(())
    }
}

pub fn parse(input: &[u8]) -> Result<Parsed> {
    if input.len() > LIMIT {
        return Err(Error::TooLarge);
    }
    if input.iter().any(|&b| !(32..=126).contains(&b)) {
        return Err(Error::Invalid);
    }
    let mut parsed = Parsed {
        bytes: [0; LIMIT],
        length: 0,
        commands: [Command::default(); COMMANDS],
        count: 0,
    };
    let mut command = Command::default();
    let mut redirect = false;
    let mut position = 0;
    while position < input.len() {
        match input[position] {
            b' ' => {
                position += 1;
                continue;
            }
            b';' | b'&' => {
                command.background = input[position] == b'&';
                parsed.add_command(&mut command, redirect)?;
                position += 1;
                continue;
            }
            b'>' => {
                if redirect || command.output.is_some() {
                    return Err(Error::Invalid);
                }
                redirect = true;
                position += 1;
                command.append = input.get(position) == Some(&b'>');
                position += usize::from(command.append);
                continue;
            }
            _ => (),
        }
        let start = parsed.length;
        let mut quote = None;
        while position < input.len() {
            let byte = input[position];
            if quote.is_none() && matches!(byte, b' ' | b';' | b'>' | b'&') {
                break;
            }
            if matches!(byte, b'\'' | b'"') && (quote.is_none() || quote == Some(byte)) {
                quote = if quote.is_none() { Some(byte) } else { None };
                position += 1;
                continue;
            }
            let value = if byte == b'\\' && quote != Some(b'\'') {
                position += 1;
                *input.get(position).ok_or(Error::Invalid)?
            } else {
                byte
            };
            parsed.bytes[parsed.length] = value;
            parsed.length += 1;
            position += 1;
        }
        if quote.is_some() {
            return Err(Error::Invalid);
        }
        parsed.add_word(
            &mut command,
            Span {
                start,
                length: parsed.length - start,
            },
            &mut redirect,
        )?;
    }
    if command.count != 0 || redirect || command.output.is_some() {
        parsed.add_command(&mut command, redirect)?;
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quotes_escapes_empty_arguments_and_literal_operators() {
        let parsed = parse(br#"echo 'a; > b' "x\"y" '' z\ z"#).unwrap();
        let command = parsed.commands[0];
        let args: std::vec::Vec<_> = command.arguments[..command.count]
            .iter()
            .map(|&s| parsed.word(s))
            .collect();
        assert_eq!(args, [b"echo".as_slice(), b"a; > b", b"x\"y", b"", b"z z"]);
    }
    #[test]
    fn sequence_redirect_append_and_background() {
        let parsed = parse(b"mkdir a; cd a; echo hi > f; cat f; ps; free").unwrap();
        assert_eq!(parsed.count, 6);
        assert_eq!(parsed.word(parsed.commands[2].output.unwrap()), b"f");
        assert!(!parsed.commands[2].append);
        let parsed = parse(b"echo x >> f; spin &").unwrap();
        assert!(parsed.commands[0].append);
        assert!(parsed.commands[1].background);
    }
    #[test]
    fn malformed_input_and_limits_are_explicit() {
        for input in [
            b"echo 'bad".as_slice(),
            b"echo \\",
            b"> f",
            b"echo >",
            b"; ls",
            b"echo > f > g",
            b"echo >>> f",
        ] {
            assert!(parse(input).is_err(), "{input:?}");
        }
        assert!(parse(&[b'x'; LIMIT + 1]).is_err());
        assert!(parse(b"x x x x x x x x x x x x x").is_err());
        assert!(parse(b"x;x;x;x;x;x;x;x;x").is_err());
    }
}
