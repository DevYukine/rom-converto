//! The `\x7FCNT` package container shared by PS4 and PS5 `.pkg` files.
//!
//! Everything read here is plaintext: the big-endian header, the entry
//! table, the entry name table and any entry payload not flagged
//! encrypted. A PS5 package may wrap its container in a `\x7FFIH` or
//! `\x7FLIH` image whose little-endian header carries the container's base
//! offset, and every offset inside the container is relative to that base.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::info::Image;
use crate::util::bytes::{cstr_ascii, u32_be, u64_be, u64_le};

const CNT_MAGIC: [u8; 4] = [0x7F, b'C', b'N', b'T'];
const FIH_MAGIC: [u8; 4] = [0x7F, b'F', b'I', b'H'];
const LIH_MAGIC: [u8; 4] = [0x7F, b'L', b'I', b'H'];

const HEADER_LEN: usize = 0x1000;
/// Shortest header that still holds every field the reader needs.
const MIN_HEADER_LEN: usize = 0x440;
/// Bytes covering the largest outer image header field (`\x7FFIH` @0x58).
const OUTER_HEADER_LEN: usize = 0x60;
const ENTRY_LEN: usize = 0x20;
/// Cap on the entry count, to bound a corrupt header's table read.
const MAX_ENTRY_COUNT: u32 = 0x10000;

/// Entry id of the name table.
const ENTRY_NAME_TABLE: u32 = 0x0200;
/// Entry id of the PS4 `param.sfo`.
pub const ENTRY_PARAM_SFO: u32 = 0x1000;
/// Entry id of `pic1.png`.
pub const ENTRY_PIC1: u32 = 0x1006;
/// Entry id of `icon0.png`.
pub const ENTRY_ICON0: u32 = 0x1200;
/// Entry id of `pic0.png`.
pub const ENTRY_PIC0: u32 = 0x1220;
/// Entry id of the PS5 `param.json`.
pub const ENTRY_PARAM_JSON: u32 = 0x2000;

/// Cap on the name table read out of an untrusted package.
const MAX_NAME_TABLE_BYTES: u64 = 1 << 20;
/// Cap on a `param.sfo` read out of an untrusted package.
pub(crate) const MAX_SFO_BYTES: u64 = 1 << 20;
/// Cap on a `param.json` read out of an untrusted package.
pub(crate) const MAX_JSON_BYTES: u64 = 4 << 20;
/// Cap on an artwork entry read out of an untrusted package.
const MAX_IMAGE_BYTES: u64 = 4 << 20;

const FLAG_FIRST_PATCH: u32 = 0x0010_0000;
const FLAG_SUBSEQUENT_PATCH: u32 = 0x4000_0000;
const FLAG_DELTA_PATCH: u32 = 0x4100_0000;
const FLAG_CUMULATIVE_PATCH: u32 = 0x6000_0000;

/// One record of a PS4/PS5 package entry table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct CntEntry {
    /// Entry id, e.g. 0x1000 for `param.sfo`, 0x1200 for `icon0.png`.
    pub id: u32,
    /// Name from the package name table, when the entry has one.
    pub name: Option<String>,
    /// Absolute offset of the entry's data in the file.
    pub offset: u64,
    pub size: u64,
    /// `flags1` bit 31.
    pub encrypted: bool,
    /// `flags2` bits 12..16.
    pub key_index: u8,
}

/// Which console family a `\x7FCNT`-style package targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CntPlatform {
    Ps4,
    Ps5,
}

/// Outer image a `\x7FCNT` container was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CntImage {
    /// Bare container, base 0.
    Cnt,
    /// Finalized `\x7FFIH` image with the container embedded.
    Fih,
    /// Older `\x7FLIH` image with the container embedded.
    Lih,
}

/// Where a container sits in its file, plus what the outer image header
/// says about it.
struct Location {
    image: CntImage,
    base: u64,
    signed: Option<bool>,
    /// PFS image offset and size, when the outer header carries them.
    pfs_image: Option<(u64, u64)>,
}

/// A parsed `\x7FCNT` container, holding the file open so entry payloads
/// can be read on demand.
pub(crate) struct Cnt {
    file: File,
    /// Outer image the container was found in.
    pub image: CntImage,
    /// `\x7FFIH`/`\x7FLIH` signed byte 0x80; `None` for a bare container.
    pub signed: Option<bool>,
    /// Header flags bit 31.
    pub finalized: bool,
    pub content_id: String,
    pub drm_type: u32,
    pub content_type: u32,
    pub content_flags: u32,
    /// BCD `yyyymmdd`.
    pub version_date: u32,
    /// Entry count as the header states it; `entries` can be shorter when
    /// a record points past the end of the file.
    pub entry_count: u32,
    pub entries: Vec<CntEntry>,
    /// Whether the raw table names a `param.json` record, even one that
    /// was dropped for pointing past the end of a truncated file.
    pub has_param_json: bool,
    /// From the outer image header when there is one, else from the
    /// container header.
    pub pfs_image_offset: u64,
    pub pfs_image_size: u64,
    pub package_size: u64,
    pub file_size: u64,
}

impl Cnt {
    /// Opens the package at `path` and parses its container header, entry
    /// table and name table.
    ///
    /// # Errors
    /// Returns an error if the file carries no CNT/FIH/LIH magic, its
    /// header is truncated, or its entry table is implausible or runs past
    /// the end of the file.
    pub fn open(path: &Path) -> Result<Cnt> {
        let mut file =
            File::open(path).with_context(|| format!("ps4/ps5 pkg: open {}", path.display()))?;
        let file_size = file.metadata()?.len();
        let loc = read_location(&mut file)?
            .ok_or_else(|| anyhow!("ps4/ps5 pkg: bad magic in {}", path.display()))?;

        let mut head = Vec::with_capacity(HEADER_LEN);
        file.seek(SeekFrom::Start(loc.base))?;
        file.by_ref()
            .take(HEADER_LEN as u64)
            .read_to_end(&mut head)?;
        if head.len() < MIN_HEADER_LEN || head[..4] != CNT_MAGIC {
            bail!(
                "ps4/ps5 pkg: short or malformed container in {}",
                path.display()
            );
        }

        let entry_count = u32_be(&head, 0x10);
        if entry_count > MAX_ENTRY_COUNT {
            bail!(
                "ps4/ps5 pkg: entry count {entry_count} exceeds cap {MAX_ENTRY_COUNT} in {}",
                path.display()
            );
        }
        let table_offset = loc.base.saturating_add(u64::from(u32_be(&head, 0x18)));
        let table_bytes = u64::from(entry_count) * ENTRY_LEN as u64;
        if table_offset
            .checked_add(table_bytes)
            .is_none_or(|end| end > file_size)
        {
            bail!(
                "ps4/ps5 pkg: entry table runs past end of {}",
                path.display()
            );
        }

        let mut table = vec![0u8; table_bytes as usize];
        file.seek(SeekFrom::Start(table_offset))?;
        file.read_exact(&mut table)?;

        let mut entries = Vec::with_capacity(entry_count as usize);
        // Names are resolved once the name table entry itself can be read,
        // so each surviving record's name offset is kept alongside.
        let mut name_offsets = Vec::with_capacity(entry_count as usize);
        let mut has_param_json = false;
        for record in table.as_chunks::<ENTRY_LEN>().0 {
            let id = u32_be(record, 0);
            has_param_json |= id == ENTRY_PARAM_JSON;
            let offset = loc.base.saturating_add(u64::from(u32_be(record, 0x10)));
            let size = u64::from(u32_be(record, 0x14));
            // A record pointing outside the file is dropped, not fatal:
            // the rest of the table is still worth reporting.
            if offset.checked_add(size).is_none_or(|end| end > file_size) {
                continue;
            }
            let flags2 = u32_be(record, 0xC);
            entries.push(CntEntry {
                id,
                name: None,
                offset,
                size,
                encrypted: u32_be(record, 8) & 0x8000_0000 != 0,
                key_index: ((flags2 & 0xF000) >> 12) as u8,
            });
            name_offsets.push(u32_be(record, 4));
        }

        let (pfs_image_offset, pfs_image_size) = loc
            .pfs_image
            .unwrap_or_else(|| (u64_be(&head, 0x410), u64_be(&head, 0x418)));
        let package_size = u64_be(&head, 0x430);

        let mut cnt = Cnt {
            file,
            image: loc.image,
            signed: loc.signed,
            finalized: u32_be(&head, 4) & 0x8000_0000 != 0,
            // The id is 0x24 bytes; the 0xC after it is padding that a
            // NUL-terminated read of the whole 0x30 slot would leak.
            content_id: cstr_ascii(&head[0x40..0x64]),
            drm_type: u32_be(&head, 0x70),
            content_type: u32_be(&head, 0x74),
            content_flags: u32_be(&head, 0x78),
            version_date: u32_be(&head, 0x80),
            entry_count,
            entries,
            has_param_json,
            pfs_image_offset,
            pfs_image_size,
            package_size,
            file_size,
        };
        cnt.resolve_names(&name_offsets);
        Ok(cnt)
    }

    /// Fills every entry's name from the package name table, falling back
    /// to the well-known name for the entry's id.
    fn resolve_names(&mut self, name_offsets: &[u32]) {
        let names = self
            .entries
            .iter()
            .find(|e| e.id == ENTRY_NAME_TABLE && !e.encrypted)
            .cloned()
            .and_then(|e| self.read_entry(&e, MAX_NAME_TABLE_BYTES))
            .unwrap_or_default();

        for (entry, &offset) in self.entries.iter_mut().zip(name_offsets) {
            // Offset 0 is the table's own leading terminator, so it means
            // "this entry has no name".
            entry.name = (offset != 0)
                .then(|| names.get(offset as usize..).map(cstr_ascii))
                .flatten()
                .filter(|n| !n.is_empty())
                .or_else(|| name_for_id(entry.id));
        }
    }

    /// Reads one entry's payload, or `None` when it is encrypted or larger
    /// than `max_bytes`.
    fn read_entry(&mut self, entry: &CntEntry, max_bytes: u64) -> Option<Vec<u8>> {
        if entry.encrypted || entry.size == 0 || entry.size > max_bytes {
            return None;
        }
        let mut buf = vec![0u8; entry.size as usize];
        self.file.seek(SeekFrom::Start(entry.offset)).ok()?;
        self.file.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    /// Reads the entry called `name` (compared case-insensitively), else
    /// the one with id `id`. `None` on any miss, encrypted entry, or read
    /// failure.
    pub fn read_named(&mut self, name: &str, id: u32, max_bytes: u64) -> Option<Vec<u8>> {
        let entry = self
            .entries
            .iter()
            .find(|e| {
                !e.encrypted
                    && e.name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .or_else(|| self.entries.iter().find(|e| e.id == id))
            .cloned()?;
        self.read_entry(&entry, max_bytes)
    }

    /// Reads the entry called `name`, else the one with id `id`, and
    /// decodes it as a PNG.
    pub fn read_image(&mut self, name: &str, id: u32) -> Option<Image> {
        Image::from_png(self.read_named(name, id, MAX_IMAGE_BYTES)?)
    }
}

/// Reads the outer image header, resolving the container's base offset.
/// `None` when the file carries none of the CNT, FIH or LIH magics.
fn read_location(file: &mut File) -> Result<Option<Location>> {
    let mut head = Vec::with_capacity(OUTER_HEADER_LEN);
    file.seek(SeekFrom::Start(0))?;
    file.by_ref()
        .take(OUTER_HEADER_LEN as u64)
        .read_to_end(&mut head)?;
    if head.len() < 4 {
        return Ok(None);
    }

    if head[..4] == CNT_MAGIC {
        return Ok(Some(Location {
            image: CntImage::Cnt,
            base: 0,
            signed: None,
            pfs_image: None,
        }));
    }
    let image = if head[..4] == FIH_MAGIC {
        CntImage::Fih
    } else if head[..4] == LIH_MAGIC {
        CntImage::Lih
    } else {
        return Ok(None);
    };
    if head.len() < OUTER_HEADER_LEN {
        bail!("ps4/ps5 pkg: short outer image header");
    }

    // LIH version 1 keeps the embedded container offset in its own slot;
    // every other image stores it where FIH does.
    let base_offset = match image {
        CntImage::Lih if u16::from_le_bytes([head[6], head[7]]) == 1 => 0x30,
        _ => 0x58,
    };

    Ok(Some(Location {
        image,
        base: u64_le(&head, base_offset),
        signed: Some(head[5] == 0x80),
        pfs_image: Some((u64_le(&head, 0x10), u64_le(&head, 0x18))),
    }))
}

/// Sniffs the `.pkg` at `path`, resolving an embedded container inside a
/// FIH or LIH image. Returns `None` when the file carries none of the CNT,
/// FIH or LIH magics.
///
/// # Errors
/// Returns an error if the file cannot be opened, or carries one of the
/// magics but no parseable container.
pub fn detect(path: &Path) -> Result<Option<CntPlatform>> {
    let mut file =
        File::open(path).with_context(|| format!("ps4/ps5 pkg: open {}", path.display()))?;
    if read_location(&mut file)?.is_none() {
        return Ok(None);
    }

    let cnt = Cnt::open(path)?;
    // A bare container is PS4 unless it carries the PS5 `param.json` or
    // the PS5 content type; the FIH and LIH images only wrap PS5 packages.
    let ps5 = cnt.image != CntImage::Cnt || cnt.has_param_json || cnt.content_type == 0x20;
    Ok(Some(if ps5 {
        CntPlatform::Ps5
    } else {
        CntPlatform::Ps4
    }))
}

/// Human label for a container `content_type`, when the code is known.
pub(crate) fn content_type_label(content_type: u32) -> Option<String> {
    let label = match content_type {
        0x1A => "PS4 game data",
        0x1B => "PS4 additional content",
        0x1C => "PS4 additional content (no data)",
        0x1E => "PS4 delta patch",
        0x20 => "PS5 game data",
        _ => return None,
    };
    Some(label.to_string())
}

/// Decodes `content_flags` into bit names, in ascending bit order.
pub(crate) fn content_flag_labels(flags: u32) -> Vec<String> {
    const BITS: &[(u32, &str)] = &[
        (0x0002_0000, "GD_BASE"),
        (FLAG_FIRST_PATCH, "FIRST_PATCH"),
        (0x0020_0000, "PATCHGO"),
        (0x0040_0000, "REMASTER"),
        (0x0080_0000, "PS_CLOUD"),
        (0x0200_0000, "GD_AC"),
        (0x0400_0000, "NON_GAME"),
    ];

    let mut out: Vec<String> = BITS
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| (*name).to_string())
        .collect();
    // Both combos include SUBSEQUENT_PATCH's bit, so a package matching
    // one is labelled by the combo alone.
    let top = if flags & FLAG_DELTA_PATCH == FLAG_DELTA_PATCH {
        Some("DELTA_PATCH")
    } else if flags & FLAG_CUMULATIVE_PATCH == FLAG_CUMULATIVE_PATCH {
        Some("CUMULATIVE_PATCH")
    } else if flags & FLAG_SUBSEQUENT_PATCH != 0 {
        Some("SUBSEQUENT_PATCH")
    } else {
        None
    };
    out.extend(top.map(str::to_string));
    out
}

/// True when `content_flags` marks the package as a patch.
pub(crate) fn is_patch(flags: u32) -> bool {
    flags & (FLAG_FIRST_PATCH | FLAG_SUBSEQUENT_PATCH) != 0
}

/// Well-known file name for an entry id, for the entries the name table
/// does not cover.
fn name_for_id(id: u32) -> Option<String> {
    match id {
        0x1201..=0x121F => return Some(format!("icon0_{:02}.png", id - 0x1201)),
        0x1241..=0x125F => return Some(format!("pic1_{:02}.png", id - 0x1241)),
        0x1261..=0x127F => return Some(format!("changeinfo/changeinfo_{:02}.xml", id - 0x1261)),
        _ => {}
    }
    let name = match id {
        0x0001 => "digests",
        0x0010 => "entry_keys",
        0x0020 => "image_key",
        0x0080 => "general_digests",
        0x0100 => "metas",
        ENTRY_NAME_TABLE => "entry_names",
        0x0400 => "license.dat",
        0x0401 => "license.info",
        0x0402 => "nptitle.dat",
        0x0403 => "npbind.dat",
        0x0404 => "selfinfo.dat",
        0x0406 => "imageinfo.dat",
        0x0407 => "target-deltainfo.dat",
        0x0408 => "origin-deltainfo.dat",
        0x0409 => "psreserved.dat",
        ENTRY_PARAM_SFO => "param.sfo",
        0x1001 => "playgo-chunk.dat",
        0x1002 => "playgo-chunk.sha",
        0x1003 => "playgo-manifest.xml",
        0x1004 => "pronunciation.xml",
        0x1005 => "pronunciation.sig",
        ENTRY_PIC1 => "pic1.png",
        0x1007 => "pubtoolinfo.dat",
        0x1008 => "app/playgo-chunk.dat",
        0x1009 => "app/playgo-chunk.sha",
        0x100A => "app/playgo-manifest.xml",
        0x100B => "shareparam.json",
        0x100C => "shareoverlayimage.png",
        0x100D => "save_data.png",
        0x100E => "shareprivacyguardimage.png",
        ENTRY_ICON0 => "icon0.png",
        ENTRY_PIC0 => "pic0.png",
        0x1240 => "snd0.at9",
        0x1260 => "changeinfo/changeinfo.xml",
        0x1280 => "icon0.dds",
        0x12A0 => "pic0.dds",
        0x12C0 => "pic1.dds",
        ENTRY_PARAM_JSON => "param.json",
        0x2010 => "playgo_hash_table",
        0x2011 => "playgo_ficm",
        _ => return None,
    };
    Some(name.to_string())
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;

    /// One entry to place in a synthetic container.
    pub struct Entry {
        pub id: u32,
        /// `None` leaves the entry out of the name table, so the reader has
        /// to fall back to the id map.
        pub name: Option<&'static str>,
        pub data: Vec<u8>,
        pub encrypted: bool,
    }

    /// PFS image offset a synthetic container header reports.
    pub const CNT_PFS_OFFSET: u64 = 0x8000;
    /// PFS image size a synthetic container header reports.
    pub const CNT_PFS_SIZE: u64 = 0x4000;
    /// Offset a [`wrap_fih`] image places the container at.
    pub const FIH_CNT_BASE: u64 = 0x1000;

    /// Builds a `\x7FCNT` container holding `entries` plus the name table
    /// entry (id 0x200) that names them.
    pub fn build_cnt(content_type: u32, content_flags: u32, entries: &[Entry]) -> Vec<u8> {
        // A leading NUL keeps the first real name off offset 0, which the
        // format reserves for "no name".
        let mut names = vec![0u8];
        let mut name_offsets = Vec::new();
        for e in entries {
            match e.name {
                Some(n) => {
                    name_offsets.push(names.len() as u32);
                    names.extend_from_slice(n.as_bytes());
                    names.push(0);
                }
                None => name_offsets.push(0),
            }
        }

        let mut records: Vec<(u32, u32, &[u8], bool)> =
            vec![(ENTRY_NAME_TABLE, 0, names.as_slice(), false)];
        for (e, offset) in entries.iter().zip(&name_offsets) {
            records.push((e.id, *offset, e.data.as_slice(), e.encrypted));
        }

        let table_offset = HEADER_LEN as u64;
        let payload_base = table_offset + (records.len() * ENTRY_LEN) as u64;
        let mut table = Vec::new();
        let mut blob = Vec::new();
        for (id, name_offset, data, encrypted) in &records {
            let offset = payload_base + blob.len() as u64;
            table.extend_from_slice(&id.to_be_bytes());
            table.extend_from_slice(&name_offset.to_be_bytes());
            table.extend_from_slice(&if *encrypted { 0x8000_0000u32 } else { 0 }.to_be_bytes());
            table.extend_from_slice(&0u32.to_be_bytes());
            table.extend_from_slice(&(offset as u32).to_be_bytes());
            table.extend_from_slice(&(data.len() as u32).to_be_bytes());
            table.extend_from_slice(&0u64.to_be_bytes());
            blob.extend_from_slice(data);
        }

        let mut out = vec![0u8; HEADER_LEN];
        out[0..4].copy_from_slice(&CNT_MAGIC);
        out[4..8].copy_from_slice(&0x8000_0001u32.to_be_bytes());
        out[0x10..0x14].copy_from_slice(&(records.len() as u32).to_be_bytes());
        out[0x18..0x1C].copy_from_slice(&(table_offset as u32).to_be_bytes());
        let content_id = b"UP9000-CUSA00001_00-SYNTHETICPS4000";
        out[0x40..0x40 + content_id.len()].copy_from_slice(content_id);
        out[0x70..0x74].copy_from_slice(&0xFu32.to_be_bytes());
        out[0x74..0x78].copy_from_slice(&content_type.to_be_bytes());
        out[0x78..0x7C].copy_from_slice(&content_flags.to_be_bytes());
        out[0x80..0x84].copy_from_slice(&0x2023_0115u32.to_be_bytes());
        out[0x404..0x408].copy_from_slice(&1u32.to_be_bytes());
        out[0x410..0x418].copy_from_slice(&CNT_PFS_OFFSET.to_be_bytes());
        out[0x418..0x420].copy_from_slice(&CNT_PFS_SIZE.to_be_bytes());
        out.extend_from_slice(&table);
        out.extend_from_slice(&blob);
        out
    }

    /// Wraps a container in a `\x7FFIH` image, the way a finalized PS5
    /// package ships it.
    /// Wraps `cnt` in a `\x7FLIH` image of `version`: version 1 stores the
    /// container offset at 0x30, anything else where FIH does (0x58).
    pub fn wrap_lih(cnt: &[u8], version: u16) -> Vec<u8> {
        let mut out = vec![0u8; FIH_CNT_BASE as usize];
        out[0..4].copy_from_slice(&LIH_MAGIC);
        out[6..8].copy_from_slice(&version.to_le_bytes());
        let slot = if version == 1 { 0x30 } else { 0x58 };
        out[slot..slot + 8].copy_from_slice(&FIH_CNT_BASE.to_le_bytes());
        out.extend_from_slice(cnt);
        out
    }

    pub fn wrap_fih(cnt: &[u8], signed: u8, pfs_offset: u64, pfs_size: u64) -> Vec<u8> {
        let mut out = vec![0u8; FIH_CNT_BASE as usize];
        out[0..4].copy_from_slice(&FIH_MAGIC);
        out[5] = signed;
        out[6..8].copy_from_slice(&3u16.to_le_bytes());
        out[0x10..0x18].copy_from_slice(&pfs_offset.to_le_bytes());
        out[0x18..0x20].copy_from_slice(&pfs_size.to_le_bytes());
        out[0x58..0x60].copy_from_slice(&FIH_CNT_BASE.to_le_bytes());
        out.extend_from_slice(cnt);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{Entry, build_cnt, wrap_fih};
    use super::*;

    fn entry(id: u32, name: Option<&'static str>, data: Vec<u8>) -> Entry {
        Entry {
            id,
            name,
            data,
            encrypted: false,
        }
    }

    #[test]
    fn detects_ps4_ps5_and_foreign_magics() {
        let dir = tempfile::tempdir().unwrap();

        let ps4 = dir.path().join("ps4.pkg");
        std::fs::write(
            &ps4,
            build_cnt(
                0x1A,
                0,
                &[entry(ENTRY_PARAM_SFO, Some("param.sfo"), vec![0; 8])],
            ),
        )
        .unwrap();
        assert_eq!(detect(&ps4).unwrap(), Some(CntPlatform::Ps4));

        let ps5 = dir.path().join("ps5.pkg");
        let cnt = build_cnt(
            0x20,
            0,
            &[entry(ENTRY_PARAM_JSON, Some("param.json"), b"{}".to_vec())],
        );
        std::fs::write(&ps5, &cnt).unwrap();
        assert_eq!(detect(&ps5).unwrap(), Some(CntPlatform::Ps5));

        let fih = dir.path().join("fih.pkg");
        std::fs::write(&fih, wrap_fih(&cnt, 0x80, 0x2000, 0x1000)).unwrap();
        assert_eq!(detect(&fih).unwrap(), Some(CntPlatform::Ps5));

        let other = dir.path().join("legacy.pkg");
        std::fs::write(&other, b"\x7FPKG and then some padding bytes").unwrap();
        assert_eq!(detect(&other).unwrap(), None);
    }

    #[test]
    fn truncated_header_and_bad_entry_count_error_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let full = build_cnt(0x1A, 0, &[entry(ENTRY_PARAM_SFO, None, vec![0; 8])]);

        let short = dir.path().join("short.pkg");
        std::fs::write(&short, &full[..0x100]).unwrap();
        assert!(Cnt::open(&short).is_err());

        let huge = dir.path().join("huge.pkg");
        let mut bytes = full.clone();
        bytes[0x10..0x14].copy_from_slice(&0x00FF_FFFFu32.to_be_bytes());
        std::fs::write(&huge, bytes).unwrap();
        let err = Cnt::open(&huge)
            .err()
            .expect("capped entry count")
            .to_string();
        assert!(err.contains("exceeds cap"), "{err}");

        let past_eof = dir.path().join("past.pkg");
        let mut bytes = full;
        bytes[0x10..0x14].copy_from_slice(&0x1000u32.to_be_bytes());
        std::fs::write(&past_eof, bytes).unwrap();
        let err = Cnt::open(&past_eof)
            .err()
            .expect("table past end of file")
            .to_string();
        assert!(err.contains("past end of"), "{err}");
    }

    #[test]
    fn combo_flags_replace_the_bits_they_cover() {
        assert_eq!(content_flag_labels(0x4100_0000), vec!["DELTA_PATCH"]);
        assert_eq!(content_flag_labels(0x6000_0000), vec!["CUMULATIVE_PATCH"]);
        assert_eq!(content_flag_labels(0x4000_0000), vec!["SUBSEQUENT_PATCH"]);
        assert_eq!(
            content_flag_labels(0x0010_0000 | 0x0400_0000),
            vec!["FIRST_PATCH", "NON_GAME"]
        );
    }
}
