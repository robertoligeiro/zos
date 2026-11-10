#![no_std]
#![no_main]

use zos_console::{
    editor::{Action, Editor},
    keyboard::Key,
    *,
};
use zos_libos as os;

fn redraw(editor: &Editor) {
    let mut bytes = [0; 64];
    bytes[0] = LINE;
    bytes[1] = editor.cursor as u8;
    bytes[2..2 + editor.line.length].copy_from_slice(&editor.line.bytes[..editor.line.length]);
    os::send(CONSOLE, &bytes[..2 + editor.line.length], &mut []).unwrap();
}

#[no_mangle]
pub extern "C" fn _start(test: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(TTY));
    assert_eq!(os::framebuffer_map().unwrap_err(), os::Error::Denied);
    assert_eq!(os::io_port_grant(0x60, 1), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x64, 1), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(TTY, 12), Err(os::Error::Denied));
    if test == 0 {
        assert_eq!(os::console_test_finish(), Err(os::Error::Denied));
    }
    let mut editor = Editor::default();
    redraw(&editor);
    os::send(INPUT, b"ready", &mut []).unwrap();
    os::debug_write("CAPABILITIES PASS: framebuffer, PS/2 ports and IRQs isolated").unwrap();
    os::debug_write("ZOS ready: console checkpoint; input demo, not a shell.").unwrap();
    let mut bytes = [0; 64];
    let mut keys = 0;
    let mut submitted = 0;
    loop {
        let (token, length) = os::receive(TTY, &mut bytes).unwrap();
        assert!(token != 0);
        let key = Key::decode(&bytes[..length]).expect("Invalid key protocol");
        match editor.key(key) {
            Action::Submit(line) => {
                let text = &line.bytes[..line.length];
                output(b"\nYou typed: ");
                output(text);
                output(b"\n");
                log(format_args!(
                    "TTY LINE: {}",
                    core::str::from_utf8(text).unwrap()
                ));
                if test != 0 {
                    let expected: [&[u8]; 6] = [
                        b"zombie", b"zombie!", b"zombie!", b"ZOS", b"ab!c", b"~finish",
                    ];
                    assert_eq!(text, expected[submitted]);
                    submitted += 1;
                    if submitted == expected.len() {
                        let mut count = [0; 8];
                        assert_eq!(os::send(CONSOLE, &[SCROLLS], &mut count), Ok(8));
                        assert!(u64::from_le_bytes(count) > 0);
                        os::debug_write("TTY PASS: real keys, editing, modifiers and history")
                            .unwrap();
                        os::reply(token, &[]).unwrap();
                        os::console_test_finish().unwrap();
                        unreachable!();
                    }
                }
            }
            Action::Clear => {
                os::send(CONSOLE, &[CLEAR], &mut []).unwrap();
            }
            Action::Full => {
                os::debug_write("TTY LIMIT: 48 characters").unwrap();
            }
            Action::Changed => (),
        }
        redraw(&editor);
        os::reply(token, &[]).unwrap();
        keys += 1;
        if test != 0 {
            log(format_args!("TTY KEY: {keys}"));
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
