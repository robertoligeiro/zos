use crate::{Error, Result};

pub fn rpc(channel: usize, request: &[u8], response: &mut [u8]) -> Result<usize> {
    let length = zos_libos::send(channel, request, response).map_err(|_| Error::Io)?;
    if length == 0 {
        return Err(Error::Corrupt);
    }
    if response[0] != 0 {
        return Err(Error::decode(response[0])?);
    }
    Ok(length - 1)
}

pub struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    pub fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self.position.checked_add(length).ok_or(Error::Invalid)?;
        let bytes = self.bytes.get(self.position..end).ok_or(Error::Invalid)?;
        self.position = end;
        Ok(bytes)
    }
}

pub struct Request {
    bytes: [u8; zos_libos::MAX_STORAGE_MESSAGE],
    length: usize,
}
impl Request {
    pub fn new(op: u8) -> Self {
        let mut request = Self {
            bytes: [0; zos_libos::MAX_STORAGE_MESSAGE],
            length: 0,
        };
        request.byte(op).unwrap();
        request
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self
            .length
            .checked_add(bytes.len())
            .filter(|&end| end <= self.bytes.len())
            .ok_or(Error::TooLarge)?;
        self.bytes[self.length..end].copy_from_slice(bytes);
        self.length = end;
        Ok(())
    }
    pub fn byte(&mut self, value: u8) -> Result<()> {
        self.push(&[value])
    }
    pub fn u16(&mut self, value: u16) -> Result<()> {
        self.push(&value.to_le_bytes())
    }
    pub fn u32(&mut self, value: u32) -> Result<()> {
        self.push(&value.to_le_bytes())
    }
    pub fn u64(&mut self, value: u64) -> Result<()> {
        self.push(&value.to_le_bytes())
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

use crate::vfs::{Backend, Entry};
use crate::{FILESYSTEM, PAYLOAD, VFS};
pub struct FsClient;
impl FsClient {
    fn entry(request: &Request) -> Result<Entry> {
        let mut reply = [0; 33];
        let length = rpc(FILESYSTEM, request.bytes(), &mut reply)?;
        Entry::decode(&reply[1..length + 1])
    }
}
impl Backend for FsClient {
    fn lookup(&mut self, path: &[u8]) -> Result<Entry> {
        let mut request = Request::new(0);
        request.push(path)?;
        Self::entry(&request)
    }
    fn create(&mut self, path: &[u8], kind: u8) -> Result<Entry> {
        let mut request = Request::new(1);
        request.byte(kind)?;
        request.push(path)?;
        Self::entry(&request)
    }
    fn stat(&mut self, inode: usize) -> Result<Entry> {
        let mut request = Request::new(9);
        request.u16(inode as u16)?;
        Self::entry(&request)
    }
    fn read(&mut self, inode: usize, offset: usize, bytes: &mut [u8]) -> Result<usize> {
        if bytes.len() > PAYLOAD {
            return Err(Error::TooLarge);
        }
        let mut request = Request::new(2);
        request.u16(inode as u16)?;
        request.u32(offset.try_into().map_err(|_| Error::TooLarge)?)?;
        request.u16(bytes.len() as u16)?;
        let mut reply = [0; PAYLOAD + 1];
        let count = rpc(FILESYSTEM, request.bytes(), &mut reply)?;
        if count > bytes.len() {
            return Err(Error::Corrupt);
        }
        bytes[..count].copy_from_slice(&reply[1..count + 1]);
        Ok(count)
    }
    fn write(&mut self, inode: usize, offset: usize, bytes: &[u8]) -> Result<usize> {
        if bytes.len() > PAYLOAD {
            return Err(Error::TooLarge);
        }
        let mut request = Request::new(3);
        request.u16(inode as u16)?;
        request.u32(offset.try_into().map_err(|_| Error::TooLarge)?)?;
        request.push(bytes)?;
        let mut reply = [0; 5];
        if rpc(FILESYSTEM, request.bytes(), &mut reply)? != 4 {
            return Err(Error::Corrupt);
        }
        Ok(u32::from_le_bytes(reply[1..].try_into().unwrap()) as usize)
    }
    fn truncate(&mut self, inode: usize) -> Result<()> {
        let mut request = Request::new(7);
        request.u16(inode as u16)?;
        rpc(FILESYSTEM, request.bytes(), &mut [0]).map(|_| ())
    }
    fn remove(&mut self, path: &[u8]) -> Result<()> {
        let mut request = Request::new(4);
        request.push(path)?;
        rpc(FILESYSTEM, request.bytes(), &mut [0]).map(|_| ())
    }
    fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<()> {
        let mut request = Request::new(5);
        request.u16(from.len() as u16)?;
        request.push(from)?;
        request.push(to)?;
        rpc(FILESYSTEM, request.bytes(), &mut [0]).map(|_| ())
    }
    fn readdir(&mut self, inode: usize, ordinal: usize) -> Result<Option<Entry>> {
        let mut request = Request::new(6);
        request.u16(inode as u16)?;
        request.u16(ordinal.try_into().map_err(|_| Error::TooLarge)?)?;
        let mut reply = [0; 33];
        let count = rpc(FILESYSTEM, request.bytes(), &mut reply)?;
        if count == 0 {
            Ok(None)
        } else {
            Entry::decode(&reply[1..count + 1]).map(Some)
        }
    }
    fn sync(&mut self) -> Result<()> {
        rpc(FILESYSTEM, &[8], &mut [0]).map(|_| ())
    }
}

pub fn open(path: &[u8], flags: u8) -> Result<u64> {
    let mut request = Request::new(0);
    request.byte(flags)?;
    request.push(path)?;
    let mut reply = [0; 9];
    if rpc(VFS, request.bytes(), &mut reply)? != 8 {
        return Err(Error::Corrupt);
    }
    Ok(u64::from_le_bytes(reply[1..].try_into().unwrap()))
}
pub fn read(fd: u64, bytes: &mut [u8]) -> Result<usize> {
    if bytes.len() > PAYLOAD {
        return Err(Error::TooLarge);
    }
    let mut request = Request::new(1);
    request.u64(fd)?;
    request.u16(bytes.len() as u16)?;
    let mut reply = [0; PAYLOAD + 1];
    let count = rpc(VFS, request.bytes(), &mut reply)?;
    if count > bytes.len() {
        return Err(Error::Corrupt);
    }
    bytes[..count].copy_from_slice(&reply[1..count + 1]);
    Ok(count)
}
pub fn write(fd: u64, bytes: &[u8]) -> Result<usize> {
    if bytes.len() > PAYLOAD {
        return Err(Error::TooLarge);
    }
    let mut request = Request::new(2);
    request.u64(fd)?;
    request.push(bytes)?;
    let mut reply = [0; 5];
    if rpc(VFS, request.bytes(), &mut reply)? != 4 {
        return Err(Error::Corrupt);
    }
    Ok(u32::from_le_bytes(reply[1..].try_into().unwrap()) as usize)
}
pub fn close(fd: u64) -> Result<()> {
    let mut request = Request::new(3);
    request.u64(fd)?;
    rpc(VFS, request.bytes(), &mut [0]).map(|_| ())
}
/// A new pipe: (read end, write end), both owned by the caller.
pub fn pipe() -> Result<(u64, u64)> {
    let mut reply = [0; 17];
    if rpc(VFS, &[11], &mut reply)? != 16 {
        return Err(Error::Corrupt);
    }
    Ok((
        u64::from_le_bytes(reply[1..9].try_into().unwrap()),
        u64::from_le_bytes(reply[9..17].try_into().unwrap()),
    ))
}
/// Another descriptor for the same open file or pipe end.
pub fn dup(fd: u64) -> Result<u64> {
    let mut request = Request::new(12);
    request.u64(fd)?;
    let mut reply = [0; 9];
    if rpc(VFS, request.bytes(), &mut reply)? != 8 {
        return Err(Error::Corrupt);
    }
    Ok(u64::from_le_bytes(reply[1..].try_into().unwrap()))
}
/// Hands `fd` to the process in slot `target` (a child that has not started running
/// its program yet); the caller no longer owns it.
pub fn grant(fd: u64, target: u8) -> Result<()> {
    let mut request = Request::new(13);
    request.u64(fd)?;
    request.byte(target)?;
    rpc(VFS, request.bytes(), &mut [0]).map(|_| ())
}
pub fn stat(path: &[u8]) -> Result<Entry> {
    let mut request = Request::new(4);
    request.push(path)?;
    let mut reply = [0; 33];
    let count = rpc(VFS, request.bytes(), &mut reply)?;
    Entry::decode(&reply[1..count + 1])
}
pub fn readdir(path: &[u8], ordinal: u16) -> Result<Option<Entry>> {
    let mut request = Request::new(5);
    request.u16(ordinal)?;
    request.push(path)?;
    let mut reply = [0; 33];
    let count = rpc(VFS, request.bytes(), &mut reply)?;
    if count == 0 {
        Ok(None)
    } else {
        Entry::decode(&reply[1..count + 1]).map(Some)
    }
}
pub fn mkdir(path: &[u8]) -> Result<()> {
    let mut request = Request::new(6);
    request.push(path)?;
    rpc(VFS, request.bytes(), &mut [0]).map(|_| ())
}
pub fn unlink(path: &[u8]) -> Result<()> {
    let mut request = Request::new(7);
    request.push(path)?;
    rpc(VFS, request.bytes(), &mut [0]).map(|_| ())
}
pub fn rename(from: &[u8], to: &[u8]) -> Result<()> {
    let mut request = Request::new(8);
    request.u16(from.len() as u16)?;
    request.push(from)?;
    request.push(to)?;
    rpc(VFS, request.bytes(), &mut [0]).map(|_| ())
}
pub fn sync() -> Result<()> {
    rpc(VFS, &[9], &mut [0]).map(|_| ())
}
impl<'a> Reader<'a> {
    pub fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let bytes = &self.bytes[self.position..];
        self.position = self.bytes.len();
        bytes
    }
    pub fn end(&self) -> Result<()> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::Invalid)
        }
    }
}
