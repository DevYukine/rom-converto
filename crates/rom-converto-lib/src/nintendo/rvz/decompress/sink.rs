//! Output sinks for the parallel RVZ decompressor. The same
//! reconstruction pipeline writes either a raw ISO ([`IsoSink`]) or a
//! scrubbed WBFS container ([`WbfsSink`]) through the [`DiscSink`]
//! trait, so `.rvz -> .iso` and `.rvz -> .wbfs` share one parallel
//! decode path.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use crate::nintendo::rvz::error::RvzResult;
use crate::nintendo::wbfs::format::DISC_HEADER_COPY_SIZE;
use crate::nintendo::wbfs::usage::DiscUsage;
use crate::nintendo::wbfs::writer::{build_block0, compute_layout};
use crate::util::pread::file_write_all_at;

/// Where reconstructed disc bytes go. Writes arrive in ascending offset
/// order within a region but not globally (all raw regions are written
/// before all partitions), so the sink must accept positional writes at
/// arbitrary offsets. `write_at` runs on the dispatcher thread inside
/// the decode `drive` loop, so impls need no locking.
pub(crate) trait DiscSink {
    fn write_at(&mut self, offset: u64, data: &[u8]) -> RvzResult<()>;
}

/// Plain ISO output: a buffered file written mostly sequentially. The
/// position tracker elides redundant seeks so the `BufWriter` does not
/// flush on contiguous runs.
pub(crate) struct IsoSink {
    writer: BufWriter<File>,
    pos: u64,
}

impl IsoSink {
    pub(crate) fn create(output: &Path, iso_file_size: u64) -> RvzResult<Self> {
        let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
        // Pre-size so trailing/sparse regions never seek past the end.
        if iso_file_size > 0 {
            writer.seek(SeekFrom::Start(iso_file_size - 1))?;
            writer.write_all(&[0u8])?;
            writer.seek(SeekFrom::Start(0))?;
        }
        Ok(Self { writer, pos: 0 })
    }

    pub(crate) fn finish(mut self) -> RvzResult<()> {
        self.writer.flush()?;
        Ok(())
    }
}

impl DiscSink for IsoSink {
    fn write_at(&mut self, offset: u64, data: &[u8]) -> RvzResult<()> {
        if offset != self.pos {
            self.writer.seek(SeekFrom::Start(offset))?;
            self.pos = offset;
        }
        self.writer.write_all(data)?;
        self.pos += data.len() as u64;
        Ok(())
    }
}

/// Scrubbed WBFS output. The physical layout (which logical blocks are
/// stored, and in which slot) is computed up front from the FST usage
/// map, so reconstructed blocks can be written to their slots in any
/// order. Logical block 0's first `0x100` bytes are captured for the
/// disc-info header copy; [`WbfsSink::finish`] writes physical block 0.
pub(crate) struct WbfsSink {
    file: File,
    wlba: Vec<u16>,
    wbfs_sec_sz: u64,
    hd_sec_sz_s: u8,
    wbfs_sec_sz_s: u8,
    total_blocks: u64,
    header_copy: [u8; DISC_HEADER_COPY_SIZE],
}

impl WbfsSink {
    pub(crate) fn create(
        output: &Path,
        usage: &DiscUsage,
        disc_size: u64,
        hd_sec_sz_s: u8,
        wbfs_sec_sz_s: u8,
    ) -> RvzResult<Self> {
        let (wlba, total_blocks) = compute_layout(usage, disc_size, wbfs_sec_sz_s)?;
        let wbfs_sec_sz = 1u64 << wbfs_sec_sz_s;
        let file = File::create(output)?;
        // Pre-size so unwritten tails of stored blocks read back as zero.
        file.set_len(total_blocks * wbfs_sec_sz)?;
        Ok(Self {
            file,
            wlba,
            wbfs_sec_sz,
            hd_sec_sz_s,
            wbfs_sec_sz_s,
            total_blocks,
            header_copy: [0u8; DISC_HEADER_COPY_SIZE],
        })
    }

    pub(crate) fn finish(self) -> RvzResult<()> {
        let sector0 = build_block0(
            self.hd_sec_sz_s,
            self.wbfs_sec_sz_s,
            self.total_blocks,
            &self.header_copy,
            &self.wlba,
        );
        file_write_all_at(&self.file, &sector0, 0)?;
        self.file.sync_all()?;
        Ok(())
    }
}

impl DiscSink for WbfsSink {
    fn write_at(&mut self, offset: u64, data: &[u8]) -> RvzResult<()> {
        let mut abs = offset;
        let mut src = 0usize;
        while src < data.len() {
            let block_idx = (abs / self.wbfs_sec_sz) as usize;
            let in_block = abs % self.wbfs_sec_sz;
            let take = ((self.wbfs_sec_sz - in_block) as usize).min(data.len() - src);
            let slot = self.wlba.get(block_idx).copied().unwrap_or(0);
            if slot != 0 {
                let phys_off = slot as u64 * self.wbfs_sec_sz + in_block;
                file_write_all_at(&self.file, &data[src..src + take], phys_off)?;
            }
            // Logical block 0's first 0x100 bytes become the disc-info
            // header copy written in block 0 at finish.
            if block_idx == 0 && in_block < DISC_HEADER_COPY_SIZE as u64 {
                let copy_end = (in_block + take as u64).min(DISC_HEADER_COPY_SIZE as u64) as usize;
                let n = copy_end - in_block as usize;
                self.header_copy[in_block as usize..copy_end].copy_from_slice(&data[src..src + n]);
            }
            abs += take as u64;
            src += take;
        }
        Ok(())
    }
}

/// Block-aware keep predicate for the work-item builders: a chunk or
/// cluster is reconstructed only if it touches a block the layout
/// stores (block 0, or a block the usage map marks used). Junk that
/// lands only in scrubbed blocks is never read or decompressed.
pub(crate) struct UsageFilter<'a> {
    pub(crate) usage: &'a DiscUsage,
    pub(crate) wbfs_sec_sz: u64,
    pub(crate) sectors_per_block: u64,
}

impl UsageFilter<'_> {
    pub(crate) fn keeps(&self, byte_off: u64, byte_len: u64) -> bool {
        if byte_len == 0 {
            return false;
        }
        let first = byte_off / self.wbfs_sec_sz;
        let last = (byte_off + byte_len - 1) / self.wbfs_sec_sz;
        (first..=last).any(|b| b == 0 || self.usage.block_used(b, self.sectors_per_block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wbfs::format::{WII_MAGIC, WII_MAGIC_OFFSET};
    use crate::nintendo::wbfs::reader::WbfsReader;
    use crate::nintendo::wbfs::usage::DiscUsage;
    use std::io::{Read, Seek, SeekFrom};

    const HD_SECTOR_SHIFT: u8 = 9;
    const WBFS_SECTOR_SHIFT: u8 = 21;
    const BLOCK: usize = 1 << 21;

    fn read_fully(reader: &mut WbfsReader, len: usize) -> Vec<u8> {
        let mut got = vec![0u8; len];
        let mut read = 0;
        while read < len {
            let n = reader.read(&mut got[read..]).unwrap();
            if n == 0 {
                break;
            }
            read += n;
        }
        got
    }

    #[test]
    fn wbfs_sink_stores_used_blocks_and_scrubs_unused() {
        let blocks = 4;
        let mut disc = vec![0u8; blocks * BLOCK];
        disc[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4].copy_from_slice(&WII_MAGIC.to_be_bytes());
        for (i, b) in disc.iter_mut().enumerate().skip(0x100) {
            *b = ((i % 251) + 1) as u8;
        }
        let disc_size = disc.len() as u64;

        // Mark blocks 0 and 2 used; leave 1 and 3 unused.
        let mut usage = DiscUsage::new(disc_size);
        usage.mark_range(0, BLOCK as u64);
        usage.mark_range(2 * BLOCK as u64, BLOCK as u64);

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("game.wbfs");
        let mut sink =
            WbfsSink::create(&out, &usage, disc_size, HD_SECTOR_SHIFT, WBFS_SECTOR_SHIFT).unwrap();
        // Write the whole disc; the sink routes used blocks to their
        // slots and drops the rest.
        sink.write_at(0, &disc).unwrap();
        sink.finish().unwrap();

        // Stored physical blocks: meta + block 0 + block 2 = 3 blocks.
        let file_len = std::fs::metadata(&out).unwrap().len();
        assert_eq!(file_len, 3 * BLOCK as u64);

        let mut reader = WbfsReader::open(&out).unwrap();
        let got = read_fully(&mut reader, disc.len());
        assert_eq!(&got[..BLOCK], &disc[..BLOCK], "block 0 verbatim");
        assert_eq!(
            &got[2 * BLOCK..3 * BLOCK],
            &disc[2 * BLOCK..3 * BLOCK],
            "block 2 verbatim"
        );
        assert!(
            got[BLOCK..2 * BLOCK].iter().all(|&b| b == 0),
            "unused block 1 reads back zero"
        );
        assert!(
            got[3 * BLOCK..4 * BLOCK].iter().all(|&b| b == 0),
            "unused block 3 reads back zero"
        );
    }

    #[test]
    fn wbfs_sink_write_spans_block_boundary() {
        let disc_size = (2 * BLOCK) as u64;
        let mut usage = DiscUsage::new(disc_size);
        usage.mark_all();

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("span.wbfs");
        let mut sink =
            WbfsSink::create(&out, &usage, disc_size, HD_SECTOR_SHIFT, WBFS_SECTOR_SHIFT).unwrap();
        let payload: Vec<u8> = (0..200u32).map(|i| (i as u8).wrapping_add(1)).collect();
        let off = BLOCK as u64 - 100; // 100 bytes in block 0, 100 in block 1
        sink.write_at(off, &payload).unwrap();
        sink.finish().unwrap();

        let mut reader = WbfsReader::open(&out).unwrap();
        reader.seek(SeekFrom::Start(off)).unwrap();
        let got = read_fully(&mut reader, payload.len());
        assert_eq!(got, payload, "boundary-spanning write round-trips");
    }

    #[test]
    fn wbfs_sink_unwritten_tail_reads_zero() {
        let disc_size = (3 * BLOCK) as u64;
        let mut usage = DiscUsage::new(disc_size);
        usage.mark_all();

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("tail.wbfs");
        let mut sink =
            WbfsSink::create(&out, &usage, disc_size, HD_SECTOR_SHIFT, WBFS_SECTOR_SHIFT).unwrap();
        // Write only the first 1000 bytes of block 2.
        let payload = vec![0xCDu8; 1000];
        sink.write_at(2 * BLOCK as u64, &payload).unwrap();
        sink.finish().unwrap();

        let mut reader = WbfsReader::open(&out).unwrap();
        reader.seek(SeekFrom::Start(2 * BLOCK as u64)).unwrap();
        let got = read_fully(&mut reader, 1500);
        assert!(got[..1000].iter().all(|&b| b == 0xCD), "written prefix");
        assert!(
            got[1000..].iter().all(|&b| b == 0),
            "unwritten tail of a stored block reads back zero"
        );
    }

    #[test]
    fn split_container_reads_identically_to_single_file() {
        // Every byte nonzero + mark_all so nothing is scrubbed and the
        // reconstruction is byte-identical to the source.
        let disc_size = (2 * BLOCK) as u64;
        let mut disc = vec![0u8; 2 * BLOCK];
        for (i, b) in disc.iter_mut().enumerate() {
            *b = ((i % 251) + 1) as u8;
        }
        let mut usage = DiscUsage::new(disc_size);
        usage.mark_all();

        let dir = tempfile::tempdir().unwrap();
        let single = dir.path().join("single.wbfs");
        let mut sink = WbfsSink::create(
            &single,
            &usage,
            disc_size,
            HD_SECTOR_SHIFT,
            WBFS_SECTOR_SHIFT,
        )
        .unwrap();
        sink.write_at(0, &disc).unwrap();
        sink.finish().unwrap();

        // Split into game.wbfs + game.wbf1 at a non-block-aligned offset
        // so a physical block read spans the part boundary.
        let bytes = std::fs::read(&single).unwrap();
        let split_at = bytes.len() / 2 + 12345;
        let base = dir.path().join("game.wbfs");
        std::fs::write(&base, &bytes[..split_at]).unwrap();
        std::fs::write(dir.path().join("game.wbf1"), &bytes[split_at..]).unwrap();

        let mut single_reader = WbfsReader::open(&single).unwrap();
        let mut split_reader = WbfsReader::open(&base).unwrap();
        assert_eq!(single_reader.disc_size(), split_reader.disc_size());

        let from_single = read_fully(&mut single_reader, disc.len());
        let from_split = read_fully(&mut split_reader, disc.len());
        assert_eq!(
            from_single, from_split,
            "split read matches single-file read"
        );
        assert_eq!(from_single, disc, "reconstruction matches the source");
    }
}
