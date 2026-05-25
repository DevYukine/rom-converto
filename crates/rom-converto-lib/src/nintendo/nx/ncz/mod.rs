//! NCZ codec: per-NCA decompose into a 0x4000 encrypted prefix plus a
//! zstd-compressed plaintext payload, with stored per-section AES keys
//! so the decompressor can re-encrypt without `prod.keys`. We still
//! demand a keyfile higher up; the cached keys make the zstd payload
//! itself replayable on hosts without keys.

pub mod compress;
pub mod compress_worker;
pub mod decompress;
pub mod decompress_worker;
pub mod header;
pub mod reader;
pub mod reencrypt;

pub use compress::{NcaToNczOptions, NczMode, nca_to_ncz};
pub use decompress::ncz_to_nca;
pub use reader::NczReader;
