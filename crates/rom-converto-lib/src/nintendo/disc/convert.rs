//! One-step conversion between supported GameCube and Wii disc containers.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use super::gcz::{self, DEFAULT_BLOCK_SIZE};
use super::legacy::{
    ALL_MIGRATE_FORMATS, LEGACY_MAGIC_PROBE_LEN, LegacyFormat, detect_legacy_format,
    gate_legacy_input,
};
use super::nkit::format::{NKIT_HEADER_OFFSET, NKIT_MAGIC_VERSION};
use super::rvz::compress::{RvzCompressOptions, compress_iso};
use super::rvz::constants::RVZ_MAGIC;
use super::rvz::decompress::{decompress_disc, decompress_disc_to_wbfs};
use super::rvz::error::{RvzError, RvzResult};
use super::wbfs::format::WBFS_MAGIC;
use super::wia::format::WIA_MAGIC;
use super::wia::{WiaCompressOptions, compress_to_wia};
use crate::nintendo::{dol::is_gamecube, rvl::is_wii};
use crate::util::{CancelToken, ProgressReporter};

/// A writable GameCube or Wii disc container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscFormat {
    /// Plain logical disc bytes.
    Iso,
    /// Scrubbed Wii disc blocks.
    Wbfs,
    /// Deflate-compressed disc blocks.
    Gcz,
    /// Wii disc groups compressed with a selectable codec.
    Wia,
    /// Zstandard-compressed GameCube or Wii disc groups.
    Rvz,
}

impl DiscFormat {
    /// Returns the container's lowercase filename extension.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Iso => "iso",
            Self::Wbfs => "wbfs",
            Self::Gcz => "gcz",
            Self::Wia => "wia",
            Self::Rvz => "rvz",
        }
    }

    /// Recognizes a supported container extension, including `.gcm` as ISO.
    pub fn from_extension(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        if extension.eq_ignore_ascii_case("gcm") {
            return Some(Self::Iso);
        }
        extension.parse().ok()
    }
}

impl FromStr for DiscFormat {
    type Err = RvzError;

    fn from_str(value: &str) -> RvzResult<Self> {
        if value.eq_ignore_ascii_case("iso") {
            Ok(Self::Iso)
        } else if value.eq_ignore_ascii_case("wbfs") {
            Ok(Self::Wbfs)
        } else if value.eq_ignore_ascii_case("gcz") {
            Ok(Self::Gcz)
        } else if value.eq_ignore_ascii_case("wia") {
            Ok(Self::Wia)
        } else if value.eq_ignore_ascii_case("rvz") {
            Ok(Self::Rvz)
        } else {
            Err(RvzError::Custom(format!(
                "unknown disc format {value:?}; expected iso, wbfs, gcz, wia, or rvz"
            )))
        }
    }
}

/// Target container, codec settings, and legacy-input integrity policy.
#[derive(Clone, Copy, Debug)]
pub struct DiscConvertOptions {
    /// Container to write.
    pub format: DiscFormat,
    /// Settings used for RVZ output.
    pub rvz: RvzCompressOptions,
    /// Settings used for WIA output.
    pub wia: WiaCompressOptions,
    /// Uncompressed block size used for GCZ output.
    pub gcz_block_size: u32,
    /// Skips the legacy-container integrity pass before conversion.
    pub skip_verify: bool,
    /// Legacy containers accepted by the caller's console policy.
    pub allowed_legacy: &'static [LegacyFormat],
    /// Command suggested when the source belongs to another console.
    pub next_command: &'static str,
}

impl Default for DiscConvertOptions {
    fn default() -> Self {
        Self {
            format: DiscFormat::Iso,
            rvz: RvzCompressOptions::default(),
            wia: WiaCompressOptions::default(),
            gcz_block_size: DEFAULT_BLOCK_SIZE,
            skip_verify: false,
            allowed_legacy: ALL_MIGRATE_FORMATS,
            next_command: "rvl convert",
        }
    }
}

/// Verifies legacy inputs when required, then converts the logical disc to the target container.
///
/// # Errors
/// Returns an error for a disallowed or corrupt source, invalid codec settings,
/// I/O failures, or cancellation. Failed conversions remove their partial output.
pub async fn convert_disc(
    input: &Path,
    output: &Path,
    options: DiscConvertOptions,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> RvzResult<()> {
    gate_legacy_input(
        input,
        options.allowed_legacy,
        options.next_command,
        options.skip_verify,
        false,
        progress,
        &cancel,
    )
    .await?;
    match options.format {
        DiscFormat::Iso => decompress_disc(input, output, progress, cancel).await,
        DiscFormat::Wbfs => decompress_disc_to_wbfs(input, output, progress, cancel).await,
        DiscFormat::Gcz => {
            gcz::compress_to_gcz(input, output, options.gcz_block_size, progress, cancel).await?;
            Ok(())
        }
        DiscFormat::Wia => compress_to_wia(input, output, options.wia, progress, cancel).await,
        DiscFormat::Rvz => compress_iso(input, output, options.rvz, progress, cancel).await,
    }
}

/// Derives the target filename, collapsing `.nkit.iso` and `.nkit.gcz` double extensions.
pub fn derive_convert_path(input: &Path, format: DiscFormat) -> PathBuf {
    if let Some(name) = input.file_name().and_then(|name| name.to_str())
        && let Some(suffix) = name.get(name.len().saturating_sub(9)..)
        && (suffix.eq_ignore_ascii_case(".nkit.iso") || suffix.eq_ignore_ascii_case(".nkit.gcz"))
    {
        return input.with_file_name(format!(
            "{}.{}",
            &name[..name.len() - 9],
            format.extension()
        ));
    }
    input.with_extension(format.extension())
}

/// Detects the source container by magic bytes without opening a container reader.
/// NKit containers have no writable equivalent and return `None`.
///
/// # Errors
/// Returns an I/O error when the source cannot be opened or its head cannot be read.
pub fn detect_disc_format(input: &Path) -> std::io::Result<Option<DiscFormat>> {
    let mut file = File::open(input)?;
    let mut head = [0u8; LEGACY_MAGIC_PROBE_LEN];
    let mut len = 0;
    while len < head.len() {
        let read = file.read(&mut head[len..])?;
        if read == 0 {
            break;
        }
        len += read;
    }
    let format = disc_format_from_head(&head[..len]);
    if format == Some(DiscFormat::Gcz)
        && detect_legacy_format(input)? == Some(LegacyFormat::NkitGcz)
    {
        return Ok(None);
    }
    Ok(format)
}

/// Identifies a writable container from its head bytes without opening or decoding it.
/// GCZ heads identify GCZ even when the compressed disc contains NKit data.
pub fn disc_format_from_head(head: &[u8]) -> Option<DiscFormat> {
    let magic = head.get(..4)?;
    if magic == RVZ_MAGIC {
        Some(DiscFormat::Rvz)
    } else if magic == WBFS_MAGIC {
        Some(DiscFormat::Wbfs)
    } else if magic == WIA_MAGIC {
        Some(DiscFormat::Wia)
    } else if magic == gcz::format::GCZ_MAGIC.to_le_bytes() {
        Some(DiscFormat::Gcz)
    } else if head.get(NKIT_HEADER_OFFSET..NKIT_HEADER_OFFSET + 4) == Some(&NKIT_MAGIC_VERSION[..4])
    {
        None
    } else {
        let disc_head: &[u8; 0x80] = head.get(..0x80)?.try_into().ok()?;
        (is_gamecube(disc_head) || is_wii(disc_head)).then_some(DiscFormat::Iso)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::disc::input::open_disc_input;
    use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso;
    use crate::util::NoProgress;

    #[test]
    fn derive_paths_strip_nkit_and_replace_extensions() {
        for (input, format, expected) in [
            ("a.nkit.iso", DiscFormat::Gcz, "a.gcz"),
            ("a.NKIT.GCZ", DiscFormat::Iso, "a.iso"),
            ("a.rvz", DiscFormat::Iso, "a.iso"),
            ("dir/a.iso", DiscFormat::Wbfs, "dir/a.wbfs"),
            ("a.nkit.wia", DiscFormat::Rvz, "a.nkit.rvz"),
        ] {
            assert_eq!(
                derive_convert_path(Path::new(input), format),
                Path::new(expected)
            );
        }
    }

    #[test]
    fn formats_parse_case_insensitively() {
        for format in [
            DiscFormat::Iso,
            DiscFormat::Wbfs,
            DiscFormat::Gcz,
            DiscFormat::Wia,
            DiscFormat::Rvz,
        ] {
            assert_eq!(
                format
                    .extension()
                    .to_ascii_uppercase()
                    .parse::<DiscFormat>()
                    .unwrap(),
                format
            );
            assert_eq!(
                DiscFormat::from_extension(Path::new(&format!("game.{}", format.extension()))),
                Some(format)
            );
        }
        assert_eq!(
            DiscFormat::from_extension(Path::new("game.GCM")),
            Some(DiscFormat::Iso)
        );
        assert!("nkit".parse::<DiscFormat>().is_err());
    }

    #[test]
    fn detection_uses_magic_not_extensions_or_container_validity() {
        use crate::nintendo::disc::{gcz, wia};
        use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;

        let dir = tempfile::tempdir().unwrap();
        let iso = make_fake_wii_iso(0x80000);
        let gcz = gcz::test_fixtures::make_gcz(&iso, 0x8000, 1);
        let mut damaged_gcz = gcz.clone();
        damaged_gcz[..4].fill(0);
        let random: Vec<u8> = (0..4096).map(|i| (i * 73 + 19) as u8).collect();
        for (name, bytes, expected) in [
            ("wii.bin", iso.clone(), Some(DiscFormat::Iso)),
            (
                "gamecube.bin",
                make_fake_gamecube_iso(0x10000),
                Some(DiscFormat::Iso),
            ),
            ("gcz.bin", gcz, Some(DiscFormat::Gcz)),
            (
                "wia.bin",
                wia::test_fixtures::make_wia(&iso, 0, 0x20_0000),
                Some(DiscFormat::Wia),
            ),
            ("damaged.gcz", damaged_gcz, None),
            ("random.iso", random, None),
            ("tiny.iso", vec![0; 2], None),
        ] {
            let input = dir.path().join(name);
            std::fs::write(&input, &bytes).unwrap();
            assert_eq!(detect_disc_format(&input).unwrap(), expected, "{name}");
            assert_eq!(disc_format_from_head(&bytes), expected, "{name}");
        }
        for (magic, format) in [
            (RVZ_MAGIC, DiscFormat::Rvz),
            (WBFS_MAGIC, DiscFormat::Wbfs),
            (WIA_MAGIC, DiscFormat::Wia),
            (gcz::format::GCZ_MAGIC.to_le_bytes(), DiscFormat::Gcz),
        ] {
            assert_eq!(disc_format_from_head(&magic), Some(format));
        }
    }

    #[tokio::test]
    async fn every_format_round_trips_wii_disc() {
        let dir = tempfile::tempdir().unwrap();
        let iso = make_fake_wii_iso(2 * 1024 * 1024);
        let input = dir.path().join("source.iso");
        std::fs::write(&input, &iso).unwrap();
        assert_eq!(detect_disc_format(&input).unwrap(), Some(DiscFormat::Iso));
        for format in [
            DiscFormat::Iso,
            DiscFormat::Wbfs,
            DiscFormat::Gcz,
            DiscFormat::Wia,
            DiscFormat::Rvz,
        ] {
            let output = dir.path().join(format!("target.{}", format.extension()));
            convert_disc(
                &input,
                &output,
                DiscConvertOptions {
                    format,
                    ..Default::default()
                },
                &NoProgress,
                CancelToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(detect_disc_format(&output).unwrap(), Some(format));
            let mut reader = open_disc_input(&output).unwrap();
            let mut restored = vec![0u8; iso.len()];
            reader.read_exact(&mut restored).unwrap();
            assert_eq!(restored, iso, "{} round trip", format.extension());
        }
    }

    #[tokio::test]
    async fn legacy_verification_and_console_gate_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let mut iso = make_fake_wii_iso(0x80000);
        iso[0x40000..0x40020].fill(0);
        let mut gcz = super::super::gcz::test_fixtures::make_gcz(&iso, 0x8000, 1);
        *gcz.last_mut().unwrap() ^= 1;
        let input = dir.path().join("corrupt.gcz");
        let output = dir.path().join("out.iso");
        std::fs::write(&input, gcz).unwrap();
        let error = convert_disc(
            &input,
            &output,
            DiscConvertOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RvzError::Gcz(_)), "{error}");
        assert!(!output.exists());

        let wia = dir.path().join("source.wia");
        std::fs::write(
            &wia,
            super::super::wia::test_fixtures::make_wia(&iso, 0, 0x20_0000),
        )
        .unwrap();
        let error = convert_disc(
            &wia,
            &output,
            DiscConvertOptions {
                allowed_legacy: super::super::legacy::DOL_MIGRATE_FORMATS,
                skip_verify: true,
                ..Default::default()
            },
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RvzError::Custom(_)), "{error}");
        assert!(!output.exists());
    }

    #[test]
    fn nkit_containers_never_match_writable_formats() {
        use super::super::nkit::test_fixtures::{
            crc_of, make_fake_gc_fs_iso, make_nkit_gc, make_nkit_gcz,
        };

        let dir = tempfile::tempdir().unwrap();
        let iso = make_fake_gc_fs_iso();
        let nkit = make_nkit_gc(&iso);
        for (name, bytes, head_format) in [
            ("game.nkit.iso", nkit.clone(), None),
            (
                "game.nkit.gcz",
                make_nkit_gcz(&nkit, crc_of(&iso)),
                Some(DiscFormat::Gcz),
            ),
        ] {
            let input = dir.path().join(name);
            std::fs::write(&input, &bytes).unwrap();
            assert_eq!(detect_disc_format(&input).unwrap(), None);
            assert_eq!(disc_format_from_head(&bytes), head_format);
        }
    }

    #[tokio::test]
    async fn non_disc_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("not-disc.bin");
        let output = dir.path().join("out.iso");
        std::fs::write(&input, b"not a disc").unwrap();
        let error = convert_disc(
            &input,
            &output,
            DiscConvertOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RvzError::UnrecognizedDisc), "{error}");
        assert!(!output.exists());
    }
}
