//! The table a multi-disc PS1 EBOOT needs at PSTITLEIMG+0x28C.
//!
//! The PSP's PS1 emulator uses its first 64 bytes as the key to its own
//! disc-switch menu text. A file with two or more discs and no valid table
//! freezes at boot, and no valid table can be derived from facts about the
//! format alone. Without one, each disc becomes its own EBOOT.

use super::format::MULTIDISC_TABLE_LEN;

/// The multi-disc table, when this build carries one.
pub(crate) fn table() -> Option<&'static [u8; MULTIDISC_TABLE_LEN]> {
    None
}
