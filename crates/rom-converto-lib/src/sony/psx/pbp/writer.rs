//! Writes one PS1 `EBOOT.PBP`: the PBP container, `PARAM.SFO`, the
//! loader, then `DATA.PSAR` with one PSISOIMG per disc and the STARTDAT
//! boot image after the last.
//!
//! The disc data streams through once. Each PSISOIMG header, whose index
//! depends on the encoded block sizes, is written after its disc's data by
//! seeking back; the gaps the seeks leave are zeros.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use super::blocks::{BlockEncoder, BlockEncoding, STORED_BLOCK_LEN};
use super::disc::{DiscId, DiscImage};
use super::error::{PsxPbpError, PsxPbpResult};
use super::format::{
    BLOCK_SIZE, DISC_ALIGN, DISC_DATA_START, IndexEntry, MAX_BLOCKS, MULTIDISC_TABLE_LEN,
    PSAR_ALIGN, PSTITLEIMG_LEN, block_count, data_end, psisoimg_header, pstitleimg_header,
    startdat_header, startdat_len, truncate_utf8,
};
use crate::sony::psp::pbp::{self, DATA_PSAR, SEGMENT_COUNT};
use crate::util::sfo::{self, SfoEntry, SfoValue};
use crate::util::{CancelToken, Cancelled, ProgressReporter};

/// Largest file a FAT32 Memory Stick holds.
pub(crate) const FAT32_MAX_FILE: u64 = u32::MAX as u64;

const SFO_TITLE_SLOT: u32 = 128;
const SFO_LICENSE: &str = "rom-converto";

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

    /// Writes the EBOOT to `out`, an empty file, and returns its size.
    pub fn write(
        &self,
        out: &mut File,
        progress: &dyn ProgressReporter,
        cancel: &CancelToken,
    ) -> PsxPbpResult<u64> {
        let sfo = self.param_sfo()?;
        let offsets = self.layout()?;
        let header = pbp::write_header(pbp::VERSION, &offsets).map_err(io::Error::other)?;
        let assets = self.assets;
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
        let mut encoder = BlockEncoder::new(self.encoding);
        let mut block = vec![0u8; BLOCK_SIZE];
        let mut disc_offsets = Vec::with_capacity(self.discs.len());
        let mut disc_offset = self.first_disc_offset();
        let mut stored_len = 0;
        for (disc, id) in &self.discs {
            let blocks = self.checked_blocks(disc)?;
            let base = psar + disc_offset;
            out.seek(SeekFrom::Start(base + DISC_DATA_START))?;
            let mut reader = disc.reader()?;
            let mut remaining = disc.image_size();
            let mut entries = Vec::with_capacity(blocks as usize);
            stored_len = 0;
            for _ in 0..blocks {
                if cancel.is_cancelled() {
                    return Err(Cancelled.into());
                }
                let take = remaining.min(BLOCK_SIZE as u64) as usize;
                reader.read_exact(&mut block[..take])?;
                block[take..].fill(0);
                let encoded = encoder.encode(&block);
                out.write_all(encoded)?;
                entries.push(IndexEntry {
                    offset: stored_len as u32,
                    size: encoded.len() as u16,
                });
                stored_len += encoded.len() as u64;
                remaining -= take as u64;
                progress.inc(take as u64);
            }
            out.seek(SeekFrom::Start(base))?;
            out.write_all(&psisoimg_header(
                disc,
                id,
                self.title,
                &entries,
                stored_len,
                startdat_len(assets.startdat_png.len()),
            ))?;
            disc_offsets.push(disc_offset);
            disc_offset = (disc_offset + data_end(stored_len)).next_multiple_of(DISC_ALIGN);
        }

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
            ))?;
        }
        progress.finish();
        Ok(size)
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
                path: disc.cue.clone(),
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

    /// The nine keys of a PS1 `PARAM.SFO`. Fixed slots make it 944 bytes,
    /// so the disc ID and title can be patched in place.
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
                truncate_utf8(self.title, SFO_TITLE_SLOT as usize - 1),
                SFO_TITLE_SLOT,
            ),
        ])?)
    }
}
