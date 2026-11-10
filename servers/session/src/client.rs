use crate::PROCMGR;
use zos_storage::{
    protocol::{rpc, Reader, Request},
    Error, Result,
};

#[derive(Clone, Copy)]
pub struct Startup {
    pub bytes: [u8; 768],
    pub length: usize,
}
impl Default for Startup {
    fn default() -> Self {
        Self {
            bytes: [0; 768],
            length: 0,
        }
    }
}
pub struct Invocation<'a> {
    pub cwd: &'a [u8],
    pub output: &'a [u8],
    pub append: bool,
    pub argv: [&'a [u8]; 12],
    pub argc: usize,
}
impl Startup {
    pub fn new(cwd: &[u8], output: Option<(&[u8], bool)>, argv: &[&[u8]]) -> Result<Self> {
        if argv.is_empty() || argv.len() > 12 {
            return Err(Error::Invalid);
        }
        let mut request = Request::new(output.is_some_and(|(_, a)| a) as u8);
        request.u16(cwd.len().try_into().map_err(|_| Error::TooLarge)?)?;
        request.u16(
            output
                .map_or(0, |(p, _)| p.len())
                .try_into()
                .map_err(|_| Error::TooLarge)?,
        )?;
        request.byte(argv.len() as u8)?;
        request.push(cwd)?;
        if let Some((path, _)) = output {
            request.push(path)?;
        }
        for arg in argv {
            request.u16(arg.len().try_into().map_err(|_| Error::TooLarge)?)?;
            request.push(arg)?;
        }
        if request.bytes().len() > 768 {
            return Err(Error::TooLarge);
        }
        let mut startup = Self {
            length: request.bytes().len(),
            ..Self::default()
        };
        startup.bytes[..startup.length].copy_from_slice(request.bytes());
        startup.decode()?;
        Ok(startup)
    }
    pub fn decode(&self) -> Result<Invocation<'_>> {
        if self.length > self.bytes.len() {
            return Err(Error::TooLarge);
        }
        let mut reader = Reader::new(&self.bytes[..self.length]);
        let append = match reader.byte()? {
            0 => false,
            1 => true,
            _ => return Err(Error::Invalid),
        };
        let cwd_len = reader.u16()? as usize;
        let out_len = reader.u16()? as usize;
        let argc = reader.byte()? as usize;
        if argc == 0 || argc > 12 {
            return Err(Error::Invalid);
        }
        let cwd = reader.take(cwd_len)?;
        let output = reader.take(out_len)?;
        zos_storage::vfs::normalize(cwd)?;
        if !output.is_empty() {
            zos_storage::vfs::normalize(output)?;
        }
        let mut argv = [b"".as_slice(); 12];
        for arg in &mut argv[..argc] {
            let length = reader.u16()? as usize;
            *arg = reader.take(length)?;
            if arg.iter().any(|b| !(32..=126).contains(b)) {
                return Err(Error::Invalid);
            }
        }
        reader.end()?;
        Ok(Invocation {
            cwd,
            output,
            append,
            argv,
            argc,
        })
    }
}
pub fn spawn(path: &[u8], startup: &Startup) -> Result<u64> {
    startup.decode()?;
    let mut request = Request::new(0);
    request.u16(path.len().try_into().map_err(|_| Error::TooLarge)?)?;
    request.push(path)?;
    request.push(&startup.bytes[..startup.length])?;
    let mut reply = [0; 9];
    if rpc(PROCMGR, request.bytes(), &mut reply)? != 8 {
        return Err(Error::Corrupt);
    }
    Ok(u64::from_le_bytes(reply[1..].try_into().unwrap()))
}
fn pid_call(op: u8, pid: u64, reply: &mut [u8]) -> Result<usize> {
    let mut request = Request::new(op);
    request.u64(pid)?;
    rpc(PROCMGR, request.bytes(), reply)
}
pub fn wait(pid: u64) -> Result<i64> {
    let mut reply = [0; 9];
    if pid_call(1, pid, &mut reply)? != 8 {
        return Err(Error::Corrupt);
    }
    Ok(i64::from_le_bytes(reply[1..].try_into().unwrap()))
}
pub fn kill(pid: u64) -> Result<()> {
    pid_call(2, pid, &mut [0]).map(|_| ())
}
pub fn started(pid: u64) -> Result<()> {
    pid_call(7, pid, &mut [0]).map(|_| ())
}
pub fn list(slot: u8) -> Result<Option<zos_libos::ProcessInfo>> {
    let mut reply = [0; 51];
    let length = rpc(PROCMGR, &[3, slot], &mut reply)?;
    if length == 0 {
        return Ok(None);
    }
    zos_libos::ProcessInfo::decode(&reply[1..length + 1])
        .map(Some)
        .map_err(crate::kernel)
}
pub fn memory() -> Result<[u64; 4]> {
    let mut reply = [0; 33];
    if rpc(PROCMGR, &[4], &mut reply)? != 32 {
        return Err(Error::Corrupt);
    }
    Ok(core::array::from_fn(|i| {
        u64::from_le_bytes(reply[1 + i * 8..9 + i * 8].try_into().unwrap())
    }))
}
pub fn startup() -> Result<Startup> {
    let mut reply = [0; 769];
    let length = rpc(PROCMGR, &[5], &mut reply)?;
    let mut startup = Startup::default();
    if length > startup.bytes.len() {
        return Err(Error::Corrupt);
    }
    startup.bytes[..length].copy_from_slice(&reply[1..length + 1]);
    startup.length = length;
    startup.decode()?;
    Ok(startup)
}
pub fn control(action: u8) -> Result<()> {
    rpc(PROCMGR, &[6, action], &mut [0]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invocation_round_trip_and_malformed_packets() {
        let startup = Startup::new(b"/a", Some((b"/a/f", true)), &[b"echo", b"a b", b""]).unwrap();
        let invocation = startup.decode().unwrap();
        assert_eq!(invocation.cwd, b"/a");
        assert_eq!(invocation.output, b"/a/f");
        assert!(invocation.append);
        assert_eq!(
            &invocation.argv[..invocation.argc],
            [b"echo".as_slice(), b"a b", b""]
        );
        for length in 0..startup.length {
            let mut truncated = startup;
            truncated.length = length;
            assert!(truncated.decode().is_err());
        }
        let mut trailing = startup;
        trailing.length += 1;
        assert!(trailing.decode().is_err());
        assert!(Startup::new(b"/../escape", None, &[b"true"]).is_err());
        let oversized = Startup {
            length: 769,
            ..Startup::default()
        };
        assert!(matches!(oversized.decode(), Err(Error::TooLarge)));
        assert_eq!(spawn(b"/bin/true", &oversized), Err(Error::TooLarge));
    }
}
