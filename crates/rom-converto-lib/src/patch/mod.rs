//! ROM patch application for the organize pass: detect a patch file's
//! format, index it by the source ROM it targets, and apply it to a source
//! ROM through a scratch sibling of the output path that is published by
//! rename only on success.
//!
//! Detection is by extension (case insensitive) plus the format's magic
//! bytes where it has one; [`Patch::open`] reads only header/footer
//! metadata, so a [`Patch`] is cheap to hold, and [`Patch::apply`] streams
//! everything through [`COPY_CHUNK_BYTES`] windows with positional IO.

mod aps;
mod bps;
mod io;
mod ips;
mod ninja;
mod ppf;
mod ups;
mod vcdiff;

use crate::util::hash::crc32_of_file;
use crate::util::{CancelToken, Cancelled};
use anyhow::{Context, Result, bail, ensure};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Extensions recognized as patch files during the organize scan.
pub const PATCH_EXTENSIONS: &[&str] = &[
    "aps", "bps", "ebp", "ips", "ips32", "ppf", "rup", "ups", "vcdiff", "xdelta",
];

/// The parsed format family driving [`Patch::apply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// IPS with 3-byte offsets.
    Ips,
    /// IPS with 4-byte offsets (`IPS32` magic or `.ips32` extension).
    Ips32,
    /// IPS body with a trailing JSON blob after the end marker (ignored).
    Ebp,
    Bps,
    Ups,
    Ppf,
    ApsGba,
    /// N64-flavoured APS with the cartridge identification header.
    ApsN64,
    Vcdiff,
    /// NINJA 2 single-file raw/binary patch.
    Ninja,
}

/// A ROM patch file: its path, format, and the source checksum used to
/// match it against ROMs during the organize scan.
#[derive(Debug, Clone)]
pub struct Patch {
    path: PathBuf,
    format: &'static str,
    kind: Format,
    /// Embedded source CRC32 when the format carries one, else the CRC32
    /// parsed from the file name.
    source_crc: Option<u32>,
    /// Embedded source CRC32 alone, so apply can require it even when a
    /// file-name CRC is also present.
    embedded_source_crc: Option<u32>,
}

impl Patch {
    /// Opens the patch at `path`, detecting its format by extension and
    /// reading the header/footer metadata a scan needs: the BPS/UPS footers
    /// (with the patch-contents CRC verified on open, hashed with
    /// cancellation) and the variant magic of IPS/APS/PPF. A file too short
    /// to carry its magic is accepted with no embedded metadata; matching
    /// then happens on the file name, and a truncated body surfaces when
    /// the patch is applied.
    pub fn open(path: &Path, cancel: &CancelToken) -> Result<Self> {
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let Some(&format) = PATCH_EXTENSIONS.iter().find(|known| **known == ext) else {
            bail!("unsupported patch format: .{ext}");
        };

        let head = read_head(path)?;
        let kind = match format {
            "ips" => {
                if head.len() >= 5 {
                    match &head[..5] {
                        b"IPS32" => Format::Ips32,
                        b"PATCH" => Format::Ips,
                        other => bail!(
                            "not an IPS patch: bad magic {:?}",
                            String::from_utf8_lossy(other)
                        ),
                    }
                } else {
                    Format::Ips
                }
            }
            "ips32" => {
                if head.len() >= 5 && &head[..5] != b"IPS32" {
                    bail!(
                        "not an IPS32 patch: bad magic {:?}",
                        String::from_utf8_lossy(&head[..head.len().min(5)])
                    );
                }
                Format::Ips32
            }
            "ebp" => {
                if head.len() >= 5 && &head[..5] != b"PATCH" {
                    bail!(
                        "not an EBP patch: bad magic {:?}",
                        String::from_utf8_lossy(&head[..head.len().min(5)])
                    );
                }
                Format::Ebp
            }
            "bps" => {
                if head.len() >= 4 && &head[..4] != b"BPS1" {
                    bail!(
                        "not a BPS patch: bad magic {:?}",
                        &head[..head.len().min(4)]
                    );
                }
                Format::Bps
            }
            "ups" => {
                if head.len() >= 4 && &head[..4] != b"UPS1" {
                    bail!(
                        "not an UPS patch: bad magic {:?}",
                        &head[..head.len().min(4)]
                    );
                }
                Format::Ups
            }
            "ppf" => {
                if head.len() >= 5
                    && &head[..5] != b"PPF10"
                    && &head[..5] != b"PPF20"
                    && &head[..5] != b"PPF30"
                {
                    bail!(
                        "not a PPF patch: bad magic {:?}",
                        &head[..head.len().min(5)]
                    );
                }
                Format::Ppf
            }
            "aps" => {
                // `APS10` is `APS1` plus one byte, so the N64 route needs the
                // type byte to be 0 or 1. Record-aligned bodies route to GBA
                // first, with an N64 fallback on the GBA original-size
                // mismatch (see the aps module docs for the residual).
                let patch_len = path.metadata().map(|meta| meta.len()).unwrap_or(0);
                let n64_magic = aps::aps10_shaped(&head);
                // A GBA body is at least one whole 65544-byte record,
                // optionally plus a final record of at least its 8-byte
                // head with the XOR bytes present.
                let body = patch_len.saturating_sub(12);
                let tail = body % (8 + 65536);
                let record_aligned =
                    patch_len >= 12 && (tail == 0 || (body >= 8 + 65536 && tail >= 8));
                if n64_magic && !record_aligned {
                    Format::ApsN64
                } else if head.len() < 4 || &head[..4] == b"APS1" {
                    Format::ApsGba
                } else {
                    bail!(
                        "not an APS patch: bad magic {:?}",
                        &head[..head.len().min(5)]
                    );
                }
            }
            "vcdiff" | "xdelta" => {
                if head.len() >= 4 && head[..4] != [0xD6, 0xC3, 0xC4, 0x00] {
                    bail!("not a VCDIFF patch: bad magic");
                }
                Format::Vcdiff
            }
            "rup" => {
                if head.len() >= 6 && &head[..6] != b"NINJA2" {
                    bail!(
                        "not a NINJA 2 patch: bad magic {:?}",
                        &head[..head.len().min(6)]
                    );
                }
                Format::Ninja
            }
            _ => unreachable!("every PATCH_EXTENSIONS entry is handled"),
        };

        // BPS and UPS close with the source and target CRC32 of the files
        // they were built from; read and validate that footer now so the
        // patch index can match on it. The trailing checksum covers every
        // byte of the patch before it, so a corrupt patch is rejected here.
        let embedded_source_crc = match kind {
            Format::Bps | Format::Ups => footer_crcs(path, kind == Format::Ups, cancel)?,
            _ => None,
        };
        let source_crc = embedded_source_crc.or_else(|| crc_from_name(path));

        // The label follows the parsed body, not the extension: an `.ips`
        // file with `IPS32` magic is an `"ips32"` patch, and `.vcdiff` is
        // always `"vcdiff"`.
        let format = match kind {
            Format::Ips => "ips",
            Format::Ips32 => "ips32",
            Format::Ebp => "ebp",
            Format::Bps => "bps",
            Format::Ups => "ups",
            Format::Ppf => "ppf",
            Format::ApsGba | Format::ApsN64 => "aps",
            Format::Vcdiff => "vcdiff",
            Format::Ninja => "rup",
        };

        Ok(Self {
            path: path.to_path_buf(),
            format,
            kind,
            source_crc,
            embedded_source_crc,
        })
    }

    /// The patch file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The format label: `"ips"`, `"ips32"`, `"ebp"`, `"bps"`, `"ups"`,
    /// `"ppf"`, `"aps"`, `"vcdiff"` or `"rup"`.
    pub fn format(&self) -> &'static str {
        self.format
    }

    /// The CRC32 identifying the source ROM this patch applies to: the
    /// checksum embedded in the format when it carries one, else parsed
    /// from the file name, else `None`.
    pub fn source_crc(&self) -> Option<u32> {
        self.source_crc
    }

    /// Applies the patch to `source`, writing the result to `output`
    /// through a scratch sibling that is renamed into place only on
    /// success. When the format carries a source checksum it is verified
    /// before anything is written, and a carried target checksum (or MD5)
    /// is verified on the result before it is published.
    pub fn apply(&self, source: &Path, output: &Path, cancel: &CancelToken) -> Result<()> {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        if let Some(crc) = self.embedded_source_crc {
            // Formats whose footer carries the source CRC32 verify it before
            // anything is written; the checksum-bearing formats without one
            // (NINJA 2's MD5, the N64 APS identification) verify inside
            // their applier.
            let actual = crc32_of_file(source, cancel).context("hashing the source")?;
            ensure!(
                actual == crc,
                "source does not match the patch: embedded CRC32 {crc:08x}, source has {actual:08x}"
            );
        }
        let (out, temp) = crate::util::scratch_output_file(output)
            .context("creating the patched output")?
            .into_parts();
        // Positional IO through `File` is unbuffered, so no flush is needed.
        self.apply_to(source, &out, cancel)?;
        drop(out);
        crate::util::publish_temp(temp, output, true).context("publishing the patched output")?;
        Ok(())
    }

    fn apply_to(&self, source: &Path, out: &File, cancel: &CancelToken) -> Result<()> {
        let mut buf = vec![0u8; io::COPY_CHUNK_BYTES];
        match self.kind {
            Format::Ips | Format::Ips32 | Format::Ebp => {
                ips::apply(&self.path, source, out, self.kind, &mut buf, cancel)
            }
            Format::Bps => bps::apply(&self.path, source, out, &mut buf, cancel),
            Format::Ups => ups::apply(&self.path, source, out, &mut buf, cancel),
            Format::Ppf => ppf::apply(&self.path, source, out, &mut buf, cancel),
            Format::ApsGba | Format::ApsN64 => {
                aps::apply(&self.path, source, out, self.kind, &mut buf, cancel)
            }
            Format::Vcdiff => vcdiff::apply(&self.path, source, out, &mut buf, cancel),
            Format::Ninja => ninja::apply(&self.path, source, out, &mut buf, cancel),
        }
    }
}

/// Reads the source CRC32 from a BPS/UPS footer. The footer also carries the
/// target CRC32 and a checksum over the patch contents, each stored
/// little-endian; the contents checksum is verified here so a corrupt patch
/// fails on open.
fn footer_crcs(path: &Path, ups: bool, cancel: &CancelToken) -> Result<Option<u32>> {
    let name = if ups { "UPS" } else { "BPS" };
    let file = File::open(path).context(format!("opening the {name} patch"))?;
    let len = file.metadata()?.len();
    if len < 16 {
        return Ok(None);
    }
    let mut footer = [0u8; 12];
    crate::util::pread::file_read_exact_at(&file, &mut footer, len - 12)?;
    let source = u32::from_le_bytes(footer[0..4].try_into().expect("fixed-size slice"));
    let patch = u32::from_le_bytes(footer[8..12].try_into().expect("fixed-size slice"));
    let mut buf = vec![0u8; io::COPY_CHUNK_BYTES];
    let actual = io::crc32_of_range(&file, 0, len - 4, &mut buf, cancel)?;
    ensure!(
        actual == patch,
        "{name} patch contents checksum mismatch: footer says {patch:08x}, contents hash to {actual:08x}"
    );
    Ok(Some(source))
}

/// Reads up to the first 16 bytes of a file, whatever the file holds; a
/// shorter file yields the bytes it has.
fn read_head(path: &Path) -> Result<Vec<u8>> {
    let mut head = [0u8; 16];
    let mut file = File::open(path).context("opening the patch")?;
    let mut n = 0;
    loop {
        let read = file.read(&mut head[n..])?;
        if read == 0 {
            break;
        }
        n += read;
        if n == head.len() {
            break;
        }
    }
    Ok(head[..n].to_vec())
}

/// The CRC32 parsed from a patch file's name: a run of exactly eight hex
/// digits in the file stem whose edges are the start or end of the stem, a
/// bracket, a parenthesis, a dot, a space, an underscore, a dash, or a
/// `0x`/`0X` prefix (itself preceded by one of those edges). A run with a
/// `0x`/`0X` prefix or an A-F letter outranks a run of only decimal
/// digits, and the last run of the best class wins; a bare run of only
/// decimal digits is a date or a counter, not a checksum. `Game
/// [1A2B3C4D].ips`, `1A2B3C4D game.bps`, `game (0x1a2b3c4d).ups` and
/// `game.1A2B3C4D.ips` all carry the CRC32 `0x1a2b3c4d`, and
/// `Game [1A2B3C4D] (20240101).ips` still carries `0x1a2b3c4d` rather than
/// the parenthesised date.
fn crc_from_name(path: &Path) -> Option<u32> {
    let stem = path.file_stem()?.to_str()?;
    let bytes = stem.as_bytes();
    let is_edge = |byte: u8| matches!(byte, b'[' | b']' | b'(' | b')' | b'.' | b' ' | b'_' | b'-');
    let mut last: Option<u32> = None;
    let mut last_checksum_like: Option<u32> = None;
    for start in 0..bytes.len() {
        // A token begins at the start of the stem or right after an edge.
        if start > 0 && !is_edge(bytes[start - 1]) {
            continue;
        }
        // A `0x`/`0X` prefix directly after that edge introduces the digits.
        let (digits_start, digits_end) = if bytes[start..].len() >= 2
            && bytes[start] == b'0'
            && (bytes[start + 1] == b'x' || bytes[start + 1] == b'X')
        {
            (start + 2, start + 10)
        } else {
            (start, start + 8)
        };
        let Some(digits) = bytes.get(digits_start..digits_end) else {
            continue;
        };
        if !digits.iter().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        if digits_end < bytes.len() && !is_edge(bytes[digits_end]) {
            continue;
        }
        // A bare run (not `0x`-prefixed and not bracketed or parenthesised)
        // must carry at least one A-F letter: an all-decimal run is a date
        // or a counter, not a CRC32.
        let prefixed = digits_start == start + 2;
        let bracketed = (start > 0 && matches!(bytes[start - 1], b'[' | b'('))
            && (digits_end < bytes.len() && matches!(bytes[digits_end], b']' | b')'));
        if !prefixed
            && !bracketed
            && !digits
                .iter()
                .any(|b| matches!(b, b'A'..=b'F' | b'a'..=b'f'))
        {
            continue;
        }
        let value = u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
        last = Some(value);
        if prefixed
            || digits
                .iter()
                .any(|b| matches!(b, b'A'..=b'F' | b'a'..=b'f'))
        {
            last_checksum_like = Some(value);
        }
    }
    last_checksum_like.or(last)
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// Writes `patch` and `source` into `dir`, applies the patch through
    /// the public API, and returns the patched output bytes.
    pub(super) fn apply_ok(
        dir: &Path,
        patch_name: &str,
        patch: &[u8],
        source: &[u8],
        cancel: &CancelToken,
    ) -> Vec<u8> {
        let patch_path = dir.join(patch_name);
        std::fs::write(&patch_path, patch).expect("write patch");
        let source_path = dir.join("source.bin");
        std::fs::write(&source_path, source).expect("write source");
        let parsed = Patch::open(&patch_path, &CancelToken::new()).expect("open patch");
        let output_path = dir.join("output.bin");
        parsed
            .apply(&source_path, &output_path, cancel)
            .expect("apply patch");
        std::fs::read(&output_path).expect("read output")
    }

    /// Like [`apply_ok`], but returns the error instead of unwrapping.
    pub(super) fn apply_err(
        dir: &Path,
        patch_name: &str,
        patch: &[u8],
        source: &[u8],
        cancel: &CancelToken,
    ) -> anyhow::Error {
        let patch_path = dir.join(patch_name);
        std::fs::write(&patch_path, patch).expect("write patch");
        let source_path = dir.join("source.bin");
        std::fs::write(&source_path, source).expect("write source");
        let parsed = Patch::open(&patch_path, &CancelToken::new()).expect("open patch");
        parsed
            .apply(&source_path, &dir.join("output.bin"), cancel)
            .expect_err("apply must fail")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crc::{CRC_32_ISO_HDLC, Crc};

    fn crc32(data: &[u8]) -> u32 {
        Crc::<u32>::new(&CRC_32_ISO_HDLC).checksum(data)
    }

    #[test]
    fn parses_the_last_crc_token_from_names() {
        let cases = [
            ("Game [1A2B3C4D].ips", Some(0x1A2B_3C4D)),
            ("1A2B3C4D game.bps", Some(0x1A2B_3C4D)),
            ("game (0x1a2b3c4d).ups", Some(0x1A2B_3C4D)),
            ("game.1A2B3C4D.ips", Some(0x1A2B_3C4D)),
            ("deadbeef-a.ips", Some(0xDEAD_BEEF)),
            // A bare run of only decimal digits is a date or a counter.
            ("abc 00112233 998877AB.ppf", Some(0x9988_77AB)),
            ("abc-def-00112233.ppf", None),
            ("Hack (20240101).ips", Some(0x2024_0101)),
            ("Hack 20240101.ips", None),
            // A parenthesised date must not override the bracketed checksum.
            ("Game [1A2B3C4D] (20240101).ips", Some(0x1A2B_3C4D)),
            ("game 00112233 (20240101).ups", Some(0x2024_0101)),
            ("nocrc.ips", None),
            ("deadbeefs.ips", None),
        ];
        for (name, expected) in cases {
            assert_eq!(crc_from_name(Path::new(name)), expected, "name: {name}");
        }
    }

    #[test]
    fn empty_and_tiny_patches_open_without_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deadbeef.ips");
        std::fs::write(&path, b"").unwrap();
        let parsed = Patch::open(&path, &CancelToken::new()).unwrap();
        assert_eq!(parsed.format(), "ips");
        assert_eq!(parsed.source_crc(), Some(0xDEAD_BEEF));

        let path = dir.path().join("patch-only.ips");
        std::fs::write(&path, b"PATCH").unwrap();
        let parsed = Patch::open(&path, &CancelToken::new()).unwrap();
        assert_eq!(parsed.source_crc(), None);
    }

    #[test]
    fn detects_ips32_by_magic_and_vcdiff_by_magic_and_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flavor.ips");
        std::fs::write(&path, b"IPS32rest").unwrap();
        assert_eq!(
            Patch::open(&path, &CancelToken::new()).unwrap().format(),
            "ips32"
        );

        let path = dir.path().join("delta.ips32");
        std::fs::write(&path, b"IPS32").unwrap();
        assert_eq!(
            Patch::open(&path, &CancelToken::new()).unwrap().format(),
            "ips32"
        );

        let path = dir.path().join("delta.vcdiff");
        std::fs::write(&path, [0xD6, 0xC3, 0xC4, 0x00, 0x00]).unwrap();
        assert_eq!(
            Patch::open(&path, &CancelToken::new()).unwrap().format(),
            "vcdiff"
        );

        // The same magic under the `.xdelta` extension of the VCDIFF
        // format detects as vcdiff too.
        let path = dir.path().join("delta.xdelta");
        std::fs::write(&path, [0xD6, 0xC3, 0xC4, 0x00, 0x00]).unwrap();
        assert_eq!(
            Patch::open(&path, &CancelToken::new()).unwrap().format(),
            "vcdiff"
        );

        let path = dir.path().join("wrong.vcdiff");
        std::fs::write(&path, b"JUNKJUNK").unwrap();
        assert!(Patch::open(&path, &CancelToken::new()).is_err());
    }

    #[test]
    fn rejects_unknown_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("patch.bdf");
        std::fs::write(&path, b"").unwrap();
        let err = Patch::open(&path, &CancelToken::new()).unwrap_err();
        assert!(err.to_string().contains(".bdf"), "{err}");
    }

    #[test]
    fn bps_patch_contents_checksum_is_verified_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut patch = bps_test_patch(b"source", b"target");
        let corrupt_at = patch.len() - 5;
        patch[corrupt_at] ^= 0xFF;
        let path = dir.path().join("bad.bps");
        std::fs::write(&path, patch).unwrap();
        let err = Patch::open(&path, &CancelToken::new()).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
    }

    #[test]
    fn apply_verifies_the_embedded_source_checksum_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let patch = bps_test_patch(b"actual source", b"target");
        let output = test_support::apply_err(
            dir.path(),
            "deadbeef.bps",
            &patch,
            b"different source",
            &CancelToken::new(),
        );
        assert!(output.to_string().contains("does not match"), "{output}");
        assert!(!dir.path().join("output.bin").exists());
    }

    #[test]
    fn open_rejects_a_bad_ips_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.ips");
        std::fs::write(&path, b"NOTCHnothing").unwrap();
        let err = Patch::open(&path, &CancelToken::new()).unwrap_err();
        assert!(err.to_string().contains("bad magic"), "{err}");
    }

    #[test]
    fn cancelled_apply_reports_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let patch = bps_test_patch(b"source", b"target");
        let patch_path = dir.path().join("cancel.bps");
        std::fs::write(&patch_path, patch).unwrap();
        let source_path = dir.path().join("source.bin");
        std::fs::write(&source_path, b"source").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let parsed = Patch::open(&patch_path, &CancelToken::new()).unwrap();
        let err = parsed
            .apply(&source_path, &dir.path().join("out.bin"), &cancel)
            .unwrap_err();
        assert!(Cancelled::in_chain(&err), "{err}");
    }

    #[test]
    fn cancelled_open_reports_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let patch = bps_test_patch(b"source", b"target");
        let path = dir.path().join("cancel.bps");
        std::fs::write(&path, patch).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = Patch::open(&path, &cancel).unwrap_err();
        assert!(Cancelled::in_chain(&err), "{err}");
    }

    /// Builds a minimal valid BPS patch turning `source` into `target` via
    /// a single TargetRead of the whole target.
    fn bps_test_patch(source: &[u8], target: &[u8]) -> Vec<u8> {
        use io::test_support::bps_varint;
        let mut patch = b"BPS1".to_vec();
        patch.extend(bps_varint(source.len() as u64));
        patch.extend(bps_varint(target.len() as u64));
        patch.extend(bps_varint(0));
        patch.extend(bps_varint((((target.len() as u64) - 1) << 2) | 1));
        patch.extend_from_slice(target);
        let source_crc = crc32(source);
        let target_crc = crc32(target);
        patch.extend(source_crc.to_le_bytes());
        patch.extend(target_crc.to_le_bytes());
        let patch_crc = crc32(&patch);
        patch.extend(patch_crc.to_le_bytes());
        patch
    }
}
