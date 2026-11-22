//! The calculator's arithmetic: integer expressions with `+ - * / %`, unary signs and
//! parentheses, evaluated with checked 64-bit math. Division truncates toward zero
//! (`%` is the matching remainder). Every failure is an explicit error, never a wrong
//! number.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not an expression.
    Syntax,
    DivideByZero,
    /// The result does not fit in 64 bits.
    Overflow,
    /// Parentheses or signs nested too deeply.
    TooDeep,
}

impl Error {
    pub fn message(self) -> &'static [u8] {
        match self {
            Error::Syntax => b"syntax error",
            Error::DivideByZero => b"divide by zero",
            Error::Overflow => b"overflow",
            Error::TooDeep => b"too deep",
        }
    }
}

const MAX_DEPTH: usize = 24;

struct Parser<'a> {
    input: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&mut self) -> Option<u8> {
        while self.input.get(self.at).is_some_and(|b| *b == b' ') {
            self.at += 1;
        }
        self.input.get(self.at).copied()
    }

    fn expression(&mut self, depth: usize) -> Result<i64, Error> {
        let mut value = self.term(depth)?;
        while let Some(op @ (b'+' | b'-')) = self.peek() {
            self.at += 1;
            let right = self.term(depth)?;
            value = if op == b'+' {
                value.checked_add(right)
            } else {
                value.checked_sub(right)
            }
            .ok_or(Error::Overflow)?;
        }
        Ok(value)
    }

    fn term(&mut self, depth: usize) -> Result<i64, Error> {
        let mut value = self.unary(depth)?;
        while let Some(op @ (b'*' | b'/' | b'%')) = self.peek() {
            self.at += 1;
            let right = self.unary(depth)?;
            value = match op {
                b'*' => value.checked_mul(right).ok_or(Error::Overflow)?,
                _ if right == 0 => return Err(Error::DivideByZero),
                b'/' => value.checked_div(right).ok_or(Error::Overflow)?,
                _ => value.checked_rem(right).ok_or(Error::Overflow)?,
            };
        }
        Ok(value)
    }

    fn unary(&mut self, depth: usize) -> Result<i64, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        match self.peek() {
            Some(b'-') => {
                self.at += 1;
                self.unary(depth + 1)?.checked_neg().ok_or(Error::Overflow)
            }
            Some(b'+') => {
                self.at += 1;
                self.unary(depth + 1)
            }
            Some(b'(') => {
                self.at += 1;
                let value = self.expression(depth + 1)?;
                if self.peek() != Some(b')') {
                    return Err(Error::Syntax);
                }
                self.at += 1;
                Ok(value)
            }
            Some(b'0'..=b'9') => {
                let mut value: i64 = 0;
                while let Some(digit @ b'0'..=b'9') = self.input.get(self.at).copied() {
                    value = value
                        .checked_mul(10)
                        .and_then(|v| v.checked_add(i64::from(digit - b'0')))
                        .ok_or(Error::Overflow)?;
                    self.at += 1;
                }
                Ok(value)
            }
            _ => Err(Error::Syntax),
        }
    }
}

/// Evaluates `expression`.
pub fn eval(expression: &[u8]) -> Result<i64, Error> {
    let mut parser = Parser {
        input: expression,
        at: 0,
    };
    let value = parser.expression(0)?;
    if parser.peek().is_some() {
        return Err(Error::Syntax);
    }
    Ok(value)
}

/// Formats a number as decimal text.
pub fn format(value: i64, out: &mut [u8; 24]) -> &[u8] {
    let mut at = out.len();
    let mut rest = value.unsigned_abs();
    loop {
        at -= 1;
        out[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if value < 0 {
        at -= 1;
        out[at] = b'-';
    }
    &out[at..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    fn run(text: &str) -> Result<i64, Error> {
        eval(text.as_bytes())
    }

    #[test]
    fn arithmetic_follows_precedence_and_associativity() {
        assert_eq!(run("1+2*3"), Ok(7));
        assert_eq!(run("(1+2)*3"), Ok(9));
        assert_eq!(run("10-4-3"), Ok(3), "subtraction is left associative");
        assert_eq!(run("100/10/5"), Ok(2));
        assert_eq!(run("2*3%4"), Ok(2));
        assert_eq!(run("  7 +  8 "), Ok(15));
        assert_eq!(run("-3*-4"), Ok(12));
        assert_eq!(run("--5"), Ok(5));
        assert_eq!(run("+5"), Ok(5));
        assert_eq!(run("-(2+3)"), Ok(-5));
        assert_eq!(run("0"), Ok(0));
    }

    #[test]
    fn division_truncates_toward_zero_with_a_matching_remainder() {
        assert_eq!(run("7/2"), Ok(3));
        assert_eq!(run("-7/2"), Ok(-3));
        assert_eq!(run("7%3"), Ok(1));
        assert_eq!(run("-7%3"), Ok(-1));
        for (a, b) in [(17, 5), (-17, 5), (17, -5), (-17, -5)] {
            let q = run(&std::format!("{a}/{b}")).unwrap();
            let r = run(&std::format!("{a}%{b}")).unwrap();
            assert_eq!(q * b + r, a);
        }
    }

    #[test]
    fn every_failure_is_an_explicit_error() {
        assert_eq!(run("1/0"), Err(Error::DivideByZero));
        assert_eq!(run("1%0"), Err(Error::DivideByZero));
        assert_eq!(run(""), Err(Error::Syntax));
        assert_eq!(run("1+"), Err(Error::Syntax));
        assert_eq!(run("(1"), Err(Error::Syntax));
        assert_eq!(run("1)"), Err(Error::Syntax));
        assert_eq!(run("1 2"), Err(Error::Syntax));
        assert_eq!(run("*3"), Err(Error::Syntax));
        assert_eq!(run("3x"), Err(Error::Syntax));
        assert_eq!(run("()"), Err(Error::Syntax));
    }

    #[test]
    fn overflow_is_detected_everywhere_it_can_happen() {
        assert_eq!(run("9223372036854775807"), Ok(i64::MAX));
        assert_eq!(run("9223372036854775808"), Err(Error::Overflow));
        assert_eq!(run("9223372036854775807+1"), Err(Error::Overflow));
        assert_eq!(run("-9223372036854775807-2"), Err(Error::Overflow));
        assert_eq!(run("4611686018427387904*2"), Err(Error::Overflow));
        assert_eq!(run("99999999999999999999"), Err(Error::Overflow));
        // The one negation that cannot be represented.
        assert_eq!(run("-(0-9223372036854775807-1)"), Err(Error::Overflow));
        assert_eq!(
            run("(0-9223372036854775807-1)/(0-1)"),
            Err(Error::Overflow),
            "MIN / -1"
        );
        assert_eq!(
            run("(0-9223372036854775807-1)%(0-1)"),
            Err(Error::Overflow),
            "MIN % -1"
        );
    }

    #[test]
    fn nesting_is_bounded() {
        let deep = "(".repeat(100) + "1" + &")".repeat(100);
        assert_eq!(run(&deep), Err(Error::TooDeep));
        let signs = "-".repeat(100) + "1";
        assert_eq!(run(&signs), Err(Error::TooDeep));
        let fine = "(".repeat(10) + "1" + &")".repeat(10);
        assert_eq!(run(&fine), Ok(1));
    }

    #[test]
    fn numbers_format_as_decimal_text() {
        let mut out = [0; 24];
        for value in [0, 7, -7, 1234567890, i64::MAX, i64::MIN] {
            assert_eq!(format(value, &mut out), value.to_string().as_bytes());
        }
    }

    #[test]
    fn random_text_never_panics() {
        let mut seed = 7u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) as u8
        };
        let alphabet = b"0123456789+-*/%() x";
        for length in 0..200 {
            let text: std::vec::Vec<u8> = (0..length)
                .map(|_| alphabet[next() as usize % alphabet.len()])
                .collect();
            let _ = eval(&text);
        }
    }
}
