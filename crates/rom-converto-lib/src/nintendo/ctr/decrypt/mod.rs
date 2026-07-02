//! NCCH and NCSD decryption: parses CIA/CCI container headers, derives the
//! per-partition AES keys, and streams decrypted ExeFS and RomFS content.

pub mod cia;
pub(crate) mod model;
pub(crate) mod reader;
pub(crate) mod romfs_worker;
pub mod util;
