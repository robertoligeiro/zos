//! Browser addresses: what a typed address or a link target means, relative to the
//! page it was found on. Three kinds exist today: `about:` pages built into the
//! browser, `http://` and `https://` addresses (which need a network the system does
//! not have yet) and paths in the ZOS filesystem (also written `file:///path`).

pub const URL: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    About(&'a [u8]),
    Web(&'a [u8]),
    File(&'a [u8]),
}

/// A short address kept by value.
#[derive(Clone, Copy)]
pub struct Address {
    bytes: [u8; URL],
    length: usize,
}

impl Address {
    pub const EMPTY: Address = Address {
        bytes: [0; URL],
        length: 0,
    };
    pub fn new(text: &[u8]) -> Option<Self> {
        if text.len() > URL || text.iter().any(|b| !(32..=126).contains(b)) {
            return None;
        }
        let mut address = Self::EMPTY;
        address.bytes[..text.len()].copy_from_slice(text);
        address.length = text.len();
        Some(address)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
    pub fn push(&mut self, byte: u8) -> bool {
        if self.length == URL || !(32..=126).contains(&byte) {
            return false;
        }
        self.bytes[self.length] = byte;
        self.length += 1;
        true
    }
    pub fn pop(&mut self) {
        self.length = self.length.saturating_sub(1);
    }
    pub fn clear(&mut self) {
        self.length = 0;
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
    pub fn target(&self) -> Target<'_> {
        classify(self.as_bytes())
    }
}

fn starts_with_ignore_case(text: &[u8], prefix: &[u8]) -> bool {
    text.len() >= prefix.len()
        && text
            .iter()
            .zip(prefix)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
}

pub fn classify(address: &[u8]) -> Target<'_> {
    if starts_with_ignore_case(address, b"about:") {
        Target::About(&address[6..])
    } else if starts_with_ignore_case(address, b"http://")
        || starts_with_ignore_case(address, b"https://")
    {
        Target::Web(address)
    } else if starts_with_ignore_case(address, b"file://") {
        Target::File(&address[7..])
    } else {
        Target::File(address)
    }
}

/// The address a link `href` on the page at `base` leads to. `None` for something
/// too long, not printable, or a path that climbs out of the root.
pub fn resolve(base: &[u8], href: &[u8]) -> Option<Address> {
    // A fragment names a place on the page: the page itself.
    let href = match href.iter().position(|&b| b == b'#') {
        Some(at) => &href[..at],
        None => href,
    };
    let href = if let Some(rest) = href.strip_prefix(b"file://") {
        rest
    } else {
        href
    };
    if href.is_empty() {
        return Address::new(base);
    }
    if starts_with_ignore_case(href, b"about:")
        || starts_with_ignore_case(href, b"http://")
        || starts_with_ignore_case(href, b"https://")
    {
        return Address::new(href);
    }
    // A file path, absolute or relative to the base page's directory.
    let directory: &[u8] = match classify(base) {
        Target::File(path) => match path.iter().rposition(|&b| b == b'/') {
            Some(at) => &path[..at + 1],
            None => b"/",
        },
        _ => b"/",
    };
    let mut joined = [0u8; zos_storage::vfs::PATH];
    let mut length = 0;
    let mut add = |text: &[u8], joined: &mut [u8; zos_storage::vfs::PATH]| -> bool {
        if length + text.len() > joined.len() {
            return false;
        }
        joined[length..length + text.len()].copy_from_slice(text);
        length += text.len();
        true
    };
    if !href.starts_with(b"/") && !add(directory, &mut joined) {
        return None;
    }
    if !add(href, &mut joined) {
        return None;
    }
    let normal = zos_storage::vfs::normalize(&joined[..length]).ok()?;
    Address::new(normal.bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(base: &str, href: &str) -> Option<std::string::String> {
        resolve(base.as_bytes(), href.as_bytes())
            .map(|a| std::string::String::from_utf8(a.as_bytes().to_vec()).unwrap())
    }

    #[test]
    fn addresses_are_classified() {
        assert_eq!(classify(b"about:home"), Target::About(b"home"));
        assert_eq!(classify(b"ABOUT:Help"), Target::About(b"Help"));
        assert_eq!(
            classify(b"http://example.com/a"),
            Target::Web(b"http://example.com/a")
        );
        assert_eq!(classify(b"HTTPS://x"), Target::Web(b"HTTPS://x"));
        assert_eq!(
            classify(b"file:///www/a.html"),
            Target::File(b"/www/a.html")
        );
        assert_eq!(classify(b"/www/a.html"), Target::File(b"/www/a.html"));
        assert_eq!(classify(b"notes.txt"), Target::File(b"notes.txt"));
    }

    #[test]
    fn links_resolve_against_the_page_they_are_on() {
        assert_eq!(
            go("/www/index.html", "two.html").as_deref(),
            Some("/www/two.html")
        );
        assert_eq!(
            go("/www/index.html", "./two.html").as_deref(),
            Some("/www/two.html")
        );
        assert_eq!(
            go("/www/sub/a.html", "../b.html").as_deref(),
            Some("/www/b.html")
        );
        assert_eq!(go("/www/index.html", "/etc/x").as_deref(), Some("/etc/x"));
        assert_eq!(
            go("/www/index.html", "file:///a/b").as_deref(),
            Some("/a/b")
        );
        assert_eq!(go("/www/index.html", "sub/").as_deref(), Some("/www/sub"));
        assert_eq!(go("about:home", "x.html").as_deref(), Some("/x.html"));
        assert_eq!(go("/a.html", "about:help").as_deref(), Some("about:help"));
        assert_eq!(go("/a.html", "http://h/p").as_deref(), Some("http://h/p"));
    }

    #[test]
    fn fragments_stay_on_the_page_and_hostile_links_fail_cleanly() {
        assert_eq!(go("/www/i.html", "#top").as_deref(), Some("/www/i.html"));
        assert_eq!(
            go("/www/i.html", "two.html#part").as_deref(),
            Some("/www/two.html")
        );
        assert_eq!(go("/www/i.html", "").as_deref(), Some("/www/i.html"));
        assert_eq!(
            go("/a.html", "../../../../x"),
            None,
            "cannot climb above the root"
        );
        assert_eq!(go("/a.html", &"x".repeat(200)), None, "too long");
        assert_eq!(go("/a.html", "bad\u{1}name"), None, "control characters");
        assert_eq!(go("/a.html", &"/a".repeat(60)), None);
    }

    #[test]
    fn an_address_is_edited_within_its_bounds() {
        let mut a = Address::EMPTY;
        assert!(a.is_empty());
        for _ in 0..URL {
            assert!(a.push(b'a'));
        }
        assert!(!a.push(b'b'), "full");
        assert!(!{
            a.clear();
            a.push(7)
        });
        a.push(b'x');
        a.push(b'y');
        a.pop();
        assert_eq!(a.as_bytes(), b"x");
        a.pop();
        a.pop();
        assert!(a.is_empty());
        assert!(Address::new(&[b'a'; URL + 1]).is_none());
        assert!(Address::new(b"ok").is_some());
    }
}
