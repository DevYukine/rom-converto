//! PS Vita `.pkg` packages: big-endian header and plaintext metadata for
//! [`read_info`], plus AES-128-CTR item extraction in [`extract`].
//!
//! The Vita is end-of-life and its package keys are long published, so they
//! are embedded here. Key selection and derivation follow the `pkg2zip`
//! lineage: type 1 uses the PSP key directly, types 2 to 4 derive the CTR
//! key by AES-ECB encrypting the header's `pkg_data_iv` under the matching
//! Vita key.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use aes::Aes128;
use aes::cipher::{BlockCipherEncrypt, KeyInit, KeyIvInit, StreamCipher, StreamCipherSeek};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::util::ProgressReporter;
use crate::util::sfo::Sfo;

type Aes128Ctr = ctr::Ctr128BE<Aes128>;

const PKG_MAGIC: u32 = 0x7F50_4B47;
/// The header plus its extended block; the key index lives at 0xE7.
const HEADER_LEN: usize = 0x100;
const ITEM_LEN: u64 = 32;
const CHUNK: usize = 1 << 20;

/// Cap on the plaintext `PARAM.SFO` pulled from the metadata region.
const MAX_SFO_BYTES: u32 = 1 << 20;
/// Cap on a single item name, which the format keeps short.
const MAX_NAME_BYTES: u32 = 4096;
/// Cap on the metadata entry count, to bound a corrupt header's loop.
const MAX_META_COUNT: u32 = 4096;

const PKG_PS3_KEY: [u8; 16] = [
    0x2e, 0x7b, 0x71, 0xd7, 0xc9, 0xc9, 0xa1, 0x4e, 0xa3, 0x22, 0x1f, 0x18, 0x88, 0x28, 0xb8, 0xf8,
];
const PKG_PSP_KEY: [u8; 16] = [
    0x07, 0xf2, 0xc6, 0x82, 0x90, 0xb5, 0x0d, 0x2c, 0x33, 0x81, 0x8d, 0x70, 0x9b, 0x60, 0xe6, 0x2b,
];
const PKG_VITA_2: [u8; 16] = [
    0xe3, 0x1a, 0x70, 0xc9, 0xce, 0x1d, 0xd7, 0x2b, 0xf3, 0xc0, 0x62, 0x29, 0x63, 0xf2, 0xec, 0xcb,
];
const PKG_VITA_3: [u8; 16] = [
    0x42, 0x3a, 0xca, 0x3a, 0x2b, 0xd5, 0x64, 0x9f, 0x96, 0x86, 0xab, 0xad, 0x6f, 0xd8, 0x80, 0x1f,
];
const PKG_VITA_4: [u8; 16] = [
    0xaf, 0x07, 0xfd, 0x59, 0x65, 0x25, 0x27, 0xba, 0xf1, 0x33, 0x89, 0x66, 0x8b, 0x17, 0xd9, 0xea,
];

/// Metadata read from a `.pkg` header and its plaintext metadata block.
/// Every field here is readable without any key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PkgInfo {
    pub content_id: String,
    pub pkg_revision: u16,
    pub pkg_type: u16,
    /// Raw metadata content type, as stored.
    pub content_type: u32,
    /// Human label for [`PkgInfo::content_type`], when the code is known.
    pub content_type_label: Option<String>,
    /// `CATEGORY` from the package's plaintext `PARAM.SFO`.
    pub category: Option<String>,
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub item_count: u32,
    pub total_size: u64,
    pub data_offset: u64,
    pub data_size: u64,
    /// Key index from header byte 0xE7, masked to three bits.
    pub key_type: u8,
    pub drm_type: Option<u32>,
    pub package_flags: Option<u32>,
    /// Every metadata entry id seen, in file order.
    pub meta_ids: Vec<u32>,
}

/// One entry of the package's item table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgItem {
    pub name: String,
    pub name_offset: u64,
    pub name_size: u32,
    /// Offset of the item's data, relative to the encrypted data region.
    pub data_offset: u64,
    pub data_size: u64,
    pub is_dir: bool,
}

/// Fields parsed straight out of the fixed header.
struct Header {
    revision: u16,
    pkg_type: u16,
    meta_offset: u64,
    meta_count: u32,
    item_count: u32,
    total_size: u64,
    data_offset: u64,
    data_size: u64,
    content_id: String,
    iv: [u8; 16],
    key_type: u8,
}

/// Metadata entries the extractor and the info reader care about.
#[derive(Default)]
struct Meta {
    content_type: u32,
    drm_type: Option<u32>,
    package_flags: Option<u32>,
    items_offset: u64,
    sfo_offset: u64,
    sfo_size: u32,
    ids: Vec<u32>,
}

/// Reads the header and plaintext metadata of the `.pkg` file at `path`.
pub fn read_info(path: &Path) -> Result<PkgInfo> {
    let mut file =
        File::open(path).with_context(|| format!("vita pkg info: open {}", path.display()))?;
    let header = read_header(&mut file, path)?;
    let meta = read_meta(&mut file, &header)?;

    let mut info = PkgInfo {
        content_id: header.content_id,
        pkg_revision: header.revision,
        pkg_type: header.pkg_type,
        content_type: meta.content_type,
        content_type_label: None,
        category: None,
        title: None,
        title_id: None,
        item_count: header.item_count,
        total_size: header.total_size,
        data_offset: header.data_offset,
        data_size: header.data_size,
        key_type: header.key_type,
        drm_type: meta.drm_type,
        package_flags: meta.package_flags,
        meta_ids: meta.ids,
    };

    // The metadata PARAM.SFO sits outside the encrypted region, so it reads
    // without a key.
    if meta.sfo_offset != 0 && meta.sfo_size != 0 && meta.sfo_size <= MAX_SFO_BYTES {
        let mut buf = vec![0u8; meta.sfo_size as usize];
        if file.seek(SeekFrom::Start(meta.sfo_offset)).is_ok()
            && file.read_exact(&mut buf).is_ok()
            && let Ok(sfo) = Sfo::parse(&buf)
        {
            info.category = sfo.get_str("CATEGORY").map(str::to_string);
            info.title = sfo.get_str("TITLE").map(str::to_string);
            info.title_id = sfo.get_str("TITLE_ID").map(str::to_string);
        }
    }
    info.content_type_label = content_type_label(meta.content_type, info.category.as_deref());

    Ok(info)
}

/// Decrypts the item table of the `.pkg` at `path` and writes every file
/// item under `out_dir`, reporting bytes written through `progress`.
/// Returns the item table as parsed.
pub fn extract(
    path: &Path,
    out_dir: &Path,
    progress: &dyn ProgressReporter,
) -> Result<Vec<PkgItem>> {
    let mut file =
        File::open(path).with_context(|| format!("vita pkg extract: open {}", path.display()))?;
    let pkg_size = file.metadata()?.len();
    let header = read_header(&mut file, path)?;
    let meta = read_meta(&mut file, &header)?;

    let main_key = derive_key(header.key_type, &header.iv)?;
    let raw = read_items(&mut file, &header, &meta, &main_key, pkg_size)?;

    let total: u64 = raw
        .iter()
        .filter(|r| !r.item.is_dir)
        .fold(0u64, |acc, r| acc.saturating_add(r.item.data_size));
    progress.start(total, "vita pkg: extract");

    let mut buf = vec![0u8; CHUNK];
    for entry in &raw {
        let item = &entry.item;
        let dest = safe_join(out_dir, &item.name)?;
        if item.is_dir {
            std::fs::create_dir_all(&dest)
                .with_context(|| format!("vita pkg extract: mkdir {}", dest.display()))?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("vita pkg extract: mkdir {}", parent.display()))?;
        }

        let mut cipher = ctr_at(&entry.key, &header.iv, item.data_offset)?;
        let mut out = File::create(&dest)
            .with_context(|| format!("vita pkg extract: create {}", dest.display()))?;
        file.seek(SeekFrom::Start(header.data_offset + item.data_offset))?;

        let mut left = item.data_size;
        while left > 0 {
            let n = left.min(CHUNK as u64) as usize;
            file.read_exact(&mut buf[..n])?;
            cipher.apply_keystream(&mut buf[..n]);
            out.write_all(&buf[..n])?;
            left -= n as u64;
            progress.inc(n as u64);
        }
    }
    progress.finish();

    Ok(raw.into_iter().map(|r| r.item).collect())
}

fn read_header(file: &mut File, path: &Path) -> Result<Header> {
    let mut head = [0u8; HEADER_LEN];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut head)
        .with_context(|| format!("vita pkg: short header in {}", path.display()))?;

    if be_u32(&head, 0) != PKG_MAGIC {
        bail!("vita pkg: bad magic in {}", path.display());
    }

    let content_id = {
        let raw = &head[0x30..0x30 + 0x24];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..end]).into_owned()
    };
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&head[0x70..0x80]);

    Ok(Header {
        revision: be_u16(&head, 4),
        pkg_type: be_u16(&head, 6),
        meta_offset: u64::from(be_u32(&head, 8)),
        meta_count: be_u32(&head, 12),
        item_count: be_u32(&head, 20),
        total_size: be_u64(&head, 24),
        data_offset: be_u64(&head, 32),
        data_size: be_u64(&head, 40),
        content_id,
        iv,
        key_type: head[0xE7] & 7,
    })
}

fn read_meta(file: &mut File, header: &Header) -> Result<Meta> {
    let mut meta = Meta::default();
    let mut offset = header.meta_offset;
    for _ in 0..header.meta_count.min(MAX_META_COUNT) {
        let mut block = [0u8; 16];
        if file.seek(SeekFrom::Start(offset)).is_err() || file.read_exact(&mut block).is_err() {
            break;
        }
        let id = be_u32(&block, 0);
        let size = be_u32(&block, 4);
        meta.ids.push(id);
        match id {
            1 => meta.drm_type = Some(be_u32(&block, 8)),
            2 => meta.content_type = be_u32(&block, 8),
            3 => meta.package_flags = Some(be_u32(&block, 8)),
            13 => meta.items_offset = u64::from(be_u32(&block, 8)),
            14 => {
                meta.sfo_offset = u64::from(be_u32(&block, 8));
                meta.sfo_size = be_u32(&block, 12);
            }
            _ => {}
        }
        offset = offset.saturating_add(8).saturating_add(u64::from(size));
    }
    Ok(meta)
}

/// An item table entry plus the key its name and payload are encrypted under.
struct RawItem {
    item: PkgItem,
    key: [u8; 16],
}

fn read_items(
    file: &mut File,
    header: &Header,
    meta: &Meta,
    main_key: &[u8; 16],
    pkg_size: u64,
) -> Result<Vec<RawItem>> {
    let table_bytes = u64::from(header.item_count)
        .checked_mul(ITEM_LEN)
        .ok_or_else(|| anyhow!("vita pkg: item count overflows"))?;
    let table_end = header
        .data_offset
        .checked_add(meta.items_offset)
        .and_then(|v| v.checked_add(table_bytes));
    if table_end.is_none_or(|end| end > pkg_size) {
        bail!("vita pkg: item table runs past end of file");
    }

    let mut table = vec![0u8; table_bytes as usize];
    file.seek(SeekFrom::Start(header.data_offset + meta.items_offset))?;
    file.read_exact(&mut table)?;
    ctr_at(main_key, &header.iv, meta.items_offset)?.apply_keystream(&mut table);

    // PSX and PSP packages key each item by its psp_type byte; Vita ones
    // always use the main key.
    let psp_style = matches!(meta.content_type, 6 | 7 | 0xE | 0xF | 0x10);

    let mut items = Vec::with_capacity(header.item_count as usize);
    for i in 0..header.item_count as usize {
        let e = &table[i * 32..i * 32 + 32];
        let name_offset = u64::from(be_u32(e, 0));
        let name_size = be_u32(e, 4);
        let data_offset = be_u64(e, 8);
        let data_size = be_u64(e, 16);
        let psp_type = e[24];
        let flags = e[27];

        if name_size > MAX_NAME_BYTES {
            bail!("vita pkg: item {i} has an implausible name length");
        }
        let name_end = header
            .data_offset
            .checked_add(name_offset)
            .and_then(|v| v.checked_add(u64::from(name_size)));
        let data_end = header
            .data_offset
            .checked_add(data_offset)
            .and_then(|v| v.checked_add(data_size));
        if name_end.is_none_or(|end| end > pkg_size) || data_end.is_none_or(|end| end > pkg_size) {
            bail!("vita pkg: item {i} runs past end of file");
        }

        let key = if psp_style && psp_type != 0x90 {
            PKG_PS3_KEY
        } else {
            *main_key
        };

        let mut name = vec![0u8; name_size as usize];
        file.seek(SeekFrom::Start(header.data_offset + name_offset))?;
        file.read_exact(&mut name)?;
        ctr_at(&key, &header.iv, name_offset)?.apply_keystream(&mut name);

        items.push(RawItem {
            item: PkgItem {
                name: String::from_utf8_lossy(&name).into_owned(),
                name_offset,
                name_size,
                data_offset,
                data_size,
                is_dir: flags == 4 || flags == 18,
            },
            key,
        });
    }
    Ok(items)
}

/// Builds an AES-128-CTR stream positioned at `offset` bytes into the
/// package's encrypted data region.
fn ctr_at(key: &[u8; 16], iv: &[u8; 16], offset: u64) -> Result<Aes128Ctr> {
    let mut cipher = Aes128Ctr::new(key.into(), iv.into());
    cipher
        .try_seek(offset)
        .map_err(|e| anyhow!("vita pkg: cannot seek keystream to {offset}: {e}"))?;
    Ok(cipher)
}

/// Resolves the CTR key for `key_type` from the embedded package keys.
fn derive_key(key_type: u8, iv: &[u8; 16]) -> Result<[u8; 16]> {
    let vita_key = match key_type {
        1 => return Ok(PKG_PSP_KEY),
        2 => PKG_VITA_2,
        3 => PKG_VITA_3,
        4 => PKG_VITA_4,
        other => bail!("vita pkg: unsupported key type {other}"),
    };
    let mut block = *iv;
    Aes128::new(&vita_key.into()).encrypt_block((&mut block).into());
    Ok(block)
}

/// Joins an item name onto `root`, rejecting absolute paths and any
/// component that would escape the output directory.
fn safe_join(root: &Path, name: &str) -> Result<PathBuf> {
    let rel = Path::new(name.trim_start_matches('/'));
    let mut out = root.to_path_buf();
    for comp in rel.components() {
        match comp {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => bail!("vita pkg: refusing unsafe item path {name:?}"),
        }
    }
    if out == root {
        bail!("vita pkg: item has an empty path");
    }
    Ok(out)
}

fn content_type_label(content_type: u32, category: Option<&str>) -> Option<String> {
    let label = match content_type {
        6 => "PSX game",
        7 => "PSP game",
        0xE => "PSP-Go game",
        0xF => "PSP-Mini game",
        0x10 => "PSP-NeoGeo game",
        0x15 => match category {
            Some("gp") => "Vita patch",
            _ => "Vita application",
        },
        0x16 => "Vita additional content",
        0x18 | 0x1D => "Vita PSM application",
        _ => return None,
    };
    Some(label.to_string())
}

fn be_u16(d: &[u8], off: usize) -> u16 {
    u16::from_be_bytes(d[off..off + 2].try_into().expect("2-byte slice"))
}

fn be_u32(d: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(d[off..off + 4].try_into().expect("4-byte slice"))
}

fn be_u64(d: &[u8], off: usize) -> u64 {
    u64::from_be_bytes(d[off..off + 8].try_into().expect("8-byte slice"))
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;
    use crate::util::sfo::test_fixtures::{Val, build_sfo};

    pub struct Entry {
        pub name: &'static str,
        pub data: Vec<u8>,
        pub is_dir: bool,
        /// 0x90 keeps a PSX/PSP item on the main key; anything else moves it
        /// to the PS3 key. Ignored for Vita content types.
        pub psp_type: u8,
    }

    /// Builds a synthetic Vita `.pkg` encrypted with the embedded key for
    /// `key_type`, laid out the way the real format is.
    pub fn build_pkg(key_type: u8, content_type: u32, entries: &[Entry]) -> Vec<u8> {
        let iv = [0x11u8; 16];
        let key = derive_key(key_type, &iv).expect("derive key");

        let sfo = build_sfo(&[
            ("CATEGORY", Val::Str("gd")),
            ("TITLE", Val::Str("Synthetic")),
            ("TITLE_ID", Val::Str("PCSF00002")),
        ]);

        // Plaintext region: header, metadata entries, then PARAM.SFO.
        let meta_offset = HEADER_LEN as u64;
        let meta_entries: [(u32, u32, u32); 4] = [
            (1, 0, 0),
            (2, content_type, 0),
            (13, 0, 0),
            (14, 0, sfo.len() as u32),
        ];
        let meta_len = meta_entries.len() * 16;
        let sfo_offset = meta_offset + meta_len as u64;
        let data_offset = align16(sfo_offset + sfo.len() as u64);

        // Encrypted region: item table, then names, then payloads.
        let psp_style = matches!(content_type, 6 | 7 | 0xE | 0xF | 0x10);
        let items_offset = 0u64;
        let table_len = (entries.len() as u64) * ITEM_LEN;
        let mut cursor = align16(items_offset + table_len);

        // (offset, plaintext, key) for every region outside the item table.
        let mut regions: Vec<(u64, Vec<u8>, [u8; 16])> = Vec::new();
        let mut table = Vec::new();
        for e in entries {
            let item_key = if psp_style && e.psp_type != 0x90 {
                PKG_PS3_KEY
            } else {
                key
            };

            let name_offset = cursor;
            regions.push((name_offset, e.name.as_bytes().to_vec(), item_key));
            cursor = align16(cursor + e.name.len() as u64);

            let data_offset_rel = cursor;
            if !e.is_dir {
                regions.push((data_offset_rel, e.data.clone(), item_key));
                cursor = align16(cursor + e.data.len() as u64);
            }

            table.extend_from_slice(&(name_offset as u32).to_be_bytes());
            table.extend_from_slice(&(e.name.len() as u32).to_be_bytes());
            table.extend_from_slice(&data_offset_rel.to_be_bytes());
            let size = if e.is_dir { 0 } else { e.data.len() as u64 };
            table.extend_from_slice(&size.to_be_bytes());
            table.extend_from_slice(&[e.psp_type, 0, 0, if e.is_dir { 4 } else { 3 }]);
            table.extend_from_slice(&[0, 0, 0, 0]);
        }

        // Each region is its own keystream run positioned at its own offset,
        // the way the reader decrypts them.
        let enc_len = cursor;
        let mut enc = vec![0u8; enc_len as usize];
        ctr_at(&key, &iv, items_offset)
            .expect("ctr")
            .apply_keystream(&mut table);
        enc[items_offset as usize..(items_offset + table_len) as usize].copy_from_slice(&table);
        for (off, bytes, region_key) in &mut regions {
            ctr_at(region_key, &iv, *off)
                .expect("ctr")
                .apply_keystream(bytes);
            enc[*off as usize..*off as usize + bytes.len()].copy_from_slice(bytes);
        }

        let total_size = data_offset + enc_len;
        let mut out = vec![0u8; HEADER_LEN];
        out[0..4].copy_from_slice(&PKG_MAGIC.to_be_bytes());
        out[4..6].copy_from_slice(&1u16.to_be_bytes());
        out[6..8].copy_from_slice(&1u16.to_be_bytes());
        out[8..12].copy_from_slice(&(meta_offset as u32).to_be_bytes());
        out[12..16].copy_from_slice(&(meta_entries.len() as u32).to_be_bytes());
        out[20..24].copy_from_slice(&(entries.len() as u32).to_be_bytes());
        out[24..32].copy_from_slice(&total_size.to_be_bytes());
        out[32..40].copy_from_slice(&data_offset.to_be_bytes());
        out[40..48].copy_from_slice(&enc_len.to_be_bytes());
        let content_id = b"EP9000-PCSF00002_00-SYNTHETIC000000";
        out[0x30..0x30 + content_id.len()].copy_from_slice(content_id);
        out[0x70..0x80].copy_from_slice(&iv);
        out[0xE7] = key_type;

        for (id, value, extra) in meta_entries {
            out.extend_from_slice(&id.to_be_bytes());
            out.extend_from_slice(&8u32.to_be_bytes());
            let payload = match id {
                13 => items_offset as u32,
                14 => sfo_offset as u32,
                _ => value,
            };
            out.extend_from_slice(&payload.to_be_bytes());
            let second = match id {
                13 => table_len as u32,
                14 => extra,
                _ => 0,
            };
            out.extend_from_slice(&second.to_be_bytes());
        }
        out.extend_from_slice(&sfo);
        out.resize(data_offset as usize, 0);
        out.extend_from_slice(&enc);
        out
    }

    fn align16(v: u64) -> u64 {
        v.div_ceil(16) * 16
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{Entry, build_pkg};
    use super::*;
    use crate::util::NoProgress;

    fn entries_with(psp_type: u8) -> Vec<Entry> {
        vec![
            Entry {
                name: "sce_sys",
                data: Vec::new(),
                is_dir: true,
                psp_type,
            },
            Entry {
                name: "sce_sys/param.sfo",
                data: vec![0xAB; 300],
                is_dir: false,
                psp_type,
            },
            Entry {
                name: "eboot.bin",
                data: (0..5000u32).map(|i| i as u8).collect(),
                is_dir: false,
                psp_type,
            },
        ]
    }

    fn entries() -> Vec<Entry> {
        entries_with(0x90)
    }

    #[test]
    fn reads_header_and_plaintext_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.pkg");
        std::fs::write(&path, build_pkg(3, 0x15, &entries())).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.content_id, "EP9000-PCSF00002_00-SYNTHETIC000000");
        assert_eq!(info.key_type, 3);
        assert_eq!(info.content_type, 0x15);
        assert_eq!(info.content_type_label.as_deref(), Some("Vita application"));
        assert_eq!(info.item_count, 3);
        assert_eq!(info.title.as_deref(), Some("Synthetic"));
        assert_eq!(info.title_id.as_deref(), Some("PCSF00002"));
        assert_eq!(info.category.as_deref(), Some("gd"));
        assert_eq!(info.meta_ids, vec![1, 2, 13, 14]);
    }

    #[test]
    fn corrupted_magic_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.pkg");
        let mut bytes = build_pkg(3, 0x15, &entries());
        bytes[0] = 0x00;
        std::fs::write(&path, bytes).unwrap();

        let err = read_info(&path).unwrap_err().to_string();
        assert!(err.contains("bad magic"), "{err}");
    }

    #[test]
    fn extracts_items_for_every_vita_key_type() {
        for key_type in [2u8, 3, 4] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("app.pkg");
            let out = dir.path().join("out");
            let want = entries();
            std::fs::write(&path, build_pkg(key_type, 0x15, &want)).unwrap();

            let items = extract(&path, &out, &NoProgress).unwrap();
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].name, "sce_sys");
            assert!(items[0].is_dir);

            assert!(out.join("sce_sys").is_dir());
            assert_eq!(
                std::fs::read(out.join("sce_sys/param.sfo")).unwrap(),
                want[1].data
            );
            assert_eq!(std::fs::read(out.join("eboot.bin")).unwrap(), want[2].data);
        }
    }

    #[test]
    fn psp_key_type_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        let out = dir.path().join("out");
        let want = entries();
        std::fs::write(&path, build_pkg(1, 7, &want)).unwrap();

        extract(&path, &out, &NoProgress).unwrap();
        assert_eq!(std::fs::read(out.join("eboot.bin")).unwrap(), want[2].data);
    }

    #[test]
    fn psp_items_without_type_0x90_use_the_ps3_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        let out = dir.path().join("out");
        let want = entries_with(0x00);
        std::fs::write(&path, build_pkg(1, 7, &want)).unwrap();

        let items = extract(&path, &out, &NoProgress).unwrap();
        assert_eq!(items[2].name, "eboot.bin");
        assert_eq!(std::fs::read(out.join("eboot.bin")).unwrap(), want[2].data);
    }

    #[test]
    fn unsupported_key_type_errors() {
        assert!(derive_key(5, &[0u8; 16]).is_err());
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let root = Path::new("/tmp/out");
        assert!(safe_join(root, "../escape").is_err());
        assert!(safe_join(root, "a/../../escape").is_err());
        assert_eq!(
            safe_join(root, "sce_sys/param.sfo").unwrap(),
            root.join("sce_sys").join("param.sfo")
        );
    }

    #[test]
    fn truncated_pkg_errors_without_panic() {
        let full = build_pkg(3, 0x15, &entries());
        let dir = tempfile::tempdir().unwrap();
        for len in (0..full.len()).step_by(37) {
            let path = dir.path().join(format!("t{len}.pkg"));
            std::fs::write(&path, &full[..len]).unwrap();
            let _ = read_info(&path);
            let _ = extract(&path, &dir.path().join(format!("o{len}")), &NoProgress);
        }
    }
}
