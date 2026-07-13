//! NSP/XCI -> NSZ/XCZ container pipelines.
//!
//! Strategy: write a placeholder header, stream each file's data into
//! the output sequentially (NCAs through `ncz::nca_to_ncz`, everything
//! else copied verbatim), then seek back and rewrite the header with
//! the final sizes plus (for HFS0) per-file SHA-256 chunk hashes.
//! The placeholder and the final header have the same on-disk length
//! because PFS0 / HFS0 header size depends only on file names and
//! count, both of which are known up front.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::nintendo::nx::constants::{
    DEFAULT_BLOCK_SIZE_EXP, DEFAULT_ZSTD_LEVEL, MAX_BLOCK_SIZE_EXP, MAX_ZSTD_LEVEL,
    MIN_BLOCK_SIZE_EXP, MIN_ZSTD_LEVEL, NCA_PREFIX_SIZE,
};

const NCA_PREFIX_SIZE_U64: u64 = NCA_PREFIX_SIZE as u64;
use crate::nintendo::nx::container::{ContainerKind, detect_container, read_xci_hfs0_offset};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::models::hfs0::{
    self as hfs0_mod, DEFAULT_HASHED_REGION, Hfs0FileSpec, Hfs0LayoutHints,
};
use crate::nintendo::nx::models::nca::{CONTENT_TYPE_PROGRAM, CONTENT_TYPE_PUBLIC_DATA};
use crate::nintendo::nx::models::pfs0 as pfs0_mod;
use crate::nintendo::nx::models::ticket::Ticket;
use crate::nintendo::nx::ncz::compress::{NcaToNczOptions, NczMode, nca_to_ncz};
use crate::nintendo::nx::walker::NcaWalker;
use crate::util::pread::file_read_exact_at;
use crate::util::{CancelToken, ProgressReporter, await_with_progress_cancel, scratch_output_path};

#[derive(Debug, Clone, Copy)]
pub struct NxCompressOptions {
    pub level: i32,
    pub mode: NczMode,
}

impl Default for NxCompressOptions {
    fn default() -> Self {
        Self {
            level: DEFAULT_ZSTD_LEVEL,
            mode: NczMode::Solid,
        }
    }
}

impl NxCompressOptions {
    /// Match nsz: solid for NSP (smaller), block-1 MiB for XCI (random
    /// read friendly for emulators that mount the .xcz live).
    pub fn for_kind(kind: ContainerKind) -> Self {
        let mode = if kind.is_xci() {
            NczMode::Block {
                size_exp: DEFAULT_BLOCK_SIZE_EXP,
            }
        } else {
            NczMode::Solid
        };
        Self {
            level: DEFAULT_ZSTD_LEVEL,
            mode,
        }
    }

    pub fn validate(self) -> NxResult<Self> {
        if !(MIN_ZSTD_LEVEL..=MAX_ZSTD_LEVEL).contains(&self.level) {
            return Err(NxError::InvalidCompressionLevel {
                level: self.level,
                min: MIN_ZSTD_LEVEL,
                max: MAX_ZSTD_LEVEL,
            });
        }
        if let NczMode::Block { size_exp } = self.mode
            && !(MIN_BLOCK_SIZE_EXP..=MAX_BLOCK_SIZE_EXP).contains(&size_exp)
        {
            return Err(NxError::BlockSizeOutOfRange(size_exp));
        }
        Ok(self)
    }
}

pub fn compress_container(
    input: &Path,
    output: &Path,
    opts: NxCompressOptions,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
    cancel: Option<&CancelToken>,
) -> NxResult<()> {
    let kind = detect_container(input)?;
    if kind.is_compressed() {
        return Err(NxError::WrongContainerKind(format!("{kind:?}"), "compress"));
    }
    let opts = opts.validate()?;
    match kind {
        ContainerKind::Nsp => compress_pfs0(input, output, opts, keys, progress, cancel),
        ContainerKind::Xci => compress_xci(input, output, opts, keys, progress, cancel),
        _ => unreachable!(),
    }
}

pub async fn compress_container_async(
    input: PathBuf,
    output: PathBuf,
    opts: NxCompressOptions,
    keys: KeySet,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    compress_container_async_cancellable(input, output, opts, keys, progress, CancelToken::new())
        .await
}

/// Like [`compress_container_async`] but observes `cancel` between NCA
/// entries; on cancel the partial output is removed (the writer targets
/// a sibling temp file renamed into place only on success).
pub async fn compress_container_async_cancellable(
    input: PathBuf,
    output: PathBuf,
    opts: NxCompressOptions,
    keys: KeySet,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> NxResult<()> {
    let total = tokio::fs::metadata(&input).await?.len();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();
    progress.start(total, "Compressing Switch container");
    let proxy = AtomicProgress {
        counter: bytes_done_bg,
    };

    let write_path = scratch_output_path(&output)?;
    let write_owned = write_path.to_path_buf();
    let cancel_bg = cancel.clone();

    let handle = tokio::task::spawn_blocking(move || -> NxResult<()> {
        compress_container(&input, &write_owned, opts, &keys, &proxy, Some(&cancel_bg))
    });

    let cleanup = {
        let write_path = write_path.to_path_buf();
        move || -> NxError {
            let _ = std::fs::remove_file(&write_path);
            NxError::Cancelled
        }
    };
    if let Err(err) =
        await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await
    {
        let _ = tokio::fs::remove_file(&write_path).await;
        return Err(err);
    }
    crate::util::publish_temp(write_path, &output, true)?;
    Ok(())
}

struct AtomicProgress {
    counter: Arc<AtomicU64>,
}

impl ProgressReporter for AtomicProgress {
    fn start(&self, _: u64, _: &str) {}
    fn inc(&self, delta: u64) {
        self.counter.fetch_add(delta, Ordering::Relaxed);
    }
    fn finish(&self) {}
}

fn compress_pfs0(
    input: &Path,
    output: &Path,
    opts: NxCompressOptions,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
    cancel: Option<&CancelToken>,
) -> NxResult<()> {
    let in_file = Arc::new(File::open(input)?);
    let mut reader = BufReader::new(File::open(input)?);
    let pfs0 = pfs0_mod::Pfs0::read(&mut reader)?;
    drop(reader);

    let mut keys = keys.clone();
    load_tickets_into_keyset(&in_file, &pfs0, &mut keys)?;

    // Mirror nsz: only rename .nca -> .ncz for PROGRAM and PUBLICDATA
    // content types whose first section sits at or past 0x4000. CONTROL,
    // MANUAL, META, and DATA NCAs stay as .nca; otherwise this would write
    // a .ncz whose NCZSECTN entry references bytes inside the prefix
    // and nsz's `nca_size = 0x4000 + sum(section.size)` formula would
    // overcount the decompressed size.
    let new_names: Vec<String> = pfs0
        .files
        .iter()
        .map(|f| -> NxResult<String> {
            if !f.name.to_ascii_lowercase().ends_with(".nca") || f.size <= NCA_PREFIX_SIZE_U64 {
                return Ok(f.name.clone());
            }
            let abs = pfs0.data_section_offset + f.data_offset;
            let walker = NcaWalker::open(in_file.clone(), abs, f.size, &keys)?;
            let nca_byte_offset = walker
                .sections
                .iter()
                .map(|s| s.raw_offset - walker.nca_offset())
                .min()
                .unwrap_or(u64::MAX);
            let compressible = matches!(
                walker.header.content_type,
                CONTENT_TYPE_PROGRAM | CONTENT_TYPE_PUBLIC_DATA
            ) && nca_byte_offset >= NCA_PREFIX_SIZE_U64;
            Ok(if compressible {
                renamed_to_compressed(&f.name)
            } else {
                f.name.clone()
            })
        })
        .collect::<NxResult<Vec<_>>>()?;
    let placeholder_specs: Vec<(String, u64)> = new_names
        .iter()
        .zip(&pfs0.files)
        .map(|(n, f)| (n.clone(), f.size))
        .collect();
    let hints = pfs0_mod::Pfs0LayoutHints {
        target_total_header_size: Some(pfs0.data_section_offset as usize),
        first_file_data_offset: pfs0.files.first().map(|f| f.data_offset).unwrap_or(0),
    };
    let placeholder_header = pfs0_mod::build_header(&placeholder_specs, &hints)?;

    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)?;
    out.write_all(&placeholder_header.bytes)?;
    if hints.first_file_data_offset > 0 {
        let pad = vec![0u8; hints.first_file_data_offset as usize];
        out.write_all(&pad)?;
    }

    let mut sizes = Vec::with_capacity(pfs0.files.len());
    for (i, f) in pfs0.files.iter().enumerate() {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(NxError::Cancelled);
        }
        let abs = pfs0.data_section_offset + f.data_offset;
        let new_name = &new_names[i];
        let written = if new_name.ends_with(".ncz") {
            let walker = NcaWalker::open(in_file.clone(), abs, f.size, &keys)?;
            let pos_before = out.stream_position()?;
            nca_to_ncz(
                &walker,
                &mut out,
                NcaToNczOptions {
                    mode: opts.mode,
                    level: opts.level,
                },
                progress,
            )?;
            out.stream_position()? - pos_before
        } else {
            let written = copy_range(&in_file, abs, f.size, &mut out)?;
            progress.inc(written);
            written
        };
        sizes.push(written);
    }

    let final_specs: Vec<(String, u64)> = new_names
        .iter()
        .zip(&sizes)
        .map(|(name, size)| (name.clone(), *size))
        .collect();
    let final_header = pfs0_mod::build_header(&final_specs, &hints)?;
    debug_assert_eq!(final_header.bytes.len(), placeholder_header.bytes.len());
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&final_header.bytes)?;
    out.flush()?;
    Ok(())
}

fn compress_xci(
    input: &Path,
    output: &Path,
    opts: NxCompressOptions,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
    cancel: Option<&CancelToken>,
) -> NxResult<()> {
    let in_file = Arc::new(File::open(input)?);
    let hfs0_off = {
        let mut probe = File::open(input)?;
        read_xci_hfs0_offset(&mut probe)?
    };
    let mut reader = BufReader::new(File::open(input)?);

    let mut xci_prefix = vec![0u8; hfs0_off as usize];
    reader.read_exact(&mut xci_prefix)?;

    reader.seek(SeekFrom::Start(hfs0_off))?;
    let root = hfs0_mod::Hfs0::read(&mut reader)?;

    let mut sub_partitions = Vec::with_capacity(root.files.len());
    for root_entry in &root.files {
        let part_abs = root.data_section_offset + root_entry.data_offset;
        reader.seek(SeekFrom::Start(part_abs))?;
        let sub = hfs0_mod::Hfs0::read(&mut reader)?;
        sub_partitions.push(SubPartitionPlan {
            partition_name: root_entry.name.clone(),
            partition_hashed_size: root_entry.hashed_region_size.max(DEFAULT_HASHED_REGION),
            sub_header_start: part_abs,
            sub,
        });
    }
    drop(reader);

    let mut keys = keys.clone();
    load_tickets_from_xci(&in_file, &sub_partitions, &mut keys)?;

    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)?;
    out.write_all(&xci_prefix)?;

    let placeholder_root_specs: Vec<Hfs0FileSpec> = sub_partitions
        .iter()
        .map(|p| Hfs0FileSpec {
            name: p.partition_name.clone(),
            size: 0,
            sha256: [0u8; 32],
            hashed_region_size: p.partition_hashed_size,
        })
        .collect();
    let root_hints = Hfs0LayoutHints {
        target_total_header_size: Some((root.data_section_offset - hfs0_off) as usize),
        first_file_data_offset: root.files.first().map(|f| f.data_offset).unwrap_or(0),
    };
    let placeholder_root_header = hfs0_mod::build_header(&placeholder_root_specs, &root_hints)?;
    out.write_all(&placeholder_root_header.bytes)?;
    if root_hints.first_file_data_offset > 0 {
        let pad = vec![0u8; root_hints.first_file_data_offset as usize];
        out.write_all(&pad)?;
    }

    let mut new_partition_sizes = Vec::with_capacity(sub_partitions.len());
    let mut new_partition_first_chunk_hashes = Vec::with_capacity(sub_partitions.len());

    for (plan, root_entry) in sub_partitions.iter().zip(&root.files) {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(NxError::Cancelled);
        }
        // nsz's XCZ decompressor (Hfs0Stream) starts each partition at the
        // previous partition's header end, so any non-secure partition that
        // carries files gets its data overwritten by the partitions that
        // follow it. nsz -C therefore always writes empty update/logo/normal
        // partitions; do the same or `nsz -D` produces a corrupt XCI.
        if plan.partition_name != "secure" && !plan.sub.files.is_empty() {
            let stub = hfs0_mod::build_header(
                &[],
                &Hfs0LayoutHints {
                    target_total_header_size: Some(DEFAULT_HASHED_REGION as usize),
                    first_file_data_offset: 0,
                },
            )?;
            out.write_all(&stub.bytes)?;
            new_partition_sizes.push(stub.bytes.len() as u64);
            new_partition_first_chunk_hashes.push(hfs0_mod::hash_first_chunk(
                &stub.bytes,
                DEFAULT_HASHED_REGION,
            ));
            progress.inc(root_entry.size);
            continue;
        }
        let part_start = out.stream_position()?;
        write_sub_partition(
            &mut out,
            &in_file,
            plan,
            opts,
            &keys,
            progress,
            &mut new_partition_sizes,
            &mut new_partition_first_chunk_hashes,
            part_start,
            cancel,
        )?;
    }

    let mut final_root_specs = Vec::with_capacity(sub_partitions.len());
    for (i, plan) in sub_partitions.iter().enumerate() {
        final_root_specs.push(Hfs0FileSpec {
            name: plan.partition_name.clone(),
            size: new_partition_sizes[i],
            // Stubbed partitions shrink below the input's hashed region.
            hashed_region_size: (plan.partition_hashed_size as u64).min(new_partition_sizes[i])
                as u32,
            sha256: new_partition_first_chunk_hashes[i],
        });
    }
    let final_root_header = hfs0_mod::build_header(&final_root_specs, &root_hints)?;
    debug_assert_eq!(
        final_root_header.bytes.len(),
        placeholder_root_header.bytes.len()
    );
    out.seek(SeekFrom::Start(hfs0_off))?;
    out.write_all(&final_root_header.bytes)?;
    out.flush()?;
    Ok(())
}

struct SubPartitionPlan {
    partition_name: String,
    partition_hashed_size: u32,
    /// Absolute file offset of the sub-HFS0 header in the input. Used
    /// to recover the input's header size for layout preservation.
    sub_header_start: u64,
    sub: hfs0_mod::Hfs0,
}

#[allow(clippy::too_many_arguments)]
fn write_sub_partition(
    out: &mut File,
    in_file: &Arc<File>,
    plan: &SubPartitionPlan,
    opts: NxCompressOptions,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
    new_partition_sizes: &mut Vec<u64>,
    new_partition_first_chunk_hashes: &mut Vec<[u8; 32]>,
    part_start: u64,
    cancel: Option<&CancelToken>,
) -> NxResult<()> {
    let new_names: Vec<String> = plan
        .sub
        .files
        .iter()
        .map(|f| -> NxResult<String> {
            if !f.name.to_ascii_lowercase().ends_with(".nca") || f.size <= NCA_PREFIX_SIZE_U64 {
                return Ok(f.name.clone());
            }
            let abs = plan.sub.data_section_offset + f.data_offset;
            let walker = NcaWalker::open(in_file.clone(), abs, f.size, keys)?;
            let nca_byte_offset = walker
                .sections
                .iter()
                .map(|s| s.raw_offset - walker.nca_offset())
                .min()
                .unwrap_or(u64::MAX);
            let compressible = matches!(
                walker.header.content_type,
                CONTENT_TYPE_PROGRAM | CONTENT_TYPE_PUBLIC_DATA
            ) && nca_byte_offset >= NCA_PREFIX_SIZE_U64;
            Ok(if compressible {
                renamed_to_compressed(&f.name)
            } else {
                f.name.clone()
            })
        })
        .collect::<NxResult<Vec<_>>>()?;

    let placeholder_specs: Vec<Hfs0FileSpec> = plan
        .sub
        .files
        .iter()
        .zip(&new_names)
        .map(|(f, n)| Hfs0FileSpec {
            name: n.clone(),
            size: f.size,
            sha256: [0u8; 32],
            hashed_region_size: DEFAULT_HASHED_REGION,
        })
        .collect();
    let sub_hints = Hfs0LayoutHints {
        target_total_header_size: Some(
            (plan.sub.data_section_offset - plan.sub_header_start) as usize,
        ),
        first_file_data_offset: plan.sub.files.first().map(|f| f.data_offset).unwrap_or(0),
    };
    let placeholder_header = hfs0_mod::build_header(&placeholder_specs, &sub_hints)?;
    out.write_all(&placeholder_header.bytes)?;
    if sub_hints.first_file_data_offset > 0 {
        let pad = vec![0u8; sub_hints.first_file_data_offset as usize];
        out.write_all(&pad)?;
    }

    let mut sub_specs: Vec<Hfs0FileSpec> = Vec::with_capacity(plan.sub.files.len());
    for (i, f) in plan.sub.files.iter().enumerate() {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(NxError::Cancelled);
        }
        let abs = plan.sub.data_section_offset + f.data_offset;
        let new_name = &new_names[i];
        let pos_before = out.stream_position()?;
        if new_name.ends_with(".ncz") {
            let walker = NcaWalker::open(in_file.clone(), abs, f.size, keys)?;
            nca_to_ncz(
                &walker,
                out,
                NcaToNczOptions {
                    mode: opts.mode,
                    level: opts.level,
                },
                progress,
            )?;
        } else {
            let copied = copy_range(in_file, abs, f.size, out)?;
            progress.inc(copied);
        };
        let pos_after = out.stream_position()?;
        let written = pos_after - pos_before;
        // SHA-256 covers the first 0x200 bytes (or the full file if
        // shorter). For NCZ files the first 0x200 bytes are inside
        // the encrypted-NCA prefix that nca_to_ncz wrote verbatim, so
        // they are stable across any later seek-and-rewrite the inner
        // pipeline does on the NCZBLOCK header.
        let take = (DEFAULT_HASHED_REGION as u64).min(written) as usize;
        let mut sha_buf = vec![0u8; take];
        out.seek(SeekFrom::Start(pos_before))?;
        out.read_exact(&mut sha_buf)?;
        out.seek(SeekFrom::Start(pos_after))?;
        let mut hasher = Sha256::new();
        hasher.update(&sha_buf);
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&hasher.finalize());
        sub_specs.push(Hfs0FileSpec {
            name: new_name.clone(),
            size: written,
            sha256,
            hashed_region_size: DEFAULT_HASHED_REGION,
        });
    }

    let final_header = hfs0_mod::build_header(&sub_specs, &sub_hints)?;
    debug_assert_eq!(final_header.bytes.len(), placeholder_header.bytes.len());

    let part_end = out.stream_position()?;
    out.seek(SeekFrom::Start(part_start))?;
    out.write_all(&final_header.bytes)?;
    out.seek(SeekFrom::Start(part_end))?;

    let part_size = part_end - part_start;
    let mut hasher = Sha256::new();
    let take = (DEFAULT_HASHED_REGION as usize).min(final_header.bytes.len());
    hasher.update(&final_header.bytes[..take]);
    let first_chunk = hasher.finalize();
    let mut sha = [0u8; 32];
    sha.copy_from_slice(&first_chunk);

    new_partition_sizes.push(part_size);
    new_partition_first_chunk_hashes.push(sha);
    Ok(())
}

fn load_tickets_into_keyset(
    in_file: &Arc<File>,
    pfs0: &pfs0_mod::Pfs0,
    keys: &mut KeySet,
) -> NxResult<()> {
    for f in &pfs0.files {
        if !f.name.to_ascii_lowercase().ends_with(".tik") {
            continue;
        }
        let abs = pfs0.data_section_offset + f.data_offset;
        let mut buf = vec![0u8; f.size as usize];
        crate::util::pread::file_read_exact_at(in_file, &mut buf, abs)?;
        if let Ok(ticket) = Ticket::parse(&buf) {
            keys.title_keys
                .insert(ticket.rights_id, ticket.encrypted_title_key);
        }
    }
    Ok(())
}

fn load_tickets_from_xci(
    in_file: &Arc<File>,
    sub_partitions: &[SubPartitionPlan],
    keys: &mut KeySet,
) -> NxResult<()> {
    for plan in sub_partitions {
        for f in &plan.sub.files {
            if !f.name.to_ascii_lowercase().ends_with(".tik") {
                continue;
            }
            let abs = plan.sub.data_section_offset + f.data_offset;
            let mut buf = vec![0u8; f.size as usize];
            crate::util::pread::file_read_exact_at(in_file, &mut buf, abs)?;
            if let Ok(ticket) = Ticket::parse(&buf) {
                keys.title_keys
                    .insert(ticket.rights_id, ticket.encrypted_title_key);
            }
        }
    }
    Ok(())
}

fn renamed_to_compressed(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if let Some(stripped) = name.strip_suffix_inplace_ignore_case(".cnmt.nca") {
        return format!("{stripped}.cnmt.ncz");
    }
    if lower.ends_with(".nca") {
        let stem = &name[..name.len() - 4];
        return format!("{stem}.ncz");
    }
    name.to_string()
}

trait StripSuffixIgnoreCase {
    fn strip_suffix_inplace_ignore_case(&self, suffix: &str) -> Option<&str>;
}

impl StripSuffixIgnoreCase for str {
    fn strip_suffix_inplace_ignore_case(&self, suffix: &str) -> Option<&str> {
        if self.len() >= suffix.len() {
            let split = self.len() - suffix.len();
            let tail = &self[split..];
            if tail.eq_ignore_ascii_case(suffix) {
                return Some(&self[..split]);
            }
        }
        None
    }
}

fn copy_range<W: Write>(file: &File, abs_offset: u64, size: u64, out: &mut W) -> NxResult<u64> {
    const CHUNK: usize = 4 * 1024 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut remaining = size;
    let mut at = abs_offset;
    let mut written = 0u64;
    while remaining > 0 {
        let take = (CHUNK as u64).min(remaining) as usize;
        file_read_exact_at(file, &mut buf[..take], at)?;
        out.write_all(&buf[..take])?;
        at += take as u64;
        remaining -= take as u64;
        written += take as u64;
    }
    Ok(written)
}
