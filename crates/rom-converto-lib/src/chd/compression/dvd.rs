//! Raw-codec set for DVD-mode hunks.
//!
//! DVD CHDs store flat 2048-byte-sector data, so hunks go through the
//! plain codecs exactly as chdman's `createdvd` does: no CD frame
//! split, no subcode stream, no ECC stripping. The header's compressor
//! slots come straight from the resolved codec list and the per-hunk
//! trial runs those codecs in slot order.

use super::lzma::LzmaEncoder;
use super::{ChdCodec, ChdCompression, RawDecoders, compress_raw_codec, lzma_level, zstd_level};
use crate::chd::error::{ChdError, ChdResult};

/// Persistent per-worker codec state for DVD hunk compression,
/// mirroring [`super::CdCodecSet`]: encoder handles allocate once per
/// thread, every hunk reuses them. The trial runs the resolved header
/// codec list (all generic; cd* codecs are rejected for DVD by
/// [`super::validate_codecs`]).
pub(crate) struct DvdCodecSet {
    codecs: Vec<ChdCodec>,
    lzma: Option<LzmaEncoder>,
    deflate: flate2::Compress,
    zstd: Option<zstd::bulk::Compressor<'static>>,
}

impl DvdCodecSet {
    pub fn new(hunk_bytes: usize, codecs: Vec<ChdCodec>, level: Option<i32>) -> ChdResult<Self> {
        let needs_lzma = codecs.contains(&ChdCodec::Lzma);
        let needs_zstd = codecs.contains(&ChdCodec::Zstd);
        Ok(Self {
            lzma: needs_lzma
                .then(|| LzmaEncoder::new(hunk_bytes, lzma_level(level) as i32))
                .transpose()?,
            deflate: flate2::Compress::new(super::deflate_level(level), false),
            zstd: needs_zstd
                .then(|| zstd::bulk::Compressor::new(zstd_level(level)))
                .transpose()?,
            codecs,
        })
    }

    /// Compress a hunk trying every resolved codec in slot order,
    /// return the smallest result as `(data, slot_index)`. An
    /// incompressible hunk comes back verbatim with
    /// [`ChdCompression::None`].
    pub fn compress_hunk(&mut self, hunk: &[u8]) -> ChdResult<(Vec<u8>, u8)> {
        let mut best: Option<Vec<u8>> = None;
        let mut best_slot = ChdCompression::None as u8;
        let best_len = |best: &Option<Vec<u8>>| best.as_ref().map_or(hunk.len(), |b| b.len());

        for slot in 0..self.codecs.len() {
            let candidate = compress_raw_codec(
                self.codecs[slot],
                hunk,
                self.lzma.as_ref(),
                &mut self.deflate,
                self.zstd.as_mut(),
            );
            if let Some(result) = candidate
                && result.len() < best_len(&best)
            {
                best_slot = slot as u8;
                best = Some(result);
            }
        }

        match best {
            Some(data) => Ok((data, best_slot)),
            None => Ok((hunk.to_vec(), ChdCompression::None as u8)),
        }
    }
}

/// Persistent per-worker decoder state for DVD hunks. Slots are
/// resolved from the file's compressor fourcc tags, not assumed by
/// position, so any tag order a producer chose decodes correctly. All
/// DVD codecs are generic, so decode routes straight through the shared
/// [`RawDecoders`]; a `cd*` tag in a DVD file comes back as an
/// unsupported-codec error.
pub(crate) struct DvdDecoderSet {
    slots: [Option<ChdCodec>; 4],
    raw: RawDecoders,
}

impl DvdDecoderSet {
    pub fn new(compressors: [[u8; 4]; 4], hunk_bytes: usize) -> ChdResult<Self> {
        let mut slots = [None; 4];
        for (slot, tag) in slots.iter_mut().zip(compressors.iter()) {
            *slot = match tag {
                [0, 0, 0, 0] => None,
                other => Some(
                    ChdCodec::from_tag(*other).ok_or(ChdError::UnknownCompressionCodec(*other))?,
                ),
            };
        }
        Ok(Self {
            slots,
            raw: RawDecoders::new(hunk_bytes)?,
        })
    }

    pub fn decompress(&mut self, slot: u8, data: &[u8], hunk_bytes: usize) -> ChdResult<Vec<u8>> {
        let codec = self
            .slots
            .get(slot as usize)
            .copied()
            .flatten()
            .ok_or(ChdError::UnknownCompressionCodec([slot, 0, 0, 0]))?;
        self.raw.decode(codec, data, hunk_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chd::compression::lzma::LzmaDecoder;
    use crate::chd::compression::{deflate_decompress, deflate_with_reset};

    fn compressible_hunk(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i / 64) as u8).collect()
    }

    fn xorshift_hunk(len: usize) -> Vec<u8> {
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    #[test]
    fn best_of_round_trips_through_the_winning_codec() {
        let hunk = compressible_hunk(4096);
        let codecs = vec![ChdCodec::Lzma, ChdCodec::Zlib, ChdCodec::Zstd];
        let mut set = DvdCodecSet::new(hunk.len(), codecs.clone(), None).unwrap();
        let (data, slot) = set.compress_hunk(&hunk).unwrap();
        assert!(data.len() < hunk.len());

        let mut decoder = DvdDecoderSet::new(
            crate::chd::compression::codec_header_slots(&codecs),
            hunk.len(),
        )
        .unwrap();
        let decoded = decoder.decompress(slot, &data, hunk.len()).unwrap();
        assert_eq!(decoded, hunk);
    }

    #[test]
    fn each_codec_round_trips() {
        let hunk = compressible_hunk(4096);
        let mut set = DvdCodecSet::new(
            hunk.len(),
            vec![ChdCodec::Lzma, ChdCodec::Zlib, ChdCodec::Zstd],
            None,
        )
        .unwrap();

        let lzma = set.lzma.as_ref().unwrap().compress(&hunk).unwrap();
        let back = LzmaDecoder::new(hunk.len())
            .unwrap()
            .decompress(&lzma, hunk.len())
            .unwrap();
        assert_eq!(back, hunk);

        let zlib = deflate_with_reset(&mut set.deflate, &hunk).unwrap();
        assert_eq!(deflate_decompress(&zlib, hunk.len()).unwrap(), hunk);

        let zstd_out = set.zstd.as_mut().unwrap().compress(&hunk).unwrap();
        assert_eq!(zstd::decode_all(&zstd_out[..]).unwrap(), hunk);
    }

    #[test]
    fn incompressible_hunk_stores_raw() {
        let hunk = xorshift_hunk(2048);
        let mut set =
            DvdCodecSet::new(hunk.len(), vec![ChdCodec::Lzma, ChdCodec::Zlib], None).unwrap();
        let (data, slot) = set.compress_hunk(&hunk).unwrap();
        assert_eq!(slot, ChdCompression::None as u8);
        assert_eq!(data, hunk);
    }

    #[test]
    fn map_slot_matches_codec_order() {
        let hunk = compressible_hunk(4096);
        // huff first: it wins over raw on this repetitive hunk, and its
        // slot index must be 0 to match the reader's tag dispatch.
        let codecs = vec![ChdCodec::Huff, ChdCodec::Lzma];
        let mut set = DvdCodecSet::new(hunk.len(), codecs.clone(), None).unwrap();
        let (data, slot) = set.compress_hunk(&hunk).unwrap();
        let mut decoder = DvdDecoderSet::new(
            crate::chd::compression::codec_header_slots(&codecs),
            hunk.len(),
        )
        .unwrap();
        assert_eq!(decoder.decompress(slot, &data, hunk.len()).unwrap(), hunk);
    }
}
