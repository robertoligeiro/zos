//! A text grid for one terminal window: the console protocol (WRITE, COLORS, CLEAR,
//! LINE) applied to a fixed 80x24 array of cells, with per-row dirty tracking so a
//! window only repaints the rows that changed.

pub const COLS: usize = 80;
pub const ROWS: usize = 24;
pub const DEFAULT_FG: u32 = 0x90ee90;
pub const DEFAULT_BG: u32 = 0x081008;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub byte: u8,
    pub fg: u32,
    pub bg: u32,
}

pub struct Grid {
    cells: [Cell; COLS * ROWS],
    dirty: [bool; ROWS],
    pub x: usize,
    pub y: usize,
    fg: u32,
    bg: u32,
    /// How many times the grid has scrolled; the shell polls this in tests.
    pub scrolls: usize,
}

impl Default for Grid {
    fn default() -> Self {
        Self::new()
    }
}

impl Grid {
    pub const fn new() -> Self {
        Self {
            cells: [Cell {
                byte: b' ',
                fg: DEFAULT_FG,
                bg: DEFAULT_BG,
            }; COLS * ROWS],
            dirty: [true; ROWS],
            x: 0,
            y: 0,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            scrolls: 0,
        }
    }
    fn blank(&self) -> Cell {
        Cell {
            byte: b' ',
            fg: self.fg,
            bg: self.bg,
        }
    }
    pub fn cell(&self, row: usize, col: usize) -> Cell {
        self.cells[row.min(ROWS - 1) * COLS + col.min(COLS - 1)]
    }
    pub fn colors(&mut self, fg: u32, bg: u32) {
        (self.fg, self.bg) = (fg & 0xff_ffff, bg & 0xff_ffff);
    }
    pub fn clear(&mut self) {
        let blank = self.blank();
        self.cells.fill(blank);
        self.dirty.fill(true);
        (self.x, self.y) = (0, 0);
    }
    pub fn invalidate(&mut self) {
        self.dirty.fill(true);
    }
    /// Rows changed since the last `take_dirty`, as a bit mask (row 0 is bit 0).
    pub fn take_dirty(&mut self) -> u32 {
        let mut mask = 0;
        for (row, dirty) in self.dirty.iter_mut().enumerate() {
            if core::mem::take(dirty) {
                mask |= 1 << row;
            }
        }
        mask
    }
    fn newline(&mut self) {
        self.x = 0;
        self.y += 1;
        if self.y == ROWS {
            self.cells.copy_within(COLS.., 0);
            let blank = self.blank();
            self.cells[(ROWS - 1) * COLS..].fill(blank);
            self.dirty.fill(true);
            self.y = ROWS - 1;
            self.scrolls += 1;
        }
    }
    pub fn write(&mut self, text: &[u8]) {
        for &byte in text {
            self.dirty[self.y] = true;
            match byte {
                b'\n' => self.newline(),
                b'\r' => self.x = 0,
                8 if self.x > 0 => {
                    self.x -= 1;
                    self.cells[self.y * COLS + self.x] = self.blank();
                }
                32..=126 => {
                    if self.x == COLS {
                        self.newline();
                    }
                    self.cells[self.y * COLS + self.x] = Cell {
                        byte,
                        ..self.blank()
                    };
                    self.x += 1;
                }
                _ => (),
            }
            self.dirty[self.y] = true;
        }
    }
    /// The shell's editable input line: "> " then the text, with the cursor at
    /// `cursor` (a byte offset into `text`). Long lines scroll horizontally.
    pub fn line(&mut self, text: &[u8], cursor: usize) {
        let blank = self.blank();
        self.cells[self.y * COLS..(self.y + 1) * COLS].fill(blank);
        self.x = 0;
        self.write(b"> ");
        let cursor = cursor.min(text.len());
        let start = cursor.saturating_sub(COLS - 3);
        let end = (start + COLS - 2).min(text.len());
        self.write(&text[start..end]);
        self.x = cursor - start + 2;
        self.dirty[self.y] = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(grid: &Grid, row: usize) -> std::string::String {
        (0..COLS)
            .map(|c| grid.cell(row, c).byte as char)
            .collect::<std::string::String>()
            .trim_end()
            .into()
    }

    #[test]
    fn text_newlines_carriage_return_and_backspace() {
        let mut g = Grid::new();
        g.write(b"hello\nab\x08c\r>x");
        assert_eq!(row(&g, 0), "hello");
        // "ab", backspace, "c" gives "ac"; the carriage return rewinds and ">x" overwrites it.
        assert_eq!(row(&g, 1), ">x");
        assert_eq!((g.x, g.y), (2, 1));
        // Backspace at column 0 does nothing; control bytes are ignored.
        let mut h = Grid::new();
        h.write(b"\x08\x07\x01\xffz");
        assert_eq!(row(&h, 0), "z");
    }

    #[test]
    fn wrapping_and_scrolling_keep_count_and_cursor() {
        let mut g = Grid::new();
        g.write(&[b'a'; COLS + 3]);
        assert_eq!(row(&g, 1), "aaa");
        assert_eq!(g.y, 1);
        for i in 0..ROWS + 5 {
            g.write(std::format!("line {i}\n").as_bytes());
        }
        assert!(g.scrolls >= 5);
        assert_eq!(g.y, ROWS - 1);
        assert_eq!(row(&g, ROWS - 2), std::format!("line {}", ROWS + 4));
        assert_eq!(row(&g, ROWS - 1), "");
        assert_eq!(
            g.take_dirty(),
            (1u32 << ROWS) - 1,
            "scrolling dirties every row"
        );
    }

    #[test]
    fn dirty_rows_are_tracked_individually() {
        let mut g = Grid::new();
        assert_eq!(
            g.take_dirty().count_ones() as usize,
            ROWS,
            "a new grid needs a full paint"
        );
        assert_eq!(g.take_dirty(), 0);
        g.write(b"x");
        assert_eq!(g.take_dirty(), 1);
        g.write(b"\n\ny");
        assert_eq!(g.take_dirty(), 0b111);
        g.invalidate();
        assert_eq!(g.take_dirty().count_ones() as usize, ROWS);
    }

    #[test]
    fn colors_apply_to_new_text_and_clear_uses_the_current_background() {
        let mut g = Grid::new();
        g.colors(0xff0000, 0x0000ff);
        g.write(b"r");
        assert_eq!(
            g.cell(0, 0),
            Cell {
                byte: b'r',
                fg: 0xff0000,
                bg: 0x0000ff
            }
        );
        assert_eq!(
            g.cell(0, 1).fg,
            DEFAULT_FG,
            "text written earlier keeps its colors"
        );
        g.clear();
        assert_eq!(
            g.cell(0, 0),
            Cell {
                byte: b' ',
                fg: 0xff0000,
                bg: 0x0000ff
            }
        );
        assert_eq!((g.x, g.y), (0, 0));
        g.colors(0xff12_3456, 0);
        assert_eq!(g.cell(0, 0).fg, 0xff0000, "unchanged until new text");
        g.write(b"q");
        assert_eq!(g.cell(0, 0).fg, 0x123456, "the alpha byte is dropped");
    }

    #[test]
    fn the_input_line_shows_a_prompt_cursor_and_scrolls_long_text() {
        let mut g = Grid::new();
        g.line(b"ls -l", 5);
        assert_eq!(row(&g, 0), "> ls -l");
        assert_eq!(g.x, 7);
        g.line(b"ls", 1);
        assert_eq!(row(&g, 0), "> ls");
        assert_eq!(g.x, 3);
        let long = [b'x'; 120];
        g.line(&long, 120);
        assert_eq!(
            g.x,
            COLS - 1,
            "the cursor sits at the last column when the text overflows"
        );
        assert!(row(&g, 0).starts_with("> xxx"));
        // An out-of-range cursor is clamped, never a panic.
        g.line(b"abc", 99);
        assert_eq!(g.x, 5);
        g.line(b"", 0);
        assert_eq!(row(&g, 0), ">");
    }

    #[test]
    fn out_of_range_cell_reads_are_clamped() {
        let g = Grid::new();
        assert_eq!(g.cell(1000, 1000), g.cell(ROWS - 1, COLS - 1));
    }
}
