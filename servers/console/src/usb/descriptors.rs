use super::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keyboard,
    Mouse,
    Storage,
    Hub,
}

#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    pub address: u8,
    pub packet: u16,
    pub interval: u8,
    pub burst: u8,
    pub companion: bool,
    pub bytes_per_interval: u16,
}
impl Endpoint {
    pub fn index(self) -> usize {
        (self.address as usize & 15) * 2 + ((self.address >> 7) as usize)
    }
}

#[derive(Debug)]
pub struct Configuration {
    pub value: u8,
    pub interface: u8,
    pub alternate: u8,
    pub kind: Kind,
    pub hub_multi_tt: bool,
    pub input: Endpoint,
    pub output: Option<Endpoint>,
}

pub fn parse(bytes: &[u8]) -> Result<Configuration> {
    if bytes.len() < 9
        || bytes[0..2] != [9, 2]
        || bytes[4] == 0
        || u16::from_le_bytes([bytes[2], bytes[3]]) as usize != bytes.len()
        || bytes[5] == 0
    {
        return Err(Error::Invalid);
    }
    let mut seen = [false; 256];
    let mut interfaces = 0;
    let mut selected = None;
    let mut start = 9;
    while start < bytes.len() {
        let length = *bytes.get(start).ok_or(Error::Invalid)? as usize;
        if length < 2 || start + length > bytes.len() {
            return Err(Error::Invalid);
        }
        if bytes[start + 1] == 11 && length == 8 {
            start += length;
            continue;
        }
        if bytes[start + 1] != 4 || length != 9 {
            return Err(Error::Invalid);
        }
        let number = bytes[start + 2] as usize;
        if !seen[number] {
            seen[number] = true;
            interfaces += 1;
        }
        let mut end = start + length;
        while end < bytes.len() {
            let size = bytes[end] as usize;
            if size < 2 || end + size > bytes.len() {
                return Err(Error::Invalid);
            }
            if bytes[end + 1] == 4 {
                break;
            }
            end += size;
        }
        if selected.is_none() {
            match parse_interface(&bytes[start..end], bytes[5]) {
                Ok(config) => selected = Some(config),
                Err(Error::Unsupported) => (),
                Err(error) => return Err(error),
            }
        }
        start = end;
    }
    if interfaces != bytes[4] as usize {
        return Err(Error::Invalid);
    }
    selected.ok_or(Error::Unsupported)
}

fn parse_interface(bytes: &[u8], value: u8) -> Result<Configuration> {
    let mut kind = None;
    let mut interface = 0;
    let mut alternate = 0;
    let mut hub_multi_tt = false;
    let mut input = None;
    let mut output = None;
    let mut expected = 0;
    let mut count = 0;
    let mut last_endpoint = 0;
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes.len() - offset < 2 {
            return Err(Error::Invalid);
        }
        let length = bytes[offset] as usize;
        if length < 2 || length > bytes.len() - offset {
            return Err(Error::Invalid);
        }
        let d = &bytes[offset..offset + length];
        match d[1] {
            4 => {
                if length != 9 || kind.is_some() {
                    return Err(Error::Unsupported);
                }
                kind = Some(match (d[5], d[6], d[7]) {
                    (3, 1, 1) => Kind::Keyboard,
                    (3, 1, 2) => Kind::Mouse,
                    (8, 6, 0x50) => Kind::Storage,
                    (9, 0, 0..=3) => Kind::Hub,
                    _ => return Err(Error::Unsupported),
                });
                interface = d[2];
                alternate = d[3];
                hub_multi_tt = d[7] == 2;
                expected = d[4];
            }
            5 => {
                let kind = kind.ok_or(Error::Invalid)?;
                if length != 7
                    || d[2] & 0x70 != 0
                    || d[2] & 15 == 0
                    || d[3] != if kind == Kind::Storage { 2 } else { 3 }
                {
                    return Err(Error::Invalid);
                }
                let packet = u16::from_le_bytes([d[4], d[5]]);
                if packet == 0 || packet > 1024 || (kind != Kind::Storage && d[6] == 0) {
                    return Err(Error::Unsupported);
                }
                let endpoint = Endpoint {
                    address: d[2],
                    packet,
                    interval: d[6],
                    burst: 0,
                    companion: false,
                    bytes_per_interval: 0,
                };
                let slot = if d[2] & 0x80 != 0 {
                    &mut input
                } else {
                    &mut output
                };
                if slot.replace(endpoint).is_some() {
                    return Err(Error::Unsupported);
                }
                last_endpoint = d[2];
                count += 1;
            }
            48 => {
                if length != 6 || last_endpoint == 0 || d[2] > 15 || d[3] != 0 {
                    return Err(Error::Unsupported);
                }
                let endpoint = if last_endpoint & 0x80 != 0 {
                    input.as_mut()
                } else {
                    output.as_mut()
                };
                let endpoint = endpoint.ok_or(Error::Invalid)?;
                endpoint.burst = d[2];
                endpoint.companion = true;
                endpoint.bytes_per_interval = u16::from_le_bytes([d[4], d[5]]);
                last_endpoint = 0;
            }
            _ => (),
        }
        offset += length;
    }
    let kind = kind.ok_or(Error::Invalid)?;
    if count != expected
        || (kind == Kind::Storage && (count != 2 || output.is_none()))
        || (kind != Kind::Storage && !(1..=2).contains(&count))
    {
        return Err(Error::Unsupported);
    }
    Ok(Configuration {
        value,
        interface,
        alternate,
        kind,
        hub_multi_tt,
        input: input.ok_or(Error::Invalid)?,
        output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keyboard() -> [u8; 25] {
        [
            9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 3, 1, 1, 0, 7, 5, 0x81, 3, 8, 0, 10,
        ]
    }
    #[test]
    fn bounded_descriptors_and_boot_interface() {
        let valid = keyboard();
        assert_eq!(parse(&valid).unwrap().kind, Kind::Keyboard);
        for end in 0..valid.len() {
            assert!(parse(&valid[..end]).is_err());
        }
        for (index, value) in [(9, 0), (9, 255), (15, 9), (20, 0x80), (21, 2), (22, 0)] {
            let mut broken = valid;
            broken[index] = value;
            assert!(parse(&broken).is_err(), "offset {index}");
        }
        let mut composite = valid;
        composite[4] = 2;
        assert_eq!(parse(&composite).unwrap_err(), Error::Invalid);
        let mut composite = valid[..9].to_vec();
        composite[4] = 2;
        composite.extend_from_slice(&[9, 4, 0, 0, 0, 0xff, 0, 0, 0]);
        composite.extend_from_slice(&valid[9..]);
        composite[20] = 1;
        let size = composite.len() as u16;
        composite[2..4].copy_from_slice(&size.to_le_bytes());
        let config = parse(&composite).unwrap();
        assert_eq!(config.interface, 1);
        assert_eq!(config.kind, Kind::Keyboard);
    }
    #[test]
    fn alternate_bulk_interface_and_hub_class() {
        let mut data = [
            9, 2, 41, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 8, 6, 0x62, 0, 9, 4, 0, 1, 2, 8, 6,
            0x50, 0, 7, 5, 0x81, 2, 64, 0, 0, 7, 5, 2, 2, 64, 0, 0,
        ];
        let config = parse(&data).unwrap();
        assert_eq!(config.alternate, 1);
        assert_eq!(config.kind, Kind::Storage);
        data[19] = 0;
        assert!(parse(&data).is_err());
        let mut hub = keyboard();
        hub[14..17].copy_from_slice(&[9, 0, 0]);
        assert_eq!(parse(&hub).unwrap().kind, Kind::Hub);
    }
    #[test]
    fn bulk_pair_and_super_speed_companion() {
        let data = [
            9, 2, 44, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 2, 8, 6, 0x50, 0, 7, 5, 0x81, 2, 0, 4, 0,
            6, 48, 15, 0, 0, 0, 7, 5, 2, 2, 0, 4, 0, 6, 48, 15, 0, 0, 0,
        ];
        let config = parse(&data).unwrap();
        assert_eq!(config.kind, Kind::Storage);
        assert_eq!(config.input.index(), 3);
        assert_eq!(config.output.unwrap().index(), 4);
        assert_eq!(config.input.burst, 15);
    }
}
