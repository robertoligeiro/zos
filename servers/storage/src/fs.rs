use crate::*;

pub const BODY_SECTORS: usize = 9;
pub const BANKS: [u32; 2] = [8, 24];
const RESERVED: usize = 8;
const SUPER_MAGIC: &[u8; 8] = b"ZOSFS001";
const BANK_MAGIC: &[u8; 8] = b"ZOSBANK1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: u8,
    pub parent: u16,
    pub size: u32,
    pub length: u8,
    pub name: [u8; NAME],
    blocks: [u16; DIRECT],
}
const EMPTY: Node = Node {
    kind: 0,
    parent: 0,
    size: 0,
    length: 0,
    name: [0; NAME],
    blocks: [0; DIRECT],
};

#[derive(Clone)]
pub struct Metadata {
    pub generation: u64,
    bitmap: [u8; BLOCKS / 8],
    pub nodes: [Node; INODES],
}

impl Metadata {
    fn bit(&self, block: usize) -> bool {
        self.bitmap[block / 8] & (1 << (block % 8)) != 0
    }
    fn mark(&mut self, block: usize, used: bool) {
        if used {
            self.bitmap[block / 8] |= 1 << (block % 8);
        } else {
            self.bitmap[block / 8] &= !(1 << (block % 8));
        }
    }
    pub fn empty() -> Self {
        let mut metadata = Self {
            generation: 1,
            bitmap: [0; BLOCKS / 8],
            nodes: [EMPTY; INODES],
        };
        for block in 0..RESERVED {
            metadata.mark(block, true);
        }
        metadata.nodes[0].kind = 1;
        metadata
    }
    pub fn encode(&self) -> [u8; BODY_SECTORS * SECTOR] {
        let mut bytes = [0; BODY_SECTORS * SECTOR];
        bytes[..SECTOR].copy_from_slice(&self.bitmap);
        for (index, node) in self.nodes.iter().enumerate() {
            let start = SECTOR + index * 64;
            let record = &mut bytes[start..start + 64];
            record[0] = node.kind;
            record[1] = node.length;
            record[2..4].copy_from_slice(&node.parent.to_le_bytes());
            record[4..8].copy_from_slice(&node.size.to_le_bytes());
            for (i, block) in node.blocks.iter().enumerate() {
                record[8 + i * 2..10 + i * 2].copy_from_slice(&block.to_le_bytes());
            }
            record[40..].copy_from_slice(&node.name);
        }
        bytes
    }
    fn decode(generation: u64, bytes: &[u8; BODY_SECTORS * SECTOR]) -> Result<Self> {
        let mut metadata = Self {
            generation,
            bitmap: bytes[..SECTOR].try_into().unwrap(),
            nodes: [EMPTY; INODES],
        };
        for (index, node) in metadata.nodes.iter_mut().enumerate() {
            let start = SECTOR + index * 64;
            let record = &bytes[start..start + 64];
            node.kind = record[0];
            node.length = record[1];
            node.parent = u16::from_le_bytes(record[2..4].try_into().unwrap());
            node.size = u32::from_le_bytes(record[4..8].try_into().unwrap());
            for (i, block) in node.blocks.iter_mut().enumerate() {
                *block = u16::from_le_bytes(record[8 + i * 2..10 + i * 2].try_into().unwrap());
            }
            node.name.copy_from_slice(&record[40..]);
        }
        metadata.validate()?;
        Ok(metadata)
    }
    fn validate(&self) -> Result<()> {
        if self.generation == 0 || self.nodes[0] != (Node { kind: 1, ..EMPTY }) {
            return Err(Error::Corrupt);
        }
        let mut expected = [0; BLOCKS / 8];
        expected[0] = 255;
        for (id, node) in self.nodes.iter().enumerate() {
            if node.kind == 0 {
                if *node != EMPTY {
                    return Err(Error::Corrupt);
                }
                continue;
            }
            if !matches!(node.kind, 1 | 2) || node.length as usize > NAME {
                return Err(Error::Corrupt);
            }
            if id != 0 {
                let name = &node.name[..node.length as usize];
                if valid_name(name).is_err()
                    || node.name[node.length as usize..].iter().any(|&b| b != 0)
                {
                    return Err(Error::Corrupt);
                }
                let mut parent = id;
                let mut rooted = false;
                for _ in 0..INODES {
                    parent = self.nodes[parent].parent as usize;
                    if parent >= INODES || self.nodes[parent].kind != 1 {
                        return Err(Error::Corrupt);
                    }
                    if parent == 0 {
                        rooted = true;
                        break;
                    }
                }
                if !rooted {
                    return Err(Error::Corrupt);
                }
                if self.nodes[..id].iter().any(|other| {
                    other.kind != 0
                        && other.parent == node.parent
                        && other.length == node.length
                        && other.name == node.name
                }) {
                    return Err(Error::Corrupt);
                }
            }
            if node.size as usize > MAX_FILE || (node.kind == 1 && node.size != 0) {
                return Err(Error::Corrupt);
            }
            let count = (node.size as usize).div_ceil(BLOCK);
            for (index, &block) in node.blocks.iter().enumerate() {
                let block = block as usize;
                if index >= count {
                    if block != 0 {
                        return Err(Error::Corrupt);
                    }
                } else {
                    if !(RESERVED..BLOCKS).contains(&block)
                        || expected[block / 8] & (1 << (block % 8)) != 0
                    {
                        return Err(Error::Corrupt);
                    }
                    expected[block / 8] |= 1 << (block % 8);
                }
            }
        }
        if expected != self.bitmap {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
}

fn valid_name(name: &[u8]) -> Result<()> {
    if name.is_empty()
        || name.len() > NAME
        || matches!(name, b"." | b"..")
        || name.iter().any(|b| !(32..=126).contains(b) || *b == b'/')
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

pub fn superblock() -> [u8; SECTOR] {
    let mut bytes = [0; SECTOR];
    bytes[..8].copy_from_slice(SUPER_MAGIC);
    bytes[8..12].copy_from_slice(&1_u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&SECTORS.to_le_bytes());
    bytes[16..20].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    bytes[20..24].copy_from_slice(&(INODES as u32).to_le_bytes());
    let crc = crc32(&bytes[..508]);
    bytes[508..].copy_from_slice(&crc.to_le_bytes());
    bytes
}

pub fn bank_header(generation: u64, body: &[u8]) -> [u8; SECTOR] {
    let mut bytes = [0; SECTOR];
    bytes[..8].copy_from_slice(BANK_MAGIC);
    bytes[8..16].copy_from_slice(&generation.to_le_bytes());
    bytes[16..20].copy_from_slice(&crc32(body).to_le_bytes());
    let crc = crc32(&bytes[..508]);
    bytes[508..].copy_from_slice(&crc.to_le_bytes());
    bytes
}

fn read_bank(disk: &mut impl Disk, index: usize) -> Result<Option<Metadata>> {
    let mut header = [0; SECTOR];
    disk.read(BANKS[index], &mut header)?;
    if &header[..8] != BANK_MAGIC
        || crc32(&header[..508]) != u32::from_le_bytes(header[508..].try_into().unwrap())
    {
        return Ok(None);
    }
    let generation = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let mut body = [0; BODY_SECTORS * SECTOR];
    for (i, sector) in body.chunks_exact_mut(SECTOR).enumerate() {
        disk.read(BANKS[index] + 1 + i as u32, sector.try_into().unwrap())?;
    }
    if crc32(&body) != u32::from_le_bytes(header[16..20].try_into().unwrap()) {
        return Ok(None);
    }
    match Metadata::decode(generation, &body) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(Error::Corrupt) => Ok(None),
        Err(error) => Err(error),
    }
}

pub struct Filesystem<D: Disk> {
    disk: D,
    pub metadata: Metadata,
    protected: [u8; BLOCKS / 8],
    bank: usize,
    poisoned: bool,
}

impl<D: Disk> Filesystem<D> {
    pub fn mount(mut disk: D) -> Result<Self> {
        let mut bytes = [0; SECTOR];
        disk.read(0, &mut bytes)?;
        if bytes != superblock() {
            return Err(Error::Corrupt);
        }
        let banks = [read_bank(&mut disk, 0)?, read_bank(&mut disk, 1)?];
        let bank = match (&banks[0], &banks[1]) {
            (Some(a), Some(b)) => {
                if a.generation == b.generation && a.encode() != b.encode() {
                    return Err(Error::Corrupt);
                }
                usize::from(b.generation > a.generation)
            }
            (Some(_), None) => 0,
            (None, Some(_)) => 1,
            (None, None) => return Err(Error::Corrupt),
        };
        let metadata = banks[bank].as_ref().unwrap().clone();
        let mut protected = metadata.bitmap;
        if let Some(other) = &banks[1 - bank] {
            for (byte, value) in protected.iter_mut().zip(other.bitmap) {
                *byte |= value;
            }
        }
        Ok(Self {
            disk,
            metadata,
            protected,
            bank,
            poisoned: false,
        })
    }
    fn ready(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
    pub fn into_disk(self) -> D {
        self.disk
    }
    fn commit(&mut self, mut next: Metadata) -> Result<()> {
        next.generation = self
            .metadata
            .generation
            .checked_add(1)
            .ok_or(Error::NoSpace)?;
        next.validate()?;
        let body = next.encode();
        let target = 1 - self.bank;
        let result = (|| {
            self.disk.flush()?;
            for (index, bytes) in body.chunks_exact(SECTOR).enumerate() {
                self.disk
                    .write(BANKS[target] + 1 + index as u32, bytes.try_into().unwrap())?;
            }
            self.disk.flush()?;
            self.disk
                .write(BANKS[target], &bank_header(next.generation, &body))?;
            self.disk.flush()
        })();
        if result.is_err() {
            self.poisoned = true;
            return result;
        }
        self.protected = next.bitmap;
        for (byte, old) in self.protected.iter_mut().zip(self.metadata.bitmap) {
            *byte |= old;
        }
        self.metadata = next;
        self.bank = target;
        Ok(())
    }
    pub fn lookup(&self, path: &[u8]) -> Result<usize> {
        self.ready()?;
        if !path.starts_with(b"/") {
            return Err(Error::Invalid);
        }
        let mut id = 0;
        for name in path.split(|&b| b == b'/').filter(|name| !name.is_empty()) {
            valid_name(name)?;
            if self.metadata.nodes[id].kind != 1 {
                return Err(Error::NotDir);
            }
            id = self
                .metadata
                .nodes
                .iter()
                .position(|node| {
                    node.kind != 0
                        && node.parent as usize == id
                        && &node.name[..node.length as usize] == name
                })
                .ok_or(Error::NotFound)?;
        }
        Ok(id)
    }
    pub fn stat(&self, id: usize) -> Result<Node> {
        self.ready()?;
        self.metadata
            .nodes
            .get(id)
            .copied()
            .filter(|node| node.kind != 0)
            .ok_or(Error::NotFound)
    }
    fn parent<'a>(&self, path: &'a [u8]) -> Result<(usize, &'a [u8])> {
        let slash = path
            .iter()
            .rposition(|&b| b == b'/')
            .ok_or(Error::Invalid)?;
        let name = &path[slash + 1..];
        valid_name(name)?;
        let parent = self.lookup(if slash == 0 { b"/" } else { &path[..slash] })?;
        if self.stat(parent)?.kind != 1 {
            return Err(Error::NotDir);
        }
        Ok((parent, name))
    }
    pub fn create(&mut self, path: &[u8], kind: u8) -> Result<usize> {
        self.ready()?;
        if !matches!(kind, 1 | 2) {
            return Err(Error::Invalid);
        }
        match self.lookup(path) {
            Ok(_) => return Err(Error::Exists),
            Err(Error::NotFound) => (),
            Err(error) => return Err(error),
        }
        let (parent, name) = self.parent(path)?;
        let id = self
            .metadata
            .nodes
            .iter()
            .position(|node| node.kind == 0)
            .ok_or(Error::NoSpace)?;
        let mut next = self.metadata.clone();
        let mut node = Node {
            kind,
            parent: parent as u16,
            length: name.len() as u8,
            ..EMPTY
        };
        node.name[..name.len()].copy_from_slice(name);
        next.nodes[id] = node;
        self.commit(next)?;
        Ok(id)
    }
    pub fn read(&mut self, id: usize, offset: usize, bytes: &mut [u8]) -> Result<usize> {
        let node = self.stat(id)?;
        if node.kind != 2 {
            return Err(Error::IsDir);
        }
        let count = bytes.len().min((node.size as usize).saturating_sub(offset));
        let mut done = 0;
        let mut sector = [0; SECTOR];
        while done < count {
            let position = offset + done;
            let block = node.blocks[position / BLOCK] as u32;
            self.disk
                .read(block * 8 + (position % BLOCK / SECTOR) as u32, &mut sector)?;
            let within = position % SECTOR;
            let amount = (SECTOR - within).min(count - done);
            bytes[done..done + amount].copy_from_slice(&sector[within..within + amount]);
            done += amount;
        }
        Ok(count)
    }
    pub fn write(&mut self, id: usize, offset: usize, bytes: &[u8]) -> Result<usize> {
        let node = self.stat(id)?;
        if node.kind != 2 {
            return Err(Error::IsDir);
        }
        let end = offset
            .checked_add(bytes.len())
            .filter(|&end| end <= MAX_FILE)
            .ok_or(Error::TooLarge)?;
        if bytes.is_empty() {
            return Ok(0);
        }
        let size = end.max(node.size as usize);
        let count = size.div_ceil(BLOCK);
        let mut next = self.metadata.clone();
        let mut replacement = [0; DIRECT];
        for pointer in replacement.iter_mut().take(count) {
            let block = (RESERVED..BLOCKS)
                .find(|&block| {
                    self.protected[block / 8] & (1 << (block % 8)) == 0 && !next.bit(block)
                })
                .ok_or(Error::NoSpace)?;
            next.mark(block, true);
            *pointer = block as u16;
        }
        for &block in node.blocks.iter().filter(|&&block| block != 0) {
            next.mark(block as usize, false);
        }
        next.nodes[id].blocks = replacement;
        next.nodes[id].size = size as u32;
        let result = (|| {
            let mut buffer = [0; BLOCK];
            for (index, &block) in replacement.iter().enumerate().take(count) {
                buffer.fill(0);
                if node.blocks[index] != 0 {
                    for (sector, part) in buffer.chunks_exact_mut(SECTOR).enumerate() {
                        self.disk.read(
                            node.blocks[index] as u32 * 8 + sector as u32,
                            part.try_into().unwrap(),
                        )?;
                    }
                }
                let start = index * BLOCK;
                let low = offset.max(start);
                let high = end.min(start + BLOCK);
                if low < high {
                    buffer[low - start..high - start]
                        .copy_from_slice(&bytes[low - offset..high - offset]);
                }
                if size < start + BLOCK {
                    buffer[size - start..].fill(0);
                }
                for (sector, part) in buffer.chunks_exact(SECTOR).enumerate() {
                    self.disk
                        .write(block as u32 * 8 + sector as u32, part.try_into().unwrap())?;
                }
            }
            self.commit(next)
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result.map(|_| bytes.len())
    }
    pub fn truncate(&mut self, id: usize) -> Result<()> {
        let node = self.stat(id)?;
        if node.kind != 2 {
            return Err(Error::IsDir);
        }
        let mut next = self.metadata.clone();
        for &block in node.blocks.iter().filter(|&&block| block != 0) {
            next.mark(block as usize, false);
        }
        next.nodes[id].blocks.fill(0);
        next.nodes[id].size = 0;
        self.commit(next)
    }
    pub fn remove(&mut self, path: &[u8]) -> Result<()> {
        let id = self.lookup(path)?;
        if id == 0 {
            return Err(Error::Denied);
        }
        if self
            .metadata
            .nodes
            .iter()
            .any(|node| node.kind != 0 && node.parent as usize == id)
        {
            return Err(Error::NotEmpty);
        }
        let mut next = self.metadata.clone();
        for &block in next.nodes[id].blocks.iter().filter(|&&block| block != 0) {
            let block = block as usize;
            next.bitmap[block / 8] &= !(1 << (block % 8));
        }
        next.nodes[id] = EMPTY;
        self.commit(next)
    }
    pub fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<()> {
        let id = self.lookup(from)?;
        if id == 0 {
            return Err(Error::Denied);
        }
        if from == to {
            return Ok(());
        }
        match self.lookup(to) {
            Ok(_) => return Err(Error::Exists),
            Err(Error::NotFound) => (),
            Err(error) => return Err(error),
        }
        let (parent, name) = self.parent(to)?;
        let mut ancestor = parent;
        loop {
            if ancestor == id {
                return Err(Error::Invalid);
            }
            if ancestor == 0 {
                break;
            }
            ancestor = self.metadata.nodes[ancestor].parent as usize;
        }
        let mut next = self.metadata.clone();
        next.nodes[id].parent = parent as u16;
        next.nodes[id].length = name.len() as u8;
        next.nodes[id].name.fill(0);
        next.nodes[id].name[..name.len()].copy_from_slice(name);
        self.commit(next)
    }
    pub fn readdir(&self, id: usize, ordinal: usize) -> Result<Option<(usize, Node)>> {
        if self.stat(id)?.kind != 1 {
            return Err(Error::NotDir);
        }
        Ok(self
            .metadata
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| *index != 0 && node.kind != 0 && node.parent as usize == id)
            .nth(ordinal)
            .map(|(index, node)| (index, *node)))
    }
    pub fn sync(&mut self) -> Result<()> {
        self.ready()?;
        let result = self.disk.flush();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}

#[cfg(test)]
mod tests;
