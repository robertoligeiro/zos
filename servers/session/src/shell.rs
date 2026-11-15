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
    client::{self, Startup, ENV_MAX},
    env::{self, glob_match, Env},
    parser::{self, Command, Connector, Parsed, Pipeline, WORDS},
    *,
};
use zos_storage::{protocol as api, vfs::*, Error, Result};

/// Expanded words of one command, stored back to back in a fixed buffer.
struct Words {
    bytes: [u8; 512],
    used: usize,
    spans: [(usize, usize); WORDS],
    count: usize,
}
impl Words {
    fn new() -> Self {
        Self {
            bytes: [0; 512],
            used: 0,
            spans: [(0, 0); WORDS],
            count: 0,
        }
    }
    fn push(&mut self, text: &[u8]) -> Result<()> {
        if self.count == WORDS || self.used + text.len() > self.bytes.len() {
            return Err(Error::TooLarge);
        }
        self.bytes[self.used..self.used + text.len()].copy_from_slice(text);
        self.spans[self.count] = (self.used, text.len());
        self.used += text.len();
        self.count += 1;
        Ok(())
    }
    fn get(&self, index: usize) -> &[u8] {
        let (start, length) = self.spans[index];
        &self.bytes[start..start + length]
    }
}

fn is_builtin(name: &[u8]) -> bool {
    matches!(
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
            | b"export"
            | b"unset"
            | b"env"
            | b"set"
            | b"reboot"
            | b"halt"
    )
}

/// Removes the backslash escapes `env::expand` adds in pattern mode.
fn unescape(pattern: &[u8], out: &mut [u8; PATH]) -> usize {
    let (mut length, mut index) = (0, 0);
    while index < pattern.len() && length < out.len() {
        if pattern[index] == b'\\' && index + 1 < pattern.len() {
            index += 1;
        }
        out[length] = pattern[index];
        length += 1;
        index += 1;
    }
    length
}

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
    env: Env,
}
impl Shell {
    fn new() -> Self {
        let mut cwd = [0; PATH];
        cwd[0] = b'/';
        let mut env = Env::new();
        env.set(b"PATH", b"/bin", Some(true)).unwrap();
        env.set(b"HOME", b"/", Some(true)).unwrap();
        env.set(b"PWD", b"/", Some(false)).unwrap();
        Self {
            cwd,
            length: 1,
            env,
        }
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
                self.env.set(b"PWD", path.bytes(), None)?;
            }
            b"export" => {
                if args.len() == 1 {
                    for (name, value, exported) in self.env.all() {
                        if exported {
                            out.bytes(b"export ")?;
                            out.bytes(name)?;
                            out.bytes(b"=")?;
                            out.bytes(value)?;
                            out.bytes(b"\n")?;
                        }
                    }
                }
                for arg in &args[1..] {
                    match arg.iter().position(|&b| b == b'=') {
                        Some(at) => self.env.set(&arg[..at], &arg[at + 1..], Some(true))?,
                        None => self.env.export(arg, true)?,
                    }
                }
            }
            b"unset" => {
                if args.len() < 2 {
                    return Err(Error::Invalid);
                }
                for arg in &args[1..] {
                    if !env::valid_name(arg) {
                        return Err(Error::Invalid);
                    }
                    self.env.unset(arg);
                }
            }
            b"env" | b"set" => {
                exact(1)?;
                for (name, value, exported) in self.env.all() {
                    if exported || args[0] == b"set" {
                        out.bytes(name)?;
                        out.bytes(b"=")?;
                        out.bytes(value)?;
                        out.bytes(b"\n")?;
                    }
                }
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
    /// Expands a filename pattern against its directory (only the last path
    /// component may contain wildcards) and adds the sorted matches as words.
    /// Returns how many words were added; 0 means nothing matched.
    fn glob(&self, pattern: &[u8], words: &mut Words) -> Result<usize> {
        let split = pattern
            .iter()
            .rposition(|&b| b == b'/')
            .map_or(0, |i| i + 1);
        let (dir, name) = pattern.split_at(split);
        let mut escaped = false;
        for &b in dir {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if matches!(b, b'*' | b'?' | b'[') {
                return Err(Error::Invalid);
            }
        }
        let mut plain = [0; PATH];
        let dir_length = unescape(dir, &mut plain);
        let resolved;
        let base: &[u8] = if dir_length == 0 {
            self.cwd()
        } else {
            resolved = path(self.cwd(), &plain[..dir_length])?;
            resolved.bytes()
        };
        let mut names = [[0u8; 24]; 32];
        let mut lengths = [0usize; 32];
        let mut found = 0;
        let mut ordinal = 0;
        while let Some(entry) = api::readdir(base, ordinal)? {
            ordinal += 1;
            let entry_name = &entry.name[..entry.length as usize];
            if glob_match(name, entry_name) {
                if found == names.len() {
                    return Err(Error::TooLarge);
                }
                names[found][..entry_name.len()].copy_from_slice(entry_name);
                lengths[found] = entry_name.len();
                found += 1;
            }
        }
        for i in 1..found {
            let mut j = i;
            while j > 0 && names[j - 1][..lengths[j - 1]] > names[j][..lengths[j]] {
                names.swap(j - 1, j);
                lengths.swap(j - 1, j);
                j -= 1;
            }
        }
        for i in 0..found {
            let mut text = [0u8; PATH + 24];
            text[..dir_length].copy_from_slice(&plain[..dir_length]);
            text[dir_length..dir_length + lengths[i]].copy_from_slice(&names[i][..lengths[i]]);
            words.push(&text[..dir_length + lengths[i]])?;
        }
        Ok(found)
    }
    /// Finds a program: a name with a '/' is a path; otherwise each directory of
    /// $PATH is tried in order.
    fn find_executable(&self, name: &[u8]) -> Result<Path> {
        if name.contains(&b'/') {
            return path(self.cwd(), name);
        }
        let search = self.env.get(b"PATH").unwrap_or(b"/bin");
        for dir in search.split(|&b| b == b':').filter(|d| !d.is_empty()) {
            let mut candidate = [0u8; PATH];
            if dir.len() + 1 + name.len() > candidate.len() {
                continue;
            }
            candidate[..dir.len()].copy_from_slice(dir);
            candidate[dir.len()] = b'/';
            candidate[dir.len() + 1..dir.len() + 1 + name.len()].copy_from_slice(name);
            let candidate = path(self.cwd(), &candidate[..dir.len() + 1 + name.len()])?;
            if api::stat(candidate.bytes()).is_ok_and(|entry| entry.kind == 2) {
                return Ok(candidate);
            }
        }
        Err(Error::NotFound)
    }
    fn execute(&mut self, parsed: &Parsed, command: &Command, background: bool) -> Result<i64> {
        let mut words = Words::new();
        for &word in &command.words[..command.count] {
            let raw = parsed.word(word);
            let mut pattern = [0u8; PATH];
            let expanded = env::expand(raw, &self.env, &mut pattern, true)?;
            if expanded.glob && self.glob(&pattern[..expanded.length], &mut words)? > 0 {
                continue;
            }
            // No wildcard, or nothing matched: the word stands for its own text.
            let mut plain = [0u8; PATH];
            let length = env::expand(raw, &self.env, &mut plain, false)?.length;
            words.push(&plain[..length])?;
        }
        let mut values = [[0u8; env::VALUE]; parser::ASSIGNS];
        let mut value_lengths = [0usize; parser::ASSIGNS];
        for (i, assign) in command.assigns[..command.assign_count].iter().enumerate() {
            value_lengths[i] =
                env::expand(parsed.word(assign.value), &self.env, &mut values[i], false)?.length;
        }
        if words.count == 0 {
            // Only assignments: they set shell variables.
            for (i, assign) in command.assigns[..command.assign_count].iter().enumerate() {
                self.env.set(
                    parsed.word(assign.name),
                    &values[i][..value_lengths[i]],
                    None,
                )?;
            }
            return Ok(0);
        }
        let output = match command.redirects.output {
            Some(target) => {
                let mut buffer = [0u8; PATH];
                let length =
                    env::expand(parsed.word(target.path), &self.env, &mut buffer, false)?.length;
                Some((path(self.cwd(), &buffer[..length])?, target.append))
            }
            None => None,
        };
        let args: [&[u8]; WORDS] =
            core::array::from_fn(|i| if i < words.count { words.get(i) } else { b"" });
        let args = &args[..words.count];
        if is_builtin(args[0]) {
            if background {
                return Err(Error::Invalid);
            }
            let fd = output
                .as_ref()
                .map(|(p, append)| {
                    api::open(
                        p.bytes(),
                        WRITE | CREATE | if *append { APPEND } else { TRUNCATE },
                    )
                })
                .transpose()?;
            let result = self.builtin(args, &mut Output(fd));
            let closed = fd.map_or(Ok(()), api::close);
            return result.and_then(|_| closed.map(|_| 0));
        }
        let executable = self.find_executable(args[0])?;
        // The child sees the exported variables, with this command's own
        // `NAME=value` prefixes applied on top.
        let mut pairs: [(&[u8], &[u8]); ENV_MAX] = [(b"", b""); ENV_MAX];
        let mut count = 0;
        for (name, value, exported) in self.env.all() {
            if exported {
                *pairs.get_mut(count).ok_or(Error::TooLarge)? = (name, value);
                count += 1;
            }
        }
        for (i, assign) in command.assigns[..command.assign_count].iter().enumerate() {
            let name = parsed.word(assign.name);
            let value = &values[i][..value_lengths[i]];
            match pairs[..count]
                .iter_mut()
                .find(|(existing, _)| *existing == name)
            {
                Some(entry) => entry.1 = value,
                None => {
                    *pairs.get_mut(count).ok_or(Error::TooLarge)? = (name, value);
                    count += 1;
                }
            }
        }
        let startup = Startup::new(
            self.cwd(),
            output.as_ref().map(|(p, append)| (p.bytes(), *append)),
            args,
            &pairs[..count],
        )?;
        let pid = client::spawn(executable.bytes(), &startup)?;
        if background {
            zos_console::log(format_args!("SHELL BACKGROUND: {pid}"));
            writeln!(Output(None), "[PID {pid}]").map_err(|_| Error::Io)?;
            Ok(0)
        } else {
            client::wait(pid)
        }
    }
    /// Runs one pipeline (a single command at this checkpoint) and records `$?`.
    fn run(&mut self, parsed: &Parsed, pipeline: &Pipeline) {
        let command = &pipeline.commands[0];
        let status = match self.execute(parsed, command, pipeline.background) {
            Ok(status) => status,
            Err(error) => {
                let mut name = [0u8; 32];
                let length = if command.count == 0 {
                    name[..10].copy_from_slice(b"assignment");
                    10
                } else {
                    parsed.plain(command.words[0], &mut name)
                };
                let name = core::str::from_utf8(&name[..length]).unwrap_or("?");
                writeln!(Output(None), "shell: {name}: {error:?}").unwrap();
                1
            }
        };
        let status = if pipeline.negate {
            i64::from(status == 0)
        } else {
            status
        };
        self.env.status = status;
        zos_console::log(format_args!("SHELL STATUS: {status}"));
    }
    fn line(&mut self, bytes: &[u8]) {
        let parsed = match parser::parse(bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                writeln!(Output(None), "shell: parse: {error:?}").unwrap();
                return;
            }
        };
        // Refuse what this build cannot run before running any part of the line.
        for pipeline in parsed.pipelines() {
            let redirects = pipeline.commands[..pipeline.count].iter().any(|c| {
                c.redirects.input.is_some()
                    || c.redirects.error.is_some()
                    || c.redirects.error_to_output
            });
            if pipeline.count > 1 || redirects {
                writeln!(
                    Output(None),
                    "shell: pipes, <, 2> and 2>&1 are not supported yet"
                )
                .unwrap();
                return;
            }
        }
        let mut previous = Connector::Seq;
        for pipeline in parsed.pipelines() {
            let run = match previous {
                Connector::Seq | Connector::End => true,
                Connector::And => self.env.status == 0,
                Connector::Or => self.env.status != 0,
            };
            if run {
                self.run(&parsed, pipeline);
            }
            previous = pipeline.connector;
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
        &Startup::new(b"/", None, args, &[]).unwrap(),
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
        client::spawn(
            b"/bad-elf",
            &Startup::new(b"/", None, &[b"bad"], &[]).unwrap()
        ),
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
    let pid = client::spawn(
        b"/custom",
        &Startup::new(b"/", None, &[b"true"], &[]).unwrap(),
    )
    .unwrap();
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
