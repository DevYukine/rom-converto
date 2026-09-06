//! Reads, converts, compresses, decompresses, encrypts, decrypts, and
//! verifies ROMs and disc images for the Nintendo 3DS, GameCube, Wii,
//! Wii U, and Switch, the Xbox and Xbox 360, plus CD and DVD disc images
//! and PSP/PS2 ISOs.
//!
//! Each Nintendo platform lives under [`crate::nintendo`]
//! ([`crate::nintendo::ctr`], [`crate::nintendo::dol`],
//! [`crate::nintendo::rvl`], [`crate::nintendo::wup`],
//! [`crate::nintendo::nx`]); Microsoft platforms under [`crate::microsoft`]
//! ([`crate::microsoft::xbox`], [`crate::microsoft::xenon`]), which share
//! the [`crate::zar`] ZArchive container with the Wii U.
//!
//! The rest, one module each: [`crate::cd`] holds the CD sector and
//! subchannel primitives, [`crate::chd`] and [`crate::cue`] the CD and DVD
//! disc images, [`crate::cso`] the PSP/PS2 ISO compressors,
//! [`crate::laserdisc`] the LaserDisc AVI captures, [`crate::sony::ps3`] the
//! PS3 disc and package formats, [`crate::sony`] the PSP and Vita packages,
//! [`crate::sony::disc`] the PS1, PS2, and PSP disc metadata, and
//! [`crate::retro`] the cartridge systems. [`crate::pipeline`] chains
//! CSO/ZSO and CHD conversion through a temporary ISO, [`crate::dat`]
//! matches files against Redump and No-Intro DATs, [`crate::runner`]
//! drives batch runs for the CLI and GUI, [`crate::config`] loads the
//! config file and presets, [`crate::info`] renders per-format metadata,
//! [`crate::playlist`] writes multi-disc `.m3u` files, and [`crate::util`]
//! holds the shared conflict resolution, hashing, dry-run planning, and
//! reporting machinery every format uses.

pub mod cd;
pub mod chd;
pub mod config;
pub mod cso;
pub mod cue;
pub mod dat;
pub mod info;
pub mod laserdisc;
pub mod microsoft;
pub mod nintendo;
pub mod pipeline;
pub mod playlist;
pub mod retro;
pub mod runner;
pub mod sony;
pub mod util;
pub mod zar;
