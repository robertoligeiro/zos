mod descriptors;
mod gpt;
mod hid;
mod policy;
mod xhci;

use crate::keyboard::Key;
pub use xhci::Controller;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unsupported,
    DmaAddress(u64),
    ReadOnly,
    Timeout,
    Completion(u8),
    Io,
    Scsi(u8),
}
pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy)]
pub enum Event {
    Key(Key),
    Pointer { dx: i16, dy: i16, buttons: u8 },
}
