#![no_std]
#![no_main]

use core::fmt::{self, Write as _};
use zos_console::{
    editor::{Action, Editor},
    keyboard::Key,
    CLEAR, CONSOLE, INPUT, LINE, TTY,
};
use zos_libos as os;
use zos_session::{
    client::{self, Startup},
    parser::{self, Command, Parsed},
    *,
};
use zos_storage::{protocol as api, vfs::*, Error, Result};

struct Output(Option<u64>);
impl Output {
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        match self.0 {
            Some(fd) => write_all(fd, bytes),
            None => {
                console(bytes);
                Ok(())
            }
        }
    }
}
impl fmt::Write for Output {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.bytes(text.as_bytes()).map_err(|_| fmt::Error)
    }
}

struct Shell {
    cwd: [u8; PATH],
    length: usize,
}
impl Shell {
    fn new() -> Self {
        let mut cwd = [0; PATH];
        cwd[0] = b'/';
        Self { cwd, length: 1 }
    }
    fn cwd(&self) -> &[u8] {
        &self.cwd[..self.length]
    }
    fn builtin(&mut self, args: &[&[u8]], out: &mut Output) -> Result<bool> {
        let arg = |index: usize| args.get(index).copied().ok_or(Error::Invalid);
        let exact = |count: usize| {
            if args.len() == count {
                Ok(())
            } else {
                Err(Error::Invalid)
            }
        };
        match args[0] {
            b"help" => {
                exact(1)?;
                out.bytes(b"ZOS shell: quotes, escapes, ;, >, >> and background &\nhelp clear ls cd pwd mkdir rmdir touch cat echo rm cp mv\nfree ps kill wait uname dmesg reboot halt\ndmesg prints the system log; dmesg > /name saves it.\nPrograms load from /bin or an explicit path.\n")?;
            }
            b"clear" => {
                exact(1)?;
                os::send(CONSOLE, &[CLEAR], &mut []).map_err(kernel)?;
            }
            b"pwd" => {
                exact(1)?;
                out.bytes(self.cwd())?;
                out.bytes(b"\n")?;
            }
            b"cd" => {
                if args.len() > 2 {
                    return Err(Error::Invalid);
                }
                let path = path(self.cwd(), args.get(1).copied().unwrap_or(b"/"))?;
                if api::stat(path.bytes())?.kind != 1 {
                    return Err(Error::NotDir);
                }
                self.length = path.bytes().len();
                self.cwd[..self.length].copy_from_slice(path.bytes());
            }
            b"ls" => {
                if args.len() > 2 {
                    return Err(Error::Invalid);
                }
                let path = path(self.cwd(), args.get(1).copied().unwrap_or(self.cwd()))?;
                let entry = api::stat(path.bytes())?;
                if entry.kind == 2 {
                    out.bytes(&entry.name[..entry.length as usize])?;
                    out.bytes(b"\n")?;
                } else {
                    let mut ordinal = 0;
                    while let Some(entry) = api::readdir(path.bytes(), ordinal)? {
                        out.bytes(&entry.name[..entry.length as usize])?;
                        if entry.kind == 1 {
                            out.bytes(b"/")?;
                        }
                        out.bytes(b"\n")?;
                        ordinal += 1;
                    }
                }
            }
            b"mkdir" => {
                exact(2)?;
                api::mkdir(path(self.cwd(), arg(1)?)?.bytes())?;
            }
            b"rmdir" | b"rm" => {
                exact(2)?;
                let path = path(self.cwd(), arg(1)?)?;
                let kind = api::stat(path.bytes())?.kind;
                if args[0] == b"rmdir" && kind != 1 {
                    return Err(Error::NotDir);
                }
                if args[0] == b"rm" && kind != 2 {
                    return Err(Error::IsDir);
                }
                api::unlink(path.bytes())?;
            }
            b"touch" => {
                exact(2)?;
                let fd = api::open(path(self.cwd(), arg(1)?)?.bytes(), WRITE | CREATE)?;
                api::close(fd)?;
            }
            b"mv" => {
                exact(3)?;
                let from = path(self.cwd(), arg(1)?)?;
                let to = path(self.cwd(), arg(2)?)?;
                api::rename(from.bytes(), to.bytes())?;
            }
            b"free" => {
                exact(1)?;
                let [total, free, used, heap] = client::memory()?;
                writeln!(
                    out,
                    "MEMORY bytes: total={total} free={free} used={used} kernel_heap={heap}"
                )
                .map_err(|_| Error::Io)?;
            }
            b"ps" => {
                exact(1)?;
                out.bytes(b"PID NAME STATE MEMORY(bytes)\n")?;
                for slot in 0..os::PROCESS_SLOTS {
                    if let Some(info) = client::list(slot as u8)? {
                        let state = match info.state {
                            0 => "ready",
                            1 => "blocked",
                            2 => "exited",
                            3 => "faulted",
                            _ => return Err(Error::Corrupt),
                        };
                        writeln!(
                            out,
                            "{} {} {} {}",
                            info.pid,
                            info.name(),
                            state,
                            info.memory
                        )
                        .map_err(|_| Error::Io)?;
                    }
                }
            }
            b"kill" | b"wait" => {
                exact(2)?;
                let text = core::str::from_utf8(arg(1)?).map_err(|_| Error::Invalid)?;
                let pid: u64 = text.parse().map_err(|_| Error::Invalid)?;
                if args[0] == b"kill" {
                    client::kill(pid)?;
                } else {
                    let status = client::wait(pid)?;
                    writeln!(out, "PID {pid} exited: {status}").map_err(|_| Error::Io)?;
                }
            }
            b"uname" => {
                exact(1)?;
                out.bytes(b"ZOS - Zombie OS (x86_64)\n")?;
            }
            b"dmesg" => {
                // A snapshot of the log: text produced while printing it is not
                // chased, so `dmesg > /file` terminates.
                exact(1)?;
                let mut chunk = [0; 960];
                let (end, _) = client::log(u64::MAX, &mut chunk)?;
                let mut offset = 0;
                while offset < end {
                    let (next, count) = client::log(offset, &mut chunk)?;
                    if count == 0 {
                        break;
                    }
                    out.bytes(&chunk[..count])?;
                    offset = next;
                }
            }
            b"reboot" | b"halt" => {
                exact(1)?;
                out.bytes(if args[0] == b"reboot" {
                    b"Rebooting ZOS.\n"
                } else {
                    b"Halting ZOS.\n"
                })?;
                client::control(u8::from(args[0] == b"reboot"))?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    fn execute(&mut self, parsed: &Parsed, command: Command) -> Result<i64> {
        let mut args = [b"".as_slice(); 12];
        for (arg, span) in args.iter_mut().zip(&command.arguments[..command.count]) {
            *arg = parsed.word(*span);
        }
        let args = &args[..command.count];
        let output = command
            .output
            .map(|span| path(self.cwd(), parsed.word(span)))
            .transpose()?;
        let name = args[0];
        let spawned = !matches!(
            name,
            b"help"
                | b"clear"
                | b"ls"
                | b"cd"
                | b"pwd"
                | b"mkdir"
                | b"rmdir"
                | b"touch"
                | b"rm"
                | b"mv"
                | b"free"
                | b"ps"
                | b"kill"
                | b"wait"
                | b"uname"
                | b"dmesg"
                | b"reboot"
                | b"halt"
        );
        if spawned {
            let executable = if name.contains(&b'/') {
                path(self.cwd(), name)?
            } else {
                path(b"/bin", name)?
            };
            let startup = Startup::new(
                self.cwd(),
                output.as_ref().map(|p| (p.bytes(), command.append)),
                args,
            )?;
            let pid = client::spawn(executable.bytes(), &startup)?;
            if command.background {
                zos_console::log(format_args!("SHELL BACKGROUND: {pid}"));
                writeln!(Output(None), "[PID {pid}]").map_err(|_| Error::Io)?;
                Ok(0)
            } else {
                client::wait(pid)
            }
        } else {
            if command.background {
                return Err(Error::Invalid);
            }
            let fd = output
                .as_ref()
                .map(|p| {
                    api::open(
                        p.bytes(),
                        WRITE | CREATE | if command.append { APPEND } else { TRUNCATE },
                    )
                })
                .transpose()?;
            let result = self.builtin(args, &mut Output(fd));
            let closed = fd.map_or(Ok(()), api::close);
            result.and_then(|_| closed.map(|_| 0))
        }
    }
    fn line(&mut self, bytes: &[u8]) {
        match parser::parse(bytes) {
            Ok(parsed) => {
                for command in parsed.commands[..parsed.count].iter().copied() {
                    match self.execute(&parsed, command) {
                        Ok(status) => zos_console::log(format_args!("SHELL STATUS: {status}")),
                        Err(error) => {
                            let name =
                                core::str::from_utf8(parsed.word(command.arguments[0])).unwrap();
                            writeln!(Output(None), "shell: {name}: {error:?}").unwrap();
                            zos_console::log(format_args!("SHELL STATUS: 1"));
                        }
                    }
                }
            }
            Err(error) => {
                writeln!(Output(None), "shell: parse: {error:?}").unwrap();
            }
        }
    }
}

fn redraw(editor: &Editor) {
    let mut bytes = [0; 128];
    bytes[0] = LINE;
    bytes[1] = editor.cursor as u8;
    bytes[2..2 + editor.line.length].copy_from_slice(&editor.line.bytes[..editor.line.length]);
    os::send(CONSOLE, &bytes[..2 + editor.line.length], &mut []).unwrap();
}

fn launch(name: &[u8], args: &[&[u8]]) -> u64 {
    client::spawn(
        path(b"/bin", name).unwrap().bytes(),
        &Startup::new(b"/", None, args).unwrap(),
    )
    .unwrap()
}
fn self_test() {
    assert_eq!(
        os::process_spawn(b"invalid", b"bad"),
        Err(os::Error::Denied)
    );
    assert_eq!(os::process_info(0).unwrap_err(), os::Error::Denied);
    assert_eq!(os::memory_stats(), Err(os::Error::Denied));
    assert_eq!(os::system_control(0), Err(os::Error::Denied));
    assert_eq!(client::kill(3), Err(Error::Denied));
    assert_eq!(
        api::rpc(zos_storage::VFS, &[10, 3], &mut [0]),
        Err(Error::Denied)
    );
    os::debug_write("PROCESS CAPABILITIES PASS: lifecycle and FD cleanup protected").unwrap();
    let pid = launch(b"true", &[b"true"]);
    assert_eq!(client::wait(pid), Ok(0));
    let baseline = client::memory().unwrap();
    let mut previous = pid;
    for _ in 0..40 {
        let pid = launch(b"true", &[b"true"]);
        assert_ne!(pid, previous);
        assert_eq!(client::kill(previous), Err(Error::Invalid));
        assert_eq!(client::wait(pid), Ok(0));
        previous = pid;
    }
    let after = client::memory().unwrap();
    assert_eq!(baseline, after, "Repeated spawn/reap leaked memory");
    os::debug_write("PROCESS REUSE PASS: 40 spawns, stale PIDs, exact memory recovery").unwrap();
    assert_eq!(client::wait(launch(b"false", &[b"false"])), Ok(1));
    assert_eq!(client::wait(launch(b"fault", &[b"fault"])), Ok(-128));
    os::debug_write("PROCESS STATUS PASS: exit 0, exit 1 and contained fault").unwrap();
    let fd = api::open(b"/bad-elf", WRITE | CREATE).unwrap();
    write_all(fd, b"not an executable").unwrap();
    api::close(fd).unwrap();
    assert_eq!(
        client::spawn(b"/bad-elf", &Startup::new(b"/", None, &[b"bad"]).unwrap()),
        Err(Error::Invalid)
    );
    api::unlink(b"/bad-elf").unwrap();
    let first = launch(b"block", &[b"block", b"/leaked"]);
    client::started(first).unwrap();
    client::kill(first).unwrap();
    assert_eq!(client::wait(first), Ok(-9));
    api::unlink(b"/leaked").unwrap();
    assert_eq!(
        os::send(8, b"stale channel", &mut []),
        Err(os::Error::Closed)
    );
    let second = launch(b"block", &[b"block", b"/leaked"]);
    client::started(second).unwrap();
    assert_eq!(
        os::send(8, b"stale channel", &mut []),
        Err(os::Error::Closed)
    );
    client::kill(second).unwrap();
    assert_eq!(client::wait(second), Ok(-9));
    api::unlink(b"/leaked").unwrap();
    assert_eq!(client::wait(launch(b"leak", &[b"leak", b"/leaked"])), Ok(0));
    api::unlink(b"/leaked").unwrap();
    os::debug_write("PROCESS CLEANUP PASS: kill blocked IPC, channels, maps and FDs").unwrap();
    assert_eq!(
        client::wait(launch(b"cp", &[b"cp", b"/bin/true", b"/custom"])),
        Ok(0)
    );
    let pid = client::spawn(b"/custom", &Startup::new(b"/", None, &[b"true"]).unwrap()).unwrap();
    assert_eq!(client::wait(pid), Ok(0));
    api::unlink(b"/custom").unwrap();
    os::debug_write("PATH SPAWN PASS: executable copied to filesystem path").unwrap();
}

/// Writes the log collected so far to `path`, replacing any previous copy.
fn save_log(path: &[u8]) -> Result<()> {
    let mut chunk = [0; 960];
    let (end, _) = client::log(u64::MAX, &mut chunk)?;
    let fd = api::open(path, WRITE | CREATE | TRUNCATE)?;
    let result = (|| {
        let mut offset = 0;
        while offset < end {
            let (next, count) = client::log(offset, &mut chunk)?;
            if count == 0 {
                break;
            }
            write_all(fd, &chunk[..count])?;
            offset = next;
        }
        Ok(())
    })();
    api::close(fd)?;
    result
}

#[no_mangle]
pub extern "C" fn _start(test: usize) -> ! {
    let usb = test & 8 != 0;
    let test = test & 1;
    assert_eq!(os::channel_create(), Ok(TTY));
    assert_eq!(os::framebuffer_map().unwrap_err(), os::Error::Denied);
    assert_eq!(os::io_port_grant(0x60, 1), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(TTY, 12), Err(os::Error::Denied));
    assert_eq!(os::usb_controller().unwrap_err(), os::Error::Denied);
    assert_eq!(os::usb_dma_page().unwrap_err(), os::Error::Denied);
    assert_eq!(os::usb_enable_dma(), Err(os::Error::Denied));
    assert_eq!(os::send_ready(TTY, &[], &mut []), Err(os::Error::Denied));
    if usb {
        let mut reply = [0];
        assert_eq!(os::send(2, &[0x13], &mut reply), Ok(1));
        assert_eq!(reply, [7]);
        os::debug_write("USB STORAGE CAPABILITY PASS: shell cannot issue raw blocks").unwrap();
    }
    os::debug_write(if usb {
        "CAPABILITIES PASS: framebuffer, USB MMIO/DMA and IRQs isolated"
    } else {
        "CAPABILITIES PASS: framebuffer, PS/2 ports and IRQs isolated"
    })
    .unwrap();
    let mut bytes = [0; 64];
    let (token, length) = os::receive(TTY, &mut bytes).unwrap();
    assert_eq!(token & 0xffff, MANAGER_TID);
    assert_eq!(&bytes[..length], b"session-ready");
    os::reply(token, &[]).unwrap();
    if test != 0 {
        self_test();
    }
    let mut shell = Shell::new();
    let mut editor = Editor::default();
    if usb {
        // Keep a copy of this boot's log on the stick so it can be read from another
        // computer (tools\zosfs.py). A failure here must never stop the shell.
        match save_log(b"/boot.log") {
            Ok(()) => os::debug_write("BOOT LOG saved to /boot.log").unwrap(),
            Err(_) => os::debug_write("BOOT LOG could not be saved").unwrap(),
        }
    }
    console(b"ZOS - Zombie OS\nInteractive shell ready. Type help for commands.\n");
    redraw(&editor);
    os::send(INPUT, b"ready", &mut []).unwrap();
    os::debug_write("ZOS ready: console checkpoint; interactive shell.").unwrap();
    os::debug_write("SHELL ready: interactive ZOS shell, persistent root filesystem").unwrap();
    let mut submitted = 0;
    let mut keys = 0;
    loop {
        let (token, length) = os::receive(TTY, &mut bytes).unwrap();
        let key = Key::decode(&bytes[..length]).expect("Invalid keyboard message");
        let action = editor.key(key);
        os::reply(token, &[]).unwrap();
        match action {
            Action::Submit(line) => {
                console(b"\n");
                if test != 0 && &line.bytes[..line.length] == b"~finish" {
                    os::debug_write(
                        "SHELL SESSION PASS: real keys, parser, commands and persistence",
                    )
                    .unwrap();
                    client::control(2).unwrap();
                    unreachable!();
                }
                shell.line(&line.bytes[..line.length]);
                submitted += 1;
                zos_console::log(format_args!("SHELL LINE DONE: {submitted}"));
            }
            Action::Clear => {
                os::send(CONSOLE, &[CLEAR], &mut []).unwrap();
            }
            Action::Full => {
                os::debug_write("SHELL LIMIT: 120 characters").unwrap();
            }
            Action::Changed => (),
        }
        redraw(&editor);
        keys += 1;
        if test != 0 {
            zos_console::log(format_args!("SHELL KEY: {keys}"));
        }
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
