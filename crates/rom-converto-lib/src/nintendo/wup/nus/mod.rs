//! NUS (Nintendo Update Server) decryption pipeline.
//!
//! This submodule turns a raw NUS-layout directory (`title.tmd` +
//! `title.tik` + `*.app` files) into the decrypted virtual files
//! Cemu expects inside a `.wua`. Each submodule owns one step:
//!
//! - [`ticket_parser`]: parse `title.tik`, decrypt the title key.
//! - [`tmd_parser`]: parse `title.tmd`, expose the content list.
//! - [`fst_parser`]: parse the FST section out of decrypted
//!   content 0 into a flat list of virtual files.
//! - [`content_stream`]: decrypt raw-mode and hashed-mode `.app`
//!   files and extract virtual file byte ranges.
//! - [`compress`]: orchestrate the whole NUS pipeline and stream
//!   decrypted files into a ZArchive writer.

pub mod compress;
pub mod content_stream;
pub mod decrypt;
pub mod fst_parser;
pub mod layout;
pub mod ticket_parser;
pub mod tmd_parser;

pub use compress::compress_nus_title;
pub use content_stream::{
    ContentLoader, decrypt_content_0, decrypt_hashed_content, decrypt_raw_content,
};
pub use fst_parser::{FstCluster, FstClusterHashMode, VirtualFile, VirtualFs, parse_fst};
pub use ticket_parser::{TitleKey, parse_ticket_bytes, read_ticket_file};
pub use tmd_parser::{parse_tmd_bytes, read_tmd_file};
