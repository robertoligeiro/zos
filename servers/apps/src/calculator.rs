//! The ZOS calculator: integer arithmetic with `+ - * / %` and parentheses. Click the
//! buttons or type; Enter or = calculates, Backspace deletes, C or Ctrl+L clears.

#![no_std]
#![no_main]

use zos_apps::calc;
use zos_console::{
    gui::{Event, Window},
    keyboard::Key,
};
use zos_libos as os;

const WIDTH: u16 = 232;
const HEIGHT: u16 = 322;
const COLUMNS: i16 = 4;
const ROWS: i16 = 5;
const BUTTON_W: i16 = 50;
const BUTTON_H: i16 = 44;
const GAP: i16 = 4;
const LEFT: i16 = 10;
const TOP: i16 = 84;
const EXPRESSION: usize = 26;

const LABELS: [[&[u8]; 4]; 5] = [
    [b"7", b"8", b"9", b"/"],
    [b"4", b"5", b"6", b"*"],
    [b"1", b"2", b"3", b"-"],
    [b"0", b"(", b")", b"+"],
    [b"C", b"%", b"<", b"="],
];

struct Calculator {
    expression: [u8; EXPRESSION],
    length: usize,
    /// The last result or error text, shown under the expression.
    result: [u8; 32],
    result_length: usize,
    error: bool,
}

impl Calculator {
    const fn new() -> Self {
        Self {
            expression: [0; EXPRESSION],
            length: 0,
            result: [0; 32],
            result_length: 0,
            error: false,
        }
    }

    fn show(&mut self, text: &[u8], error: bool) {
        let n = text.len().min(self.result.len());
        self.result[..n].copy_from_slice(&text[..n]);
        self.result_length = n;
        self.error = error;
    }

    fn clear(&mut self) {
        self.length = 0;
        self.show(b"", false);
    }

    fn press(&mut self, label: u8) {
        match label {
            b'C' => self.clear(),
            b'<' => {
                self.length = self.length.saturating_sub(1);
                self.show(b"", false);
            }
            b'=' => self.calculate(),
            b'0'..=b'9' | b'+' | b'-' | b'*' | b'/' | b'%' | b'(' | b')' => {
                if self.length < EXPRESSION {
                    self.expression[self.length] = label;
                    self.length += 1;
                    self.show(b"", false);
                }
            }
            _ => (),
        }
    }

    fn calculate(&mut self) {
        match calc::eval(&self.expression[..self.length]) {
            Ok(value) => {
                let mut digits = [0u8; 24];
                let text = calc::format(value, &mut digits);
                // The result becomes the start of the next expression.
                zos_apps::log(format_args!(
                    "CALC {} = {value}",
                    core::str::from_utf8(&self.expression[..self.length]).unwrap_or("?")
                ));
                let n = text.len().min(EXPRESSION);
                self.expression[..n].copy_from_slice(&text[..n]);
                self.length = n;
                self.show(text, false);
            }
            Err(error) => self.show(error.message(), true),
        }
    }

    fn key(&mut self, key: Key) {
        match key {
            Key::Character(b'c' | b'C') | Key::Clear => self.press(b'C'),
            Key::Character(b'=') | Key::Enter => self.press(b'='),
            Key::Character(byte) => self.press(byte),
            Key::Backspace | Key::Delete => self.press(b'<'),
            _ => (),
        }
    }

    fn click(&mut self, x: i16, y: i16) {
        let (x, y) = (x - LEFT, y - TOP);
        if x < 0 || y < 0 {
            return;
        }
        let (column, row) = (x / (BUTTON_W + GAP), y / (BUTTON_H + GAP));
        // The gaps between buttons are not buttons.
        if column >= COLUMNS
            || row >= ROWS
            || x % (BUTTON_W + GAP) >= BUTTON_W
            || y % (BUTTON_H + GAP) >= BUTTON_H
        {
            return;
        }
        self.press(LABELS[row as usize][column as usize][0]);
    }

    fn draw(&self, window: &Window) -> zos_console::gui::Result<()> {
        window.clear(0xe4e4e4)?;
        // The display: the expression, and under it the result or the error.
        window.rect(8, 8, WIDTH - 16, 68, 0x606060)?;
        window.rect(9, 9, WIDTH - 18, 66, 0xf6f6ee)?;
        window.text(16, 18, 0x202020, &self.expression[..self.length])?;
        window.text(
            16,
            46,
            if self.error { 0xa01010 } else { 0x0a5c36 },
            &self.result[..self.result_length],
        )?;
        for (row, labels) in LABELS.iter().enumerate() {
            for (column, label) in labels.iter().enumerate() {
                let x = LEFT + column as i16 * (BUTTON_W + GAP);
                let y = TOP + row as i16 * (BUTTON_H + GAP);
                let operator = matches!(label[0], b'/' | b'*' | b'-' | b'+' | b'%');
                let face = match label[0] {
                    b'=' => 0x0a5c36,
                    b'C' | b'<' => 0xc8a0a0,
                    _ if operator => 0xb8c4d8,
                    _ => 0xf8f8f8,
                };
                window.rect(x, y, BUTTON_W as u16, BUTTON_H as u16, 0x606060)?;
                window.rect(x + 1, y + 1, BUTTON_W as u16 - 2, BUTTON_H as u16 - 2, face)?;
                let ink = if label[0] == b'=' { 0xffffff } else { 0x202020 };
                window.text(x + BUTTON_W / 2 - 4, y + BUTTON_H / 2 - 8, ink, label)?;
            }
        }
        window.present()
    }
}

static mut CALCULATOR: Calculator = Calculator::new();

#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    let Ok(window) = Window::create(b"Calculator", WIDTH, HEIGHT) else {
        zos_apps::log(format_args!("CALC: no window"));
        os::exit(1)
    };
    // SAFETY: this program is single-threaded and this is the only reference.
    let calculator = unsafe { &mut *core::ptr::addr_of_mut!(CALCULATOR) };
    zos_apps::log(format_args!("CALC ready"));
    loop {
        let _ = calculator.draw(&window);
        match window.wait() {
            Ok(Event::Key(key)) => calculator.key(key),
            Ok(Event::Click { x, y, .. }) => calculator.click(x, y),
            Ok(Event::Close) | Err(_) => os::exit(0),
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    zos_apps::fail(info)
}
