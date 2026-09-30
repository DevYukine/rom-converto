//! Console header detection for cartridge ROMs (e.g. copier headers).
//!
//! A header is identified by magic bytes within the first 512 bytes of the
//! file; the extension only decides which signatures are candidates. A
//! headered dump carries the ROM body plus the header, so the copier
//! variants additionally require `size % 1024 == 512`: a whole number of
//! KiB plus the 512-byte header.

/// A detected console header: what it is, how many bytes it spans, and the
/// extension of the headerless ROM it conceals.
#[derive(Clone)]
pub(super) struct RomHeader {
    pub kind: &'static str,
    pub len: u64,
    pub headerless_ext: Option<&'static str>,
}

/// The extensions whose files [`detect_header`] scans, and the only
/// per-extension entries `remove_headers` accepts.
pub(super) const KNOWN_EXTS: [&str; 6] = ["nes", "fds", "a78", "lnx", "smc", "sfc"];

/// Detects a console header from the first bytes and total size of a ROM
/// file.
pub(super) fn detect_header(ext: &str, size: u64, head: &[u8]) -> Option<RomHeader> {
    match ext {
        // iNES/NES 2.0: "NES\x1a" magic, 16-byte header.
        "nes" => head.starts_with(b"NES\x1a").then_some(RomHeader {
            kind: "iNES",
            len: 16,
            headerless_ext: None,
        }),
        // FDS disk image side: "FDS" magic, 16-byte header.
        "fds" => head.starts_with(b"FDS").then_some(RomHeader {
            kind: "FDS",
            len: 16,
            headerless_ext: None,
        }),
        // A78: "ATARI7800" magic at offset 1, 128-byte header.
        "a78" => head
            .get(1..10)
            .is_some_and(|magic| magic == b"ATARI7800")
            .then_some(RomHeader {
                kind: "A78",
                len: 128,
                headerless_ext: None,
            }),
        // LNX: "LYNX" magic, 64-byte header hiding a .lyx ROM.
        "lnx" => head.starts_with(b"LYNX").then_some(RomHeader {
            kind: "LNX",
            len: 64,
            headerless_ext: Some("lyx"),
        }),
        // SNES copier headers; a headered dump may also carry .sfc.
        "smc" | "sfc" => detect_smc(size, head),
        _ => None,
    }
}

/// SNES copier headers, in match order: the generic 512-byte copier
/// header (509 zero bytes at offset 3) first, then the copier signatures
/// that name themselves in their first bytes. All hide the ROM body at
/// offset 512 and expose `.sfc`. A copier dump's total size is a whole
/// number of KiB plus the 512 header bytes (`size % 1024 == 512`); any
/// other size is headerless data that merely looks like a header and must
/// never be stripped.
fn detect_smc(size: u64, head: &[u8]) -> Option<RomHeader> {
    if size % 1024 != 512 {
        return None;
    }
    let header = || RomHeader {
        kind: "SMC",
        len: 512,
        headerless_ext: Some("sfc"),
    };
    if head
        .get(3..512)
        .is_some_and(|bytes| bytes.iter().all(|&b| b == 0))
    {
        return Some(header());
    }
    if head.starts_with(b"\x00\x01ME DOCTOR SF 3") || head.starts_with(b"GAME DOCTOR SF 3") {
        return Some(header());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 512-byte copier header in front of a 512 KiB ROM body: the size
    /// every copier detection in these tests requires.
    const SFC_SIZE: u64 = 512 + 512 * 1024;

    #[test]
    fn nes_header_needs_magic_and_extension() {
        let mut head = vec![0u8; 512];
        head[..4].copy_from_slice(b"NES\x1a");
        let header = detect_header("nes", SFC_SIZE, &head).expect("iNES header");
        assert_eq!(header.kind, "iNES");
        assert_eq!(header.len, 16);
        assert_eq!(header.headerless_ext, None);
        // The signature without the .nes extension is not a header.
        assert!(detect_header("gba", SFC_SIZE, &head).is_none());
        // Zero bytes carry no iNES magic.
        assert!(detect_header("nes", SFC_SIZE, &vec![0u8; 512]).is_none());
    }

    #[test]
    fn fds_header_is_three_magic_bytes() {
        let mut head = vec![0u8; 512];
        head[..4].copy_from_slice(b"FDS\x1a");
        let header = detect_header("fds", SFC_SIZE, &head).expect("FDS header");
        assert_eq!(header.kind, "FDS");
        assert_eq!(header.len, 16);
        assert_eq!(header.headerless_ext, None);
        // An iNES magic behind the .fds extension is not a header.
        let mut ines = vec![0u8; 512];
        ines[..4].copy_from_slice(b"NES\x1a");
        assert!(detect_header("fds", SFC_SIZE, &ines).is_none());
    }

    #[test]
    fn a78_header_magic_sits_at_offset_one() {
        let mut head = vec![0u8; 512];
        head[1..10].copy_from_slice(b"ATARI7800");
        let header = detect_header("a78", SFC_SIZE, &head).expect("A78 header");
        assert_eq!(header.kind, "A78");
        assert_eq!(header.len, 128);
        assert_eq!(header.headerless_ext, None);
        // At offset 0 it is not an A78 header.
        let mut misplaced = head;
        misplaced.copy_within(1..10, 0);
        assert!(detect_header("a78", SFC_SIZE, &misplaced).is_none());
    }

    #[test]
    fn lnx_header_hides_a_lyx_rom() {
        let mut head = vec![0u8; 512];
        head[..4].copy_from_slice(b"LYNX");
        let header = detect_header("lnx", SFC_SIZE, &head).expect("LNX header");
        assert_eq!(header.kind, "LNX");
        assert_eq!(header.len, 64);
        assert_eq!(header.headerless_ext, Some("lyx"));
    }

    #[test]
    fn smc_copier_header_is_509_zeros_at_offset_three() {
        // Bytes 0..3 (bank/size) may be anything; 3..512 must be zero.
        let mut head = vec![0u8; 512];
        head[..3].copy_from_slice(&[0x00, 0x02, 0x07]);
        let header = detect_header("smc", SFC_SIZE, &head).expect("copier header");
        assert_eq!(header.kind, "SMC");
        assert_eq!(header.len, 512);
        assert_eq!(header.headerless_ext, Some("sfc"));
        // A single stray byte in the zero run is not a copier header.
        let mut noisy = head;
        noisy[256] = 0x42;
        assert!(detect_header("smc", SFC_SIZE, &noisy).is_none());
        // Fewer than 512 bytes cannot hold the full zero run.
        assert!(detect_header("smc", SFC_SIZE, &vec![0u8; 511]).is_none());
    }

    /// The zero run alone is not enough: only a size that is a whole
    /// number of KiB plus 512 bytes counts as a copier dump. Any other
    /// size is a headerless ROM that merely starts with zeros, and
    /// stripping it would destroy ROM data.
    #[test]
    fn copier_header_requires_the_copier_size() {
        let head = vec![0u8; 512];
        assert!(detect_header("smc", SFC_SIZE, &head).is_some());
        // A headerless ROM body: a whole number of KiB without the header.
        assert!(detect_header("smc", 512 * 1024, &head).is_none());
        // A truncated or extended dump: not a whole number of KiB plus 512.
        assert!(detect_header("smc", 512 * 1024 + 511, &head).is_none());
        assert!(detect_header("smc", 512 * 1024 + 513, &head).is_none());
    }

    #[test]
    fn copier_signatures_match() {
        for sig in [
            b"\x00\x01ME DOCTOR SF 3".as_slice(),
            b"GAME DOCTOR SF 3".as_slice(),
        ] {
            let mut head = vec![0xA5u8; 512];
            head[..sig.len()].copy_from_slice(sig);
            let header = detect_header("smc", SFC_SIZE, &head).expect("copier signature");
            assert_eq!(header.len, 512);
            assert_eq!(header.headerless_ext, Some("sfc"));
            // The same signature at a non-copier size is not a header.
            assert!(detect_header("smc", 512 * 1024, &head).is_none());
        }
    }

    /// A headerless `.sfc` extension is scanned for copier headers; only a
    /// copier-sized file matches.
    #[test]
    fn headerless_snes_extension_is_scanned_for_copier_headers() {
        assert!(detect_header("sfc", SFC_SIZE, &vec![0u8; 512]).is_some());
        assert!(detect_header("sfc", 512 * 1024, &vec![0u8; 512]).is_none());
    }
}
