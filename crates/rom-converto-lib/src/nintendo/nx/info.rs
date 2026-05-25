//! `info` extractor for Nintendo Switch containers.
//!
//! This file owns the keyless path: container kind, file listing,
//! ticket summaries, XCI partition layout. Later tasks layer the
//! key-protected pieces (CNMT, NACP, icon) onto [`NxFullInfo`] when a
//! `prod.keys` file resolves; if it does not, this module still returns
//! useful data so the user knows what is in the file.

use crate::info::Image;
use crate::nintendo::nx::container::{
    ContainerKind, ContainerListing, list_container, read_xci_hfs0_offset,
};
use crate::nintendo::nx::keys::{KeySet, load_keyset};
use crate::nintendo::nx::models::cnmt::{
    CNMT_TYPE_ADD_ON_CONTENT, CNMT_TYPE_APPLICATION, CNMT_TYPE_DELTA, CNMT_TYPE_PATCH,
    CNMT_TYPE_SYSTEM_DATA, CNMT_TYPE_SYSTEM_PROGRAM, CNMT_TYPE_SYSTEM_UPDATE, Cnmt,
};
use crate::nintendo::nx::models::hfs0::Hfs0;
use crate::nintendo::nx::models::nacp::{Nacp, NacpLanguage};
use crate::nintendo::nx::models::nca::{CONTENT_TYPE_CONTROL, CONTENT_TYPE_META};
use crate::nintendo::nx::models::pfs0::Pfs0;
use crate::nintendo::nx::models::ticket::Ticket;
use crate::nintendo::nx::romfs::RomfsReader;
use crate::nintendo::nx::walker::NcaWalker;
use crate::util::pread::file_read_exact_at;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

/// Switch-side payload for [`crate::info::InfoResult::Nx`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NxInfo {
    pub container_kind: NxContainerKind,
    pub is_compressed: bool,
    pub distribution: NxDistribution,
    pub structure: NxStructure,
    pub physical_bytes: u64,
    pub files: Vec<ContainerFileSummary>,
    pub nca_names: Vec<String>,
    pub cnmt_nca_names: Vec<String>,
    pub tickets: Vec<TicketSummary>,
    /// Present for XCI / XCZ inputs only.
    pub xci_partitions: Option<Vec<XciPartitionSummary>>,
    /// Filled by later tasks when prod.keys resolves.
    pub full: Option<NxFullInfo>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NxContainerKind {
    #[default]
    Nsp,
    Nsz,
    Xci,
    Xcz,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NxDistribution {
    #[default]
    Digital,
    Cartridge,
}

impl NxDistribution {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Digital => "Digital",
            Self::Cartridge => "Cartridge",
        }
    }
}

/// Heuristic classification of how the container was assembled,
/// derived from which sidecar files sit next to the NCAs (scene
/// dumps ship `.cert`, CDN exports ship `.xml`, etc.).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NxStructure {
    #[default]
    Unknown,
    Scene,
    Converted,
    Cdn,
    Homebrew,
}

impl NxStructure {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Scene => "Scene",
            Self::Converted => "Converted",
            Self::Cdn => "CDN",
            Self::Homebrew => "Homebrew",
        }
    }
}

impl From<ContainerKind> for NxContainerKind {
    fn from(k: ContainerKind) -> Self {
        match k {
            ContainerKind::Nsp => Self::Nsp,
            ContainerKind::Nsz => Self::Nsz,
            ContainerKind::Xci => Self::Xci,
            ContainerKind::Xcz => Self::Xcz,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerFileSummary {
    pub partition: Option<String>,
    pub name: String,
    pub abs_offset: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketSummary {
    pub file_name: String,
    pub rights_id: String,
    pub master_key_revision: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XciPartitionSummary {
    pub name: String,
    pub file_count: usize,
    pub total_size: u64,
}

/// CNMT / NACP / icon fields populated when `prod.keys` is available.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NxFullInfo {
    pub application_title_id: u64,
    pub title_version: u32,
    pub title_kind: CnmtTitleKind,
    pub storage_id: u8,
    pub attributes: u8,
    pub required_system_version: u64,
    pub required_application_version: Option<u64>,
    pub base_application_id: Option<u64>,
    pub content_count: u16,
    pub total_content_size: u64,
    pub contents: Vec<CnmtContentSummary>,
    /// Sibling CNMTs found in the same container.
    pub related_titles: Vec<RelatedTitleSummary>,
    /// NACP fields and icon, present when a Control NCA was decryptable.
    pub control: Option<NxControl>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NxControl {
    pub titles: Vec<NxNacpTitle>,
    pub display_version: String,
    pub startup_user_account: u8,
    pub startup_user_account_name: String,
    pub screenshot: u8,
    pub video_capture: u8,
    pub video_capture_name: String,
    pub attribute_flag: u32,
    pub attributes: Vec<String>,
    pub supported_language_bitmask: u32,
    pub supported_languages: Vec<String>,
    pub parental_control_flag: u32,
    pub parental_control_flags: Vec<String>,
    pub user_account_save: i64,
    pub user_account_save_journal: i64,
    pub device_save: i64,
    pub device_save_journal: i64,
    pub bcat_save: i64,
    pub rating_age: Vec<i8>,
    pub age_ratings: Vec<AgeRatingEntry>,
    pub addon_install_policy: u8,
    pub addon_install_policy_name: String,
    pub screen_orientation: u8,
    pub screen_orientation_name: String,
    pub icon: Option<Image>,
    pub icon_language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgeRatingEntry {
    pub organization: String,
    pub age: i8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NxNacpTitle {
    pub language: String,
    pub name: String,
    pub publisher: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CnmtTitleKind {
    #[default]
    Unknown,
    Application,
    Patch,
    AddOnContent,
    Delta,
    SystemProgram,
    SystemData,
    SystemUpdate,
}

impl CnmtTitleKind {
    fn from_byte(b: u8) -> Self {
        match b {
            CNMT_TYPE_APPLICATION => Self::Application,
            CNMT_TYPE_PATCH => Self::Patch,
            CNMT_TYPE_ADD_ON_CONTENT => Self::AddOnContent,
            CNMT_TYPE_DELTA => Self::Delta,
            CNMT_TYPE_SYSTEM_PROGRAM => Self::SystemProgram,
            CNMT_TYPE_SYSTEM_DATA => Self::SystemData,
            CNMT_TYPE_SYSTEM_UPDATE => Self::SystemUpdate,
            _ => Self::Unknown,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Application => "Game",
            Self::Patch => "Update",
            Self::AddOnContent => "DLC",
            Self::Delta => "Delta",
            Self::SystemProgram => "System Program",
            Self::SystemData => "System Data",
            Self::SystemUpdate => "System Update",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CnmtContentSummary {
    pub content_id: String,
    pub content_type: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedTitleSummary {
    pub title_id: u64,
    pub kind: CnmtTitleKind,
    pub version: u32,
}

/// Read a Switch container and return its info. If `keys_path` (or the
/// default `prod.keys` lookup) resolves, [`NxInfo::full`] carries the
/// CNMT slice on top of the keyless container summary.
pub fn read_info(path: &Path, keys_path: Option<&Path>) -> Result<NxInfo> {
    let physical_bytes = std::fs::metadata(path)
        .with_context(|| format!("nx info: stat {}", path.display()))?
        .len();

    let listing: ContainerListing =
        list_container(path).with_context(|| format!("nx info: list {}", path.display()))?;
    let container_kind = NxContainerKind::from(listing.kind);

    let files: Vec<ContainerFileSummary> = listing
        .entries
        .iter()
        .map(|e| ContainerFileSummary {
            partition: e.partition.map(|s| s.to_string()),
            name: e.name.clone(),
            abs_offset: e.abs_offset,
            size: e.size,
        })
        .collect();

    let nca_names: Vec<String> = listing
        .entries
        .iter()
        .filter(|e| is_nca_entry(&e.name))
        .map(|e| e.name.clone())
        .collect();

    let cnmt_nca_names: Vec<String> = listing
        .entries
        .iter()
        .filter(|e| is_cnmt_nca_entry(&e.name))
        .map(|e| e.name.clone())
        .collect();

    let tickets = read_tickets(path, &listing).unwrap_or_default();

    let xci_partitions = if listing.kind.is_xci() {
        Some(read_xci_partition_layout(path)?)
    } else {
        None
    };

    let full = try_full_info(path, &listing, keys_path).unwrap_or_else(|e| {
        log::debug!("nx info: full extraction skipped ({})", e);
        None
    });

    let distribution = match container_kind {
        NxContainerKind::Xci | NxContainerKind::Xcz => NxDistribution::Cartridge,
        _ => NxDistribution::Digital,
    };
    let structure = classify_structure(&listing);

    Ok(NxInfo {
        container_kind,
        is_compressed: matches!(container_kind, NxContainerKind::Nsz | NxContainerKind::Xcz),
        distribution,
        structure,
        physical_bytes,
        files,
        nca_names,
        cnmt_nca_names,
        tickets,
        xci_partitions,
        full,
    })
}

fn classify_structure(listing: &ContainerListing) -> NxStructure {
    let mut has_tik = false;
    let mut has_cert = false;
    let mut has_xml = false;
    for entry in &listing.entries {
        let lower = entry.name.to_ascii_lowercase();
        if lower.ends_with(".tik") {
            has_tik = true;
        }
        if lower.ends_with(".cert") {
            has_cert = true;
        }
        if lower.ends_with(".xml") {
            has_xml = true;
        }
    }
    if !has_tik {
        return NxStructure::Homebrew;
    }
    if has_xml {
        return NxStructure::Cdn;
    }
    if has_cert {
        return NxStructure::Scene;
    }
    NxStructure::Converted
}

fn try_full_info(
    path: &Path,
    listing: &ContainerListing,
    keys_path: Option<&Path>,
) -> Result<Option<NxFullInfo>> {
    let mut keys = load_keyset(keys_path).with_context(|| "nx info: load prod.keys")?;
    merge_inline_tickets(path, listing, &mut keys);

    let file = Arc::new(File::open(path)?);
    let mut cnmts: Vec<Cnmt> = Vec::new();
    for entry in &listing.entries {
        if !entry.name.to_ascii_lowercase().ends_with(".cnmt.nca") {
            continue;
        }
        match read_meta_cnmt(file.clone(), entry.abs_offset, entry.size, &keys) {
            Ok(c) => cnmts.push(c),
            Err(e) => log::debug!("nx info: skipping {} ({})", entry.name, e),
        }
    }
    if cnmts.is_empty() {
        return Ok(None);
    }

    let primary_idx = pick_primary_cnmt(&cnmts);
    let primary = &cnmts[primary_idx];

    let control = try_read_control(path, listing, &keys).unwrap_or_else(|e| {
        log::debug!("nx info: control NCA read skipped ({})", e);
        None
    });

    let total_content_size = primary.contents.iter().map(|c| c.size).sum();
    let contents: Vec<CnmtContentSummary> = primary
        .contents
        .iter()
        .map(|c| CnmtContentSummary {
            content_id: hex::encode(c.content_id),
            content_type: cnmt_content_type_name(c.content_type).to_string(),
            size: c.size,
        })
        .collect();

    let related_titles: Vec<RelatedTitleSummary> = cnmts
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != primary_idx)
        .map(|(_, c)| RelatedTitleSummary {
            title_id: c.title_id,
            kind: CnmtTitleKind::from_byte(c.content_type),
            version: c.version,
        })
        .collect();

    Ok(Some(NxFullInfo {
        application_title_id: primary.title_id,
        title_version: primary.version,
        title_kind: CnmtTitleKind::from_byte(primary.content_type),
        storage_id: primary.storage_id,
        attributes: primary.attributes,
        required_system_version: primary.required_system_version(),
        required_application_version: primary.required_application_version(),
        base_application_id: primary.base_application_id(),
        content_count: primary.content_count,
        total_content_size,
        contents,
        related_titles,
        control,
    }))
}

fn try_read_control(
    path: &Path,
    listing: &ContainerListing,
    keys: &KeySet,
) -> Result<Option<NxControl>> {
    let file = Arc::new(File::open(path)?);
    for entry in &listing.entries {
        let name = entry.name.as_str();
        if !is_nca_entry(name) || is_cnmt_nca_entry(name) {
            continue;
        }
        let (walker_file, walker_off, walker_size): (
            Arc<dyn crate::nintendo::nx::walker::NcaInput>,
            u64,
            u64,
        ) = if is_ncz_entry(name) {
            match crate::nintendo::nx::ncz::NczReader::open(
                file.clone(),
                entry.abs_offset,
                entry.size,
            ) {
                Ok(reader) => {
                    let nca_size = reader.decompressed_nca_size();
                    (Arc::new(reader), 0u64, nca_size)
                }
                Err(e) => {
                    log::debug!("nx info: open ncz {} ({})", name, e);
                    continue;
                }
            }
        } else {
            (file.clone(), entry.abs_offset, entry.size)
        };
        let walker = match NcaWalker::open(walker_file, walker_off, walker_size, keys) {
            Ok(w) => w,
            Err(e) => {
                log::debug!("nx info: open {} ({})", name, e);
                continue;
            }
        };
        if walker.header.content_type != CONTENT_TYPE_CONTROL {
            continue;
        }
        match read_control_payload(&walker) {
            Ok(c) => return Ok(Some(c)),
            Err(e) => log::debug!("nx info: control parse failed for {} ({})", name, e),
        }
    }
    Ok(None)
}

fn is_nca_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".nca") || lower.ends_with(".ncz")
}

fn is_cnmt_nca_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".cnmt.nca") || lower.ends_with(".cnmt.ncz")
}

fn is_ncz_entry(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".ncz")
}

fn read_control_payload(walker: &NcaWalker) -> Result<NxControl> {
    let section = walker
        .sections
        .first()
        .ok_or_else(|| anyhow::anyhow!("control NCA has no sections"))?;

    // The IVFC hash hierarchy preceding the RomFS image can push the
    // RomFS metadata tables out past a 1 MiB prefix on bigger Control
    // NCAs (some retail games push past 1 MiB). Cap at 16 MiB which
    // covers every retail Control NCA observed so far while still
    // bounding peak RAM.
    let read_len = section.raw_size.min(0x1000000);
    let read_len_aligned = (read_len + 15) & !15;
    let mut buf = vec![0u8; read_len_aligned as usize];
    walker
        .read_section_plain(section, 0, &mut buf)
        .map_err(|e| anyhow::anyhow!("read control section: {}", e))?;

    // Scan for the RomFS header (header_size=0x50 as u64 LE) at 8-byte
    // aligned offsets. Verify by parsing the header.
    let mut romfs_offset = None;
    let header_pattern: [u8; 8] = 0x50u64.to_le_bytes();
    let mut i = 0;
    while i + 0x50 < buf.len() {
        if buf[i..i + 8] == header_pattern
            && crate::nintendo::nx::romfs::RomfsHeader::parse(&buf[i..]).is_ok()
        {
            romfs_offset = Some(i);
            break;
        }
        i += 8;
    }
    let romfs_offset = romfs_offset
        .ok_or_else(|| anyhow::anyhow!("no RomFS header signature in control section"))?;
    let romfs_image = &buf[romfs_offset..];

    let reader = RomfsReader::new(romfs_image)?;

    let nacp_entry = reader
        .find_root_file("control.nacp")?
        .ok_or_else(|| anyhow::anyhow!("control.nacp missing from RomFS"))?;
    let nacp_bytes = reader.read_file(&nacp_entry)?;
    let nacp = Nacp::parse(&nacp_bytes)?;

    let (icon, icon_language) = extract_icon(&reader, &nacp).unwrap_or((None, None));

    let titles = nacp
        .titles
        .iter()
        .map(|t| NxNacpTitle {
            language: format!("{:?}", t.language),
            name: t.name.clone(),
            publisher: t.publisher.clone(),
        })
        .collect();

    let supported_languages = decode_supported_languages(nacp.supported_language_bitmask);
    let attributes = decode_attribute_flags(nacp.attribute_flag);
    let parental_control_flags = decode_parental_control_flags(nacp.parental_control_flag);
    let age_ratings = decode_age_ratings(&nacp.rating_age);

    Ok(NxControl {
        titles,
        display_version: nacp.display_version,
        startup_user_account: nacp.startup_user_account,
        startup_user_account_name: startup_user_account_name(nacp.startup_user_account).to_string(),
        screenshot: nacp.screenshot,
        video_capture: nacp.video_capture,
        video_capture_name: video_capture_name(nacp.video_capture).to_string(),
        attribute_flag: nacp.attribute_flag,
        attributes,
        supported_language_bitmask: nacp.supported_language_bitmask,
        supported_languages,
        parental_control_flag: nacp.parental_control_flag,
        parental_control_flags,
        user_account_save: nacp.user_account_save,
        user_account_save_journal: nacp.user_account_save_journal,
        device_save: nacp.device_save,
        device_save_journal: nacp.device_save_journal,
        bcat_save: nacp.bcat_save,
        rating_age: nacp.rating_age.to_vec(),
        age_ratings,
        addon_install_policy: nacp.addon_install_policy,
        addon_install_policy_name: addon_install_policy_name(nacp.addon_install_policy).to_string(),
        screen_orientation: nacp.screen_orientation,
        screen_orientation_name: screen_orientation_name(nacp.screen_orientation).to_string(),
        icon,
        icon_language,
    })
}

fn decode_supported_languages(mask: u32) -> Vec<String> {
    NacpLanguage::ALL
        .iter()
        .enumerate()
        .filter_map(|(idx, lang)| {
            if mask & (1 << idx) != 0 {
                Some(format!("{:?}", lang))
            } else {
                None
            }
        })
        .collect()
}

fn decode_attribute_flags(flags: u32) -> Vec<String> {
    let mut out = Vec::new();
    if flags & 0x1 != 0 {
        out.push("Demo".to_string());
    }
    if flags & 0x2 != 0 {
        out.push("RetailInteractiveDisplay".to_string());
    }
    out
}

fn decode_parental_control_flags(flags: u32) -> Vec<String> {
    let mut out = Vec::new();
    if flags & 0x1 != 0 {
        out.push("FreeCommunication".to_string());
    }
    out
}

fn decode_age_ratings(rating_age: &[i8; 32]) -> Vec<AgeRatingEntry> {
    const ORGS: &[(usize, &str)] = &[
        (0, "CERO"),
        (1, "GRACGCRB"),
        (2, "GSRMR"),
        (3, "ESRB"),
        (4, "ClassInd"),
        (5, "USK"),
        (6, "PEGI"),
        (7, "PEGIPortugal"),
        (8, "PEGIBBFC"),
        (9, "Russian"),
        (10, "ACB"),
        (11, "OFLC"),
        (12, "IARCGeneric"),
    ];
    ORGS.iter()
        .filter_map(|(idx, name)| {
            let v = rating_age[*idx];
            if v < 0 || v as u8 == 0xFF {
                None
            } else {
                Some(AgeRatingEntry {
                    organization: name.to_string(),
                    age: v,
                })
            }
        })
        .collect()
}

fn startup_user_account_name(v: u8) -> &'static str {
    match v {
        0 => "None",
        1 => "Required",
        2 => "RequiredWithNetworkServiceAccountAvailable",
        _ => "Unknown",
    }
}

fn video_capture_name(v: u8) -> &'static str {
    match v {
        0 => "Disabled",
        1 => "Manual",
        2 => "Enabled",
        _ => "Unknown",
    }
}

fn screen_orientation_name(v: u8) -> &'static str {
    match v {
        0 => "LandscapeAndPortrait",
        1 => "Landscape",
        2 => "Portrait",
        3 => "LandscapeAndPortraitInverted",
        _ => "Unknown",
    }
}

fn addon_install_policy_name(v: u8) -> &'static str {
    match v {
        0 => "AllowedOnlyOnUserAccount",
        1 => "Allowed",
        _ => "Unknown",
    }
}

fn extract_icon(reader: &RomfsReader<'_>, nacp: &Nacp) -> Result<(Option<Image>, Option<String>)> {
    // Prefer the icon for the first language that NACP has a title for,
    // then fall back to AmericanEnglish.
    let mut candidates: Vec<NacpLanguage> = nacp.titles.iter().map(|t| t.language).collect();
    if !candidates.contains(&NacpLanguage::AmericanEnglish) {
        candidates.push(NacpLanguage::AmericanEnglish);
    }
    for lang in candidates {
        if let Some(file) = reader.find_root_file(lang.icon_file_name())? {
            let jpeg_bytes = reader.read_file(&file)?;
            match jpeg_to_png(&jpeg_bytes) {
                Ok((png_bytes, w, h)) => {
                    return Ok((
                        Some(Image::new(png_bytes, w, h)),
                        Some(format!("{:?}", lang)),
                    ));
                }
                Err(e) => {
                    log::debug!("nx info: jpeg->png failed for {:?} ({})", lang, e);
                }
            }
        }
    }
    Ok((None, None))
}

fn jpeg_to_png(jpeg: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    use image::ImageReader;
    let cursor = std::io::Cursor::new(jpeg);
    let img = ImageReader::with_format(cursor, image::ImageFormat::Jpeg).decode()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let png = crate::util::pixel::encode_png(rgba.as_raw(), w, h)?;
    Ok((png, w, h))
}

fn merge_inline_tickets(path: &Path, listing: &ContainerListing, keys: &mut KeySet) {
    let Ok(file) = File::open(path) else {
        return;
    };
    let file = Arc::new(file);
    for entry in &listing.entries {
        if !entry.name.to_ascii_lowercase().ends_with(".tik") {
            continue;
        }
        let mut buf = vec![0u8; entry.size as usize];
        if file_read_exact_at(&file, &mut buf, entry.abs_offset).is_err() {
            continue;
        }
        if let Ok(ticket) = Ticket::parse(&buf) {
            keys.title_keys
                .insert(ticket.rights_id, ticket.encrypted_title_key);
        }
    }
}

fn read_meta_cnmt(file: Arc<File>, nca_offset: u64, nca_size: u64, keys: &KeySet) -> Result<Cnmt> {
    let walker = NcaWalker::open(file, nca_offset, nca_size, keys)
        .map_err(|e| anyhow::anyhow!("open meta nca: {}", e))?;
    if walker.header.content_type != CONTENT_TYPE_META {
        return Err(anyhow::anyhow!(
            "not a meta NCA (content_type={})",
            walker.header.content_type
        ));
    }
    let section = walker
        .sections
        .first()
        .ok_or_else(|| anyhow::anyhow!("meta NCA has no sections"))?;

    // Hash region precedes the PFS0; scan the first chunk for the PFS0
    // magic instead of plumbing through HashedHierarchicalSha256 offsets.
    let scan_len = section.raw_size.min(0x4000);
    let scan_len_aligned = (scan_len + 15) & !15;
    let mut scan = vec![0u8; scan_len_aligned as usize];
    walker
        .read_section_plain(section, 0, &mut scan)
        .map_err(|e| anyhow::anyhow!("read meta section: {}", e))?;
    let pfs0_off = scan
        .windows(4)
        .position(|w| w == b"PFS0")
        .ok_or_else(|| anyhow::anyhow!("no PFS0 magic in meta section"))? as u64;

    let pfs0_section_offset = pfs0_off;
    // Read enough bytes to cover the PFS0 header + string table + at
    // least one .cnmt file. 64 KB is conservative; meta NCAs are tiny.
    let read_len = section
        .raw_size
        .saturating_sub(pfs0_section_offset)
        .min(0x10000);
    let read_len_aligned = (read_len + 15) & !15;
    let read_start = pfs0_section_offset & !15;
    let read_offset_in_data = (pfs0_section_offset - read_start) as usize;
    let mut buf = vec![0u8; read_len_aligned as usize];
    walker
        .read_section_plain(section, read_start, &mut buf)
        .map_err(|e| anyhow::anyhow!("read meta pfs0: {}", e))?;
    let pfs0_bytes = &buf[read_offset_in_data..];

    let mut cur = Cursor::new(pfs0_bytes);
    let pfs0 = Pfs0::read(&mut cur).map_err(|e| anyhow::anyhow!("parse meta pfs0: {}", e))?;
    let cnmt_entry = pfs0
        .files
        .iter()
        .find(|f| f.name.to_ascii_lowercase().ends_with(".cnmt"))
        .ok_or_else(|| anyhow::anyhow!("no .cnmt file in meta pfs0"))?;

    // The PFS0 was read at `read_start`; file payload starts at the
    // PFS0's reported data_section_offset (relative to its own start).
    let data_start_in_buf = (read_offset_in_data as u64 + pfs0.data_section_offset) as usize;
    let cnmt_start = data_start_in_buf + cnmt_entry.data_offset as usize;
    let cnmt_end = cnmt_start + cnmt_entry.size as usize;
    if cnmt_end > buf.len() {
        return Err(anyhow::anyhow!("meta pfs0 read window too small"));
    }
    let cnmt_bytes = &buf[cnmt_start..cnmt_end];
    Cnmt::parse(cnmt_bytes).map_err(|e| anyhow::anyhow!("parse cnmt: {}", e))
}

fn pick_primary_cnmt(cnmts: &[Cnmt]) -> usize {
    cnmts
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| match c.content_type {
            CNMT_TYPE_APPLICATION => (0u8, c.title_id),
            CNMT_TYPE_PATCH => (1u8, c.title_id),
            CNMT_TYPE_ADD_ON_CONTENT => (2u8, c.title_id),
            CNMT_TYPE_DELTA => (3u8, c.title_id),
            _ => (4u8, c.title_id),
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn cnmt_content_type_name(b: u8) -> &'static str {
    use crate::nintendo::nx::models::cnmt;
    match b {
        cnmt::CNMT_CONTENT_TYPE_META => "meta",
        cnmt::CNMT_CONTENT_TYPE_PROGRAM => "program",
        cnmt::CNMT_CONTENT_TYPE_DATA => "data",
        cnmt::CNMT_CONTENT_TYPE_CONTROL => "control",
        cnmt::CNMT_CONTENT_TYPE_HTML_DOCUMENT => "html_document",
        cnmt::CNMT_CONTENT_TYPE_LEGAL_INFORMATION => "legal_information",
        cnmt::CNMT_CONTENT_TYPE_DELTA_FRAGMENT => "delta_fragment",
        _ => "unknown",
    }
}

fn read_tickets(path: &Path, listing: &ContainerListing) -> Result<Vec<TicketSummary>> {
    let mut out = Vec::new();
    let mut file = File::open(path)?;
    for entry in &listing.entries {
        if !entry.name.to_ascii_lowercase().ends_with(".tik") {
            continue;
        }
        file.seek(SeekFrom::Start(entry.abs_offset))?;
        let mut buf = vec![0u8; entry.size as usize];
        if file.read_exact(&mut buf).is_err() {
            continue;
        }
        match Ticket::parse(&buf) {
            Ok(t) => out.push(TicketSummary {
                file_name: entry.name.clone(),
                rights_id: hex::encode(t.rights_id),
                master_key_revision: t.master_key_revision,
            }),
            Err(_) => continue,
        }
    }
    Ok(out)
}

fn read_xci_partition_layout(path: &Path) -> Result<Vec<XciPartitionSummary>> {
    let mut probe = File::open(path)?;
    let hfs0_off = read_xci_hfs0_offset(&mut probe)?;
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(hfs0_off))?;
    let root = Hfs0::read(&mut reader)?;

    let mut out = Vec::with_capacity(root.files.len());
    for entry in &root.files {
        let part_abs_offset = root.data_section_offset + entry.data_offset;
        reader.seek(SeekFrom::Start(part_abs_offset))?;
        let sub = Hfs0::read(&mut reader)?;
        let total_size = sub.files.iter().map(|f| f.size).sum();
        out.push(XciPartitionSummary {
            name: entry.name.clone(),
            file_count: sub.files.len(),
            total_size,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nx_container_kind_from_container_kind() {
        assert_eq!(
            NxContainerKind::from(ContainerKind::Nsp),
            NxContainerKind::Nsp
        );
        assert_eq!(
            NxContainerKind::from(ContainerKind::Xcz),
            NxContainerKind::Xcz
        );
    }
}
