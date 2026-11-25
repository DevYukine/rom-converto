//! Groups extract inputs by CNMT and lists the target directories.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::storage::{NcaRef, SectionRef};
use super::{NxExtractPart, NxExtractParts, NxExtractPlan, NxExtractTarget};
use crate::nintendo::nx::container::{ContainerEntry, ContainerListing, list_container};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::meta::{merge_inline_tickets, read_meta_cnmt};
use crate::nintendo::nx::models::cnmt::{
    CNMT_CONTENT_TYPE_DATA, CNMT_CONTENT_TYPE_PROGRAM, CNMT_TYPE_ADD_ON_CONTENT,
    CNMT_TYPE_APPLICATION, CNMT_TYPE_PATCH,
};
use crate::nintendo::nx::models::nca::{FS_TYPE_PARTITION_FS, FS_TYPE_ROMFS};
use crate::nintendo::nx::walker::{NcaSection, NcaWalker};

/// Bit that turns an application's program id into its update's.
const PATCH_ID_BIT: u64 = 0x800;

/// Classifies `inputs`, pairs updates with their base, and lists every
/// target. Reads only headers, CNMTs and tickets (never RomFS tables).
/// Blocking: callers run it through `spawn_blocking`.
///
/// # Errors
/// Fails when an input is not a Switch container, a CNMT or a content
/// NCA header cannot be read (a missing title key included), a Program
/// or Data record of the newest version of a title has no matching
/// `.nca`/`.ncz`, or no target remains ([`NxError::NothingToExtract`]).
pub fn plan_extract(
    inputs: &[PathBuf],
    output_dir: &Path,
    parts: NxExtractParts,
    keys: &KeySet,
) -> NxResult<NxExtractPlan> {
    let mut keys = keys.clone();
    let mut listings = Vec::with_capacity(inputs.len());
    for path in inputs {
        let listing = list_container(path)?;
        merge_inline_tickets(path, &listing, &mut keys);
        listings.push(listing);
    }
    let mut metas = BTreeMap::new();
    let mut warnings = Vec::new();
    for (input, (path, listing)) in inputs.iter().zip(&listings).enumerate() {
        read_metas(input, path, listing, &keys, &mut metas, &mut warnings)?;
    }
    let titles = Titles::open(&metas, inputs, &listings, &keys)?;
    let targets = titles.targets(output_dir, parts, &mut warnings);
    if targets.is_empty() {
        return Err(NxError::NothingToExtract);
    }
    Ok(NxExtractPlan {
        targets,
        warnings,
        keys,
    })
}

/// The newest CNMT of one title: its version, the input holding it, and
/// the ids of the Program (or DLC Data) NCAs it lists.
struct Meta {
    version: u32,
    input: usize,
    content_ids: Vec<String>,
}

/// Reads every CNMT of `listing` into `metas`, keyed by CNMT type and
/// title id. Only the newest version is kept, so the content of a
/// superseded one is never opened.
fn read_metas(
    input: usize,
    path: &Path,
    listing: &ContainerListing,
    keys: &KeySet,
    metas: &mut BTreeMap<(u8, u64), Meta>,
    warnings: &mut Vec<String>,
) -> NxResult<()> {
    let file = Arc::new(File::open(path)?);
    for meta in game_entries(listing) {
        if !meta.name.to_ascii_lowercase().ends_with(".cnmt.nca") {
            continue;
        }
        let cnmt = match read_meta_cnmt(file.clone(), meta.abs_offset, meta.size, keys) {
            Ok(cnmt) => cnmt,
            Err(err) if !matches!(meta.partition, None | Some("secure")) => {
                warnings.push(format!(
                    "Skipped unreadable {} in {}: {err}",
                    meta.name,
                    path.display()
                ));
                continue;
            }
            Err(err) => return Err(err),
        };
        let record_type = match cnmt.content_type {
            CNMT_TYPE_APPLICATION | CNMT_TYPE_PATCH => CNMT_CONTENT_TYPE_PROGRAM,
            CNMT_TYPE_ADD_ON_CONTENT => CNMT_CONTENT_TYPE_DATA,
            _ => continue,
        };
        let key = (cnmt.content_type, cnmt.title_id);
        if metas
            .get(&key)
            .is_some_and(|kept| kept.version >= cnmt.version)
        {
            continue;
        }
        let content_ids = cnmt
            .contents
            .iter()
            .filter(|r| r.content_type == record_type)
            .map(|r| hex::encode(r.content_id))
            .collect();
        metas.insert(
            key,
            Meta {
                version: cnmt.version,
                input,
                content_ids,
            },
        );
    }
    Ok(())
}

/// The entries of a container's game partitions: an XCI's update
/// partition carries system firmware, not the game.
fn game_entries(listing: &ContainerListing) -> impl Iterator<Item = &ContainerEntry> {
    listing
        .entries
        .iter()
        .filter(|e| e.partition != Some("update"))
}

/// The Program (or DLC Data) content of every newest title, per program id.
#[derive(Default)]
struct Titles {
    bases: BTreeMap<u64, Content>,
    /// Keyed by the base program id the update patches.
    patches: BTreeMap<u64, Content>,
    dlcs: BTreeMap<u64, Content>,
}

/// One Program or Data NCA and the filesystems it holds.
struct Content {
    title_id: u64,
    exefs: Option<Filesystem>,
    romfs: Option<Filesystem>,
}

/// A section a target can read, with its header size estimate.
struct Filesystem {
    section: SectionRef,
    bytes: u64,
    /// Section size, which bounds the unchecked `bytes` of a plain section.
    size: u64,
    /// The section is an update's patch over the base RomFS.
    patch: bool,
}

impl Titles {
    /// Opens the header of every content NCA the metas list.
    fn open(
        metas: &BTreeMap<(u8, u64), Meta>,
        inputs: &[PathBuf],
        listings: &[ContainerListing],
        keys: &KeySet,
    ) -> NxResult<Self> {
        let mut titles = Self::default();
        for (&(meta_type, _), meta) in metas {
            let path = &inputs[meta.input];
            let file = Arc::new(File::open(path)?);
            for id in &meta.content_ids {
                let (nca, ncz) = (format!("{id}.nca"), format!("{id}.ncz"));
                let entry = game_entries(&listings[meta.input])
                    .find(|e| {
                        e.name.eq_ignore_ascii_case(&nca) || e.name.eq_ignore_ascii_case(&ncz)
                    })
                    .ok_or_else(|| NxError::MissingReferencedNca {
                        input: path.clone(),
                        nca_id: id.clone(),
                    })?;
                let content = Content::read(path, &file, entry, keys)?;
                let (map, key) = match meta_type {
                    CNMT_TYPE_APPLICATION => (&mut titles.bases, content.title_id),
                    CNMT_TYPE_PATCH => (&mut titles.patches, content.title_id & !PATCH_ID_BIT),
                    _ => (&mut titles.dlcs, content.title_id),
                };
                map.insert(key, content);
            }
        }
        Ok(titles)
    }

    fn targets(
        &self,
        output_dir: &Path,
        parts: NxExtractParts,
        warnings: &mut Vec<String>,
    ) -> Vec<NxExtractTarget> {
        let target = |title_id: u64, part, fs: &Filesystem, base: Option<&Filesystem>| {
            let folder = match part {
                NxExtractPart::Romfs => "romfs",
                NxExtractPart::Exefs => "exefs",
            };
            // The walk caps the output at the header's image size. A
            // patched image may map one base range many times, so only a
            // plain one is bounded by its section.
            let bytes = match base {
                Some(_) => fs.bytes,
                None => fs.bytes.min(fs.size),
            };
            NxExtractTarget {
                title_id,
                part,
                dir: output_dir.join(format!("{title_id:016X}")).join(folder),
                bytes,
                overwrite: false,
                source: fs.section.clone(),
                base: base.map(|b| b.section.clone()),
            }
        };
        let mut targets = Vec::new();
        let programs: BTreeSet<u64> = self
            .bases
            .keys()
            .chain(self.patches.keys())
            .copied()
            .collect();
        for id in programs {
            let base = self.bases.get(&id);
            let patch = self.patches.get(&id);
            if parts.exefs {
                // An update without an ExeFS runs the base game's.
                let exefs = patch
                    .and_then(|c| c.exefs.as_ref())
                    .or_else(|| base.and_then(|c| c.exefs.as_ref()));
                match exefs {
                    Some(fs) => targets.push(target(id, NxExtractPart::Exefs, fs, None)),
                    None => warnings.push(format!("{id:016X} has no ExeFS; skipped it")),
                }
            }
            if !parts.romfs {
                continue;
            }
            let base_romfs = base.and_then(|c| c.romfs.as_ref());
            match (patch.and_then(|c| c.romfs.as_ref()), base_romfs) {
                (Some(p), Some(b)) if p.patch => {
                    targets.push(target(id, NxExtractPart::Romfs, p, Some(b)));
                }
                (Some(p), _) if !p.patch => targets.push(target(id, NxExtractPart::Romfs, p, None)),
                (None, Some(b)) => targets.push(target(id, NxExtractPart::Romfs, b, None)),
                _ => match (base, patch) {
                    (None, Some(update)) => warnings.push(format!(
                        "Update {:016X} has no base game among the inputs; its RomFS needs the base game, so it was skipped",
                        update.title_id
                    )),
                    _ => warnings.push(format!("{id:016X} has no RomFS; skipped it")),
                },
            }
        }
        for (id, dlc) in &self.dlcs {
            match (parts.romfs, &dlc.romfs) {
                (true, Some(fs)) => {
                    let dlc_target = target(*id, NxExtractPart::Romfs, fs, None);
                    if targets.iter().any(|t| t.dir == dlc_target.dir) {
                        warnings.push(format!(
                            "DLC {id:016X} has the title id of a program among the inputs; kept the program's RomFS"
                        ));
                    } else {
                        targets.push(dlc_target);
                    }
                }
                (true, None) => warnings.push(format!("DLC {id:016X} has no RomFS; skipped it")),
                (false, _) => warnings.push(format!("DLC {id:016X} has no ExeFS; skipped it")),
            }
        }
        targets
    }
}

impl Content {
    fn read(
        path: &Path,
        file: &Arc<File>,
        entry: &ContainerEntry,
        keys: &KeySet,
    ) -> NxResult<Self> {
        // An NCZ keeps the NCA header verbatim, so it reads straight from
        // the container without decompressing anything.
        let walker = NcaWalker::open(file.clone(), entry.abs_offset, entry.size, keys)?;
        let nca = NcaRef {
            path: path.to_path_buf(),
            name: entry.name.clone(),
            offset: entry.abs_offset,
            size: entry.size,
            ncz: entry.name.to_ascii_lowercase().ends_with(".ncz"),
        };
        let filesystem = |section: &NcaSection| {
            let fs = walker.header.fs_headers[section.index];
            Filesystem {
                section: SectionRef {
                    nca: nca.clone(),
                    index: section.index,
                },
                bytes: fs.hash_target_size.unwrap_or(0),
                size: section.raw_size,
                patch: fs.patch.indirect_size != 0,
            }
        };
        let fs_type = |index: usize| walker.header.fs_headers[index].fs_type;
        // Section 0 of a Program NCA is the ExeFS; the logo is a later
        // PartitionFs section.
        let exefs = walker
            .sections
            .iter()
            .find(|s| s.index == 0 && fs_type(0) == FS_TYPE_PARTITION_FS)
            .map(filesystem);
        let romfs = walker
            .sections
            .iter()
            .find(|s| fs_type(s.index) == FS_TYPE_ROMFS)
            .map(filesystem);
        Ok(Self {
            title_id: walker.header.title_id,
            exefs,
            romfs,
        })
    }
}
