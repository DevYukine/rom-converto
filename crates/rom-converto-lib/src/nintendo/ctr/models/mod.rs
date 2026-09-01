/// Certificate and public key structs from the signature chain.
pub mod certificate;
/// CIA container header and top-level file structs.
pub mod cia;
/// ExeFS entry header layout.
pub mod exe_fs_header;
/// NCCH partition header layout.
pub mod ncch_header;
/// NCSD (cartridge image) header and partition table layout.
pub mod ncsd_header;
/// Seed database (`seeddb.bin`) entry layout.
pub mod seeddb;
/// Signature type and generic signature data structs.
pub mod signature;
/// SMDH (icon/title metadata) parser.
pub mod smdh;
/// Ticket structs, carrying the encrypted title key.
pub mod ticket;
/// Title metadata (TMD) structs, listing a title's contents and their hashes.
pub mod title_metadata;
