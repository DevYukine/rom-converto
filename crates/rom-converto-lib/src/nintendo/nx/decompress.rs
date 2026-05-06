//! NSZ/XCZ -> NSP/XCI container pipelines. Mirror of compress.rs:
//! same placeholder-header-then-rewrite pattern, NCZ files routed
//! through `ncz::ncz_to_nca`, everything else copied verbatim.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::nintendo::nx::container::{ContainerKind, detect_container, read_xci_hfs0_offset};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::models::hfs0::{
    self as hfs0_mod, DEFAULT_HASHED_REGION, Hfs0FileSpec, Hfs0LayoutHints,
};
use crate::nintendo::nx::models::pfs0 as pfs0_mod;
use crate::nintendo::nx::ncz::ncz_to_nca;
use crate::nintendo::nx::util::PositionalReader;
use crate::util::ProgressReporter;
use crate::util::pread::file_read_exact_at;

pub fn decompress_container(
    input: &Path,
    output: &Path,
    _keys: &KeySet,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    let kind = detect_container(input)?;
    if !kind.is_compressed() {
        return Err(NxError::WrongContainerKind(
            format!("{kind:?}"),
            "decompress",
        ));
    }
    match kind {
        ContainerKind::Nsz => decompress_pfs0(input, output, progress),
        ContainerKind::Xcz => decompress_xci(input, output, progress),
        _ => unreachable!(),
    }
}

pub async fn decompress_container_async(
    input: PathBuf,
    output: PathBuf,
    keys: KeySet,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();
    progress.start(0, "Decompressing Switch container");
    let proxy = AtomicProgress {
        counter: bytes_done_bg,
    };

    let mut handle = tokio::task::spawn_blocking(move || -> NxResult<()> {
        decompress_container(&input, &output, &keys, &proxy)
    });

    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => {
                result??;
                break;
            }
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    }
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();
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

fn decompress_pfs0(input: &Path, output: &Path, progress: &dyn ProgressReporter) -> NxResult<()> {
    let in_file = Arc::new(File::open(input)?);
    let mut reader = BufReader::new(File::open(input)?);
    let pfs0 = pfs0_mod::Pfs0::read(&mut reader)?;
    drop(reader);

    let new_names: Vec<String> = pfs0
        .files
        .iter()
        .map(|f| renamed_to_decompressed(&f.name))
        .collect();
    let placeholder_specs: Vec<(String, u64)> = new_names
        .iter()
        .zip(&pfs0.files)
        .map(|(n, f)| (n.clone(), f.size))
        .collect();
    // Match nsz: preserve the input PFS0's stringTable size + the
    // first file's data_offset (typically 0x7E30 in a 0x8000-aligned
    // NSP). nsz applies the same `getStringTableSize` and
    // `files[0].offset` to its output, so doing the same yields a
    // byte-identical container after decompression.
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
    for f in &pfs0.files {
        let abs = pfs0.data_section_offset + f.data_offset;
        // Source extension picks the path: only `.ncz` files carry
        // NCZ headers. Loose `.nca` and ticket/cert payloads in an
        // NSZ are already uncompressed and copy through verbatim.
        // Acting on the *output* extension would try to run zstd
        // over plain NCAs.
        let source_lower = f.name.to_ascii_lowercase();
        let written = if source_lower.ends_with(".ncz") {
            decompress_one_file(&in_file, abs, f.size, &mut out, progress)?
        } else {
            let n = copy_range(&in_file, abs, f.size, &mut out)?;
            progress.inc(n);
            n
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

fn decompress_xci(input: &Path, output: &Path, progress: &dyn ProgressReporter) -> NxResult<()> {
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

    for plan in &sub_partitions {
        let part_start = out.stream_position()?;
        let new_names: Vec<String> = plan
            .sub
            .files
            .iter()
            .map(|f| renamed_to_decompressed(&f.name))
            .collect();
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
        let placeholder_sub_header = hfs0_mod::build_header(&placeholder_specs, &sub_hints)?;
        out.write_all(&placeholder_sub_header.bytes)?;
        if sub_hints.first_file_data_offset > 0 {
            let pad = vec![0u8; sub_hints.first_file_data_offset as usize];
            out.write_all(&pad)?;
        }

        let mut sub_specs = Vec::with_capacity(plan.sub.files.len());
        for (i, f) in plan.sub.files.iter().enumerate() {
            let abs = plan.sub.data_section_offset + f.data_offset;
            let new_name = &new_names[i];
            let mut wrapper = HashTeeWriter::new(&mut out, DEFAULT_HASHED_REGION as usize);
            let source_lower = f.name.to_ascii_lowercase();
            let written = if source_lower.ends_with(".ncz") {
                decompress_one_file(&in_file, abs, f.size, &mut wrapper, progress)?
            } else {
                let n = copy_range(&in_file, abs, f.size, &mut wrapper)?;
                progress.inc(n);
                n
            };
            let (sha256, _) = wrapper.finalize();
            sub_specs.push(Hfs0FileSpec {
                name: new_name.clone(),
                size: written,
                sha256,
                hashed_region_size: DEFAULT_HASHED_REGION,
            });
        }

        let final_sub_header = hfs0_mod::build_header(&sub_specs, &sub_hints)?;
        debug_assert_eq!(
            final_sub_header.bytes.len(),
            placeholder_sub_header.bytes.len()
        );
        let part_end = out.stream_position()?;
        out.seek(SeekFrom::Start(part_start))?;
        out.write_all(&final_sub_header.bytes)?;
        out.seek(SeekFrom::Start(part_end))?;

        new_partition_sizes.push(part_end - part_start);
        let mut hasher = Sha256::new();
        let take = (DEFAULT_HASHED_REGION as usize).min(final_sub_header.bytes.len());
        hasher.update(&final_sub_header.bytes[..take]);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&hasher.finalize());
        new_partition_first_chunk_hashes.push(sha);
    }

    let mut final_root_specs = Vec::with_capacity(sub_partitions.len());
    for (i, plan) in sub_partitions.iter().enumerate() {
        final_root_specs.push(Hfs0FileSpec {
            name: plan.partition_name.clone(),
            size: new_partition_sizes[i],
            sha256: new_partition_first_chunk_hashes[i],
            hashed_region_size: plan.partition_hashed_size,
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
    /// to recover the input's header size (`data_section_offset -
    /// header_start`) for byte-identical layout preservation.
    sub_header_start: u64,
    sub: hfs0_mod::Hfs0,
}

fn decompress_one_file<W: Write>(
    in_file: &Arc<File>,
    abs_offset: u64,
    size: u64,
    out: &mut W,
    progress: &dyn ProgressReporter,
) -> NxResult<u64> {
    let mut reader = PositionalReader::new(in_file.clone(), abs_offset, size);
    let mut counter = ByteCounter::new(out);
    ncz_to_nca(&mut reader, &mut counter, progress)?;
    Ok(counter.bytes_written)
}

fn renamed_to_decompressed(name: &str) -> String {
    if let Some(stem) = name.strip_suffix_inplace_ignore_case(".cnmt.ncz") {
        return format!("{stem}.cnmt.nca");
    }
    if name.to_ascii_lowercase().ends_with(".ncz") {
        let stem = &name[..name.len() - 4];
        return format!("{stem}.nca");
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

struct ByteCounter<W> {
    inner: W,
    bytes_written: u64,
}

impl<W: Write> ByteCounter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            bytes_written: 0,
        }
    }
}

impl<W: Write> Write for ByteCounter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes_written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct HashTeeWriter<'a, W: Write> {
    inner: &'a mut W,
    hasher: Sha256,
    remaining_to_hash: usize,
    bytes_written: u64,
}

impl<'a, W: Write> HashTeeWriter<'a, W> {
    fn new(inner: &'a mut W, hash_limit: usize) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            remaining_to_hash: hash_limit,
            bytes_written: 0,
        }
    }
    fn finalize(self) -> ([u8; 32], u64) {
        let arr = self.hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&arr);
        (out, self.bytes_written)
    }
}

impl<'a, W: Write> Write for HashTeeWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        if self.remaining_to_hash > 0 {
            let take = n.min(self.remaining_to_hash);
            self.hasher.update(&buf[..take]);
            self.remaining_to_hash -= take;
        }
        self.bytes_written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::compress::{NxCompressOptions, compress_container};
    use crate::nintendo::nx::constants::{NCA_FS_ENTRY_OFFSET, NCA_FS_HEADER_OFFSET, NCA3_MAGIC};
    use crate::nintendo::nx::crypto::aes_ctr::apply_ctr;
    use crate::nintendo::nx::crypto::aes_xts::encrypt_nca_header;
    use crate::nintendo::nx::models::nca::{FsHeader, initial_ctr_for_offset};
    use crate::nintendo::nx::models::pfs0;
    use crate::nintendo::nx::ncz::compress::NczMode;
    use crate::nintendo::nx::test_fixtures::{
        TEST_BODY_KEY, encrypt_key_area_block, synthetic_keyset,
    };
    use crate::util::NoProgress;
    use sha2::{Digest, Sha256};
    use std::fs;

    fn build_synthetic_nca(plaintext_len: usize) -> Vec<u8> {
        const ENC_AES_CTR: u8 = 3;
        let mut header = [0u8; 0xC00];
        header[0x200..0x204].copy_from_slice(&NCA3_MAGIC);
        header[0x207] = 0;
        header[0x220] = 1;

        let section_start_byte = 0x4000u64;
        let section_end_byte = section_start_byte + plaintext_len as u64;
        let start_sector = (section_start_byte / 0x200) as u32;
        let end_sector = (section_end_byte / 0x200) as u32;
        header[NCA_FS_ENTRY_OFFSET..NCA_FS_ENTRY_OFFSET + 4]
            .copy_from_slice(&start_sector.to_le_bytes());
        header[NCA_FS_ENTRY_OFFSET + 4..NCA_FS_ENTRY_OFFSET + 8]
            .copy_from_slice(&end_sector.to_le_bytes());

        let fs0_off = NCA_FS_HEADER_OFFSET;
        header[fs0_off + 4] = ENC_AES_CTR;
        let ctr_low: u32 = 0xCAFEBABE;
        let ctr_high: u32 = 0xDEADBEEF;
        header[fs0_off + 0x140..fs0_off + 0x144].copy_from_slice(&ctr_low.to_le_bytes());
        header[fs0_off + 0x144..fs0_off + 0x148].copy_from_slice(&ctr_high.to_le_bytes());

        let key_area = encrypt_key_area_block([[0x11; 16], [0x22; 16], TEST_BODY_KEY, [0x44; 16]]);
        header[0x300..0x340].copy_from_slice(&key_area);

        let keys = synthetic_keyset();
        encrypt_nca_header(&mut header, keys.header_key().unwrap()).unwrap();

        let mut nca = vec![0u8; section_start_byte as usize];
        nca[..0xC00].copy_from_slice(&header);

        let plaintext: Vec<u8> = (0..plaintext_len).map(|i| (i & 0xFF) as u8).collect();
        let mut encrypted = plaintext.clone();
        let counter = initial_ctr_for_offset(
            &FsHeader {
                section_ctr_low: ctr_low,
                section_ctr_high: ctr_high,
                ..Default::default()
            },
            section_start_byte,
        );
        apply_ctr(&TEST_BODY_KEY, &counter, &mut encrypted).unwrap();
        nca.extend_from_slice(&encrypted);
        nca
    }

    fn build_synthetic_nsp(nca_bytes: &[u8]) -> Vec<u8> {
        let specs = vec![
            ("game.nca".to_string(), nca_bytes.len() as u64),
            ("ticket.tik".to_string(), 16),
        ];
        let hdr = pfs0::build_header(&specs, &pfs0::Pfs0LayoutHints::default()).unwrap();
        let mut out = hdr.bytes;
        out.extend_from_slice(nca_bytes);
        out.extend_from_slice(&[0xAB; 16]);
        out
    }

    #[test]
    fn nsp_round_trip_through_files() {
        let nca = build_synthetic_nca(0x40200);
        let nsp_blob = build_synthetic_nsp(&nca);

        let dir = tempfile::tempdir().unwrap();
        let nsp_path = dir.path().join("game.nsp");
        let nsz_path = dir.path().join("game.nsz");
        let recovered_path = dir.path().join("recovered.nsp");
        fs::write(&nsp_path, &nsp_blob).unwrap();

        let keys = synthetic_keyset();
        compress_container(
            &nsp_path,
            &nsz_path,
            NxCompressOptions {
                level: 3,
                mode: NczMode::Solid,
            },
            &keys,
            &NoProgress,
        )
        .unwrap();

        decompress_container(&nsz_path, &recovered_path, &keys, &NoProgress).unwrap();

        let recovered = fs::read(&recovered_path).unwrap();
        let original_sha = Sha256::digest(&nsp_blob);
        let recovered_sha = Sha256::digest(&recovered);
        assert_eq!(
            original_sha.as_slice(),
            recovered_sha.as_slice(),
            "round trip lost bytes (orig={}, rec={})",
            nsp_blob.len(),
            recovered.len()
        );
    }
}
