//! Chunk and metadata-table codecs for RVZ and WIA output.

use super::EncodeOptions;
use crate::nintendo::disc::rvz::error::{RvzError, RvzResult};
use crate::nintendo::disc::wia::WiaCompression;
use crate::nintendo::disc::wia::format::{
    WIA_COMPR_BZIP2, WIA_COMPR_LZMA, WIA_COMPR_LZMA2, WIA_COMPR_NONE,
};
use std::io::Write;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Container {
    Rvz,
    Wia,
}

pub(super) enum ChunkCodec {
    Zstd(zstd::bulk::Compressor<'static>),
    Bzip2 { level: u32 },
    Lzma(lzma_rust2::LzmaOptions),
    Lzma2(lzma_rust2::Lzma2Options),
    None,
}

impl ChunkCodec {
    pub(super) fn new_for(options: EncodeOptions) -> RvzResult<Self> {
        if options.container == Container::Rvz {
            return Ok(Self::Zstd(
                zstd::bulk::Compressor::new(options.level)
                    .map_err(|e| RvzError::Custom(format!("zstd init: {e}")))?,
            ));
        }
        let level = options.level as u32;
        Ok(match options.compression {
            WiaCompression::None => Self::None,
            WiaCompression::Bzip2 => Self::Bzip2 { level },
            WiaCompression::Lzma => {
                let mut options = lzma_rust2::LzmaOptions::with_preset(level);
                options.dict_size = options.dict_size.min(4 * 1024 * 1024);
                Self::Lzma(options)
            }
            WiaCompression::Lzma2 => {
                let mut options = lzma_rust2::Lzma2Options::with_preset(level);
                options.lzma_options.dict_size =
                    options.lzma_options.dict_size.min(4 * 1024 * 1024);
                Self::Lzma2(options)
            }
        })
    }

    pub(super) fn compress(&mut self, data: &[u8]) -> RvzResult<Vec<u8>> {
        match self {
            Self::Zstd(compressor) => compressor
                .compress(data)
                .map_err(|e| RvzError::Custom(format!("zstd compress: {e}"))),
            Self::Bzip2 { level } => {
                let mut writer =
                    bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(*level));
                writer.write_all(data)?;
                Ok(writer.finish()?)
            }
            Self::Lzma(options) => {
                let mut writer = lzma_rust2::LzmaWriter::new_no_header(Vec::new(), options, true)?;
                writer.write_all(data)?;
                Ok(writer.finish()?)
            }
            Self::Lzma2(options) => {
                let mut writer = lzma_rust2::Lzma2Writer::new(Vec::new(), options.clone());
                writer.write_all(data)?;
                Ok(writer.finish()?)
            }
            Self::None => unreachable!("uncompressed data bypasses the codec"),
        }
    }

    /// Per-group encoder footprint for pool admission. The LZMA figure is
    /// the usual match-finder rule of thumb (about ten times the dictionary
    /// for bt4); the crate's own estimate reports inconsistent units.
    pub(super) fn memory_usage(&self) -> usize {
        match self {
            Self::Lzma(options) => options.dict_size as usize * 10 + (1 << 20),
            Self::Lzma2(options) => options.lzma_options.dict_size as usize * 10 + (1 << 20),
            Self::Bzip2 { .. } => 8 * 1024 * 1024,
            Self::None => 0,
            Self::Zstd(_) => unreachable!("RVZ retains its existing worker admission"),
        }
    }

    pub(super) fn compression_id(&self) -> u32 {
        match self {
            Self::Zstd(_) => 5,
            Self::Bzip2 { .. } => WIA_COMPR_BZIP2,
            Self::Lzma(_) => WIA_COMPR_LZMA,
            Self::Lzma2(_) => WIA_COMPR_LZMA2,
            Self::None => WIA_COMPR_NONE,
        }
    }

    pub(super) fn compr_data(&self) -> ([u8; 7], u8) {
        let mut data = [0; 7];
        let len = match self {
            Self::Lzma(options) => {
                data[0] = options.get_props();
                data[1..5].copy_from_slice(&options.dict_size.to_le_bytes());
                5
            }
            Self::Lzma2(options) => {
                let dict = options.lzma_options.dict_size as u64;
                data[0] = (0..40u8)
                    .find(|&p| ((2 | (p as u64 & 1)) << (p / 2 + 11)) >= dict)
                    .expect("the writer caps LZMA2 dictionaries at 4 MiB");
                1
            }
            _ => 0,
        };
        (data, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::disc::wia::format::WIA_CHUNK_GRANULARITY;
    use crate::util::worker_pool::{Budget, MEMORY_TARGET_BYTES};

    #[test]
    fn wia_codec_admission_fits_every_preset() {
        for compression in [
            WiaCompression::None,
            WiaCompression::Bzip2,
            WiaCompression::Lzma,
            WiaCompression::Lzma2,
        ] {
            for level in compression.level_range().unwrap_or(0..=0) {
                let codec = ChunkCodec::new_for(EncodeOptions {
                    container: Container::Wia,
                    level: level as i32,
                    chunk_size: WIA_CHUNK_GRANULARITY,
                    compression,
                })
                .unwrap();
                let chunk = WIA_CHUNK_GRANULARITY as usize;
                let budget = Budget {
                    codec_per_worker: codec.memory_usage(),
                    per_job: chunk * 2,
                    writer_slot: chunk,
                    fixed: 8 * 1024 * 1024,
                };
                let admission = budget.admit(64, 1024).unwrap();
                assert!(
                    budget.fixed
                        + admission.workers * budget.codec_per_worker
                        + admission.max_in_flight * budget.per_job
                        + (admission.writer_capacity + 1) * budget.writer_slot
                        <= MEMORY_TARGET_BYTES
                );
            }
        }
    }
}
