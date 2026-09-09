//! Laserdisc AVI input and VBI parsing.
//!
//! [`avi`] reads the uncompressed YUY2/UYVY AVI a laserdisc rip ships as and
//! derives the field geometry that fixes a laserdisc CHD's hunk size;
//! [`vbi`] recovers each field's white flag and Philips codes for the
//! per-field `AVLD` metadata blob; [`info`] combines both for the `info`
//! command.

pub mod avi;
pub mod info;
pub mod vbi;

pub use vbi::{LdClvTime, LdDiscType};
