//! Reads, converts, compresses, decompresses, encrypts, decrypts, and
//! verifies ROMs and disc images for the Nintendo 3DS, GameCube, Wii,
//! Wii U, and Switch, plus CD and DVD disc images and PSP/PS2 ISOs.
//!
//! Each platform lives under [`crate::nintendo`] ([`crate::nintendo::ctr`],
//! [`crate::nintendo::dol`], [`crate::nintendo::rvl`],
//! [`crate::nintendo::wup`], [`crate::nintendo::nx`]); CD and DVD disc
//! images go through [`crate::chd`] and [`crate::cue`], and PSP/PS2 ISOs
//! through [`crate::cso`]. [`crate::config`] loads the config file and
//! presets, [`crate::info`] renders per-format metadata, [`crate::playlist`]
//! writes multi-disc `.m3u` files, and [`crate::util`] holds the shared
//! conflict resolution, hashing, dry-run planning, and reporting machinery
//! every format uses.

pub mod cd;
pub mod chd;
pub mod config;
pub mod cso;
pub mod cue;
pub mod info;
pub mod nintendo;
pub mod playlist;
pub mod util;
