//! The table a multi-disc PS1 EBOOT needs at PSTITLEIMG+0x28C.
//!
//! The PSP's PS1 emulator uses its first 64 bytes as the key to its own
//! disc-switch menu text. A file with two or more discs and no valid table
//! freezes at boot, and no valid table can be derived from facts about the
//! format alone. Builds without the `pops-multidisc-table` feature carry no
//! table, and each disc becomes its own EBOOT.

use super::format::MULTIDISC_TABLE_LEN;

#[cfg(feature = "pops-multidisc-table")]
mod table;

/// The multi-disc table this build carries.
#[cfg(feature = "pops-multidisc-table")]
pub(crate) fn table() -> Option<&'static [u8; MULTIDISC_TABLE_LEN]> {
    Some(&table::POPS_MULTIDISC_TABLE)
}

/// No multi-disc table: this build leaves it out.
#[cfg(not(feature = "pops-multidisc-table"))]
pub(crate) fn table() -> Option<&'static [u8; MULTIDISC_TABLE_LEN]> {
    None
}
