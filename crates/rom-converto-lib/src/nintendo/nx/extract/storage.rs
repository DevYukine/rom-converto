//! Random-access readers over decrypted NCA section bytes: one section on
//! its own, or the virtual storage an update's indirect table builds over
//! the base and patch RomFS sections.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::nintendo::nx::decrypt::bucket_tree::{IndirectEntry, parse_entries, table_size};
use crate::nintendo::nx::decrypt::nca::{CtrRun, apply_runs, check_section_layout, section_runs};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::models::nca::FsHeader;
use crate::nintendo::nx::ncz::NczReader;
use crate::nintendo::nx::romfs::MAX_TABLE_SIZE;
use crate::nintendo::nx::walker::{NcaInput, NcaWalker};
use crate::util::{CancelToken, Cancelled, extent_end};
use sha2::{Digest, Sha256};

/// Where one NCA sits: its container, its entry name, and its extent.
#[derive(Debug, Clone)]
pub(crate) struct NcaRef {
    pub path: PathBuf,
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub ncz: bool,
}

/// One section of an NCA.
#[derive(Debug, Clone)]
pub(crate) struct SectionRef {
    pub nca: NcaRef,
    pub index: usize,
}

impl SectionRef {
    /// Opens the NCA under `keys` and this section of it.
    pub(crate) fn open(&self, keys: &KeySet) -> NxResult<SectionStorage> {
        SectionStorage::open(open_nca(&self.nca, keys)?, self.index, &self.nca.name)
    }
}

fn open_nca(nca: &NcaRef, keys: &KeySet) -> NxResult<NcaWalker> {
    let file = Arc::new(File::open(&nca.path)?);
    if !nca.ncz {
        return NcaWalker::open(file, nca.offset, nca.size, keys);
    }
    // An NCZ stores each section decrypted under the header counter
    // alone, which is wrong for a BKTR section's per-generation runs.
    // NczReader re-encrypts that payload back into the original NCA
    // bytes, so the section runs then decrypt it like any other NCA.
    // `nx decompress` takes the container, so the budget error names it too.
    let name = format!("{} in {}", nca.name, nca.path.display());
    let reader = NczReader::open(file, nca.offset, nca.size, keys.header_key()?, &name)?;
    let size = reader.decompressed_nca_size();
    NcaWalker::open(Arc::new(reader), 0, size, keys)
}

/// Decrypted bytes of one NCA section at section-relative offsets.
pub(crate) struct SectionStorage {
    walker: NcaWalker,
    /// Section start in the walker's input.
    start: u64,
    /// Section start within the NCA, where the CTR runs are anchored.
    nca_start: u64,
    size: u64,
    key: [u8; 16],
    runs: Vec<CtrRun>,
    pub fs: FsHeader,
    pub index: usize,
}

impl SectionStorage {
    /// Opens section `index` of `walker`, resolving its CTR runs once.
    /// A plaintext NCA gets no runs, so its bytes pass through.
    ///
    /// # Errors
    /// Fails when the section is absent, runs past the NCA, has a sparse
    /// or compression layer, or carries a malformed AesCtrEx table.
    pub(crate) fn open(walker: NcaWalker, index: usize, name: &str) -> NxResult<Self> {
        let section = walker
            .sections
            .iter()
            .find(|s| s.index == index)
            .ok_or(NxError::InvalidNcaHeader)?;
        let fs = walker.header.fs_headers[index];
        check_section_layout(&fs, index, name, true)?;
        if fs.has_compression_layer {
            return Err(NxError::CompressedSectionUnsupported {
                nca: name.to_string(),
                section: index,
            });
        }
        let nca_start = section.raw_offset - walker.nca_offset();
        if extent_end(nca_start, section.raw_size, walker.nca_size()).is_none() {
            return Err(NxError::NcaTruncated(name.to_string()));
        }
        let mut runs = Vec::new();
        if !walker.plaintext {
            section_runs(&walker, section, name, &mut runs)?;
        }
        Ok(Self {
            start: section.raw_offset,
            nca_start,
            size: section.raw_size,
            key: section.key,
            runs,
            fs,
            index,
            walker,
        })
    }

    /// Returns the section size in bytes.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}

impl NcaInput for SectionStorage {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> NxResult<()> {
        check_range(offset, buf.len(), self.size)?;
        self.walker.read_exact_at(buf, self.start + offset)?;
        apply_runs(&self.key, &self.runs, self.nca_start + offset, buf)
    }
}

/// The patched RomFS section of an update: a virtual storage whose
/// indirect table maps every range onto the base section (storage 0) or
/// the patch section's data (storage 1).
pub(crate) struct IndirectStorage {
    storages: [Box<dyn NcaInput>; 2],
    entries: Vec<IndirectEntry>,
    size: u64,
    cancel: CancelToken,
}

impl IndirectStorage {
    /// Builds the patched storage from the base program's RomFS section
    /// and the update's patch section, reading the indirect table the
    /// patch FS header locates. Reads stop with [`NxError::Cancelled`]
    /// once `cancel` fires.
    ///
    /// # Errors
    /// Fails with [`NxError::InvalidBucketTree`] when the table lies
    /// outside the patch section or any entry is malformed.
    pub(crate) fn open(
        base: SectionStorage,
        patch: SectionStorage,
        name: &str,
        cancel: &CancelToken,
    ) -> NxResult<Self> {
        let info = patch.fs.patch;
        let section = patch.index;
        let invalid = |reason| NxError::InvalidBucketTree {
            nca: name.to_string(),
            section,
            reason,
        };
        let table_len = table_size::<IndirectEntry>(info.indirect_entry_count)
            .ok_or_else(|| invalid("indirect entry count is out of range"))?;
        if table_len > info.indirect_size
            || extent_end(info.indirect_offset, info.indirect_size, patch.size).is_none()
        {
            return Err(invalid("indirect table lies outside the section"));
        }
        let mut table = vec![0u8; table_len as usize];
        patch.read_exact_at(&mut table, info.indirect_offset)?;
        // Patch data precedes the tables; nothing past it is file data.
        let sizes = [base.size, info.indirect_offset];
        Self::new(
            [Box::new(base), Box::new(patch)],
            sizes,
            &table,
            info.indirect_entry_count,
            cancel.clone(),
        )
        .map_err(invalid)
    }

    fn new(
        storages: [Box<dyn NcaInput>; 2],
        sizes: [u64; 2],
        table: &[u8],
        entry_count: u32,
        cancel: CancelToken,
    ) -> Result<Self, &'static str> {
        let (entries, size) = parse_entries::<IndirectEntry>(table, entry_count)?;
        for (i, entry) in entries.iter().enumerate() {
            let end = entries.get(i + 1).map_or(size, |next| next.virtual_offset);
            let len = end
                .checked_sub(entry.virtual_offset)
                .filter(|len| *len > 0)
                .ok_or("indirect entry starts past the end of the patched storage")?;
            extent_end(
                entry.physical_offset,
                len,
                sizes[entry.storage_index as usize],
            )
            .ok_or("indirect entry maps outside its storage")?;
        }
        Ok(Self {
            storages,
            entries,
            size,
            cancel,
        })
    }

    /// Returns the size of the patched virtual storage.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}

impl NcaInput for IndirectStorage {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> NxResult<()> {
        check_range(offset, buf.len(), self.size)?;
        let mut done = 0;
        while done < buf.len() {
            // A scattered table splits one read into many, and over a
            // solid NCZ every backward jump restarts the decoder.
            if self.cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let pos = offset + done as u64;
            // The first entry starts at 0 and `pos` is inside the storage,
            // so some entry always covers it.
            let i = self.entries.partition_point(|e| e.virtual_offset <= pos) - 1;
            let entry = self.entries[i];
            let end = self
                .entries
                .get(i + 1)
                .map_or(self.size, |next| next.virtual_offset);
            let take = (end - pos).min((buf.len() - done) as u64) as usize;
            self.storages[entry.storage_index as usize].read_exact_at(
                &mut buf[done..done + take],
                entry.physical_offset + (pos - entry.virtual_offset),
            )?;
            done += take;
        }
        Ok(())
    }
}

/// The filesystem layer of section `index` (its PFS0 or RomFS image) as
/// an offset and size inside `storage_size` bytes of section storage, or
/// of the patched storage for an update.
///
/// # Errors
/// Fails with [`NxError::InvalidDataLayer`] when the FS header has no
/// layer or places it outside the storage.
pub(crate) fn data_layer(
    fs: &FsHeader,
    index: usize,
    storage_size: u64,
    name: &str,
) -> NxResult<(u64, u64)> {
    fs.hash_target_offset
        .zip(fs.hash_target_size)
        .filter(|(offset, size)| extent_end(*offset, *size, storage_size).is_some())
        .ok_or_else(|| NxError::InvalidDataLayer {
            nca: name.to_string(),
            section: index,
        })
}

/// Tells a wrong title key or key area key apart from a malformed
/// filesystem once the filesystem failed to parse. The FS header sits
/// under the header key, so its master hash holds even when the body
/// key is wrong, and the first hash block of a body decrypted with the
/// wrong key does not match it. SHA3 sections carry no checked master
/// hash.
///
/// # Errors
/// Fails with [`NxError::WrongKey`] on a mismatch, and with
/// [`NxError::InvalidDataLayer`] when the hashed block lies outside the
/// `storage_size` bytes of `storage` or is larger than 256 MiB.
pub(crate) fn check_master_hash(
    storage: &dyn NcaInput,
    storage_size: u64,
    fs: &FsHeader,
    index: usize,
    name: &str,
) -> NxResult<()> {
    let Some(master) = fs.master_hash else {
        return Ok(());
    };
    if master.block > MAX_TABLE_SIZE
        || extent_end(master.offset, master.size, storage_size).is_none()
    {
        return Err(NxError::InvalidDataLayer {
            nca: name.to_string(),
            section: index,
        });
    }
    let mut block = vec![0u8; master.block as usize];
    storage.read_exact_at(&mut block[..master.size as usize], master.offset)?;
    if Sha256::digest(&block)[..] != master.digest {
        return Err(NxError::WrongKey {
            nca: name.to_string(),
            section: index,
        });
    }
    Ok(())
}

fn check_range(offset: u64, len: usize, size: u64) -> NxResult<()> {
    match extent_end(offset, len as u64, size) {
        Some(_) => Ok(()),
        None => Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::decrypt::bucket_tree::build_table;
    use crate::nintendo::nx::test_fixtures::{
        MemInput, build_nca, build_plaintext_nca, noise, patch_romfs_section, romfs_section,
        synthetic_keyset,
    };
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn open_section(nca: &[u8], index: usize) -> NxResult<SectionStorage> {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(nca).unwrap();
        let file = Arc::new(File::open(tmp.path()).unwrap());
        let walker = NcaWalker::open(file, 0, nca.len() as u64, &synthetic_keyset())?;
        SectionStorage::open(walker, index, "t.nca")
    }

    fn read(storage: &dyn NcaInput, offset: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        storage.read_exact_at(&mut buf, offset).unwrap();
        buf
    }

    #[test]
    fn aes_ctr_ex_section_decrypts_every_run_at_any_offset() {
        let image = noise(0x3000, 5);
        let section = patch_romfs_section(&image, &[]);
        let plain = section.body.clone();
        let storage = open_section(&build_nca(0, 1, [0; 16], &[section]), 0).unwrap();

        assert_eq!(read(&storage, 0, plain.len()), plain);
        // Unaligned reads straddling the generation switch at 0x800, the
        // unencrypted run up to 0x1000, and the tables' header-IV run.
        let tables = storage.fs.patch.indirect_offset;
        for (offset, len) in [
            (0x7F3, 0x20),
            (0xFFF, 3),
            (0x123, 0x1000),
            (tables - 7, 0x31),
        ] {
            assert_eq!(
                read(&storage, offset, len),
                &plain[offset as usize..offset as usize + len],
                "{offset:#x}+{len:#x}"
            );
        }
    }

    #[test]
    fn plaintext_nca_reads_raw_and_out_of_range_reads_fail() {
        let section = patch_romfs_section(&noise(0x1000, 6), &[]);
        let plain = section.body.clone();
        let storage = open_section(&build_plaintext_nca(0, 1, &[section]), 0).unwrap();

        assert_eq!(read(&storage, 0x11, 0x2000), &plain[0x11..0x2011]);
        let mut buf = [0u8; 2];
        assert!(storage.read_exact_at(&mut buf, storage.size() - 1).is_err());
    }

    #[test]
    fn compressed_section_is_unsupported() {
        let mut section = romfs_section(&noise(0x400, 7));
        section.fs[0x178..0x180].copy_from_slice(&0x200u64.to_le_bytes());
        section.fs[0x180..0x188].copy_from_slice(&0x4000u64.to_le_bytes());
        let err = open_section(&build_nca(0, 1, [0; 16], &[section]), 0)
            .err()
            .unwrap();
        assert!(matches!(
            err,
            NxError::CompressedSectionUnsupported { section: 0, .. }
        ));
    }

    fn entry(virtual_offset: u64, physical_offset: u64, storage_index: u32) -> IndirectEntry {
        IndirectEntry {
            virtual_offset,
            physical_offset,
            storage_index,
        }
    }

    fn indirect(
        entries: &[IndirectEntry],
        end: u64,
        sizes: [u64; 2],
    ) -> Result<IndirectStorage, &'static str> {
        let storages: [Box<dyn NcaInput>; 2] = [
            Box::new(MemInput(noise(sizes[0] as usize, 1))),
            Box::new(MemInput(noise(sizes[1] as usize, 2))),
        ];
        let table = build_table(entries, end);
        IndirectStorage::new(
            storages,
            sizes,
            &table,
            entries.len() as u32,
            CancelToken::new(),
        )
    }

    #[test]
    fn indirect_reads_cross_entries_and_entry_sets() {
        // 2000 entries span three 0x4000-byte entry sets (818 per set).
        let count = 2000u64;
        let entries: Vec<IndirectEntry> = (0..count)
            .map(|i| entry(i * 0x10, (count - 1 - i) * 0x10, (i % 2) as u32))
            .collect();
        let size = count * 0x10;
        let storage = indirect(&entries, size, [size, size]).unwrap();
        let base = noise(size as usize, 1);
        let patch = noise(size as usize, 2);
        let expected: Vec<u8> = (0..size)
            .map(|v| {
                let i = v / 0x10;
                let phys = ((count - 1 - i) * 0x10 + v % 0x10) as usize;
                if i % 2 == 0 { base[phys] } else { patch[phys] }
            })
            .collect();

        assert_eq!(read(&storage, 0, size as usize), expected);
        for offset in [0x7, 818 * 0x10 - 3, 1636 * 0x10 - 9, size - 0x21] {
            assert_eq!(
                read(&storage, offset, 0x21),
                &expected[offset as usize..offset as usize + 0x21]
            );
        }
        let mut buf = [0u8; 1];
        assert!(storage.read_exact_at(&mut buf, size).is_err());
    }

    #[test]
    fn corrupt_indirect_tables_fail() {
        let good = [
            entry(0, 0, 1),
            entry(0x100, 0x40, 0),
            entry(0x200, 0x100, 1),
        ];
        assert!(indirect(&good, 0x300, [0x140, 0x200]).is_ok());

        let unsorted = [entry(0, 0, 1), entry(0x200, 0, 0), entry(0x100, 0, 1)];
        assert!(indirect(&unsorted, 0x300, [0x1000, 0x1000]).is_err());
        let bad_storage = [entry(0, 0, 1), entry(0x100, 0, 2)];
        assert!(indirect(&bad_storage, 0x300, [0x1000, 0x1000]).is_err());
        // Base range 0x40..0x140 overruns a 0x13F-byte base section.
        assert!(indirect(&good, 0x300, [0x13F, 0x200]).is_err());
        // Patch range 0x100..0x200 overruns 0x1FF bytes of patch data.
        assert!(indirect(&good, 0x300, [0x140, 0x1FF]).is_err());
        // The last entry must start before the virtual end.
        assert!(indirect(&good, 0x200, [0x1000, 0x1000]).is_err());

        // A count past what the table holds fails before decoding.
        let storages: [Box<dyn NcaInput>; 2] = [
            Box::new(MemInput(Vec::new())),
            Box::new(MemInput(Vec::new())),
        ];
        let table = build_table(&good, 0x300);
        assert!(IndirectStorage::new(storages, [0, 0], &table, 5000, CancelToken::new()).is_err());
    }

    #[test]
    fn cancelled_indirect_reads_stop() {
        let cancel = CancelToken::new();
        let storages: [Box<dyn NcaInput>; 2] = [
            Box::new(MemInput(vec![0; 0x100])),
            Box::new(MemInput(vec![0; 0x100])),
        ];
        let table = build_table(&[entry(0, 0, 1)], 0x100);
        let storage =
            IndirectStorage::new(storages, [0x100, 0x100], &table, 1, cancel.clone()).unwrap();
        let mut buf = [0u8; 0x10];
        storage.read_exact_at(&mut buf, 0).unwrap();

        cancel.cancel();

        assert!(matches!(
            storage.read_exact_at(&mut buf, 0),
            Err(NxError::Cancelled(_))
        ));
    }

    #[test]
    fn patch_table_outside_its_section_fails_before_allocating() {
        let base = romfs_section(&noise(0x1000, 3));
        let mut patch = patch_romfs_section(&noise(0x1000, 4), &[]);
        patch.fs[0x118..0x11C].copy_from_slice(&(1u32 << 22).to_le_bytes());
        let base = open_section(&build_nca(0, 1, [0; 16], &[base]), 0).unwrap();
        let patch = open_section(&build_nca(0, 1, [0; 16], &[patch]), 0).unwrap();
        let err = IndirectStorage::open(base, patch, "p.nca", &CancelToken::new())
            .err()
            .unwrap();
        assert!(matches!(err, NxError::InvalidBucketTree { .. }));
    }
}
