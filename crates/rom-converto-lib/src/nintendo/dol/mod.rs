//! GameCube (codename DOL) console support.
//!
//! Currently exposes disc-format helpers used by the shared RVZ pipeline in
//! [`crate::nintendo::rvz`]. GameCube discs have no encryption and no
//! partition table, so this module is intentionally small.

pub mod constants;
pub mod disc;

#[cfg(test)]
pub mod test_fixtures;

pub use disc::is_gamecube;
