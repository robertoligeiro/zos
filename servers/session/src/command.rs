#![no_std]
#![no_main]

use zos_libos as os;
use zos_session::{client, *};
use zos_storage::{protocol as api, vfs::*, Error, Result};

fn output(fd: Option<u64>, bytes: &[u8]) -> Result<()> {
    if let Some(fd) = fd {
        write_all(fd, bytes)
    } else {
        console(bytes);
        Ok(())
    }
}

/// Buffered output: tools that print a lot send it in large pieces, not per line.
struct Sink {
    fd: Option<u64>,
    bytes: [u8; 900],
    length: usize,
}
impl Sink {
    fn new(fd: Option<u64>) -> Self {
        Self {
            fd,
            bytes: [0; 900],
            length: 0,
        }
    }
    fn put(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            if self.length == self.bytes.len() {
                self.flush()?;
            }
            let room = (self.bytes.len() - self.length).min(data.len());
            self.bytes[self.length..self.length + room].copy_from_slice(&data[..room]);
            self.length += room;
            data = &data[room..];
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        let length = core::mem::take(&mut self.length);
        if length == 0 {
            return Ok(());
        }
        output(self.fd, &self.bytes[..length])
    }
}

/// Longest line the line-oriented tools accept.
const LINE: usize = 1024;

/// Reads `fd` (or a file, or nothing) in pieces and hands each piece to `piece`;
/// stops early, without error, when `piece` returns false.
fn stream(fd: Option<u64>, mut piece: impl FnMut(&[u8]) -> Result<bool>) -> Result<()> {
    let Some(fd) = fd else {
        return Ok(());
    };
    let mut bytes = [0; zos_storage::PAYLOAD];
    loop {
        let count = api::read(fd, &mut bytes)?;
        if count == 0 || !piece(&bytes[..count])? {
            return Ok(());
        }
    }
}

/// `stream`, but in whole lines (without their newline). A last line with no newline
/// is still delivered. Lines over `LINE` bytes are an error, not silently cut.
fn lines(fd: Option<u64>, mut line: impl FnMut(&[u8]) -> Result<bool>) -> Result<()> {
    let mut buffer = [0u8; LINE];
    let mut length = 0;
    let mut stopped = false;
    stream(fd, |piece| {
        for &byte in piece {
            if byte == b'\n' {
                if !line(&buffer[..length])? {
                    stopped = true;
                    return Ok(false);
                }
                length = 0;
            } else {
                if length == LINE {
                    return Err(Error::TooLarge);
                }
                buffer[length] = byte;
                length += 1;
            }
        }
        Ok(true)
    })?;
    if !stopped && length > 0 {
        line(&buffer[..length])?;
    }
    Ok(())
}

/// The inputs of a tool: its file arguments, or stdin when there are none. Each is
/// opened for the call and closed after it.
fn each_input(
    inv: &client::Invocation<'_>,
    files: &[&[u8]],
    mut body: impl FnMut(Option<&[u8]>, Option<u64>) -> Result<bool>,
) -> Result<()> {
    if files.is_empty() {
        body(None, inv.stdin)?;
        return Ok(());
    }
    for &name in files {
        if name == b"-" {
            if !body(Some(name), inv.stdin)? {
                break;
            }
            continue;
        }
        let fd = api::open(path(inv.cwd, name)?.bytes(), READ)?;
        let result = body(Some(name), Some(fd));
        let closed = api::close(fd);
        let more = result?;
        closed?;
        if !more {
            break;
        }
    }
    Ok(())
}

fn number(text: &[u8]) -> Result<u64> {
    core::str::from_utf8(text)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or(Error::Invalid)
}

fn decimal(value: u64, out: &mut [u8; 20]) -> &[u8] {
    let mut at = out.len();
    let mut value = value;
    loop {
        at -= 1;
        out[at] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            return &out[at..];
        }
    }
}

fn word_break(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// wc [-l] [-w] [-c] [FILE...]: lines, words and bytes (all three by default).
fn wc(inv: &client::Invocation<'_>, sink: &mut Sink) -> Result<()> {
    let (mut want_lines, mut want_words, mut want_bytes) = (false, false, false);
    let mut first_file = 1;
    while first_file < inv.argc
        && inv.argv[first_file].starts_with(b"-")
        && inv.argv[first_file].len() > 1
    {
        for &flag in &inv.argv[first_file][1..] {
            match flag {
                b'l' => want_lines = true,
                b'w' => want_words = true,
                b'c' => want_bytes = true,
                _ => return Err(Error::Invalid),
            }
        }
        first_file += 1;
    }
    if !(want_lines || want_words || want_bytes) {
        (want_lines, want_words, want_bytes) = (true, true, true);
    }
    let files = &inv.argv[first_file..inv.argc];
    let mut total = (0u64, 0u64, 0u64);
    each_input(inv, files, |name, fd| {
        let (mut line_count, mut word_count, mut byte_count) = (0u64, 0u64, 0u64);
        let mut in_word = false;
        stream(fd, |piece| {
            byte_count += piece.len() as u64;
            for &byte in piece {
                line_count += u64::from(byte == b'\n');
                let separator = word_break(byte);
                word_count += u64::from(in_word && separator);
                in_word = !separator;
            }
            Ok(true)
        })?;
        word_count += u64::from(in_word);
        total = (
            total.0 + line_count,
            total.1 + word_count,
            total.2 + byte_count,
        );
        let mut digits = [0; 20];
        let mut first = true;
        for (wanted, value) in [
            (want_lines, line_count),
            (want_words, word_count),
            (want_bytes, byte_count),
        ] {
            if wanted {
                if !first {
                    sink.put(b" ")?;
                }
                first = false;
                sink.put(decimal(value, &mut digits))?;
            }
        }
        if let Some(name) = name {
            sink.put(b" ")?;
            sink.put(name)?;
        }
        sink.put(b"\n")?;
        Ok(true)
    })?;
    if files.len() > 1 {
        let mut digits = [0; 20];
        let mut first = true;
        for (wanted, value) in [
            (want_lines, total.0),
            (want_words, total.1),
            (want_bytes, total.2),
        ] {
            if wanted {
                if !first {
                    sink.put(b" ")?;
                }
                first = false;
                sink.put(decimal(value, &mut digits))?;
            }
        }
        sink.put(b" total\n")?;
    }
    Ok(())
}

fn contains(line: &[u8], pattern: &[u8]) -> bool {
    pattern.is_empty() || line.windows(pattern.len()).any(|window| window == pattern)
}

/// grep [-v] [-c] PATTERN [FILE...]: lines containing the literal text PATTERN.
/// Status 1 when nothing matched.
fn grep(inv: &client::Invocation<'_>, sink: &mut Sink) -> Result<i64> {
    let (mut invert, mut count_only) = (false, false);
    let mut next = 1;
    while next < inv.argc && inv.argv[next].starts_with(b"-") && inv.argv[next].len() > 1 {
        for &flag in &inv.argv[next][1..] {
            match flag {
                b'v' => invert = true,
                b'c' => count_only = true,
                _ => return Err(Error::Invalid),
            }
        }
        next += 1;
    }
    if next >= inv.argc {
        return Err(Error::Invalid);
    }
    let pattern = inv.argv[next];
    let files = &inv.argv[next + 1..inv.argc];
    let mut matched = 0u64;
    each_input(inv, files, |name, fd| {
        let mut here = 0u64;
        lines(fd, |line| {
            if contains(line, pattern) != invert {
                here += 1;
                if !count_only {
                    if let (Some(name), true) = (name, files.len() > 1) {
                        sink.put(name)?;
                        sink.put(b":")?;
                    }
                    sink.put(line)?;
                    sink.put(b"\n")?;
                }
            }
            Ok(true)
        })?;
        matched += here;
        if count_only {
            let mut digits = [0; 20];
            sink.put(decimal(here, &mut digits))?;
            sink.put(b"\n")?;
        }
        Ok(true)
    })?;
    Ok(i64::from(matched == 0))
}

/// head [-n N] [FILE]: the first N lines (10 by default); stops reading after them,
/// so a producer upstream is told its reader has gone.
fn head(inv: &client::Invocation<'_>, sink: &mut Sink) -> Result<()> {
    let mut wanted = 10u64;
    let mut next = 1;
    if inv.argc > 2 && inv.argv[1] == b"-n" {
        wanted = number(inv.argv[2])?;
        next = 3;
    }
    let files = &inv.argv[next..inv.argc];
    each_input(inv, files, |_, fd| {
        let mut seen = 0u64;
        if wanted == 0 {
            return Ok(false);
        }
        lines(fd, |line| {
            sink.put(line)?;
            sink.put(b"\n")?;
            seen += 1;
            Ok(seen < wanted)
        })?;
        Ok(false)
    })
}

/// uniq [FILE]: drops a line equal to the one before it.
fn uniq(inv: &client::Invocation<'_>, sink: &mut Sink) -> Result<()> {
    let files = &inv.argv[1..inv.argc];
    let mut previous = [0u8; LINE];
    let mut previous_length = None;
    each_input(inv, files, |_, fd| {
        lines(fd, |line| {
            if previous_length != Some(line.len()) || previous[..line.len()] != *line {
                sink.put(line)?;
                sink.put(b"\n")?;
                previous[..line.len()].copy_from_slice(line);
                previous_length = Some(line.len());
            }
            Ok(true)
        })?;
        Ok(true)
    })
}

/// seq [FIRST] LAST: the numbers from FIRST (default 1) to LAST, one per line.
fn seq(inv: &client::Invocation<'_>, sink: &mut Sink) -> Result<()> {
    let (first, last) = match inv.argc {
        2 => (1, number(inv.argv[1])?),
        3 => (number(inv.argv[1])?, number(inv.argv[2])?),
        _ => return Err(Error::Invalid),
    };
    let mut value = first;
    while value <= last {
        let mut digits = [0; 20];
        sink.put(decimal(value, &mut digits))?;
        sink.put(b"\n")?;
        value += 1;
    }
    Ok(())
}

/// yes [TEXT]: TEXT (default "y") forever, until the reader is gone.
fn yes(inv: &client::Invocation<'_>, sink: &mut Sink) -> Result<()> {
    loop {
        if inv.argc > 1 {
            for (index, word) in inv.argv[1..inv.argc].iter().enumerate() {
                if index != 0 {
                    sink.put(b" ")?;
                }
                sink.put(word)?;
            }
        } else {
            sink.put(b"y")?;
        }
        sink.put(b"\n")?;
    }
}

fn cat(cwd: &[u8], source: &[u8], out: Option<u64>) -> Result<()> {
    let source = path(cwd, source)?;
    let fd = api::open(source.bytes(), READ)?;
    let result = (|| {
        let mut bytes = [0; zos_storage::PAYLOAD];
        loop {
            let count = api::read(fd, &mut bytes)?;
            if count == 0 {
                return Ok(());
            }
            output(out, &bytes[..count])?;
        }
    })();
    let closed = api::close(fd);
    result.and(closed)
}

fn copy(cwd: &[u8], source: &[u8], destination: &[u8]) -> Result<()> {
    let source = path(cwd, source)?;
    let destination = path(cwd, destination)?;
    let source_entry = api::stat(source.bytes())?;
    match api::stat(destination.bytes()) {
        Ok(entry) if entry.inode == source_entry.inode => return Err(Error::Invalid),
        Ok(_) | Err(Error::NotFound) => (),
        Err(error) => return Err(error),
    }
    let read = api::open(source.bytes(), READ)?;
    let result = (|| {
        let write = api::open(destination.bytes(), WRITE | CREATE | TRUNCATE)?;
        let result = (|| {
            let mut bytes = [0; zos_storage::PAYLOAD];
            loop {
                let count = api::read(read, &mut bytes)?;
                if count == 0 {
                    return Ok(());
                }
                write_all(write, &bytes[..count])?;
            }
        })();
        let closed = api::close(write);
        result.and(closed)
    })();
    let closed = api::close(read);
    result.and(closed)
}

fn execute(inv: &client::Invocation<'_>, out: Option<u64>) -> Result<i64> {
    let name = inv.argv[0].rsplit(|&b| b == b'/').next().unwrap();
    match name {
        b"echo" => {
            for (i, arg) in inv.argv[1..inv.argc].iter().enumerate() {
                if i != 0 {
                    output(out, b" ")?;
                }
                output(out, arg)?;
            }
            output(out, b"\n")?;
        }
        b"cat" if inv.argc >= 2 && !(inv.argc == 2 && inv.argv[1] == b"-") => {
            for arg in &inv.argv[1..inv.argc] {
                if *arg == b"-" {
                    let mut sink = Sink::new(out);
                    stream(inv.stdin, |piece| sink.put(piece).map(|_| true))?;
                    sink.flush()?;
                } else {
                    cat(inv.cwd, arg, out)?;
                }
            }
        }
        // `cat` with no file names copies its standard input.
        b"cat" => {
            let mut sink = Sink::new(out);
            stream(inv.stdin, |piece| sink.put(piece).map(|_| true))?;
            sink.flush()?;
        }
        b"wc" => {
            let mut sink = Sink::new(out);
            wc(inv, &mut sink)?;
            sink.flush()?;
        }
        b"grep" => {
            let mut sink = Sink::new(out);
            let status = grep(inv, &mut sink);
            sink.flush()?;
            return status;
        }
        b"head" => {
            let mut sink = Sink::new(out);
            head(inv, &mut sink)?;
            sink.flush()?;
        }
        b"uniq" => {
            let mut sink = Sink::new(out);
            uniq(inv, &mut sink)?;
            sink.flush()?;
        }
        b"seq" => {
            let mut sink = Sink::new(out);
            seq(inv, &mut sink)?;
            sink.flush()?;
        }
        b"yes" => {
            let mut sink = Sink::new(out);
            yes(inv, &mut sink)?;
        }
        b"cp" if inv.argc == 3 => copy(inv.cwd, inv.argv[1], inv.argv[2])?,
        b"true" if inv.argc == 1 => (),
        b"false" if inv.argc == 1 => return Ok(1),
        b"uname" if inv.argc == 1 => output(out, b"ZOS - Zombie OS (x86_64)\n")?,
        // printenv [NAME]: the variables this program inherited from the shell.
        b"printenv" if inv.argc <= 2 => {
            let mut found = inv.argc == 1;
            for (name, value) in &inv.env[..inv.envc] {
                if inv.argc == 2 {
                    if *name != inv.argv[1] {
                        continue;
                    }
                    found = true;
                    output(out, value)?;
                } else {
                    output(out, name)?;
                    output(out, b"=")?;
                    output(out, value)?;
                }
                output(out, b"\n")?;
            }
            if !found {
                return Ok(1);
            }
        }
        b"spin" if inv.argc == 1 => loop {
            core::hint::spin_loop();
        },
        b"block" | b"leak" if inv.argc == 2 => {
            api::open(path(inv.cwd, inv.argv[1])?.bytes(), WRITE | CREATE)?;
            os::map_memory(0x1000_0000, true, false).unwrap();
            if name == b"block" {
                let channel = os::channel_create().unwrap();
                api::rpc(PROCMGR, &[8], &mut [0]).unwrap();
                let mut bytes = [0; 64];
                os::receive(channel, &mut bytes).unwrap();
                return Err(Error::Invalid);
            }
        }
        b"fault" if inv.argc == 1 => {
            // SAFETY: dedicated ring-3 fault probe verifies kernel containment
            // and process-manager fault status; no memory is accessed.
            unsafe { core::arch::asm!("ud2", options(noreturn)) };
        }
        _ => return Err(Error::Invalid),
    }
    Ok(0)
}

#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    let startup = client::startup().unwrap();
    let inv = startup.decode().unwrap();
    assert_eq!(os::process_kill(3), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x170, 8), Err(os::Error::Denied));
    let result = (|| {
        if !inv.output.is_empty() && inv.argv[0].rsplit(|&b| b == b'/').next() == Some(b"cat") {
            match api::stat(inv.output) {
                Ok(destination) => {
                    for arg in &inv.argv[1..inv.argc] {
                        if api::stat(path(inv.cwd, arg)?.bytes())?.inode == destination.inode {
                            return Err(Error::Invalid);
                        }
                    }
                }
                Err(Error::NotFound) => (),
                Err(error) => return Err(error),
            }
        }
        // Standard output: the descriptor the shell granted (a pipe or a redirect), else
        // a file named in the startup packet, else the console.
        let opened = if inv.output.is_empty() {
            None
        } else {
            Some(api::open(
                inv.output,
                WRITE | CREATE | if inv.append { APPEND } else { TRUNCATE },
            )?)
        };
        let out = inv.stdout.or(opened);
        let result = execute(&inv, out);
        // Close the streams now (not at reaping) so a reader downstream sees end of
        // file as soon as this program is done.
        let mut closed = Ok(());
        for fd in [opened, inv.stdout, inv.stdin].into_iter().flatten() {
            let attempt = api::close(fd);
            if closed.is_ok() {
                closed = attempt;
            }
        }
        result.and_then(|status| closed.map(|_| status))
    })();
    let close_stderr = || {
        if let Some(fd) = inv.stderr {
            let _ = api::close(fd);
        }
    };
    match result {
        Ok(status) => {
            close_stderr();
            os::exit(status)
        }
        // The reader of our output went away: leave quietly, as a shell would.
        Err(Error::BrokenPipe) => {
            close_stderr();
            os::exit(141)
        }
        Err(error) => {
            zos_console::log(format_args!("COMMAND ERROR: {error:?}"));
            let message = b"command failed; see serial COMMAND ERROR\n";
            // A redirected standard error gets the message; otherwise the console does.
            match inv.stderr {
                Some(fd) if write_all(fd, message).is_ok() => (),
                _ => console(message),
            }
            close_stderr();
            os::exit(1)
        }
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
