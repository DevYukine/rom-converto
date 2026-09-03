//! PSN `.pkg` packages for PSP, PS3, and PS Vita: big-endian header and
//! plaintext metadata for [`read_info`], plus AES-128-CTR item extraction
//! in [`extract`].
//!
//! All three consoles are end-of-life and their package keys are long
//! published, so they are embedded here. Key selection and derivation
//! follow the `pkg2zip` lineage: PS3 packages always use the PS3 key,
//! PSP/Vita key type 1 uses the PSP key directly, and types 2 to 4 derive
//! the CTR key by AES-ECB encrypting the header's `pkg_data_iv` under the
//! matching Vita key.

use std::fs::File;
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use aes::Aes128;
use aes::cipher::{BlockCipherEncrypt, KeyInit, KeyIvInit, StreamCipher, StreamCipherSeek};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::info::{ContentKind, Image};
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
/// Cap on an `icon0.png` item read out of an untrusted package.
const MAX_ICON_BYTES: u64 = 4 << 20;

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

/// Console family a PSN `.pkg` targets.
///
/// Derived from the header's `pkg_type` field (1 for PS3, 2 for PSP or
/// Vita) and, for type 2, whether the metadata content type is one of the
/// PSX/PSP-style codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PkgPlatform {
    Ps3,
    Psp,
    #[default]
    Vita,
}

/// Metadata read from a `.pkg` header and its plaintext metadata block.
/// Every field here is readable without any key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PkgInfo {
    pub content_id: String,
    pub pkg_revision: u16,
    pub pkg_type: u16,
    /// Console family this package targets.
    #[serde(default)]
    pub platform: PkgPlatform,
    /// Raw metadata content type, as stored.
    pub content_type: u32,
    /// Human label for [`PkgInfo::content_type`], when the code is known.
    pub content_type_label: Option<String>,
    /// Normalized content category: from the `PARAM.SFO` `CATEGORY` when
    /// present, otherwise a best-effort guess from [`PkgInfo::content_type`].
    #[serde(default)]
    pub content_kind: Option<ContentKind>,
    /// `CATEGORY` from the package's plaintext `PARAM.SFO`.
    pub category: Option<String>,
    pub title: Option<String>,
    pub title_id: Option<String>,
    /// `icon0.png` (PSP/PS3 `ICON0.PNG` or Vita `sce_sys/icon0.png`),
    /// decrypted and decoded best-effort; `None` on any read or decode
    /// failure.
    #[serde(default)]
    pub icon: Option<Image>,
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
    let pkg_size = file.metadata()?.len();
    let header = read_header(&mut file, path)?;
    let meta = read_meta(&mut file, &header)?;
    let platform = classify_platform(header.pkg_type, meta.content_type);
    // Icon lookup needs whole references into `header`/`meta`, so it runs
    // before either is partially moved into the `PkgInfo` literal below.
    let icon = read_icon(&mut file, &header, &meta, platform, pkg_size);

    let mut info = PkgInfo {
        content_id: header.content_id,
        pkg_revision: header.revision,
        pkg_type: header.pkg_type,
        platform,
        content_type: meta.content_type,
        content_type_label: None,
        content_kind: None,
        category: None,
        title: None,
        title_id: None,
        icon,
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
    info.content_kind = content_kind(meta.content_type, info.category.as_deref());

    Ok(info)
}

/// Best-effort icon lookup: finds the item table entry whose name ends
/// with `icon0.png` (case-insensitively), decrypts it, and decodes it as a
/// PNG. Any failure along the way — an unsupported key type, a corrupt
/// item table, a truncated payload, or a non-PNG file — yields `None`
/// instead of failing the caller's read.
fn read_icon(
    file: &mut File,
    header: &Header,
    meta: &Meta,
    platform: PkgPlatform,
    pkg_size: u64,
) -> Option<Image> {
    let main_key = derive_key(header.key_type, &header.iv, platform).ok()?;
    let items = read_items(file, header, meta, &main_key, pkg_size, Some("icon0.png")).ok()?;
    let entry = items
        .into_iter()
        .find(|r| !r.item.is_dir && r.item.name.to_ascii_lowercase().ends_with("icon0.png"))?;
    if entry.item.data_size > MAX_ICON_BYTES {
        return None;
    }

    let mut buf = vec![0u8; entry.item.data_size as usize];
    file.seek(SeekFrom::Start(header.data_offset + entry.item.data_offset))
        .ok()?;
    file.read_exact(&mut buf).ok()?;
    ctr_at(&entry.key, &header.iv, entry.item.data_offset)
        .ok()?
        .apply_keystream(&mut buf);
    Image::from_png(buf)
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
    let platform = classify_platform(header.pkg_type, meta.content_type);

    let main_key = derive_key(header.key_type, &header.iv, platform)?;
    let raw = read_items(&mut file, &header, &meta, &main_key, pkg_size, None)?;

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

/// Streaming reader over one item's decrypted payload, seekable anywhere
/// inside it. Returned by [`open_item`].
pub struct PkgItemReader {
    file: File,
    /// Absolute offset of the item's payload in the package file.
    start: u64,
    /// The item's offset inside the encrypted region, which is also where
    /// its keystream starts.
    data_offset: u64,
    size: u64,
    key: [u8; 16],
    iv: [u8; 16],
    pos: u64,
}

/// Opens the single non-directory item whose name ends with `name_suffix`,
/// compared case-insensitively, in the `.pkg` at `path`.
///
/// # Errors
/// Returns an error if the package cannot be parsed, its key type is
/// unsupported, or no item matches `name_suffix`.
pub fn open_item(path: &Path, name_suffix: &str) -> Result<PkgItemReader> {
    let mut file =
        File::open(path).with_context(|| format!("vita pkg: open {}", path.display()))?;
    let pkg_size = file.metadata()?.len();
    let header = read_header(&mut file, path)?;
    let meta = read_meta(&mut file, &header)?;
    let platform = classify_platform(header.pkg_type, meta.content_type);
    let main_key = derive_key(header.key_type, &header.iv, platform)?;
    let raw = read_items(&mut file, &header, &meta, &main_key, pkg_size, None)?;

    let suffix = name_suffix.to_ascii_lowercase();
    let entry = raw
        .into_iter()
        .find(|r| !r.item.is_dir && r.item.name.to_ascii_lowercase().ends_with(&suffix))
        .ok_or_else(|| {
            let label = content_type_label(meta.content_type, None)
                .unwrap_or_else(|| format!("content type {}", meta.content_type));
            anyhow!(
                "vita pkg: {} ({label}) carries no {name_suffix}",
                header.content_id
            )
        })?;

    Ok(PkgItemReader {
        file,
        start: header.data_offset + entry.item.data_offset,
        data_offset: entry.item.data_offset,
        size: entry.item.data_size,
        key: entry.key,
        iv: header.iv,
        pos: 0,
    })
}

impl Read for PkgItemReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.size.saturating_sub(self.pos);
        let n = buf.len().min(left as usize);
        if n == 0 {
            return Ok(0);
        }
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        self.file.read_exact(&mut buf[..n])?;
        ctr_at(&self.key, &self.iv, self.data_offset + self.pos)
            .map_err(io::Error::other)?
            .apply_keystream(&mut buf[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for PkgItemReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
            SeekFrom::End(d) => i128::from(self.size) + i128::from(d),
        };
        self.pos = u64::try_from(next).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "vita pkg: seek outside the item",
            )
        })?;
        Ok(self.pos)
    }
}

fn read_header(file: &mut File, path: &Path) -> Result<Header> {
    let mut head = [0u8; HEADER_LEN];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut head)
        .with_context(|| format!("vita pkg: short header in {}", path.display()))?;

    if be_u32(&head, 0) != PKG_MAGIC {
        bail!("vita pkg: bad magic in {}", path.display());
    }

    // Bit 15 of pkg_revision marks a finalized (retail) package; a debug
    // build without it is not encrypted with the retail keys embedded here.
    let revision = be_u16(&head, 4);
    if revision & 0x8000 == 0 {
        bail!(
            "vita pkg: unsupported debug (non-finalized) pkg in {}",
            path.display()
        );
    }

    let content_id = {
        let raw = &head[0x30..0x30 + 0x24];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..end]).into_owned()
    };
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&head[0x70..0x80]);

    Ok(Header {
        revision,
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
    stop_at_suffix: Option<&str>,
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
        let name = String::from_utf8_lossy(&name).into_owned();

        let is_dir = flags == 4 || flags == 18;
        // A caller looking for one specific file (by name suffix) doesn't
        // need the rest of the table decrypted once it's found.
        let matched =
            !is_dir && stop_at_suffix.is_some_and(|suf| name.to_ascii_lowercase().ends_with(suf));

        items.push(RawItem {
            item: PkgItem {
                name,
                name_offset,
                name_size,
                data_offset,
                data_size,
                is_dir,
            },
            key,
        });
        if matched {
            break;
        }
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
/// PS3 packages always use the PS3 key, regardless of `key_type`.
fn derive_key(key_type: u8, iv: &[u8; 16], platform: PkgPlatform) -> Result<[u8; 16]> {
    if platform == PkgPlatform::Ps3 {
        return Ok(PKG_PS3_KEY);
    }
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

/// Classifies which console family a `.pkg` targets from the header's
/// `pkg_type` field: 1 is PS3; 2 with a PSX/PSP-style metadata content
/// type is PSP; anything else is Vita.
fn classify_platform(pkg_type: u16, content_type: u32) -> PkgPlatform {
    match pkg_type {
        1 => PkgPlatform::Ps3,
        2 if matches!(content_type, 6 | 7 | 0xE | 0xF | 0x10) => PkgPlatform::Psp,
        _ => PkgPlatform::Vita,
    }
}

/// Normalizes a package's `content_type`/`category` into the shared
/// [`ContentKind`] vocabulary. `category` takes priority when present;
/// otherwise falls back to well-known `content_type` codes.
fn content_kind(content_type: u32, category: Option<&str>) -> Option<ContentKind> {
    category
        .and_then(map_category)
        .or_else(|| map_content_type(content_type))
}

// Vita "gd" (Game) and PS3 "GD" (Update) differ only by case, so codes must
// never be case-normalized here.
fn map_category(cat: &str) -> Option<ContentKind> {
    match cat {
        "gd" | "gda" | "DG" | "HG" | "UG" | "MG" | "ME" => Some(ContentKind::Game),
        "gp" | "GD" => Some(ContentKind::Update),
        "ac" | "AC" => Some(ContentKind::Dlc),
        _ => None,
    }
}

fn map_content_type(content_type: u32) -> Option<ContentKind> {
    match content_type {
        0x16 => Some(ContentKind::Dlc),
        0x15 => Some(ContentKind::Game),
        6 | 7 | 0xE | 0xF | 0x10 => Some(ContentKind::Game),
        _ => None,
    }
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

    /// Builds a synthetic `.pkg` encrypted with the embedded key for
    /// `key_type`, laid out the way the real format is. `pkg_type` is the
    /// header field that [`classify_platform`] reads (1 for PS3, 2 for
    /// PSP/Vita).
    pub fn build_pkg(pkg_type: u16, key_type: u8, content_type: u32, entries: &[Entry]) -> Vec<u8> {
        build_pkg_with_category(pkg_type, key_type, content_type, Some("gd"), entries)
    }

    /// Same as [`build_pkg`], but lets the test choose the `PARAM.SFO`
    /// `CATEGORY` (or omit it), to exercise the `content_type` fallback in
    /// [`content_kind`].
    pub fn build_pkg_with_category(
        pkg_type: u16,
        key_type: u8,
        content_type: u32,
        category: Option<&'static str>,
        entries: &[Entry],
    ) -> Vec<u8> {
        let iv = [0x11u8; 16];
        let platform = classify_platform(pkg_type, content_type);
        let key = derive_key(key_type, &iv, platform).expect("derive key");

        let mut sfo_fields: Vec<(&str, Val)> = Vec::new();
        if let Some(cat) = category {
            sfo_fields.push(("CATEGORY", Val::Str(cat)));
        }
        sfo_fields.push(("TITLE", Val::Str("Synthetic")));
        sfo_fields.push(("TITLE_ID", Val::Str("PCSF00002")));
        let sfo = build_sfo(&sfo_fields);

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
        out[4..6].copy_from_slice(&0x8001u16.to_be_bytes()); // finalized bit set
        out[6..8].copy_from_slice(&pkg_type.to_be_bytes());
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
    use super::test_fixtures::{Entry, build_pkg, build_pkg_with_category};
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
        std::fs::write(&path, build_pkg(2, 3, 0x15, &entries())).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.content_id, "EP9000-PCSF00002_00-SYNTHETIC000000");
        assert_eq!(info.key_type, 3);
        assert_eq!(info.platform, PkgPlatform::Vita);
        assert_eq!(info.content_type, 0x15);
        assert_eq!(info.content_type_label.as_deref(), Some("Vita application"));
        assert_eq!(info.content_kind, Some(ContentKind::Game));
        assert_eq!(info.item_count, 3);
        assert_eq!(info.title.as_deref(), Some("Synthetic"));
        assert_eq!(info.title_id.as_deref(), Some("PCSF00002"));
        assert_eq!(info.category.as_deref(), Some("gd"));
        assert_eq!(info.meta_ids, vec![1, 2, 13, 14]);
        assert!(info.icon.is_none());
    }

    #[test]
    fn corrupted_magic_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.pkg");
        let mut bytes = build_pkg(2, 3, 0x15, &entries());
        bytes[0] = 0x00;
        std::fs::write(&path, bytes).unwrap();

        let err = read_info(&path).unwrap_err().to_string();
        assert!(err.contains("bad magic"), "{err}");
    }

    #[test]
    fn non_finalized_pkg_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("debug.pkg");
        let mut bytes = build_pkg(2, 3, 0x15, &entries());
        bytes[4..6].copy_from_slice(&1u16.to_be_bytes()); // clear the finalized bit
        std::fs::write(&path, bytes).unwrap();

        let err = read_info(&path).unwrap_err().to_string();
        assert!(err.contains("non-finalized"), "{err}");
    }

    #[test]
    fn extracts_items_for_every_vita_key_type() {
        for key_type in [2u8, 3, 4] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("app.pkg");
            let out = dir.path().join("out");
            let want = entries();
            std::fs::write(&path, build_pkg(2, key_type, 0x15, &want)).unwrap();

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
        std::fs::write(&path, build_pkg(2, 1, 7, &want)).unwrap();

        extract(&path, &out, &NoProgress).unwrap();
        assert_eq!(std::fs::read(out.join("eboot.bin")).unwrap(), want[2].data);
    }

    #[test]
    fn ps3_shaped_pkg_reports_ps3_platform_and_decrypts_with_the_ps3_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        let out = dir.path().join("out");
        let want = entries();
        std::fs::write(&path, build_pkg(1, 0, 0x01, &want)).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.platform, PkgPlatform::Ps3);

        extract(&path, &out, &NoProgress).unwrap();
        assert_eq!(
            std::fs::read(out.join("sce_sys/param.sfo")).unwrap(),
            want[1].data
        );
        assert_eq!(std::fs::read(out.join("eboot.bin")).unwrap(), want[2].data);
    }

    #[test]
    fn psp_shaped_pkg_reports_psp_platform_and_game_kind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        // Real PSP packages carry a PSP category ("MG", "ME", ...), never
        // the Vita "gd"; asserting Game here exercises the CATEGORY branch.
        std::fs::write(
            &path,
            build_pkg_with_category(2, 1, 6, Some("MG"), &entries()),
        )
        .unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.platform, PkgPlatform::Psp);
        assert_eq!(info.content_kind, Some(ContentKind::Game));
    }

    #[test]
    fn psp_shaped_pkg_without_category_falls_back_to_content_type() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        std::fs::write(&path, build_pkg_with_category(2, 1, 7, None, &entries())).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.platform, PkgPlatform::Psp);
        assert_eq!(info.category, None);
        assert_eq!(info.content_kind, Some(ContentKind::Game));
    }

    /// A real, minimal PNG so `Image::from_png` parses it as a valid image.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&[8, 6, 0, 0, 0]);
        out
    }

    #[test]
    fn icon_is_decoded_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.pkg");
        let mut entries = entries();
        entries.push(Entry {
            name: "sce_sys/icon0.png",
            data: png(64, 48),
            is_dir: false,
            psp_type: 0x90,
        });
        std::fs::write(&path, build_pkg(2, 3, 0x15, &entries)).unwrap();

        let info = read_info(&path).unwrap();
        let icon = info.icon.expect("icon0.png");
        assert_eq!((icon.width, icon.height), (64, 48));
    }

    #[test]
    fn psp_items_without_type_0x90_use_the_ps3_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        let out = dir.path().join("out");
        let want = entries_with(0x00);
        std::fs::write(&path, build_pkg(2, 1, 7, &want)).unwrap();

        let items = extract(&path, &out, &NoProgress).unwrap();
        assert_eq!(items[2].name, "eboot.bin");
        assert_eq!(std::fs::read(out.join("eboot.bin")).unwrap(), want[2].data);
    }

    #[test]
    fn item_reader_matches_the_extracted_bytes_at_any_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        let out = dir.path().join("out");
        let want = entries();
        std::fs::write(&path, build_pkg(2, 1, 7, &want)).unwrap();
        extract(&path, &out, &NoProgress).unwrap();
        let full = std::fs::read(out.join("eboot.bin")).unwrap();

        let mut reader = open_item(&path, "EBOOT.BIN").unwrap();
        for (start, len) in [(0usize, 16usize), (1, 4095), (4000, 1000), (4999, 1)] {
            reader.seek(SeekFrom::Start(start as u64)).unwrap();
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).unwrap();
            assert_eq!(buf, full[start..start + len], "at {start}+{len}");
        }

        reader.seek(SeekFrom::End(-8)).unwrap();
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, full[full.len() - 8..]);

        reader
            .seek(SeekFrom::Start(full.len() as u64 + 64))
            .unwrap();
        assert_eq!(reader.read(&mut [0u8; 32]).unwrap(), 0);

        assert!(reader.seek(SeekFrom::Start(0)).is_ok());
        assert!(reader.seek(SeekFrom::Current(-1)).is_err());
    }

    #[test]
    fn open_item_names_the_package_when_nothing_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.pkg");
        std::fs::write(&path, build_pkg(2, 1, 7, &entries())).unwrap();

        let err = open_item(&path, "EBOOT.PBP")
            .err()
            .expect("no EBOOT.PBP item")
            .to_string();
        assert!(err.contains("EP9000-PCSF00002_00-SYNTHETIC000000"), "{err}");
        assert!(err.contains("PSP game"), "{err}");
    }

    #[test]
    fn unsupported_key_type_errors() {
        assert!(derive_key(5, &[0u8; 16], PkgPlatform::Vita).is_err());
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
        let full = build_pkg(2, 3, 0x15, &entries());
        let dir = tempfile::tempdir().unwrap();
        for len in (0..full.len()).step_by(37) {
            let path = dir.path().join(format!("t{len}.pkg"));
            std::fs::write(&path, &full[..len]).unwrap();
            let _ = read_info(&path);
            let _ = extract(&path, &dir.path().join(format!("o{len}")), &NoProgress);
        }
    }
}
