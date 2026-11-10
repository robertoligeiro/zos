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
#[derive(Clone, Copy)]
struct Descriptor {
    owner: usize,
    inode: usize,
    offset: usize,
    flags: u8,
    generation: u64,
}

pub struct Vfs<B: Backend> {
    pub backend: B,
    pub mounts: [Mount; 1],
    descriptors: [Option<Descriptor>; DESCRIPTORS],
    generations: [u64; DESCRIPTORS],
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
    pub fn open(&mut self, owner: usize, path: &[u8], flags: u8) -> Result<u64> {
        if flags & !31 != 0
            || flags & 3 == 0
            || flags & (TRUNCATE | APPEND) != 0 && flags & WRITE == 0
        {
            return Err(Error::Invalid);
        }
        let path = self.path(path)?;
        let slot = self
            .descriptors
            .iter()
            .position(Option::is_none)
            .ok_or(Error::NoSpace)?;
        let generation = self.generations[slot]
            .checked_add(1)
            .filter(|&generation| generation <= u64::MAX >> 8)
            .ok_or(Error::NoSpace)?;
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
        self.generations[slot] = generation;
        self.descriptors[slot] = Some(Descriptor {
            owner,
            inode: entry.inode as usize,
            offset: 0,
            flags,
            generation,
        });
        Ok((generation << 8) | slot as u64)
    }
    pub fn read(&mut self, owner: usize, handle: u64, bytes: &mut [u8]) -> Result<usize> {
        let (slot, mut fd) = self.descriptor(owner, handle)?;
        if fd.flags & READ == 0 {
            return Err(Error::Denied);
        }
        let count = self.backend.read(fd.inode, fd.offset, bytes)?;
        if count > bytes.len() {
            return Err(Error::Corrupt);
        }
        fd.offset = fd.offset.checked_add(count).ok_or(Error::TooLarge)?;
        self.descriptors[slot] = Some(fd);
        Ok(count)
    }
    pub fn write(&mut self, owner: usize, handle: u64, bytes: &[u8]) -> Result<usize> {
        let (slot, mut fd) = self.descriptor(owner, handle)?;
        if fd.flags & WRITE == 0 {
            return Err(Error::Denied);
        }
        let offset = if fd.flags & APPEND != 0 {
            self.backend.stat(fd.inode)?.size as usize
        } else {
            fd.offset
        };
        let count = self.backend.write(fd.inode, offset, bytes)?;
        if count > bytes.len() {
            return Err(Error::Corrupt);
        }
        fd.offset = offset.checked_add(count).ok_or(Error::TooLarge)?;
        self.descriptors[slot] = Some(fd);
        Ok(count)
    }
    pub fn close(&mut self, owner: usize, handle: u64) -> Result<()> {
        let (slot, _) = self.descriptor(owner, handle)?;
        self.descriptors[slot] = None;
        Ok(())
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
            .descriptors
            .iter()
            .flatten()
            .any(|fd| fd.inode == entry.inode as usize)
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
