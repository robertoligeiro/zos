//! Shell variables and word expansion.
//!
//! Expansion happens when a command runs, using the quote markers the parser
//! stored in each word. Differences from Bash, on purpose: no word splitting of
//! expanded values (a value stays one word), no `$(...)`, and no arrays.

use crate::parser::{DQ, DQ_END, LIT, LIT_END};
use zos_storage::{Error, Result};

pub const VARS: usize = 32;
pub const NAME: usize = 16;
pub const VALUE: usize = 64;

#[derive(Clone, Copy)]
struct Var {
    name: [u8; NAME],
    name_length: u8,
    value: [u8; VALUE],
    value_length: u8,
    exported: bool,
    used: bool,
}

const UNUSED: Var = Var {
    name: [0; NAME],
    name_length: 0,
    value: [0; VALUE],
    value_length: 0,
    exported: false,
    used: false,
};

#[derive(Clone)]
pub struct Env {
    vars: [Var; VARS],
    /// Status of the last command (`$?`).
    pub status: i64,
}

pub fn valid_name(name: &[u8]) -> bool {
    name.len() <= NAME
        && name
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    pub fn new() -> Self {
        Self {
            vars: [UNUSED; VARS],
            status: 0,
        }
    }
    fn find(&self, name: &[u8]) -> Option<usize> {
        self.vars
            .iter()
            .position(|v| v.used && &v.name[..v.name_length as usize] == name)
    }
    pub fn get(&self, name: &[u8]) -> Option<&[u8]> {
        self.find(name)
            .map(|i| &self.vars[i].value[..self.vars[i].value_length as usize])
    }
    /// Sets a variable. `export` of `None` keeps the current export state (new
    /// variables start unexported); `Some(flag)` sets it.
    pub fn set(&mut self, name: &[u8], value: &[u8], export: Option<bool>) -> Result<()> {
        if !valid_name(name) {
            return Err(Error::Invalid);
        }
        if value.len() > VALUE || value.iter().any(|b| !(32..=126).contains(b)) {
            return Err(Error::TooLarge);
        }
        let slot = match self.find(name) {
            Some(i) => i,
            None => self
                .vars
                .iter()
                .position(|v| !v.used)
                .ok_or(Error::NoSpace)?,
        };
        let var = &mut self.vars[slot];
        if !var.used {
            *var = UNUSED;
            var.used = true;
            var.name[..name.len()].copy_from_slice(name);
            var.name_length = name.len() as u8;
        }
        var.value = [0; VALUE];
        var.value[..value.len()].copy_from_slice(value);
        var.value_length = value.len() as u8;
        if let Some(flag) = export {
            var.exported = flag;
        }
        Ok(())
    }
    /// Marks an existing or new (empty) variable exported or not.
    pub fn export(&mut self, name: &[u8], exported: bool) -> Result<()> {
        if self.find(name).is_none() {
            self.set(name, b"", None)?;
        }
        let slot = self.find(name).ok_or(Error::NotFound)?;
        self.vars[slot].exported = exported;
        Ok(())
    }
    pub fn unset(&mut self, name: &[u8]) -> bool {
        match self.find(name) {
            Some(i) => {
                self.vars[i] = UNUSED;
                true
            }
            None => false,
        }
    }
    /// Every variable as (name, value, exported), in slot order.
    pub fn all(&self) -> impl Iterator<Item = (&[u8], &[u8], bool)> {
        self.vars.iter().filter(|v| v.used).map(|v| {
            (
                &v.name[..v.name_length as usize],
                &v.value[..v.value_length as usize],
                v.exported,
            )
        })
    }
}

pub struct Expanded {
    pub length: usize,
    /// An unquoted `*`, `?` or `[` was present: the word is a filename pattern.
    pub glob: bool,
}

struct Out<'a> {
    bytes: &'a mut [u8],
    length: usize,
}

impl Out<'_> {
    fn push(&mut self, byte: u8) -> Result<()> {
        *self.bytes.get_mut(self.length).ok_or(Error::TooLarge)? = byte;
        self.length += 1;
        Ok(())
    }
    fn extend(&mut self, bytes: &[u8]) -> Result<()> {
        bytes.iter().try_for_each(|&b| self.push(b))
    }
}

fn decimal(mut value: i64, out: &mut [u8; 20]) -> &[u8] {
    let negative = value < 0;
    let mut position = out.len();
    loop {
        position -= 1;
        out[position] = b'0' + (value % 10).unsigned_abs() as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    if negative {
        position -= 1;
        out[position] = b'-';
    }
    &out[position..]
}

/// Expands one stored word into `out`. With `pattern` set, literal
/// `*`, `?`, `[` and `\` are emitted escaped with a backslash so that the result
/// can be handed to `glob_match`; otherwise everything is emitted as plain text.
pub fn expand(word: &[u8], env: &Env, out: &mut [u8], pattern: bool) -> Result<Expanded> {
    let mut out = Out {
        bytes: out,
        length: 0,
    };
    let (mut literal, mut quoted, mut glob) = (false, false, false);
    let mut position = 0;
    while position < word.len() {
        let byte = word[position];
        position += 1;
        match byte {
            LIT => literal = true,
            LIT_END => literal = false,
            DQ => quoted = true,
            DQ_END => quoted = false,
            _ if literal || quoted => {
                if !literal && byte == b'$' {
                    position += variable(&word[position..], env, &mut out)?;
                } else {
                    if pattern && matches!(byte, b'*' | b'?' | b'[' | b'\\') {
                        out.push(b'\\')?;
                    }
                    out.push(byte)?;
                }
            }
            b'$' => position += variable(&word[position..], env, &mut out)?,
            b'*' | b'?' | b'[' => {
                glob = true;
                out.push(byte)?;
            }
            _ => {
                if pattern && byte == b'\\' {
                    out.push(b'\\')?;
                }
                out.push(byte)?;
            }
        }
    }
    Ok(Expanded {
        length: out.length,
        glob,
    })
}

/// Expands the parameter after a `$`; returns how many bytes of `rest` it used.
fn variable(rest: &[u8], env: &Env, out: &mut Out<'_>) -> Result<usize> {
    match rest.first() {
        Some(b'?') => {
            let mut digits = [0; 20];
            out.extend(decimal(env.status, &mut digits))?;
            Ok(1)
        }
        Some(b'#') => {
            out.push(b'0')?;
            Ok(1)
        }
        Some(b'{') => {
            let end = rest.iter().position(|&b| b == b'}').ok_or(Error::Invalid)?;
            let name = &rest[1..end];
            if !valid_name(name) {
                return Err(Error::Invalid);
            }
            out.extend(env.get(name).unwrap_or(b""))?;
            Ok(end + 1)
        }
        Some(&first) if first.is_ascii_alphabetic() || first == b'_' => {
            let length = rest
                .iter()
                .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
                .unwrap_or(rest.len());
            if length > NAME {
                return Err(Error::TooLarge);
            }
            out.extend(env.get(&rest[..length]).unwrap_or(b""))?;
            Ok(length)
        }
        // `$0`..`$9` (no positional parameters yet) expand to nothing.
        Some(digit) if digit.is_ascii_digit() => Ok(1),
        // A `$` that starts no parameter is an ordinary character.
        _ => {
            out.push(b'$')?;
            Ok(0)
        }
    }
}

/// Filename pattern matching: `*`, `?`, `[abc]`, `[a-z]`, `[!abc]` and `\x`.
pub fn glob_match(pattern: &[u8], name: &[u8]) -> bool {
    // Names starting with '.' only match patterns that start with a literal '.'.
    if name.first() == Some(&b'.') && pattern.first() != Some(&b'.') {
        return false;
    }
    let (mut p, mut n) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        let step = match pattern.get(p) {
            Some(b'*') => {
                backtrack = Some((p, n));
                p += 1;
                continue;
            }
            Some(b'?') => Some(1),
            Some(b'[') => class(&pattern[p..], name[n]),
            Some(b'\\') => (pattern.get(p + 1) == Some(&name[n])).then_some(2),
            Some(&c) => (c == name[n]).then_some(1),
            None => None,
        };
        match step {
            Some(consumed) => {
                p += consumed;
                n += 1;
            }
            None => match backtrack {
                Some((star, at)) => {
                    backtrack = Some((star, at + 1));
                    p = star + 1;
                    n = at + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&b| b == b'*')
}

/// Matches `byte` against the bracket expression at the start of `pattern`;
/// returns how many pattern bytes it spans on a match. An unterminated
/// expression matches nothing.
fn class(pattern: &[u8], byte: u8) -> Option<usize> {
    let mut i = 1;
    let negate = matches!(pattern.get(i), Some(b'!' | b'^'));
    i += usize::from(negate);
    let first = i;
    let mut matched = false;
    loop {
        let low = *pattern.get(i)?;
        if low == b']' && i > first {
            return (matched != negate).then_some(i + 1);
        }
        if pattern.get(i + 1) == Some(&b'-') && pattern.get(i + 2).is_some_and(|&c| c != b']') {
            let high = pattern[i + 2];
            matched |= (low..=high).contains(&byte);
            i += 3;
        } else {
            matched |= low == byte;
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::parser::parse;
    use std::{string::String, vec::Vec};

    fn run(env: &Env, line: &str) -> Vec<String> {
        let parsed = parse(line.as_bytes()).unwrap();
        let command = &parsed.pipelines[0].commands[0];
        command.words[..command.count]
            .iter()
            .map(|&w| {
                let mut out = [0; 256];
                let e = expand(parsed.word(w), env, &mut out, false).unwrap();
                String::from_utf8(out[..e.length].to_vec()).unwrap()
            })
            .collect()
    }

    fn env() -> Env {
        let mut env = Env::new();
        env.set(b"A", b"one", None).unwrap();
        env.set(b"SP", b"x y", None).unwrap();
        env.status = 3;
        env
    }

    #[test]
    fn variables_follow_quote_context() {
        let env = env();
        assert_eq!(
            run(&env, "echo $A ${A}x $A$A"),
            ["echo", "one", "onex", "oneone"]
        );
        assert_eq!(
            run(&env, r#"echo "$A!" '$A' \$A "a $SP b" $SP"#),
            ["echo", "one!", "$A", "$A", "a x y b", "x y"]
        );
        assert_eq!(
            run(&env, "echo $? $# $0 $9 $NOPE end"),
            ["echo", "3", "0", "", "", "", "end"]
        );
        // A lone or trailing '$' stays a '$'.
        assert_eq!(
            run(&env, r#"echo $ a$ "$" $-"#),
            ["echo", "$", "a$", "$", "$-"]
        );
        // Empty expansion still yields an (empty) argument; no word splitting.
        assert_eq!(run(&env, "echo $NOPE").len(), 2);
        assert_eq!(run(&env, "echo ${A}${A}"), ["echo", "oneone"]);
    }

    #[test]
    fn expansion_errors_are_explicit() {
        let env = env();
        let parsed = parse(b"echo ${A").unwrap();
        let w = parsed.pipelines[0].commands[0].words[1];
        assert!(expand(parsed.word(w), &env, &mut [0; 32], false).is_err());
        let parsed = parse(b"echo ${1X}").unwrap();
        let w = parsed.pipelines[0].commands[0].words[1];
        assert!(expand(parsed.word(w), &env, &mut [0; 32], false).is_err());
        // Output that does not fit is refused, never truncated.
        let parsed = parse(b"echo $SP$SP$SP").unwrap();
        let w = parsed.pipelines[0].commands[0].words[1];
        assert_eq!(
            expand(parsed.word(w), &env, &mut [0; 8], false).err(),
            Some(Error::TooLarge)
        );
    }

    #[test]
    fn globs_are_detected_only_when_unquoted() {
        let env = env();
        let check = |line: &str| {
            let parsed = parse(line.as_bytes()).unwrap();
            let w = parsed.pipelines[0].commands[0].words[1];
            let mut out = [0; 64];
            let e = expand(parsed.word(w), &env, &mut out, true).unwrap();
            (e.glob, String::from_utf8(out[..e.length].to_vec()).unwrap())
        };
        assert_eq!(check("ls *.txt"), (true, "*.txt".into()));
        assert_eq!(check("ls f?"), (true, "f?".into()));
        assert_eq!(check("ls '*.txt'"), (false, "\\*.txt".into()));
        assert_eq!(check(r#"ls "a*" "#), (false, "a\\*".into()));
        assert_eq!(check(r"ls a\*"), (false, "a\\*".into()));
        assert_eq!(check("ls $A*"), (true, "one*".into()));
        // A variable's own '*' is plain data, never a pattern.
        let mut e2 = Env::new();
        e2.set(b"G", b"*", None).unwrap();
        let parsed = parse(b"ls $G").unwrap();
        let w = parsed.pipelines[0].commands[0].words[1];
        let mut out = [0; 8];
        assert!(!expand(parsed.word(w), &e2, &mut out, false).unwrap().glob);
    }

    #[test]
    fn variable_table_has_bounds_and_export_state() {
        let mut env = Env::new();
        env.set(b"X", b"1", None).unwrap();
        assert!(!env.all().next().unwrap().2);
        env.export(b"X", true).unwrap();
        env.set(b"X", b"2", None).unwrap();
        assert_eq!(env.all().next().unwrap(), (&b"X"[..], &b"2"[..], true));
        env.export(b"NEW", true).unwrap();
        assert_eq!(env.get(b"NEW"), Some(&b""[..]));
        assert!(env.unset(b"X") && !env.unset(b"X"));
        assert_eq!(env.get(b"X"), None);
        for bad in [&b""[..], &b"1A"[..], &b"A-B"[..], &[b'A'; NAME + 1][..]] {
            assert!(env.set(bad, b"v", None).is_err());
        }
        assert!(env.set(b"BIG", &[b'v'; VALUE + 1], None).is_err());
        assert!(env.set(b"BIG", &[b'v'; VALUE], None).is_ok());
        assert!(env.set(b"NL", b"a\nb", None).is_err());
        let mut full = Env::new();
        for i in 0..VARS {
            full.set(std::format!("V{i}").as_bytes(), b"x", None)
                .unwrap();
        }
        assert_eq!(full.set(b"ONEMORE", b"x", None), Err(Error::NoSpace));
        assert!(full.set(b"V0", b"y", None).is_ok());
        assert!(full.unset(b"V5") && full.set(b"ONEMORE", b"x", None).is_ok());
    }

    #[test]
    fn glob_matching_covers_wildcards_classes_and_dotfiles() {
        let yes = |p: &str, n: &str| glob_match(p.as_bytes(), n.as_bytes());
        assert!(yes("*", "abc"));
        assert!(yes("a*c", "abc") && yes("a*c", "ac") && yes("a*c", "abxxc") && !yes("a*c", "abd"));
        assert!(yes("*.txt", "notes.txt") && !yes("*.txt", "notes.txt.bak"));
        assert!(yes("f?", "fx") && !yes("f?", "f") && !yes("f?", "fxy"));
        assert!(yes("[abc]x", "bx") && !yes("[abc]x", "dx"));
        assert!(yes("[a-c]x", "bx") && !yes("[a-c]x", "dx"));
        assert!(yes("[!a-c]x", "dx") && !yes("[!a-c]x", "bx"));
        assert!(yes("[]]x", "]x") && yes("a[-]b", "a-b"));
        assert!(!yes("[abc", "a") && !yes("[", "["));
        assert!(yes("a\\*b", "a*b") && !yes("a\\*b", "axb"));
        assert!(yes("**x", "ax") && yes("a**", "a") && yes("*a*b*", "xxaxxbxx"));
        // Hidden files need an explicit leading dot.
        assert!(!yes("*", ".hidden") && !yes("?hidden", ".hidden") && yes(".*", ".hidden"));
        // Pathological backtracking stays bounded by the input sizes.
        assert!(!yes(
            "*a*a*a*a*a*a*a*b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac"
        ));
    }
}
