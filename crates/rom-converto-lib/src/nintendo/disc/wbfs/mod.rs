//! WBFS (Wii Backup File System) container support.
//!
//! WBFS stores a Wii or GameCube disc as a sparse array of fixed-size
//! blocks, dropping the disc regions that hold no real data. This module
//! provides a streaming reader that reconstructs the logical disc on the
//! fly ([`WbfsReader`]) plus the write primitives in [`writer`] used by the
//! parallel WBFS writer in `rvz::decompress::sink`. Both keep a small,
//! fixed working set so multi-GB discs convert without buffering the whole
//! image.

pub mod error;
pub mod format;
pub mod reader;
pub mod usage;
pub mod writer;

pub use error::{WbfsError, WbfsResult};
pub use reader::WbfsReader;
pub use usage::{DiscUsage, build_disc_usage};
