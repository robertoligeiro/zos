use crate::{
    fs::{Filesystem, Node},
    *,
};

pub const READ: u8 = 1;
pub const WRITE: u8 = 2;
pub const CREATE: u8 = 4;
pub const TRUNCATE: u8 = 8;
pub const APPEND: u8 = 16;
pub const PATH: usize = 192;
const DESCRIPTORS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub inode: u16,
    pub kind: u8,
    pub size: u32,
    pub length: u8,
    pub name: [u8; NAME],
}
impl Entry {
    pub fn from_node(inode: usize, node: Node) -> Self {
        Self {
            inode: inode as u16,
            kind: node.kind,
            size: node.size,
            length: node.length,
            name: node.name,
        }
    }
    pub fn encode(self) -> [u8; 32] {
        let mut bytes = [0; 32];
        bytes[..2].copy_from_slice(&self.inode.to_le_bytes());
        bytes[2] = self.kind;
        bytes[3..7].copy_from_slice(&self.size.to_le_bytes());
        bytes[7] = self.length;
        bytes[8..].copy_from_slice(&self.name);
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 || !matches!(bytes[2], 1 | 2) || bytes[7] as usize > NAME {
            return Err(Error::Corrupt);
        }
        Ok(Self {
            inode: u16::from_le_bytes(bytes[..2].try_into().unwrap()),
            kind: bytes[2],
            size: u32::from_le_bytes(bytes[3..7].try_into().unwrap()),
            length: bytes[7],
            name: bytes[8..].try_into().unwrap(),
        })
    }
}

pub trait Backend {
    fn lookup(&mut self, path: &[u8]) -> Result<Entry>;
    fn create(&mut self, path: &[u8], kind: u8) -> Result<Entry>;
    fn stat(&mut self, inode: usize) -> Result<Entry>;
    fn read(&mut self, inode: usize, offset: usize, bytes: &mut [u8]) -> Result<usize>;
    fn write(&mut self, inode: usize, offset: usize, bytes: &[u8]) -> Result<usize>;
    fn truncate(&mut self, inode: usize) -> Result<()>;
    fn remove(&mut self, path: &[u8]) -> Result<()>;
    fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<()>;
    fn readdir(&mut self, inode: usize, ordinal: usize) -> Result<Option<Entry>>;
    fn sync(&mut self) -> Result<()>;
}
impl<D: Disk> Backend for Filesystem<D> {
    fn lookup(&mut self, path: &[u8]) -> Result<Entry> {
        let id = Filesystem::lookup(self, path)?;
        <Self as Backend>::stat(self, id)
    }
    fn create(&mut self, path: &[u8], kind: u8) -> Result<Entry> {
        let id = Filesystem::create(self, path, kind)?;
        <Self as Backend>::stat(self, id)
    }
    fn stat(&mut self, inode: usize) -> Result<Entry> {
        Ok(Entry::from_node(inode, Filesystem::stat(self, inode)?))
    }
    fn read(&mut self, inode: usize, offset: usize, bytes: &mut [u8]) -> Result<usize> {
        Filesystem::read(self, inode, offset, bytes)
    }
    fn write(&mut self, inode: usize, offset: usize, bytes: &[u8]) -> Result<usize> {
        Filesystem::write(self, inode, offset, bytes)
    }
    fn truncate(&mut self, inode: usize) -> Result<()> {
        Filesystem::truncate(self, inode)
    }
    fn remove(&mut self, path: &[u8]) -> Result<()> {
        Filesystem::remove(self, path)
    }
    fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<()> {
        Filesystem::rename(self, from, to)
    }
    fn readdir(&mut self, inode: usize, ordinal: usize) -> Result<Option<Entry>> {
        Ok(Filesystem::readdir(self, inode, ordinal)?.map(|(id, node)| Entry::from_node(id, node)))
    }
    fn sync(&mut self) -> Result<()> {
        Filesystem::sync(self)
    }
}

pub struct Path {
    bytes: [u8; PATH],
    length: usize,
}
impl Path {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}
pub fn normalize(path: &[u8]) -> Result<Path> {
    if path.len() > PATH || !path.starts_with(b"/") || path.iter().any(|b| !(32..=126).contains(b))
    {
        return Err(Error::Invalid);
    }
    let mut normalized = Path {
        bytes: [0; PATH],
        length: 1,
    };
    normalized.bytes[0] = b'/';
    for component in path.split(|&b| b == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }
        if component == b".." {
            if normalized.length == 1 {
                return Err(Error::Invalid);
            }
            let slash = normalized.bytes[..normalized.length]
                .iter()
                .rposition(|&b| b == b'/')
                .unwrap();
            normalized.length = slash.max(1);
            continue;
        }
        if component.len() > NAME {
            return Err(Error::Invalid);
        }
        if normalized.length > 1 {
            normalized.bytes[normalized.length] = b'/';
            normalized.length += 1;
        }
        let end = normalized.length + component.len();
        if end > PATH {
            return Err(Error::Invalid);
        }
        normalized.bytes[normalized.length..end].copy_from_slice(component);
        normalized.length = end;
    }
    Ok(normalized)
}

#[derive(Clone, Copy)]
pub struct Mount {
    pub prefix: &'static [u8],
    pub channel: usize,
}
/// Bytes a pipe holds between its writer and its reader.
pub const PIPE_BUFFER: usize = 1024;
const PIPES: usize = 8;
const OPENS: usize = DESCRIPTORS;
const WAKES: usize = 2 * PIPES;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File(usize),
    PipeRead(u8),
    PipeWrite(u8),
}

/// An open file or pipe end. Descriptors made by `dup`, and the ones a parent hands to
/// a child, share one of these, so they share the file offset and keep a pipe end open
/// until the last of them is closed.
#[derive(Clone, Copy)]
struct Open {
    kind: Kind,
    offset: usize,
    flags: u8,
    refs: u16,
}

#[derive(Clone, Copy)]
struct Descriptor {
    owner: usize,
    open: u8,
    generation: u64,
}

/// The outcome of a pipe transfer that may have to wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Io {
    Done(usize),
    /// The caller's reply token is kept; the answer arrives later from `next_wake`.
    Parked,
}

/// A parked request that has finished. The server replies to `token` with `result`
/// (and, for a read, `data[..n]`).
pub struct Wake {
    pub token: usize,
    pub result: Result<usize>,
    pub reading: bool,
    pub data: [u8; PAYLOAD],
}

struct ReadWait {
    token: usize,
    owner: usize,
    count: usize,
}

struct WriteWait {
    token: usize,
    owner: usize,
    /// What the caller asked to write, reported when all of it is in the pipe.
    total: usize,
    data: [u8; PAYLOAD],
    start: usize,
    end: usize,
}

struct Pipe {
    used: bool,
    buffer: [u8; PIPE_BUFFER],
    head: usize,
    length: usize,
    readers: u8,
    writers: u8,
    reader: Option<ReadWait>,
    writer: Option<WriteWait>,
}

impl Pipe {
    const fn new() -> Self {
        Self {
            used: false,
            buffer: [0; PIPE_BUFFER],
            head: 0,
            length: 0,
            readers: 0,
            writers: 0,
            reader: None,
            writer: None,
        }
    }
    fn take(&mut self, out: &mut [u8]) -> usize {
        let count = out.len().min(self.length);
        for byte in out.iter_mut().take(count) {
            *byte = self.buffer[self.head];
            self.head = (self.head + 1) % PIPE_BUFFER;
        }
        self.length -= count;
        count
    }
    fn put(&mut self, bytes: &[u8]) -> usize {
        let count = bytes.len().min(PIPE_BUFFER - self.length);
        for &byte in &bytes[..count] {
            self.buffer[(self.head + self.length) % PIPE_BUFFER] = byte;
            self.length += 1;
        }
        count
    }
}

pub struct Vfs<B: Backend> {
    pub backend: B,
    pub mounts: [Mount; 1],
    descriptors: [Option<Descriptor>; DESCRIPTORS],
    generations: [u64; DESCRIPTORS],
    opens: [Option<Open>; OPENS],
    pipes: [Pipe; PIPES],
    wakes: [Option<Wake>; WAKES],
}
impl<B: Backend> Vfs<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            mounts: [Mount {
                prefix: b"/",
                channel: FILESYSTEM,
            }],
            descriptors: [None; DESCRIPTORS],
            generations: [0; DESCRIPTORS],
            opens: [None; OPENS],
            pipes: [const { Pipe::new() }; PIPES],
            wakes: [const { None }; WAKES],
        }
    }
    fn path(&self, path: &[u8]) -> Result<Path> {
        let path = normalize(path)?;
        if !self
            .mounts
            .iter()
            .any(|mount| path.bytes().starts_with(mount.prefix))
        {
            return Err(Error::NotFound);
        }
        Ok(path)
    }
    fn descriptor(&self, owner: usize, handle: u64) -> Result<(usize, Descriptor)> {
        let slot = (handle & 255) as usize;
        let fd = self
            .descriptors
            .get(slot)
            .and_then(|fd| *fd)
            .filter(|fd| fd.owner == owner && fd.generation == handle >> 8)
            .ok_or(Error::BadFd)?;
        Ok((slot, fd))
    }
    fn open_of(&self, fd: Descriptor) -> Open {
        self.opens[fd.open as usize].expect("descriptor without an open file")
    }
    /// A free descriptor slot with the next generation for it.
    fn fresh_descriptor(&self) -> Result<(usize, u64)> {
        let slot = self
            .descriptors
            .iter()
            .position(Option::is_none)
            .ok_or(Error::NoSpace)?;
        let generation = self.generations[slot]
            .checked_add(1)
            .filter(|&generation| generation <= u64::MAX >> 8)
            .ok_or(Error::NoSpace)?;
        Ok((slot, generation))
    }
    fn install(&mut self, slot: usize, generation: u64, owner: usize, open: usize) -> u64 {
        self.generations[slot] = generation;
        self.descriptors[slot] = Some(Descriptor {
            owner,
            open: open as u8,
            generation,
        });
        (generation << 8) | slot as u64
    }
    fn new_open(&mut self, open: Open) -> Result<usize> {
        let index = self
            .opens
            .iter()
            .position(Option::is_none)
            .ok_or(Error::NoSpace)?;
        self.opens[index] = Some(open);
        Ok(index)
    }

    pub fn open(&mut self, owner: usize, path: &[u8], flags: u8) -> Result<u64> {
        if flags & !31 != 0
            || flags & 3 == 0
            || flags & (TRUNCATE | APPEND) != 0 && flags & WRITE == 0
        {
            return Err(Error::Invalid);
        }
        let path = self.path(path)?;
        let (slot, generation) = self.fresh_descriptor()?;
        if self.opens.iter().all(Option::is_some) {
            return Err(Error::NoSpace);
        }
        let entry = match self.backend.lookup(path.bytes()) {
            Ok(entry) => entry,
            Err(Error::NotFound) if flags & CREATE != 0 => self.backend.create(path.bytes(), 2)?,
            Err(error) => return Err(error),
        };
        if entry.kind != 2 {
            return Err(Error::IsDir);
        }
        if flags & TRUNCATE != 0 {
            self.backend.truncate(entry.inode as usize)?;
        }
        let open = self.new_open(Open {
            kind: Kind::File(entry.inode as usize),
            offset: 0,
            flags,
            refs: 1,
        })?;
        Ok(self.install(slot, generation, owner, open))
    }

    /// Reads from a file, or from a pipe when it has data (or is at its end). A pipe
    /// with nothing to read parks the caller's `token` and returns `Parked`.
    pub fn read_io(
        &mut self,
        owner: usize,
        handle: u64,
        bytes: &mut [u8],
        token: usize,
    ) -> Result<Io> {
        let (_, fd) = self.descriptor(owner, handle)?;
        let open = self.open_of(fd);
        if open.flags & READ == 0 {
            return Err(Error::Denied);
        }
        match open.kind {
            Kind::File(inode) => {
                let count = self.backend.read(inode, open.offset, bytes)?;
                if count > bytes.len() {
                    return Err(Error::Corrupt);
                }
                let offset = open.offset.checked_add(count).ok_or(Error::TooLarge)?;
                self.opens[fd.open as usize].as_mut().unwrap().offset = offset;
                Ok(Io::Done(count))
            }
            Kind::PipeRead(pipe) => {
                let pipe = pipe as usize;
                if bytes.is_empty() {
                    return Ok(Io::Done(0));
                }
                let state = &mut self.pipes[pipe];
                if state.length > 0 {
                    let count = state.take(bytes);
                    self.pump(pipe);
                    return Ok(Io::Done(count));
                }
                if state.writers == 0 {
                    return Ok(Io::Done(0));
                }
                if token == 0 || state.reader.is_some() {
                    return Err(if token == 0 {
                        Error::Invalid
                    } else {
                        Error::Denied
                    });
                }
                state.reader = Some(ReadWait {
                    token,
                    owner,
                    count: bytes.len(),
                });
                Ok(Io::Parked)
            }
            Kind::PipeWrite(_) => Err(Error::Denied),
        }
    }

    /// Writes to a file, or to a pipe: what fits goes in at once, and a caller whose
    /// data does not all fit is parked until the reader makes room.
    pub fn write_io(
        &mut self,
        owner: usize,
        handle: u64,
        bytes: &[u8],
        token: usize,
    ) -> Result<Io> {
        let (_, fd) = self.descriptor(owner, handle)?;
        let open = self.open_of(fd);
        if open.flags & WRITE == 0 {
            return Err(Error::Denied);
        }
        match open.kind {
            Kind::File(inode) => {
                let offset = if open.flags & APPEND != 0 {
                    self.backend.stat(inode)?.size as usize
                } else {
                    open.offset
                };
                let count = self.backend.write(inode, offset, bytes)?;
                if count > bytes.len() {
                    return Err(Error::Corrupt);
                }
                let end = offset.checked_add(count).ok_or(Error::TooLarge)?;
                self.opens[fd.open as usize].as_mut().unwrap().offset = end;
                Ok(Io::Done(count))
            }
            Kind::PipeWrite(pipe) => {
                let pipe = pipe as usize;
                if bytes.len() > PAYLOAD {
                    return Err(Error::TooLarge);
                }
                let state = &mut self.pipes[pipe];
                if state.readers == 0 {
                    return Err(Error::BrokenPipe);
                }
                if state.writer.is_some() && !bytes.is_empty() {
                    // Another writer is already waiting; keep writes in order.
                    return Err(if token == 0 {
                        Error::Invalid
                    } else {
                        Error::Denied
                    });
                }
                let count = state.put(bytes);
                if count == bytes.len() {
                    self.pump(pipe);
                    return Ok(Io::Done(count));
                }
                if token == 0 {
                    self.pump(pipe);
                    return Err(Error::Invalid);
                }
                let mut wait = WriteWait {
                    token,
                    owner,
                    total: bytes.len(),
                    data: [0; PAYLOAD],
                    start: 0,
                    end: bytes.len() - count,
                };
                wait.data[..wait.end].copy_from_slice(&bytes[count..]);
                state.writer = Some(wait);
                self.pump(pipe);
                // The pump may already have finished it (a reader was waiting).
                if self.pipes[pipe].writer.is_some() {
                    Ok(Io::Parked)
                } else {
                    Err(Error::Corrupt)
                }
            }
            Kind::PipeRead(_) => Err(Error::Denied),
        }
    }

    pub fn read(&mut self, owner: usize, handle: u64, bytes: &mut [u8]) -> Result<usize> {
        match self.read_io(owner, handle, bytes, 0)? {
            Io::Done(count) => Ok(count),
            Io::Parked => Err(Error::Invalid),
        }
    }
    pub fn write(&mut self, owner: usize, handle: u64, bytes: &[u8]) -> Result<usize> {
        match self.write_io(owner, handle, bytes, 0)? {
            Io::Done(count) => Ok(count),
            Io::Parked => Err(Error::Invalid),
        }
    }

    /// Creates a pipe and returns its (read end, write end) descriptors.
    pub fn pipe(&mut self, owner: usize) -> Result<(u64, u64)> {
        let pipe = self
            .pipes
            .iter()
            .position(|pipe| !pipe.used)
            .ok_or(Error::NoSpace)?;
        let free_descriptors = self.descriptors.iter().filter(|fd| fd.is_none()).count();
        let free_opens = self.opens.iter().filter(|open| open.is_none()).count();
        if free_descriptors < 2 || free_opens < 2 {
            return Err(Error::NoSpace);
        }
        self.pipes[pipe] = Pipe::new();
        self.pipes[pipe].used = true;
        self.pipes[pipe].readers = 1;
        self.pipes[pipe].writers = 1;
        let mut handles = [0; 2];
        for (index, (kind, flags)) in [
            (Kind::PipeRead(pipe as u8), READ),
            (Kind::PipeWrite(pipe as u8), WRITE),
        ]
        .into_iter()
        .enumerate()
        {
            let (slot, generation) = self.fresh_descriptor()?;
            let open = self.new_open(Open {
                kind,
                offset: 0,
                flags,
                refs: 1,
            })?;
            handles[index] = self.install(slot, generation, owner, open);
        }
        Ok((handles[0], handles[1]))
    }

    /// A second descriptor for the same open file or pipe end (shared offset).
    pub fn dup(&mut self, owner: usize, handle: u64) -> Result<u64> {
        let (_, fd) = self.descriptor(owner, handle)?;
        let (slot, generation) = self.fresh_descriptor()?;
        let open = self.opens[fd.open as usize].as_mut().unwrap();
        open.refs = open.refs.checked_add(1).ok_or(Error::NoSpace)?;
        Ok(self.install(slot, generation, owner, fd.open as usize))
    }

    /// Moves a descriptor to another process (a child about to start): the handle
    /// value stays the same, `owner` loses it and `target` gains it.
    pub fn grant(&mut self, owner: usize, handle: u64, target: usize) -> Result<()> {
        let (slot, mut fd) = self.descriptor(owner, handle)?;
        fd.owner = target;
        self.descriptors[slot] = Some(fd);
        Ok(())
    }

    fn release(&mut self, open: usize) {
        let state = self.opens[open].as_mut().unwrap();
        state.refs -= 1;
        if state.refs != 0 {
            return;
        }
        let kind = state.kind;
        self.opens[open] = None;
        match kind {
            Kind::File(_) => (),
            Kind::PipeRead(pipe) => {
                let pipe = pipe as usize;
                self.pipes[pipe].readers -= 1;
                self.pump(pipe);
            }
            Kind::PipeWrite(pipe) => {
                let pipe = pipe as usize;
                self.pipes[pipe].writers -= 1;
                self.pump(pipe);
            }
        }
    }

    pub fn close(&mut self, owner: usize, handle: u64) -> Result<()> {
        let (slot, fd) = self.descriptor(owner, handle)?;
        self.descriptors[slot] = None;
        self.release(fd.open as usize);
        Ok(())
    }

    /// A process is gone: close everything it owned and forget any request of its that
    /// was parked on a pipe.
    pub fn close_owner(&mut self, owner: usize) {
        for pipe in &mut self.pipes {
            if pipe.reader.as_ref().is_some_and(|wait| wait.owner == owner) {
                pipe.reader = None;
            }
            if pipe.writer.as_ref().is_some_and(|wait| wait.owner == owner) {
                pipe.writer = None;
            }
        }
        for slot in 0..DESCRIPTORS {
            if let Some(fd) = self.descriptors[slot] {
                if fd.owner == owner {
                    self.descriptors[slot] = None;
                    self.release(fd.open as usize);
                }
            }
        }
    }

    fn wake(&mut self, wake: Wake) {
        let slot = self
            .wakes
            .iter_mut()
            .find(|slot| slot.is_none())
            .expect("more finished pipe requests than there can be waiters");
        *slot = Some(wake);
    }

    /// Finishes whatever parked requests the pipe's current state allows.
    fn pump(&mut self, index: usize) {
        loop {
            let mut progress = false;
            let pipe = &mut self.pipes[index];
            if let Some(wait) = pipe.reader.as_ref() {
                if pipe.length > 0 || pipe.writers == 0 {
                    let (token, count) = (wait.token, wait.count);
                    pipe.reader = None;
                    let mut wake = Wake {
                        token,
                        result: Ok(0),
                        reading: true,
                        data: [0; PAYLOAD],
                    };
                    let limit = count.min(PAYLOAD);
                    wake.result = Ok(pipe.take(&mut wake.data[..limit]));
                    self.wake(wake);
                    progress = true;
                }
            }
            let pipe = &mut self.pipes[index];
            if let Some(mut wait) = pipe.writer.take() {
                if pipe.readers == 0 {
                    self.wake(Wake {
                        token: wait.token,
                        result: Err(Error::BrokenPipe),
                        reading: false,
                        data: [0; PAYLOAD],
                    });
                    progress = true;
                } else if pipe.length < PIPE_BUFFER {
                    let count = pipe.put(&wait.data[wait.start..wait.end]);
                    wait.start += count;
                    progress |= count > 0;
                    if wait.start == wait.end {
                        self.wake(Wake {
                            token: wait.token,
                            result: Ok(wait.total),
                            reading: false,
                            data: [0; PAYLOAD],
                        });
                    } else {
                        pipe.writer = Some(wait);
                    }
                } else {
                    pipe.writer = Some(wait);
                }
            }
            if !progress {
                break;
            }
        }
        let pipe = &mut self.pipes[index];
        if pipe.readers == 0 && pipe.writers == 0 && pipe.reader.is_none() && pipe.writer.is_none()
        {
            pipe.used = false;
        }
    }

    /// The next parked request that has finished, if any.
    pub fn next_wake(&mut self) -> Option<Wake> {
        self.wakes.iter_mut().find_map(Option::take)
    }

    /// How many descriptors, open files and pipes exist (for leak checks).
    pub fn counts(&self) -> (usize, usize, usize) {
        (
            self.descriptors.iter().flatten().count(),
            self.opens.iter().flatten().count(),
            self.pipes.iter().filter(|pipe| pipe.used).count(),
        )
    }

    pub fn stat(&mut self, path: &[u8]) -> Result<Entry> {
        let path = self.path(path)?;
        self.backend.lookup(path.bytes())
    }
    pub fn mkdir(&mut self, path: &[u8]) -> Result<()> {
        let path = self.path(path)?;
        self.backend.create(path.bytes(), 1).map(|_| ())
    }
    pub fn remove(&mut self, path: &[u8]) -> Result<()> {
        let path = self.path(path)?;
        let entry = self.backend.lookup(path.bytes())?;
        if self
            .opens
            .iter()
            .flatten()
            .any(|open| open.kind == Kind::File(entry.inode as usize))
        {
            return Err(Error::Denied);
        }
        self.backend.remove(path.bytes())
    }
    pub fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<()> {
        let from = self.path(from)?;
        let to = self.path(to)?;
        self.backend.rename(from.bytes(), to.bytes())
    }
    pub fn readdir(&mut self, path: &[u8], ordinal: usize) -> Result<Option<Entry>> {
        let path = self.path(path)?;
        let entry = self.backend.lookup(path.bytes())?;
        self.backend.readdir(entry.inode as usize, ordinal)
    }
    pub fn sync(&mut self) -> Result<()> {
        self.backend.sync()
    }
}
