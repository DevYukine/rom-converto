//! Pure predicates behind the advisory warnings for well known format
//! caveats: conditions that would otherwise only be found by reading a
//! forum thread after something failed to boot or match. Each predicate is
//! biased toward silence, so callers can `log::warn!` the returned message
//! without a separate plausibility check.

use crate::nintendo::rvz::constants::WEAK_HW_CHUNK_WARN;

/// Dreamcast's IP.BIN identifier. It sits at the very start of the boot
/// track and is unique to Dreamcast among CD-ROM system headers, so a
/// substring match carries a low false-positive risk.
const DREAMCAST_IP_BIN_MAGIC: &[u8] = b"SEGA SEGAKATANA";

/// True when `bytes` (expected: the first ~64 KiB of a data track) carries
/// the Dreamcast IP.BIN magic.
pub fn dreamcast_boot_signature(bytes: &[u8]) -> bool {
    bytes
        .windows(DREAMCAST_IP_BIN_MAGIC.len())
        .any(|w| w == DREAMCAST_IP_BIN_MAGIC)
}

/// Warning for a cue-sourced Dreamcast image heading into a CD-mode CHD.
pub const DREAMCAST_CHD_WARNING: &str = "This looks like a Dreamcast disc image; some emulator cores only boot Dreamcast from a \
     GDI-based image and will not boot a CD-mode CHD. If it fails to boot, convert from the \
     GDI-based image instead.";

/// Returns the distinct extensions (sorted, lowercased) when `exts` mixes
/// more than one track format, or `None` when the set is consistent.
pub fn mixed_playlist_extensions<'a>(exts: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    for ext in exts {
        let lower = ext.to_ascii_lowercase();
        if !seen.contains(&lower) {
            seen.push(lower);
        }
    }
    if seen.len() <= 1 {
        return None;
    }
    seen.sort();
    Some(seen.join(", "))
}

/// Warning for an RVZ chunk size large enough to stutter on weak playback
/// hardware, or `None` when `chunk_size` stays at or below the threshold.
pub fn oversized_rvz_chunk(chunk_size: u32) -> Option<&'static str> {
    if chunk_size > WEAK_HW_CHUNK_WARN {
        Some(
            "This chunk size reads more data per seek, which can stutter on weaker playback \
             hardware; re-encode at 128 KiB (Dolphin's default) if that happens.",
        )
    } else {
        None
    }
}

/// Hint for the `unsupported` verdict a compressed nx container (nsz, xcz)
/// gets from `dat verify`/`dat scan`, since those have no inner hasher.
pub const NX_DAT_UNSUPPORTED_HINT: &str = "Compressed Switch containers (.nsz, .xcz) have no inner hasher and report as \
     unsupported. Decompress with `nx decompress` first, which needs a prod.keys file, then \
     verify the resulting .nsp/.xci.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dreamcast_signature_hit() {
        let mut bytes = vec![0u8; 0x100];
        bytes[0x10..0x10 + DREAMCAST_IP_BIN_MAGIC.len()].copy_from_slice(DREAMCAST_IP_BIN_MAGIC);
        assert!(dreamcast_boot_signature(&bytes));
    }

    #[test]
    fn dreamcast_signature_miss() {
        let bytes = vec![0u8; 0x100];
        assert!(!dreamcast_boot_signature(&bytes));
    }

    #[test]
    fn dreamcast_signature_miss_on_short_buffer() {
        let bytes = vec![0u8; 4];
        assert!(!dreamcast_boot_signature(&bytes));
    }

    #[test]
    fn mixed_playlist_extensions_single_ext_is_none() {
        let exts = ["cue", "CUE", "cue"];
        assert_eq!(mixed_playlist_extensions(exts.into_iter()), None);
    }

    #[test]
    fn mixed_playlist_extensions_case_insensitive() {
        let exts = ["ISO", "iso"];
        assert_eq!(mixed_playlist_extensions(exts.into_iter()), None);
    }

    #[test]
    fn mixed_playlist_extensions_multi_ext_reports_both() {
        let exts = ["cue", "chd"];
        assert_eq!(
            mixed_playlist_extensions(exts.into_iter()),
            Some("chd, cue".to_string())
        );
    }

    #[test]
    fn oversized_rvz_chunk_default_is_silent() {
        assert_eq!(oversized_rvz_chunk(128 * 1024), None);
    }

    #[test]
    fn oversized_rvz_chunk_at_threshold_is_silent() {
        assert_eq!(oversized_rvz_chunk(WEAK_HW_CHUNK_WARN), None);
    }

    #[test]
    fn oversized_rvz_chunk_above_threshold_warns() {
        assert!(oversized_rvz_chunk(2 * 1024 * 1024).is_some());
    }
}
