use crate::keyboard::Key;

pub const CAPACITY: usize = 48;
const HISTORY: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    pub bytes: [u8; CAPACITY],
    pub length: usize,
}

const EMPTY: Line = Line {
    bytes: [0; CAPACITY],
    length: 0,
};

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Changed,
    Submit(Line),
    Clear,
    Full,
}

pub struct Editor {
    pub line: Line,
    pub cursor: usize,
    history: [Line; HISTORY],
    count: usize,
    browsing: Option<usize>,
    draft: Line,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            line: EMPTY,
            cursor: 0,
            history: [EMPTY; HISTORY],
            count: 0,
            browsing: None,
            draft: EMPTY,
        }
    }
}

impl Editor {
    pub fn key(&mut self, key: Key) -> Action {
        match key {
            Key::Character(ch) => {
                if self.line.length == CAPACITY {
                    return Action::Full;
                }
                self.line
                    .bytes
                    .copy_within(self.cursor..self.line.length, self.cursor + 1);
                self.line.bytes[self.cursor] = ch;
                self.line.length += 1;
                self.cursor += 1;
            }
            Key::Backspace if self.cursor > 0 => {
                self.line
                    .bytes
                    .copy_within(self.cursor..self.line.length, self.cursor - 1);
                self.line.length -= 1;
                self.cursor -= 1;
            }
            Key::Delete if self.cursor < self.line.length => {
                self.line
                    .bytes
                    .copy_within(self.cursor + 1..self.line.length, self.cursor);
                self.line.length -= 1;
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.line.length),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.line.length,
            Key::Up if self.count > 0 => {
                let position = match self.browsing {
                    None => {
                        self.draft = self.line;
                        self.count - 1
                    }
                    Some(index) => index.saturating_sub(1),
                };
                self.browsing = Some(position);
                self.line = self.history[position];
                self.cursor = self.line.length;
            }
            Key::Down => {
                if let Some(position) = self.browsing {
                    if position + 1 == self.count {
                        self.line = self.draft;
                        self.browsing = None;
                    } else {
                        self.line = self.history[position + 1];
                        self.browsing = Some(position + 1);
                    }
                    self.cursor = self.line.length;
                }
            }
            Key::Enter => {
                let submitted = self.line;
                if submitted.length != 0 {
                    if self.count == HISTORY {
                        self.history.copy_within(1..HISTORY, 0);
                        self.count -= 1;
                    }
                    self.history[self.count] = submitted;
                    self.count += 1;
                }
                self.line = EMPTY;
                self.cursor = 0;
                self.browsing = None;
                self.draft = EMPTY;
                return Action::Submit(submitted);
            }
            Key::Clear => return Action::Clear,
            _ => (),
        }
        Action::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn insertion_deletion_limits_and_boundaries() {
        let mut editor = Editor::default();
        editor.key(Key::Backspace);
        editor.key(Key::Left);
        editor.key(Key::Delete);
        for ch in b"ac" {
            editor.key(Key::Character(*ch));
        }
        editor.key(Key::Left);
        editor.key(Key::Character(b'b'));
        assert_eq!(&editor.line.bytes[..3], b"abc");
        editor.key(Key::Home);
        editor.key(Key::Delete);
        editor.key(Key::End);
        editor.key(Key::Backspace);
        assert_eq!(&editor.line.bytes[..editor.line.length], b"b");
        for _ in 1..CAPACITY {
            editor.key(Key::Character(b'x'));
        }
        assert_eq!(editor.key(Key::Character(b'y')), Action::Full);
        assert_eq!(editor.line.length, CAPACITY);
        editor.key(Key::Enter);
        assert_eq!(editor.cursor, 0);
    }
    #[test]
    fn bounded_history_restores_draft_and_edits_copies() {
        let mut editor = Editor::default();
        for ch in b"0123456789" {
            editor.key(Key::Character(*ch));
            editor.key(Key::Enter);
        }
        editor.key(Key::Character(b'd'));
        for _ in 0..20 {
            editor.key(Key::Up);
        }
        assert_eq!(editor.line.bytes[0], b'2');
        editor.key(Key::Character(b'x'));
        editor.key(Key::Down);
        editor.key(Key::Up);
        assert_eq!(editor.line.length, 1);
        for _ in 0..20 {
            editor.key(Key::Down);
        }
        assert_eq!(&editor.line.bytes[..editor.line.length], b"d");
    }
}
