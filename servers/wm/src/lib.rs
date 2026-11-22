//! ZOS window manager: pure, host-testable logic. The ring-3 server in `wm.rs` wires
//! it to the framebuffer, the pointer and the console protocol.
#![no_std]

#[cfg(test)]
extern crate std;

pub mod apps;
pub mod draw;
pub mod geometry;
pub mod grid;
pub mod windows;
