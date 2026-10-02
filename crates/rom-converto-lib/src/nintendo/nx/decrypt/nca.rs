//! Plaintext (DNCA) rewrite of one NCA.
//!
//! The rewrite keeps every byte offset: the 0xC00 header is emitted
//! as plaintext with the `DNCA` magic and every FS header marked
//! unencrypted, and each section body is AES-CTR decrypted in place.
//! [`NcaPlainPlan::open`] resolves, once per NCA, which byte ranges
//! carry which CTR counter prefix:
//!
//! - `AesCtr` (3): the whole section under the FS header's IV.
//! - `AesCtrSkipLayerHash` (5): only from the hash target offset on;
//!   the hash layers before it are stored plaintext.
//! - `AesCtrEx` (4) and its skip-layer-hash twin (6): the BKTR data
//!   region `[0, aes_ctr_ex_offset)` per bucket-tree entry, each run
//!   under its own generation (or plaintext), then the tables and
//!   everything after them under the FS header's IV.
//!
//! `AesXts` (2) sections and sparse layers are rejected; NxEmu cannot
//! mount them. A plaintext NCA (NCA3 magic, header and sections in the
//! clear) only needs the header rewrite.

use std::fs::File;
use std::io::Write;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::nintendo::nx::constants::{
    DNCA_MAGIC, ENC_AES_CTR, ENC_AES_CTR_EX, ENC_AES_CTR_EX_SKIP_LAYER_HASH,
    ENC_AES_CTR_SKIP_LAYER_HASH, ENC_NONE, NCA_FS_HEADER_OFFSET, NCA_FS_HEADER_STRIDE,
    NCA_HEADER_SIZE, NCA_XTS_SECTOR, PFS0_MAGIC,
};
use crate::nintendo::nx::crypto::aes_ctr::apply_ctr_at;
use crate::nintendo::nx::decrypt::bucket_tree;
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::models::nca::{
    FS_TYPE_PARTITION_FS, FS_TYPE_ROMFS, FsHeader, HASH_TYPE_HIERARCHICAL_INTEGRITY,
    HASH_TYPE_HIERARCHICAL_SHA256,
};
use crate::nintendo::nx::romfs::ROMFS_HEADER_SIZE;
use crate::nintendo::nx::walker::{NcaSection, NcaWalker};
use crate::util::bytes::u64_le;
use crate::util::pread::file_read_exact_at;
use crate::util::{CancelToken, Cancelled, ProgressReporter};

const FS_HEADER_HASH_OFFSET: usize = 0x280;
const FS_HEADER_ENCRYPTION_TYPE: usize = 0x04;
const CHUNK: usize = 4 * 1024 * 1024;

/// One CTR-encrypted byte run of the NCA, in NCA-relative offsets,
/// with the 8-byte counter prefix (`secure_value || generation`) its
/// keystream starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CtrRun {
    start: u64,
    end: u64,
    ctr_iv: [u8; 8],
}

/// Everything needed to stream an NCA out as plaintext: the patched
/// header, the body key, and the encrypted runs in ascending order.
pub struct NcaPlainPlan {
    header: Box<[u8; NCA_HEADER_SIZE]>,
    key: [u8; 16],
    runs: Vec<CtrRun>,
    size: u64,
}

impl NcaPlainPlan {
    /// Opens the NCA of `size` bytes at `nca_offset` and resolves its
    /// plaintext layout. `name` only labels errors.
    pub fn open(
        file: Arc<File>,
        nca_offset: u64,
        size: u64,
        name: &str,
        keys: &KeySet,
    ) -> NxResult<Self> {
        if size < NCA_HEADER_SIZE as u64 {
            return Err(NxError::NcaTruncated(name.to_string()));
        }
        let mut raw_magic = [0u8; 4];
        file_read_exact_at(&file, &mut raw_magic, nca_offset + 0x200)?;
        if raw_magic == DNCA_MAGIC {
            return Err(NxError::AlreadyDecrypted(name.to_string()));
        }

        let walker = NcaWalker::open(file.clone(), nca_offset, size, keys)?;
        let mut runs = Vec::new();
        for section in &walker.sections {
            let section_end = (section.raw_offset - nca_offset).checked_add(section.raw_size);
            if section_end.is_none_or(|end| end > size) {
                return Err(NxError::NcaTruncated(name.to_string()));
            }
            check_section_layout(
                &walker.header.fs_headers[section.index],
                section.index,
                name,
            )?;
            if !walker.plaintext {
                section_runs(&walker, section, name, &mut runs)?;
            }
        }
        runs.sort_by_key(|r| r.start);
        if runs.windows(2).any(|w| w[0].end > w[1].start) {
            return Err(NxError::OverlappingSections(name.to_string()));
        }

        let key = walker
            .sections
            .first()
            .map_or([0u8; 16], |section| section.key);
        let header = plaintext_header(&walker);
        let plan = Self {
            header,
            key,
            runs,
            size,
        };
        if !walker.plaintext {
            for section in &walker.sections {
                plan.check_key(&file, nca_offset, &walker, section, name)?;
            }
        }
        Ok(plan)
    }

    /// Catches a wrong title key or key area key before anything is
    /// written: a plain CTR section must decrypt to a `PFS0` magic or
    /// a RomFS header at its hash target. BKTR sections are skipped
    /// since their data layer is only meaningful through the patch
    /// tables, and compressed sections since the hash target holds a
    /// compressed stream.
    fn check_key(
        &self,
        file: &File,
        nca_offset: u64,
        walker: &NcaWalker,
        section: &NcaSection,
        name: &str,
    ) -> NxResult<()> {
        let fs = walker.header.fs_headers[section.index];
        if !matches!(
            fs.encryption_type,
            ENC_AES_CTR | ENC_AES_CTR_SKIP_LAYER_HASH
        ) || fs.has_compression_layer
        {
            return Ok(());
        }
        let Some(target) = fs
            .hash_target_offset
            .filter(|t| t.checked_add(16).is_some_and(|end| end <= section.raw_size))
        else {
            return Ok(());
        };
        let at = section.raw_offset - nca_offset + target;
        let mut probe = [0u8; 16];
        file_read_exact_at(file, &mut probe, nca_offset + at)?;
        self.apply(at, &mut probe)?;
        let valid = match fs.fs_type {
            FS_TYPE_ROMFS => u64_le(&probe, 0) == ROMFS_HEADER_SIZE,
            FS_TYPE_PARTITION_FS => probe[..4] == PFS0_MAGIC,
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(NxError::WrongKey {
                nca: name.to_string(),
                section: section.index,
            })
        }
    }

    /// Streams the plaintext NCA to `out`, reading the encrypted bytes
    /// from `file` at `nca_offset`.
    pub fn write_plain<W: Write>(
        &self,
        file: &File,
        nca_offset: u64,
        out: &mut W,
        progress: &dyn ProgressReporter,
        cancel: Option<&CancelToken>,
    ) -> NxResult<()> {
        out.write_all(self.header.as_slice())?;
        progress.inc(NCA_HEADER_SIZE as u64);

        let mut buf = vec![0u8; CHUNK];
        let mut pos = NCA_HEADER_SIZE as u64;
        while pos < self.size {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return Err(Cancelled.into());
            }
            let take = (CHUNK as u64).min(self.size - pos) as usize;
            file_read_exact_at(file, &mut buf[..take], nca_offset + pos)?;
            self.apply(pos, &mut buf[..take])?;
            out.write_all(&buf[..take])?;
            progress.inc(take as u64);
            pos += take as u64;
        }
        Ok(())
    }

    /// The first `len` plaintext bytes of the NCA (clamped to its
    /// size), for recomputing the HFS0 entry hash of a rewritten NCA.
    pub fn plain_prefix(&self, file: &File, nca_offset: u64, len: u64) -> NxResult<Vec<u8>> {
        let len = len.min(self.size) as usize;
        let head = len.min(NCA_HEADER_SIZE);
        let mut buf = vec![0u8; len];
        buf[..head].copy_from_slice(&self.header[..head]);
        if len > head {
            file_read_exact_at(file, &mut buf[head..], nca_offset + head as u64)?;
            self.apply(head as u64, &mut buf[head..])?;
        }
        Ok(buf)
    }

    /// Decrypts the encrypted runs overlapping the chunk `buf` that
    /// sits at NCA offset `pos`; bytes outside any run pass through.
    fn apply(&self, pos: u64, buf: &mut [u8]) -> NxResult<()> {
        let end = pos + buf.len() as u64;
        let first = self.runs.partition_point(|r| r.end <= pos);
        for run in self.runs[first..].iter().take_while(|r| r.start < end) {
            let start = run.start.max(pos);
            let stop = run.end.min(end);
            let slice = &mut buf[(start - pos) as usize..(stop - pos) as usize];
            apply_ctr_at(&self.key, &run.ctr_iv, start, slice)?;
        }
        Ok(())
    }
}

/// Rejects section layouts NxEmu cannot mount, whatever the input's
/// encryption state: sparse layers, and any hash type other than the
/// SHA-256 and integrity layers.
fn check_section_layout(fs: &FsHeader, index: usize, name: &str) -> NxResult<()> {
    if fs.sparse_generation != 0 {
        return Err(NxError::SparseSectionUnsupported {
            nca: name.to_string(),
            section: index,
        });
    }
    if !matches!(
        fs.hash_type,
        HASH_TYPE_HIERARCHICAL_SHA256 | HASH_TYPE_HIERARCHICAL_INTEGRITY
    ) {
        return Err(NxError::UnsupportedHashType {
            nca: name.to_string(),
            section: index,
            hash_type: fs.hash_type,
        });
    }
    Ok(())
}

/// Appends the CTR runs of one section to `runs`.
fn section_runs(
    walker: &NcaWalker,
    section: &NcaSection,
    name: &str,
    runs: &mut Vec<CtrRun>,
) -> NxResult<()> {
    let fs = walker.header.fs_headers[section.index];
    let start = section.raw_offset - walker.nca_offset();
    let end = start + section.raw_size;
    let header_iv = ctr_iv(section.section_ctr_high, section.section_ctr_low);
    match fs.encryption_type {
        ENC_NONE => {}
        ENC_AES_CTR => runs.push(CtrRun {
            start,
            end,
            ctr_iv: header_iv,
        }),
        ENC_AES_CTR_SKIP_LAYER_HASH => {
            // A skip-layer-hash section needs a hash target inside the
            // section to know where decryption starts; anything else is
            // a malformed FS header.
            let target = fs
                .hash_target_offset
                .filter(|t| *t <= section.raw_size)
                .ok_or(NxError::InvalidNcaHeader)?;
            runs.push(CtrRun {
                start: start + target,
                end,
                ctr_iv: header_iv,
            });
        }
        ENC_AES_CTR_EX | ENC_AES_CTR_EX_SKIP_LAYER_HASH => {
            let patch = fs.patch;
            let invalid = |reason| NxError::InvalidBucketTree {
                nca: name.to_string(),
                section: section.index,
                reason,
            };
            let table_len = bucket_tree::table_size(patch.aes_ctr_ex_entry_count)
                .ok_or_else(|| invalid("AesCtrEx entry count is out of range"))?;
            let table_end = patch
                .aes_ctr_ex_size
                .checked_next_multiple_of(NCA_XTS_SECTOR as u64)
                .and_then(|aligned| patch.aes_ctr_ex_offset.checked_add(aligned))
                .filter(|e| {
                    table_len <= patch.aes_ctr_ex_size
                        && patch.aes_ctr_ex_offset.is_multiple_of(16)
                        && *e <= section.raw_size
                })
                .ok_or_else(|| invalid("AesCtrEx table lies outside the section"))?;
            // With a hashed meta layer the FS driver reads everything
            // from the indirect table onward as one plain run under
            // the header IV; without it, per-entry runs reach the
            // AesCtrEx table.
            let plain_from = if patch.has_meta_hash_layer() {
                let indirect_end = patch.indirect_offset.checked_add(patch.indirect_size);
                let hash_end = patch.meta_hash_offset.checked_add(patch.meta_hash_size);
                let ordered = indirect_end.is_some_and(|e| e <= patch.aes_ctr_ex_offset)
                    && table_end <= patch.meta_hash_offset
                    && hash_end.is_some_and(|e| e <= section.raw_size);
                if !ordered {
                    return Err(invalid("patch tables and meta hash data are out of order"));
                }
                patch.indirect_offset
            } else {
                patch.aes_ctr_ex_offset
            };
            let mut table = vec![0u8; table_len as usize];
            walker.read_section_plain(section, patch.aes_ctr_ex_offset, &mut table)?;
            let entries = bucket_tree::parse_entries(&table, patch.aes_ctr_ex_entry_count)
                .map_err(invalid)?;
            for (i, entry) in entries.iter().enumerate() {
                let run_end = entries
                    .get(i + 1)
                    .map_or(plain_from, |next| next.offset)
                    .min(plain_from);
                if entry.encrypted && entry.offset < run_end {
                    runs.push(CtrRun {
                        start: start + entry.offset,
                        end: start + run_end,
                        ctr_iv: ctr_iv(section.section_ctr_high, entry.generation),
                    });
                }
            }
            runs.push(CtrRun {
                start: start + plain_from,
                end,
                ctr_iv: header_iv,
            });
        }
        other => return Err(NxError::UnsupportedEncryption(other)),
    }
    Ok(())
}

/// Counter prefix `secure_value BE || generation BE`, the upper half
/// of the 16-byte AES-CTR counter (see `initial_ctr_for_offset`).
fn ctr_iv(secure_value: u32, generation: u32) -> [u8; 8] {
    let mut iv = [0u8; 8];
    iv[..4].copy_from_slice(&secure_value.to_be_bytes());
    iv[4..].copy_from_slice(&generation.to_be_bytes());
    iv
}

/// The DNCA header: the XTS-decrypted header with the `DNCA` magic,
/// every present FS header flagged unencrypted, and the FS header
/// hashes recomputed to match.
fn plaintext_header(walker: &NcaWalker) -> Box<[u8; NCA_HEADER_SIZE]> {
    let mut header = walker.decrypted_header.clone();
    header[0x200..0x204].copy_from_slice(&DNCA_MAGIC);
    for section in &walker.sections {
        let fs = NCA_FS_HEADER_OFFSET + section.index * NCA_FS_HEADER_STRIDE;
        header[fs + FS_HEADER_ENCRYPTION_TYPE] = ENC_NONE;
        let hash = Sha256::digest(&header[fs..fs + NCA_FS_HEADER_STRIDE]);
        let at = FS_HEADER_HASH_OFFSET + section.index * 0x20;
        header[at..at + 0x20].copy_from_slice(&hash);
    }
    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::constants::{NCA_FS_ENTRY_OFFSET, NCA3_MAGIC};
    use crate::nintendo::nx::crypto::aes_xts::encrypt_nca_header;
    use crate::nintendo::nx::decrypt::bucket_tree::{AesCtrExEntry, build_table};
    use crate::nintendo::nx::keys::KeyAreaKind;
    use crate::nintendo::nx::models::nca::NcaHeader;
    use crate::nintendo::nx::test_fixtures::{
        TEST_BODY_KEY, TEST_HEADER_KEY, encrypt_key_area_block, synthetic_keyset,
    };
    use crate::util::NoProgress;
    use tempfile::NamedTempFile;

    const SECURE_VALUE: u32 = 0xA1B2C3D4;
    const GENERATION: u32 = 0x00000003;

    /// Plaintext header with one section per `(encryption_type,
    /// plain body)` pair, sections laid out back to back from 0x4000.
    fn plain_header(sections: &[(u8, &[u8])]) -> ([u8; NCA_HEADER_SIZE], Vec<u64>) {
        let mut header = [0u8; NCA_HEADER_SIZE];
        header[0x200..0x204].copy_from_slice(&NCA3_MAGIC);
        header[0x220] = 1;
        header[0x300..0x340].copy_from_slice(&encrypt_key_area_block([
            [0x11; 16],
            [0x22; 16],
            TEST_BODY_KEY,
            [0x44; 16],
        ]));
        let mut starts = Vec::new();
        let mut at = 0x4000u64;
        for (i, (enc, body)) in sections.iter().enumerate() {
            assert!(body.len().is_multiple_of(0x200));
            let entry = NCA_FS_ENTRY_OFFSET + i * 0x10;
            header[entry..entry + 4].copy_from_slice(&((at / 0x200) as u32).to_le_bytes());
            let end = at + body.len() as u64;
            header[entry + 4..entry + 8].copy_from_slice(&((end / 0x200) as u32).to_le_bytes());
            let fs = NCA_FS_HEADER_OFFSET + i * NCA_FS_HEADER_STRIDE;
            header[fs + 2] = FS_TYPE_ROMFS;
            header[fs + 3] = HASH_TYPE_HIERARCHICAL_SHA256;
            header[fs + 4] = *enc;
            header[fs + 0x140..fs + 0x144].copy_from_slice(&GENERATION.to_le_bytes());
            header[fs + 0x144..fs + 0x148].copy_from_slice(&SECURE_VALUE.to_le_bytes());
            starts.push(at);
            at = end;
        }
        (header, starts)
    }

    fn encrypt_run(nca: &mut [u8], start: u64, end: u64, generation: u32) {
        apply_ctr_at(
            &TEST_BODY_KEY,
            &ctr_iv(SECURE_VALUE, generation),
            start,
            &mut nca[start as usize..end as usize],
        )
        .unwrap();
    }

    fn assemble(header: [u8; NCA_HEADER_SIZE], bodies: &[&[u8]]) -> Vec<u8> {
        let mut nca = vec![0u8; 0x4000];
        let mut sealed = header;
        encrypt_nca_header(&mut sealed, &TEST_HEADER_KEY).unwrap();
        nca[..NCA_HEADER_SIZE].copy_from_slice(&sealed);
        for body in bodies {
            nca.extend_from_slice(body);
        }
        nca
    }

    fn decrypt(nca: &[u8]) -> Vec<u8> {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(nca).unwrap();
        let file = Arc::new(File::open(tmp.path()).unwrap());
        let plan = NcaPlainPlan::open(
            file.clone(),
            0,
            nca.len() as u64,
            "test.nca",
            &synthetic_keyset(),
        )
        .unwrap();
        let mut out = Vec::new();
        plan.write_plain(&file, 0, &mut out, &NoProgress, None)
            .unwrap();
        out
    }

    fn body(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn ctr_section_decrypts_and_header_is_patched() {
        let plain = body(0x1000, 7);
        let (header, starts) = plain_header(&[(ENC_AES_CTR, &plain)]);
        let mut nca = assemble(header, &[&plain]);
        encrypt_run(&mut nca, starts[0], starts[0] + 0x1000, GENERATION);

        let out = decrypt(&nca);
        assert_eq!(out.len(), nca.len());
        assert_eq!(&out[0x4000..0x5000], plain.as_slice());
        assert_eq!(&out[0x200..0x204], b"DNCA");
        let fs = NCA_FS_HEADER_OFFSET;
        assert_eq!(out[fs + 4], ENC_NONE);
        let expected: [u8; 32] = Sha256::digest(&out[fs..fs + 0x200]).into();
        assert_eq!(&out[0x280..0x2A0], &expected);
        // Everything else in the header is the plaintext original.
        assert_eq!(&out[0x300..0x340], &header[0x300..0x340]);
        assert_eq!(&out[0x204..0x280], &header[0x204..0x280]);
    }

    #[test]
    fn unencrypted_section_passes_through() {
        let plain = body(0x400, 9);
        let (header, _) = plain_header(&[(ENC_NONE, &plain)]);
        let nca = assemble(header, &[&plain]);
        let out = decrypt(&nca);
        assert_eq!(&out[0x4000..], plain.as_slice());
    }

    /// Marks section 0 as a HierarchicalSha256 PartitionFs whose data
    /// layer starts at `target`.
    fn partition_fs_header(header: &mut [u8; NCA_HEADER_SIZE], target: u64) {
        let fs = NCA_FS_HEADER_OFFSET;
        header[fs + 2] = FS_TYPE_PARTITION_FS;
        header[fs + 3] = HASH_TYPE_HIERARCHICAL_SHA256;
        header[fs + 0x2C..fs + 0x30].copy_from_slice(&2u32.to_le_bytes());
        header[fs + 0x40..fs + 0x48].copy_from_slice(&target.to_le_bytes());
    }

    #[test]
    fn skip_layer_hash_leaves_hash_layers_plain() {
        let mut plain = body(0x1000, 3);
        plain[0x800..0x804].copy_from_slice(&PFS0_MAGIC);
        let (mut header, starts) = plain_header(&[(ENC_AES_CTR_SKIP_LAYER_HASH, &plain)]);
        partition_fs_header(&mut header, 0x800);
        assert_eq!(
            NcaHeader::parse(&header).unwrap().fs_headers[0].hash_target_offset,
            Some(0x800)
        );
        let mut nca = assemble(header, &[&plain]);
        encrypt_run(&mut nca, starts[0] + 0x800, starts[0] + 0x1000, GENERATION);
        assert_eq!(&nca[0x4000..0x4800], &plain[..0x800]);

        let out = decrypt(&nca);
        assert_eq!(&out[0x4000..0x5000], plain.as_slice());
    }

    #[test]
    fn wrong_key_is_detected_before_writing() {
        let mut plain = body(0x1000, 4);
        plain[0x200..0x204].copy_from_slice(&PFS0_MAGIC);
        let (mut header, starts) = plain_header(&[(ENC_AES_CTR, &plain)]);
        partition_fs_header(&mut header, 0x200);
        let mut nca = assemble(header, &[&plain]);
        encrypt_run(&mut nca, starts[0], starts[0] + 0x1000, GENERATION);
        assert_eq!(&decrypt(&nca)[0x4000..0x5000], plain.as_slice());

        let mut keys = synthetic_keyset();
        keys.key_area_keys
            .insert((KeyAreaKind::Application, 0), [0x99; 16]);
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&nca).unwrap();
        let file = Arc::new(File::open(tmp.path()).unwrap());
        let err = NcaPlainPlan::open(file, 0, nca.len() as u64, "c.nca", &keys)
            .err()
            .unwrap();
        assert!(matches!(err, NxError::WrongKey { section: 0, .. }));
    }

    #[test]
    fn bktr_section_uses_per_entry_generations() {
        // Data region 0x2000 with three runs, then the AesCtrEx table.
        let data_len = 0x2000usize;
        let entries = [
            AesCtrExEntry {
                offset: 0,
                encrypted: true,
                generation: 5,
            },
            AesCtrExEntry {
                offset: 0x800,
                encrypted: false,
                generation: 0,
            },
            AesCtrExEntry {
                offset: 0x1000,
                encrypted: true,
                generation: 9,
            },
        ];
        let table = build_table(&entries);
        let mut plain = body(data_len, 1);
        plain.extend_from_slice(&table);
        plain.resize(plain.len().next_multiple_of(0x200), 0);

        let (mut header, starts) = plain_header(&[(ENC_AES_CTR_EX, &plain)]);
        let fs = NCA_FS_HEADER_OFFSET;
        header[fs + 0x120..fs + 0x128].copy_from_slice(&(data_len as u64).to_le_bytes());
        header[fs + 0x128..fs + 0x130].copy_from_slice(&(table.len() as u64).to_le_bytes());
        header[fs + 0x138..fs + 0x13C].copy_from_slice(&(entries.len() as u32).to_le_bytes());

        let mut nca = assemble(header, &[&plain]);
        let s = starts[0];
        encrypt_run(&mut nca, s, s + 0x800, 5);
        encrypt_run(&mut nca, s + 0x1000, s + data_len as u64, 9);
        encrypt_run(
            &mut nca,
            s + data_len as u64,
            s + plain.len() as u64,
            GENERATION,
        );

        let out = decrypt(&nca);
        assert_eq!(&out[0x4000..], plain.as_slice());
    }

    #[test]
    fn bktr_meta_hash_layer_reads_tables_under_header_iv() {
        // Data 0x1000, indirect table 0x400 at 0x1000, AesCtrEx table
        // after it, then 0x200 of meta hash data; the bucket entries
        // claim a foreign generation over the tables, which the meta
        // hash layer overrides.
        let data_len = 0x1000u64;
        let entries = [
            AesCtrExEntry {
                offset: 0,
                encrypted: true,
                generation: 5,
            },
            AesCtrExEntry {
                offset: 0x1000,
                encrypted: true,
                generation: 42,
            },
        ];
        let table = build_table(&entries);
        let indirect_off = data_len;
        let indirect_size = 0x400u64;
        let ex_off = indirect_off + indirect_size;
        let hash_off = ex_off + table.len() as u64;
        let hash_size = 0x200u64;
        let mut plain = body((hash_off + hash_size) as usize, 6);
        plain[ex_off as usize..hash_off as usize].copy_from_slice(&table);

        let (mut header, starts) = plain_header(&[(ENC_AES_CTR_EX, &plain)]);
        let fs = NCA_FS_HEADER_OFFSET;
        header[fs + 0x100..fs + 0x108].copy_from_slice(&indirect_off.to_le_bytes());
        header[fs + 0x108..fs + 0x110].copy_from_slice(&indirect_size.to_le_bytes());
        header[fs + 0x120..fs + 0x128].copy_from_slice(&ex_off.to_le_bytes());
        header[fs + 0x128..fs + 0x130].copy_from_slice(&(table.len() as u64).to_le_bytes());
        header[fs + 0x138..fs + 0x13C].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        header[fs + 0x1A0..fs + 0x1A8].copy_from_slice(&hash_off.to_le_bytes());
        header[fs + 0x1A8..fs + 0x1B0].copy_from_slice(&hash_size.to_le_bytes());

        let mut nca = assemble(header, &[&plain]);
        let s = starts[0];
        encrypt_run(&mut nca, s, s + data_len, 5);
        encrypt_run(
            &mut nca,
            s + indirect_off,
            s + plain.len() as u64,
            GENERATION,
        );

        let out = decrypt(&nca);
        assert_eq!(&out[0x4000..], plain.as_slice());
    }

    #[test]
    fn rejects_sparse_sections_and_already_decrypted_input() {
        let plain = body(0x400, 2);
        let (mut header, _) = plain_header(&[(ENC_AES_CTR, &plain)]);
        header[NCA_FS_HEADER_OFFSET + 0x170] = 1;
        let nca = assemble(header, &[&plain]);
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&nca).unwrap();
        let file = Arc::new(File::open(tmp.path()).unwrap());
        let err = NcaPlainPlan::open(file, 0, nca.len() as u64, "a.nca", &synthetic_keyset())
            .err()
            .unwrap();
        assert!(matches!(
            err,
            NxError::SparseSectionUnsupported { section: 0, .. }
        ));

        // The same layout guard fires on a plaintext NCA (NCA3 magic),
        // whose header is stored in the clear and needs no key.
        let mut hactool = nca.clone();
        hactool[..NCA_HEADER_SIZE].copy_from_slice(&header);
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&hactool).unwrap();
        let file = Arc::new(File::open(tmp.path()).unwrap());
        let err = NcaPlainPlan::open(file, 0, hactool.len() as u64, "h.nca", &KeySet::default())
            .err()
            .unwrap();
        assert!(matches!(
            err,
            NxError::SparseSectionUnsupported { section: 0, .. }
        ));

        let (header, _) = plain_header(&[(ENC_AES_CTR, &plain)]);
        let dnca = decrypt(&assemble(header, &[&plain]));
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&dnca).unwrap();
        let file = Arc::new(File::open(tmp.path()).unwrap());
        let err = NcaPlainPlan::open(file, 0, dnca.len() as u64, "b.nca", &synthetic_keyset())
            .err()
            .unwrap();
        assert!(matches!(err, NxError::AlreadyDecrypted(_)));
    }
}
