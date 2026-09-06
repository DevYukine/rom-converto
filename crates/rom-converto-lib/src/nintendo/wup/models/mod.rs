//! On-disk structs for the WUP module.
//!
//! Fixed-offset parsers over the published Wii U ticket and TMD
//! layouts. These don't use binrw because the format has awkward
//! padding and variable trailing arrays; a hand-rolled parser at the
//! offsets Cemu's `ncrypto.cpp` uses is simpler. The ZArchive
//! container structures live in [`crate::zar::format`].

pub mod ticket;
pub mod tmd;

pub use ticket::WupTicket;
pub use tmd::{TmdContentEntry, TmdContentFlags, WupTmd};
