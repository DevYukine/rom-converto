//! Raw-codec set for DVD-mode hunks.
//!
//! DVD CHDs store flat 2048-byte-sector data, so hunks go through the
//! plain codecs exactly as chdman's `createdvd` does: no CD frame
//! split, no subcode stream, no ECC stripping. The writer emits
//! `[lzma, zlib]` by default and adds `zstd` only on request: the
//! libchdr builds bundled in AetherSX2/NetherSX2 reject any CHD whose
//! compressor list contains zstd, and that silent incompatibility is
//! the community pain this default avoids.

use super::lzma::{LzmaDecoder, LzmaEncoder};
use super::{ChdCompression, deflate_decompress_with, deflate_with_reset, tag_to_bytes};
use crate::chd::error::{ChdError, ChdResult};

/// chdman compresses zstd hunks at the maximum level; the level only
/// affects ratio, never decode compatibility.
const ZSTD_LEVEL: i32 = 19;

/// Header compressor slots for a DVD CHD.
pub(crate) fn dvd_compressors(allow_zstd: bool) -> [[u8; 4]; 4] {
    let mut slots = [[0u8; 4]; 4];
    slots[0] = tag_to_bytes("lzma");
    slots[1] = tag_to_bytes("zlib");
    if allow_zstd {
        slots[2] = tag_to_bytes("zstd");
    }
    slots
}

/// Persistent per-worker codec state for DVD hunk compression,
/// mirroring [`super::CdCodecSet`]: encoder handles allocate once per
/// thread, every hunk reuses them.
pub(crate) struct DvdCodecSet {
    lzma: LzmaEncoder,
    deflate: flate2::Compress,
    zstd: Option<zstd::bulk::Compressor<'static>>,
}

impl DvdCodecSet {
    pub fn new(hunk_bytes: usize, allow_zstd: bool) -> ChdResult<Self> {
        Ok(Self {
            lzma: LzmaEncoder::new(hunk_bytes)?,
            deflate: flate2::Compress::new(flate2::Compression::best(), false),
            zstd: if allow_zstd {
                Some(zstd::bulk::Compressor::new(ZSTD_LEVEL)?)
            } else {
                None
            },
        })
    }

    /// Compress a hunk trying every enabled codec, return the
    /// smallest result as `(data, codec_slot)`. Slots match
    /// [`dvd_compressors`]; an incompressible hunk comes back
    /// verbatim with [`ChdCompression::None`].
    pub fn compress_hunk(&mut self, hunk: &[u8]) -> ChdResult<(Vec<u8>, u8)> {
        let mut best: Option<Vec<u8>> = None;
        let mut best_type = ChdCompression::None as u8;
        let best_len = |best: &Option<Vec<u8>>| best.as_ref().map_or(hunk.len(), |b| b.len());

        if let Ok(result) = self.lzma.compress(hunk)
            && result.len() < best_len(&best)
        {
            best_type = 0;
            best = Some(result);
        }

        if let Ok(result) = deflate_with_reset(&mut self.deflate, hunk)
            && result.len() < best_len(&best)
        {
            best_type = 1;
            best = Some(result);
        }

        if let Some(zstd) = self.zstd.as_mut()
            && let Ok(result) = zstd.compress(hunk)
            && result.len() < best_len(&best)
        {
            best_type = 2;
            best = Some(result);
        }

        match best {
            Some(data) => Ok((data, best_type)),
            None => Ok((hunk.to_vec(), ChdCompression::None as u8)),
        }
    }
}

/// Decoder routing for one header compressor slot. `Huff` and `Flac`
/// exist for reading chdman's `createdvd` default set
/// `[lzma, zlib, huff, flac]`; our writer never emits them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DvdSlot {
    Lzma,
    Zlib,
    Zstd,
    Huff,
    Flac,
    Unused,
}

/// Persistent per-worker decoder state for DVD hunks. Slots are
/// resolved from the file's compressor fourcc tags, not assumed by
/// position, so any tag order a producer chose decodes correctly.
pub(crate) struct DvdDecoderSet {
    slots: [DvdSlot; 4],
    lzma: LzmaDecoder,
    deflate: flate2::Decompress,
    zstd: zstd::bulk::Decompressor<'static>,
}

impl DvdDecoderSet {
    pub fn new(compressors: [[u8; 4]; 4], hunk_bytes: usize) -> ChdResult<Self> {
        let mut slots = [DvdSlot::Unused; 4];
        for (slot, tag) in slots.iter_mut().zip(compressors.iter()) {
            *slot = match tag {
                b"lzma" => DvdSlot::Lzma,
                b"zlib" => DvdSlot::Zlib,
                b"zstd" => DvdSlot::Zstd,
                b"huff" => DvdSlot::Huff,
                b"flac" => DvdSlot::Flac,
                [0, 0, 0, 0] => DvdSlot::Unused,
                other => return Err(ChdError::UnknownCompressionCodec(*other)),
            };
        }
        Ok(Self {
            slots,
            lzma: LzmaDecoder::new(hunk_bytes)?,
            deflate: flate2::Decompress::new(false),
            zstd: zstd::bulk::Decompressor::new()?,
        })
    }

    pub fn decompress(&mut self, slot: u8, data: &[u8], hunk_bytes: usize) -> ChdResult<Vec<u8>> {
        match self.slots.get(slot as usize).copied() {
            Some(DvdSlot::Lzma) => self.lzma.decompress(data, hunk_bytes),
            Some(DvdSlot::Zlib) => deflate_decompress_with(&mut self.deflate, data, hunk_bytes),
            Some(DvdSlot::Zstd) => Ok(self.zstd.decompress(data, hunk_bytes)?),
            Some(DvdSlot::Huff) => super::huffman8::huffman8_decode(data, hunk_bytes),
            Some(DvdSlot::Flac) => super::flac::flac_decompress_chd_raw(data, hunk_bytes),
            Some(DvdSlot::Unused) | None => Err(ChdError::UnknownCompressionCodec([slot, 0, 0, 0])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chd::compression::deflate_decompress;
    use crate::chd::compression::lzma::LzmaDecoder;

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
        let mut set = DvdCodecSet::new(hunk.len(), true).unwrap();
        let (data, slot) = set.compress_hunk(&hunk).unwrap();
        assert!(data.len() < hunk.len());

        let decoded = match slot {
            0 => LzmaDecoder::new(hunk.len())
                .unwrap()
                .decompress(&data, hunk.len())
                .unwrap(),
            1 => deflate_decompress(&data, hunk.len()).unwrap(),
            2 => zstd::decode_all(&data[..]).unwrap(),
            other => panic!("unexpected slot {other}"),
        };
        assert_eq!(decoded, hunk);
    }

    #[test]
    fn each_codec_round_trips() {
        let hunk = compressible_hunk(4096);
        let mut set = DvdCodecSet::new(hunk.len(), true).unwrap();

        let lzma = set.lzma.compress(&hunk).unwrap();
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
        let mut set = DvdCodecSet::new(hunk.len(), false).unwrap();
        let (data, slot) = set.compress_hunk(&hunk).unwrap();
        assert_eq!(slot, ChdCompression::None as u8);
        assert_eq!(data, hunk);
    }

    #[test]
    fn compressor_slots_reflect_zstd_opt_in() {
        let compat = dvd_compressors(false);
        assert_eq!(&compat[0], b"lzma");
        assert_eq!(&compat[1], b"zlib");
        assert_eq!(compat[2], [0u8; 4]);
        assert_eq!(compat[3], [0u8; 4]);

        let modern = dvd_compressors(true);
        assert_eq!(&modern[2], b"zstd");
    }
}
