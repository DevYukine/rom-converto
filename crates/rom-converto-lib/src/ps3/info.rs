//! PS3 disc `info`: region-table stats plus metadata from the plaintext
//! `PS3_DISC.SFB` and `PARAM.SFO`. Missing or malformed metadata leaves
//! those fields `None` without failing the read.

use std::io::{BufReader, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::info::{ContentKind, Image};
use crate::ps3::embedded_keys::embedded_key;
use crate::ps3::error::{Ps3Error, Ps3Result};
use crate::ps3::fs::read_plain_files;
use crate::ps3::key::{self, Ps3Key};
use crate::ps3::region::{Region, SECTOR_SIZE, parse_region_table};
use crate::ps3::sfb::Sfb;
use crate::util::sfo::Sfo;

/// Summary of a PS3 disc's region table and plaintext `PARAM.SFO`/
/// `PS3_DISC.SFB` metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ps3Info {
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub region: Option<String>,
    pub version: Option<String>,
    pub app_ver: Option<String>,
    pub resolution: Option<String>,
    pub sound_format: Option<String>,
    pub firmware: Option<String>,
    pub parental_level: Option<u32>,
    /// The raw `PARAM.SFO` `CATEGORY` code, kept verbatim even when undecoded.
    #[serde(default)]
    pub category: Option<String>,
    /// Normalized category, derived from [`Ps3Info::category`].
    #[serde(default)]
    pub content_kind: Option<ContentKind>,
    pub region_count: usize,
    pub total_sectors: u32,
    pub encrypted_sectors: u64,
    /// Whether the disc's encrypted regions still hold ciphertext, or
    /// `None` when no verdict is possible: no disc key was available, or
    /// the probe couldn't decide. The region table survives decryption,
    /// so `encrypted_sectors > 0` alone doesn't mean encrypted.
    #[serde(default)]
    pub encrypted: Option<bool>,
    pub size_bytes: u64,
    /// `PS3_GAME/ICON0.PNG` when present.
    #[serde(default)]
    pub icon: Option<Image>,
    /// ISO9660 root directory listing, dot entries excluded.
    #[serde(default)]
    pub root_files: Vec<Ps3RootEntry>,
}

/// One entry from a PS3 disc's ISO9660 root directory listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ps3RootEntry {
    pub name: String,
    pub size: u32,
    pub is_dir: bool,
}

/// Reads a PS3 disc's region-table stats and plaintext metadata; no
/// disc key is needed since it never touches encrypted sectors.
///
/// # Errors
/// Returns an error if the sector 0 region table cannot be parsed.
pub fn read_ps3_info(path: &Path) -> Ps3Result<Ps3Info> {
    let size_bytes = std::fs::metadata(path)?.len();
    let mut file = std::fs::File::open(path)?;

    let mut sector0 = [0u8; SECTOR_SIZE];
    file.read_exact(&mut sector0)?;
    let (regions, total_sectors) = parse_region_table(&sector0)?;
    let encrypted_sectors = regions
        .iter()
        .filter(|r| !r.plain)
        .map(|r| (r.last - r.start + 1) as u64)
        .sum();

    let mut info = Ps3Info {
        region_count: regions.len(),
        total_sectors,
        encrypted_sectors,
        size_bytes,
        ..Default::default()
    };

    let mut reader = BufReader::new(file);
    if let Ok(files) = read_plain_files(&mut reader) {
        if let Some(sfo) = files.param_sfo.as_deref().and_then(|b| Sfo::parse(b).ok()) {
            info.title = sfo.get_str("TITLE").map(str::to_string);
            info.title_id = sfo.get_str("TITLE_ID").map(str::to_string);
            info.version = sfo.get_str("VERSION").map(str::to_string);
            info.app_ver = sfo.get_str("APP_VER").map(str::to_string);
            info.firmware = sfo.get_str("PS3_SYSTEM_VER").map(str::to_string);
            info.parental_level = sfo.get_u32("PARENTAL_LEVEL");
            info.resolution = sfo.get_u32("RESOLUTION").and_then(decode_resolution);
            info.sound_format = sfo.get_u32("SOUND_FORMAT").and_then(decode_sound_format);
            info.category = sfo.get_str("CATEGORY").map(str::to_string);
            info.content_kind = info
                .category
                .as_deref()
                .and_then(content_kind_from_category);
        }
        if let Some(sfb) = files.disc_sfb.as_deref().and_then(|b| Sfb::parse(b).ok()) {
            // The SFB carries the authoritative physical-disc VERSION.
            if let Some(v) = sfb.get("VERSION") {
                info.version = Some(v.to_string());
            }
            if info.title_id.is_none() {
                info.title_id = sfb.get("TITLE_ID").map(str::to_string);
            }
        }
        info.icon = files.icon0.and_then(Image::from_png);
        info.root_files = files
            .root_entries
            .into_iter()
            .map(|(name, size, is_dir)| Ps3RootEntry { name, size, is_dir })
            .collect();
    }

    info.region = info
        .title_id
        .as_deref()
        .and_then(region_from_title_id)
        .map(str::to_string);

    // Needs the TITLE_ID the block above reads, so it can't run earlier.
    let disc_key =
        info.title_id.as_deref().and_then(embedded_key).or_else(|| {
            key::resolve_sibling_key_path(path).and_then(|p| key::load_key_file(&p).ok())
        });
    info.encrypted = probe_encrypted(path, &regions, disc_key.as_ref());
    Ok(info)
}

/// Whether the encrypted regions still hold ciphertext, as far as the
/// decrypt path's sample probe can tell: `None` when there is no key to
/// probe with, or when every sample stayed high-diversity after
/// decryption (a wrong key and a decrypted disc look alike there).
pub(super) fn probe_encrypted(
    path: &Path,
    regions: &[Region],
    key: Option<&Ps3Key>,
) -> Option<bool> {
    if !regions.iter().any(|r| !r.plain) {
        return Some(false);
    }
    match super::probe_key_against_samples(path, regions, key?) {
        Ok(()) => Some(true),
        Err(Ps3Error::AlreadyDecrypted) => Some(false),
        Err(_) => None,
    }
}

/// Maps a PS3 `PARAM.SFO` `CATEGORY` code to the shared content vocabulary.
fn content_kind_from_category(category: &str) -> Option<ContentKind> {
    match category {
        "DG" | "HG" => Some(ContentKind::Game),
        "GD" => Some(ContentKind::Update),
        "AC" => Some(ContentKind::Dlc),
        _ => None,
    }
}

fn region_from_title_id(id: &str) -> Option<&'static str> {
    let b = id.as_bytes();
    if b.len() < 4 || (&b[0..2] != b"BL" && &b[0..2] != b"BC") {
        return None;
    }
    match b[2] {
        b'E' => Some("Europe"),
        b'U' => Some("USA"),
        b'J' => Some("Japan"),
        b'A' | b'K' => Some("Asia"),
        _ => None,
    }
}

fn decode_flags(bits: u32, flags: &[(u32, &str)]) -> Option<String> {
    let parts: Vec<&str> = flags
        .iter()
        .filter(|(b, _)| bits & b != 0)
        .map(|(_, l)| *l)
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn decode_resolution(bits: u32) -> Option<String> {
    decode_flags(
        bits,
        &[(0x01, "480"), (0x02, "576"), (0x04, "720"), (0x08, "1080")],
    )
}

fn decode_sound_format(bits: u32) -> Option<String> {
    decode_flags(
        bits,
        &[
            (0x01, "LPCM 2.0"),
            (0x04, "LPCM 5.1"),
            (0x10, "LPCM 7.1"),
            (0x100, "Dolby Digital 5.1"),
            (0x200, "DTS 5.1"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ps3::fs::tests::{TINY_PNG, build_ps3_iso_with_icon};
    use crate::util::sfo::test_fixtures::{Val, build_sfo};

    #[test]
    fn read_info_returns_category_and_content_kind() {
        let sfo = build_sfo(&[("CATEGORY", Val::Str("DG"))]);
        let mut image = build_ps3_iso_with_icon(b"sfb-bytes", &sfo, &[]);
        let total_sectors = (image.len() / SECTOR_SIZE) as u32;
        image[0..4].copy_from_slice(&1u32.to_be_bytes());
        image[0x0C..0x10].copy_from_slice(&(total_sectors - 1).to_be_bytes());

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("game.iso");
        std::fs::write(&path, &image).expect("write fixture");

        let info = read_ps3_info(&path).expect("read_ps3_info");
        assert_eq!(info.category.as_deref(), Some("DG"));
        assert_eq!(info.content_kind, Some(ContentKind::Game));
    }

    #[test]
    fn read_info_returns_icon_and_root_files() {
        let mut image = build_ps3_iso_with_icon(b"sfb-bytes", b"sfo-bytes", TINY_PNG);
        // Overwrite sector 0 (unused by fs.rs) with a one-region, all-plain
        // region table so `read_ps3_info` accepts the image.
        let total_sectors = (image.len() / SECTOR_SIZE) as u32;
        image[0..4].copy_from_slice(&1u32.to_be_bytes());
        image[0x0C..0x10].copy_from_slice(&(total_sectors - 1).to_be_bytes());

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("game.iso");
        std::fs::write(&path, &image).expect("write fixture");

        let info = read_ps3_info(&path).expect("read_ps3_info");
        assert!(info.icon.is_some());
        assert!(info.root_files.iter().any(|e| e.name == "PS3_GAME"));
        // No encrypted regions at all, so no key is needed to say so.
        assert_eq!(info.encrypted, Some(false));
    }

    #[test]
    fn region_mapping() {
        assert_eq!(region_from_title_id("BLES02247"), Some("Europe"));
        assert_eq!(region_from_title_id("BCES00001"), Some("Europe"));
        assert_eq!(region_from_title_id("BLUS30000"), Some("USA"));
        assert_eq!(region_from_title_id("BCUS98111"), Some("USA"));
        assert_eq!(region_from_title_id("BLJS10001"), Some("Japan"));
        assert_eq!(region_from_title_id("BLJM60001"), Some("Japan"));
        assert_eq!(region_from_title_id("BLAS50001"), Some("Asia"));
        assert_eq!(region_from_title_id("BCKS10001"), Some("Asia"));
        assert_eq!(region_from_title_id("MRTC00001"), None);
        assert_eq!(region_from_title_id("BL"), None);
    }

    #[test]
    fn region_from_title_id_handles_multibyte_prefix_without_panicking() {
        // "€a" is 4 bytes (E2 82 AC 61) but only 2 chars, so a naive str
        // slice at byte index 2 would land mid-codepoint and panic.
        assert_eq!(region_from_title_id("\u{20AC}a"), None);
        assert_eq!(region_from_title_id("BLES02247"), Some("Europe"));
        assert_eq!(region_from_title_id("BLUS12345"), Some("USA"));
        assert_eq!(region_from_title_id("BLJS00001"), Some("Japan"));
    }

    #[test]
    fn resolution_and_sound_decode() {
        assert_eq!(
            decode_resolution(0x0F).as_deref(),
            Some("480, 576, 720, 1080")
        );
        assert_eq!(decode_resolution(0x08).as_deref(), Some("1080"));
        assert_eq!(decode_resolution(0), None);
        assert_eq!(decode_sound_format(0x01).as_deref(), Some("LPCM 2.0"));
        assert_eq!(
            decode_sound_format(0x101).as_deref(),
            Some("LPCM 2.0, Dolby Digital 5.1")
        );
    }
}
