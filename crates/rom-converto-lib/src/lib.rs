//! Reads, converts, compresses, decompresses, encrypts, decrypts, and
//! verifies ROMs and disc images for the Nintendo 3DS, GameCube, Wii,
//! Wii U, and Switch, the Xbox and Xbox 360, plus CD and DVD disc images
//! and PSP/PS2 ISOs.
//!
//! Each Nintendo platform lives under [`crate::nintendo`]
//! ([`crate::nintendo::ctr`], [`crate::nintendo::dol`],
//! [`crate::nintendo::rvl`], [`crate::nintendo::wup`],
//! [`crate::nintendo::nx`]); Microsoft platforms under [`crate::microsoft`]
//! ([`crate::microsoft::xbox`], [`crate::microsoft::xenon`]); CD and DVD
//! disc images go through [`crate::chd`] and [`crate::cue`], and PSP/PS2
//! ISOs through [`crate::cso`]. PS1, PS2, and PSP disc metadata comes
//! from [`crate::sony_disc`]. [`crate::pipeline`] chains CSO/ZSO and CHD
//! conversion through a temporary ISO for one-step conversion between the
//! two. [`crate::config`] loads the config file and presets, [`crate::info`]
//! renders per-format metadata, [`crate::playlist`] writes multi-disc `.m3u`
//! files, and [`crate::util`] holds the shared conflict resolution, hashing,
//! dry-run planning, and reporting machinery every format uses.

pub mod cd;
pub mod chd;
pub mod config;
pub mod cso;
pub mod cue;
pub mod dat;
pub mod info;
pub mod microsoft;
pub mod nintendo;
pub mod pipeline;
pub mod playlist;
pub mod ps3;
pub mod runner;
pub mod sony_disc;
pub mod util;
