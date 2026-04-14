//! GameCube disc detection.

use crate::nintendo::dol::constants::{GAMECUBE_MAGIC, GAMECUBE_MAGIC_OFFSET};

/// Returns `true` if the 128-byte disc header belongs to a GameCube disc.
pub fn is_gamecube(dhead: &[u8; 128]) -> bool {
    let bytes: [u8; 4] = dhead[GAMECUBE_MAGIC_OFFSET..GAMECUBE_MAGIC_OFFSET + 4]
        .try_into()
        .unwrap();
    u32::from_be_bytes(bytes) == GAMECUBE_MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_magic() {
        let mut dhead = [0u8; 128];
        dhead[GAMECUBE_MAGIC_OFFSET..GAMECUBE_MAGIC_OFFSET + 4]
            .copy_from_slice(&GAMECUBE_MAGIC.to_be_bytes());
        assert!(is_gamecube(&dhead));
    }

    #[test]
    fn rejects_missing_magic() {
        assert!(!is_gamecube(&[0u8; 128]));
    }
}
