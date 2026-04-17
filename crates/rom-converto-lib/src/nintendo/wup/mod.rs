//! Wii U (WUP) support: Cemu `.wua` archive creation.
//!
//! `.wua` is the file extension Cemu uses for its Wii U archives.
//! The container is a ZArchive with a directory naming convention on
//! top: each title lives under `<16-hex titleId>_v<decimal version>/`
//! and contains a `meta/code/content` tree of decrypted files. One
//! archive can bundle a base title plus its update and DLC.
//!
//! Format details live inline in [`constants`] and [`models`]. The
//! codename `wup` matches the other Nintendo module folders (`ctr`,
//! `rvl`, `rvz`, `dol`).

pub mod app_xml;
pub mod common_keys;
pub mod compress;
pub mod compress_parallel;
pub mod constants;
pub mod crypto;
pub mod disc;
pub mod error;
pub mod loadiine;
pub mod models;
pub mod name_table;
pub mod nus;
pub mod path_tree;
pub mod streaming_sink;
pub mod ticket_synth;
pub mod title_key_derive;
pub mod zarchive_writer;

pub use compress::{
    TitleInput, TitleInputFormat, WupCompressOptions, compress_title, compress_title_async,
    compress_titles, compress_titles_async, derive_wua_path, detect_title_format,
};
pub use error::{WupError, WupResult};
pub use loadiine::{LoadiineTitle, detect_loadiine_title, walk_loadiine_files};
pub use nus::decrypt::{decrypt_nus_title, decrypt_nus_title_async};
pub use zarchive_writer::ZArchiveWriter;
