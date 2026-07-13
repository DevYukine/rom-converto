//! Nintendo Switch (NX) support: NSP/XCI to NSZ/XCZ compression.
//!
//! Switch is a current-generation console, so no keys are baked into
//! this crate. Every operation (compress, decompress, verify) requires
//! a `prod.keys` file. Resolution order matches `nsz`: explicit
//! `--keys` path, `$HOME/.switch/prod.keys` (Linux/macOS),
//! `%USERPROFILE%/.switch/prod.keys` (Windows), then the binary's own
//! directory.

pub mod compress;
pub mod constants;
pub mod container;
pub mod crypto;
pub mod decompress;
pub mod derive_paths;
pub mod error;
pub mod info;
pub mod keys;
pub mod models;
pub mod ncz;
pub mod romfs;
pub mod util;
pub mod verify;
pub mod walker;

#[cfg(test)]
pub mod test_fixtures;

pub use compress::{
    NxCompressOptions, compress_container, compress_container_async,
    compress_container_async_cancellable,
};
pub use container::{ContainerKind, detect_container};
pub use decompress::{
    decompress_container, decompress_container_async, decompress_container_async_cancellable,
};
pub use derive_paths::{derive_compressed_path, derive_decompressed_path};
pub use error::{NxError, NxResult};
pub use keys::{KeyAreaKind, KeySet, load_keyset};
pub use models::{Hfs0, NcaHeader, Pfs0};
pub use ncz::NczMode;
pub use verify::{
    NcaVerdict, NxVerifyResult, verify_container, verify_container_async,
    verify_container_async_cancellable, verify_container_cancellable,
};
pub use walker::{NcaSection, NcaWalker};
