//! Writes one PS1 `EBOOT.PBP`, encoding disc blocks on a worker pool and
//! seeking back to fill each PSISOIMG header once its index is known.

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};

use super::blocks::{BlockEncoder, BlockEncoding, STORED_BLOCK_LEN};
use super::disc::{DiscId, DiscImage};
use super::error::{PsxPbpError, PsxPbpResult};
use super::format::{
    BLOCK_SIZE, DISC_ALIGN, DISC_DATA_START, IndexEntry, MAX_BLOCKS, MULTIDISC_TABLE_LEN,
    PSAR_ALIGN, PSTITLEIMG_LEN, block_count, data_end, psisoimg_header, pstitleimg_header,
    startdat_header, startdat_len,
};
use crate::sony::psp::pbp::{self, DATA_PSAR, SEGMENT_COUNT};
use crate::util::sfo::{self, SfoEntry, SfoValue};
use crate::disc::cd::IO_BUFFER_SIZE;
use crate::util::template::truncate_bytes;
use crate::util::worker_pool::{Pool, PoolChannelClosed, drive, parallelism, with_writer_thread};
use crate::util::{CancelToken, Cancelled, ProgressReporter};

/// Largest file a FAT32 Memory Stick holds.
pub(crate) const FAT32_MAX_FILE: u64 = u32::MAX as u64;

const SFO_TITLE_SLOT: u32 = 128;
const SFO_LICENSE: &str = "rom-converto";

/// Bytes a disc of `image_size` bytes takes in an EBOOT at most: its
/// PSISOIMG header, every block stored, and the alignment before the next.
pub(crate) fn max_disc_bytes(image_size: u64) -> u64 {
    DISC_DATA_START + block_count(image_size) * STORED_BLOCK_LEN as u64 + DISC_ALIGN
}

/// Bytes an EBOOT takes besides its discs at most, with `assets_len` bytes
/// of loader and boot image: the PBP header and `PARAM.SFO` padded to
/// `DATA.PSAR`, the PSTITLEIMG, and the STARTDAT header.
pub(crate) fn max_fixed_bytes(assets_len: usize) -> u64 {
    PSAR_ALIGN + PSTITLEIMG_LEN + startdat_len(0) + assets_len as u64
}

/// The files a PS1 EBOOT carries besides its discs.
pub(crate) struct Assets<'a> {
    pub data_psp: &'a [u8],
    pub startdat_png: &'a [u8],
    pub icon0: &'a [u8],
    pub pic1: &'a [u8],
}

/// One EBOOT to write.
pub(crate) struct Eboot<'a> {
    pub title: &'a str,
    /// The ID in `PARAM.SFO` and PSTITLEIMG. The emulator names the save
    /// folder after the first disc's PSISOIMG ID, so per-disc EBOOTs that
    /// share a memory card all carry disc 1's.
    pub id: &'a DiscId,
    /// Each disc with the ID written into its own PSISOIMG.
    pub discs: Vec<(&'a DiscImage, &'a DiscId)>,
    /// `Some` writes a PSTITLEIMG around the discs.
    pub table: Option<&'a [u8; MULTIDISC_TABLE_LEN]>,
    pub encoding: BlockEncoding,
    pub assets: &'a Assets<'a>,
}

impl Eboot<'_> {
    /// Size of the file if every block were stored uncompressed, the most
    /// any encoding can write.
    pub fn max_size(&self) -> PsxPbpResult<u64> {
        let mut disc_offset = self.first_disc_offset();
        let mut end = 0;
        for (disc, _) in &self.discs {
            let stored = self.checked_blocks(disc)? * STORED_BLOCK_LEN as u64;
            end = disc_offset + data_end(stored);
            disc_offset = end.next_multiple_of(DISC_ALIGN);
        }
        Ok(self.layout()?[DATA_PSAR] + end + startdat_len(self.assets.startdat_png.len()))
    }

    /// Writes the EBOOT to `file`, an empty file, and returns its size.
    pub fn write(
        &self,
        file: &mut File,
        progress: &dyn ProgressReporter,
        cancel: &CancelToken,
    ) -> PsxPbpResult<u64> {
        let sfo = self.param_sfo()?;
        let offsets = self.layout()?;
        let header = pbp::write_header(pbp::VERSION, &offsets).map_err(io::Error::other)?;
        let assets = self.assets;
        let mut out = BufWriter::with_capacity(IO_BUFFER_SIZE, file);
        for part in [
            &header[..],
            &sfo,
            assets.icon0,
            assets.pic1,
            assets.data_psp,
        ] {
            out.write_all(part)?;
        }
        let psar = offsets[DATA_PSAR];

        let total: u64 = self.discs.iter().map(|(disc, _)| disc.image_size()).sum();
        progress.start(total, "Building EBOOT.PBP");
        let workers: Vec<BlockEncoder> = (0..parallelism())
            .map(|_| BlockEncoder::new(self.encoding))
            .collect();
        let pool: Pool<Vec<u8>, Vec<u8>, PsxPbpError> = Pool::spawn(workers);
        let discs = self.write_discs(&mut out, psar, &pool, progress, cancel);
        pool.shutdown();
        let (disc_offsets, stored_len) = discs?;

        let last_offset = disc_offsets.last().copied().unwrap_or_default();
        out.seek(SeekFrom::Start(psar + last_offset + data_end(stored_len)))?;
        out.write_all(&startdat_header(assets.startdat_png.len()))?;
        out.write_all(assets.startdat_png)?;
        let size = out.stream_position()?;
        if let Some(table) = self.table {
            out.seek(SeekFrom::Start(psar))?;
            out.write_all(&pstitleimg_header(
                self.id,
                self.title,
                &disc_offsets,
                stored_len,
                startdat_len(assets.startdat_png.len()),
                table,
            )?)?;
        }
        out.flush()?;
        progress.finish();
        Ok(size)
    }

    /// Writes every disc's blocks and PSISOIMG header into `out`, whose
    /// `DATA.PSAR` starts at `psar`. Returns each disc's offset in
    /// `DATA.PSAR` and the last disc's stored length.
    fn write_discs(
        &self,
        out: &mut BufWriter<&mut File>,
        psar: u64,
        pool: &Pool<Vec<u8>, Vec<u8>, PsxPbpError>,
        progress: &dyn ProgressReporter,
        cancel: &CancelToken,
    ) -> PsxPbpResult<(Vec<u64>, u64)> {
        let max_in_flight = parallelism() * 2;
        let mut disc_offsets = Vec::with_capacity(self.discs.len());
        let mut disc_offset = self.first_disc_offset();
        let mut stored_len = 0;
        for (disc, id) in &self.discs {
            let blocks = self.checked_blocks(disc)?;
            let base = psar + disc_offset;
            out.seek(SeekFrom::Start(base + DISC_DATA_START))?;
            let mut reader = disc.reader();
            let mut remaining = disc.image_size();
            let mut entries = Vec::with_capacity(blocks as usize);
            stored_len = 0;
            with_writer_thread(
                out,
                max_in_flight * 2,
                PsxPbpError::WorkerPoolPanic,
                |write_tx| {
                    drive(
                        pool,
                        blocks,
                        max_in_flight,
                        |_| -> PsxPbpResult<Vec<u8>> {
                            if cancel.is_cancelled() {
                                return Err(Cancelled.into());
                            }
                            let take = remaining.min(BLOCK_SIZE as u64) as usize;
                            let mut block = vec![0u8; BLOCK_SIZE];
                            reader.read_exact(&mut block[..take])?;
                            remaining -= take as u64;
                            progress.inc(take as u64);
                            Ok(block)
                        },
                        |_, encoded: Vec<u8>| -> PsxPbpResult<()> {
                            entries.push(IndexEntry {
                                offset: u32::try_from(stored_len).map_err(|_| {
                                    PsxPbpError::HeaderFieldOverflow {
                                        field: "PSISOIMG block offset",
                                        value: stored_len,
                                    }
                                })?,
                                size: u16::try_from(encoded.len()).map_err(|_| {
                                    PsxPbpError::HeaderFieldOverflow {
                                        field: "PSISOIMG block size",
                                        value: encoded.len() as u64,
                                    }
                                })?,
                            });
                            stored_len += encoded.len() as u64;
                            write_tx
                                .send(encoded)
                                .map_err(|_| PsxPbpError::WorkerPoolClosed(PoolChannelClosed))
                        },
                    )
                },
            )?;
            out.seek(SeekFrom::Start(base))?;
            out.write_all(&psisoimg_header(
                disc,
                id,
                self.title,
                &entries,
                stored_len,
                startdat_len(self.assets.startdat_png.len()),
            )?)?;
            disc_offsets.push(disc_offset);
            disc_offset = (disc_offset + data_end(stored_len)).next_multiple_of(DISC_ALIGN);
        }
        Ok((disc_offsets, stored_len))
    }

    fn first_disc_offset(&self) -> u64 {
        if self.table.is_some() {
            PSTITLEIMG_LEN
        } else {
            0
        }
    }

    fn checked_blocks(&self, disc: &DiscImage) -> PsxPbpResult<u64> {
        let blocks = block_count(disc.image_size());
        if blocks > MAX_BLOCKS {
            return Err(PsxPbpError::IndexOverflow {
                path: disc.source.clone(),
                blocks,
                max: MAX_BLOCKS,
            });
        }
        Ok(blocks)
    }

    fn layout(&self) -> PsxPbpResult<[u64; SEGMENT_COUNT]> {
        let mut sizes = [0u64; SEGMENT_COUNT - 1];
        sizes[pbp::PARAM_SFO] = self.param_sfo()?.len() as u64;
        sizes[pbp::ICON0_PNG] = self.assets.icon0.len() as u64;
        sizes[pbp::PIC1_PNG] = self.assets.pic1.len() as u64;
        sizes[pbp::DATA_PSP] = self.assets.data_psp.len() as u64;
        Ok(pbp::layout(&sizes, PSAR_ALIGN))
    }

    /// The nine keys of a PS1 `PARAM.SFO`, in fixed slots that make it 944
    /// bytes.
    fn param_sfo(&self) -> PsxPbpResult<Vec<u8>> {
        let text = |key, value: &str, slot| SfoEntry {
            key,
            value: SfoValue::Str(value.to_string()),
            slot,
        };
        let number = |key, value, slot| SfoEntry {
            key,
            value: SfoValue::U32(value),
            slot,
        };
        Ok(sfo::write(&[
            number("BOOTABLE", 1, 4),
            text("CATEGORY", "ME", 4),
            text("DISC_ID", &self.id.sfo(), 16),
            text("DISC_VERSION", "1.00", 8),
            text("LICENSE", SFO_LICENSE, 512),
            number("PARENTAL_LEVEL", 3, 4),
            text("PSP_SYSTEM_VER", "3.01", 8),
            number("REGION", 0x8000, 4),
            text(
                "TITLE",
                &truncate_bytes(self.title, SFO_TITLE_SLOT as usize - 1),
                SFO_TITLE_SLOT,
            ),
        ])?)
    }
}
