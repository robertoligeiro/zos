use super::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keyboard,
    Mouse,
    Storage,
}

#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    pub address: u8,
    pub packet: u16,
    pub interval: u8,
    pub burst: u8,
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
    pub kind: Kind,
    pub input: Endpoint,
    pub output: Option<Endpoint>,
}

pub fn parse(bytes: &[u8]) -> Result<Configuration> {
    if bytes.len() < 9
        || bytes[0..2] != [9, 2]
        || bytes[4] != 1
        || u16::from_le_bytes([bytes[2], bytes[3]]) as usize != bytes.len()
        || bytes[5] == 0
    {
        return Err(Error::Invalid);
    }
    let mut kind = None;
    let mut interface = 0;
    let mut input = None;
    let mut output = None;
    let mut expected = 0;
    let mut count = 0;
    let mut last_endpoint = 0;
    let mut offset = 9;
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
                if length != 9 || kind.is_some() || d[3] != 0 {
                    return Err(Error::Unsupported);
                }
                kind = Some(match (d[5], d[6], d[7]) {
                    (3, 1, 1) => Kind::Keyboard,
                    (3, 1, 2) => Kind::Mouse,
                    (8, 6, 0x50) => Kind::Storage,
                    _ => return Err(Error::Unsupported),
                });
                interface = d[2];
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
                endpoint.ok_or(Error::Invalid)?.burst = d[2];
                last_endpoint = 0;
            }
            _ => (),
        }
        offset += length;
    }
    let kind = kind.ok_or(Error::Invalid)?;
    if count != expected
        || count != if kind == Kind::Storage { 2 } else { 1 }
        || (kind != Kind::Storage && output.is_some())
    {
        return Err(Error::Unsupported);
    }
    Ok(Configuration {
        value: bytes[5],
        interface,
        kind,
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
        for (index, value) in [
            (9, 0),
            (9, 255),
            (12, 1),
            (15, 9),
            (20, 0x80),
            (21, 2),
            (22, 0),
        ] {
            let mut broken = valid;
            broken[index] = value;
            assert!(parse(&broken).is_err(), "offset {index}");
        }
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
