//! PS4 `.pkg` packages: the `\x7FCNT` container shared with the PS5, and
//! the PS4 metadata reader built on it.

pub mod cnt;
pub mod pkg;

pub use cnt::{CntEntry, CntPlatform};
pub use pkg::Ps4PkgInfo;
