use crate::cd::ecc::{has_valid_ecc, restore_sector_ecc, strip_sector_ecc};
use crate::cd::{BYTES_PER_STEREO_SAMPLE, CD_CHANNELS, FRAME_SIZE, SECTOR_SIZE, SUBCODE_SIZE};
use crate::chd::compression::cdfl::CD_SYNC_HEADER;
use crate::chd::compression::flac::{
    CD_SAMPLE_RATE, Endian, encode_flac_samples, samples_from_bytes,
};
use crate::chd::compression::lzma::LzmaEncoder;
use crate::chd::error::{ChdError, ChdResult};
use byteorder::{BigEndian, ByteOrder};
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use std::fmt::Debug;
use std::io::{self, Read, Write};

const CD_SHORT_HUNK_LIMIT: usize = 0x1_0000;
const CD_ECC_DIVISOR: usize = 8;

pub mod cdfl;
pub mod cdlz;
pub mod cdzl;
pub mod cdzs;
pub mod flac;
pub mod lzma;
pub mod zlib;
pub mod zstd;

pub const fn tag_to_bytes(tag: &str) -> [u8; 4] {
    let bytes = tag.as_bytes();
    assert!(bytes.len() == 4, "tag must be exactly 4 bytes");
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

// IMPORTANT: These values map to positions in the header, not codec IDs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)] // CHD spec values: Self_ and Parent are used in map constants.
pub enum ChdCompression {
    Codec0 = 0, // First codec in header
    Codec1 = 1, // Second codec in header
    Codec2 = 2, // Third codec in header
    Codec3 = 3, // Fourth codec in header
    None = 4,   // Uncompressed
    Self_ = 5,  // Same as another hunk
    Parent = 6, // From parent CHD
}

#[allow(dead_code)] // Trait methods are part of the CHD codec API
pub trait ChdCompressor: Debug {
    fn name(&self) -> &'static str;
    fn tag_bytes(&self) -> [u8; 4];
    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>>;
}

#[allow(dead_code)] // Trait methods are part of the CHD codec API
pub trait ChdDecompressor: Debug + Send + Sync {
    fn tag_bytes(&self) -> [u8; 4];
    fn decompress(&self, compressed: &[u8], output_len: usize) -> ChdResult<Vec<u8>>;
}

pub(crate) fn compress_cd_hunk<F1, F2>(
    data: &[u8],
    base_compress: F1,
    subcode_compress: F2,
) -> ChdResult<Vec<u8>>
where
    F1: FnOnce(&[u8]) -> ChdResult<Vec<u8>>,
    F2: FnOnce(&[u8]) -> ChdResult<Vec<u8>>,
{
    let (frames, mut base, subcode) = split_cd_frames(data)?;
    let (header_bytes, ecc_bytes, complen_bytes) = cd_header_sizes(data.len(), frames);

    let ecc_flags = strip_ecc_from_base(&mut base, frames, ecc_bytes);

    let base_compressed = base_compress(&base)?;
    let subcode_compressed = subcode_compress(&subcode)?;

    let mut output = vec![0u8; header_bytes];
    output[..ecc_bytes].copy_from_slice(&ecc_flags);
    write_cd_header(&mut output, ecc_bytes, base_compressed.len(), complen_bytes);
    output.extend_from_slice(&base_compressed);
    output.extend_from_slice(&subcode_compressed);
    Ok(output)
}

pub(crate) fn deflate_compress(data: &[u8]) -> ChdResult<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

pub(crate) fn deflate_decompress(data: &[u8], _expected_len: usize) -> ChdResult<Vec<u8>> {
    let mut decoder = DeflateDecoder::new(data);
    let mut output = Vec::new();
    decoder.read_to_end(&mut output)?;
    Ok(output)
}

pub(crate) fn decompress_cd_hunk<F1, F2>(
    data: &[u8],
    output_len: usize,
    base_decompress: F1,
    subcode_decompress: F2,
) -> ChdResult<Vec<u8>>
where
    F1: FnOnce(&[u8], usize) -> ChdResult<Vec<u8>>,
    F2: FnOnce(&[u8], usize) -> ChdResult<Vec<u8>>,
{
    let frames = output_len / FRAME_SIZE;
    let (header_bytes, ecc_bytes, complen_bytes) = cd_header_sizes(output_len, frames);

    let ecc_flags = &data[..ecc_bytes];

    let base_length = if complen_bytes == 2 {
        BigEndian::read_u16(&data[ecc_bytes..ecc_bytes + 2]) as usize
    } else {
        BigEndian::read_u24(&data[ecc_bytes..ecc_bytes + 3]) as usize
    };

    let base_compressed = &data[header_bytes..header_bytes + base_length];
    let subcode_compressed = &data[header_bytes + base_length..];

    let expected_base_len = frames * SECTOR_SIZE;
    let expected_subcode_len = frames * SUBCODE_SIZE;

    let mut base = base_decompress(base_compressed, expected_base_len)?;
    let subcode = subcode_decompress(subcode_compressed, expected_subcode_len)?;

    // ECC restoration: for each frame that had its ECC stripped, restore it
    for frame in 0..frames {
        if ecc_flags[frame / 8] & (1 << (frame % 8)) != 0 {
            let sector = &mut base[frame * SECTOR_SIZE..(frame + 1) * SECTOR_SIZE];
            restore_sector_ecc(sector);
        }
    }

    let mut output = Vec::with_capacity(output_len);
    for frame in 0..frames {
        let base_offset = frame * SECTOR_SIZE;
        let subcode_offset = frame * SUBCODE_SIZE;
        output.extend_from_slice(&base[base_offset..base_offset + SECTOR_SIZE]);
        output.extend_from_slice(&subcode[subcode_offset..subcode_offset + SUBCODE_SIZE]);
    }

    Ok(output)
}

fn strip_ecc_from_base(base: &mut [u8], frames: usize, ecc_bytes: usize) -> Vec<u8> {
    let mut ecc_flags = vec![0u8; ecc_bytes];
    for frame in 0..frames {
        let start = frame * SECTOR_SIZE;
        let end = start + SECTOR_SIZE;
        if has_valid_ecc(&base[start..end]) {
            ecc_flags[frame / 8] |= 1 << (frame % 8);
            strip_sector_ecc(&mut base[start..end]);
        }
    }
    ecc_flags
}

fn split_cd_frames(data: &[u8]) -> ChdResult<(usize, Vec<u8>, Vec<u8>)> {
    if !data.len().is_multiple_of(FRAME_SIZE) {
        return Err(ChdError::InvalidHunkSize);
    }

    let frames = data.len() / FRAME_SIZE;
    let mut base = Vec::with_capacity(frames * SECTOR_SIZE);
    let mut subcode = Vec::with_capacity(frames * SUBCODE_SIZE);

    for frame in 0..frames {
        let start = frame * FRAME_SIZE;
        base.extend_from_slice(&data[start..start + SECTOR_SIZE]);
        subcode.extend_from_slice(&data[start + SECTOR_SIZE..start + FRAME_SIZE]);
    }

    Ok((frames, base, subcode))
}

pub(crate) fn cd_header_sizes(data_len: usize, frames: usize) -> (usize, usize, usize) {
    let complen_bytes = if data_len < CD_SHORT_HUNK_LIMIT { 2 } else { 3 };
    let ecc_bytes = frames.div_ceil(CD_ECC_DIVISOR);
    let header_bytes = ecc_bytes + complen_bytes;
    (header_bytes, ecc_bytes, complen_bytes)
}

fn write_cd_header(buf: &mut [u8], ecc_bytes: usize, base_len: usize, complen_bytes: usize) {
    if complen_bytes == 2 {
        BigEndian::write_u16(&mut buf[ecc_bytes..ecc_bytes + 2], base_len as u16);
    } else {
        BigEndian::write_u24(&mut buf[ecc_bytes..ecc_bytes + 3], base_len as u32);
    }
}

/// Persistent codec state for CD hunk compression, matching chdman's approach
/// of reusing encoder instances across hunks rather than creating new ones each time.
pub(crate) struct CdCodecSet {
    lzma: LzmaEncoder,
    cdlz_subcode_deflate: flate2::Compress,
    cdzl_base_deflate: flate2::Compress,
    cdzl_subcode_deflate: flate2::Compress,
}

impl CdCodecSet {
    pub fn new(hunk_bytes: usize) -> io::Result<Self> {
        Ok(Self {
            lzma: LzmaEncoder::new(hunk_bytes)?,
            cdlz_subcode_deflate: flate2::Compress::new(Compression::best(), false),
            cdzl_base_deflate: flate2::Compress::new(Compression::best(), false),
            cdzl_subcode_deflate: flate2::Compress::new(Compression::best(), false),
        })
    }

    /// Compress a CD hunk trying all codecs, return best result.
    /// Returns `(compressed_data, codec_index)` where codec_index maps to the
    /// header codec slots (0=CDLZ, 1=CDZL, 2=CDFL).
    pub fn compress_hunk(&mut self, hunk: &[u8]) -> ChdResult<(Vec<u8>, u8)> {
        let (frames, mut base, subcode) = split_cd_frames(hunk)?;
        let (header_bytes, ecc_bytes, complen_bytes) = cd_header_sizes(hunk.len(), frames);

        // Decide whether CDFL is a candidate before stripping ECC.
        // `strip_ecc_from_base` zeros the 12-byte sync header of
        // every sector with valid Mode-1 ECC, which would otherwise
        // trick the CDFL gate into running FLAC on data hunks (an
        // expensive no-op that never wins the best-size trial).
        let cdfl_candidate = base.len() >= 12 && base[..12] != CD_SYNC_HEADER;

        let ecc_flags = strip_ecc_from_base(&mut base, frames, ecc_bytes);

        let mut best: Option<Vec<u8>> = None;
        let mut best_type = ChdCompression::None as u8;

        let best_len = |best: &Option<Vec<u8>>| best.as_ref().map_or(hunk.len(), |b| b.len());

        // Try CDLZ (LZMA base + deflate subcode).
        if let Ok(result) = self.compress_cdlz(
            &base,
            &subcode,
            &ecc_flags,
            header_bytes,
            ecc_bytes,
            complen_bytes,
        ) && result.len() < best_len(&best)
        {
            best_type = 0;
            best = Some(result);
        }

        // Try CDZL (deflate base + deflate subcode).
        if let Ok(result) = self.compress_cdzl(
            &base,
            &subcode,
            &ecc_flags,
            header_bytes,
            ecc_bytes,
            complen_bytes,
        ) && result.len() < best_len(&best)
        {
            best_type = 1;
            best = Some(result);
        }

        // Try CDFL only for audio tracks (no CD sync header in first sector).
        if cdfl_candidate
            && let Ok(result) = self.compress_cdfl(
                &base,
                &subcode,
                &ecc_flags,
                header_bytes,
                ecc_bytes,
                complen_bytes,
            )
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

    fn compress_cdlz(
        &mut self,
        base: &[u8],
        subcode: &[u8],
        ecc_flags: &[u8],
        header_bytes: usize,
        ecc_bytes: usize,
        complen_bytes: usize,
    ) -> ChdResult<Vec<u8>> {
        let base_compressed = self.lzma.compress(base)?;
        let subcode_compressed = deflate_with_reset(&mut self.cdlz_subcode_deflate, subcode)?;
        Ok(assemble_cd_output(
            ecc_flags,
            header_bytes,
            ecc_bytes,
            complen_bytes,
            &base_compressed,
            &subcode_compressed,
        ))
    }

    fn compress_cdzl(
        &mut self,
        base: &[u8],
        subcode: &[u8],
        ecc_flags: &[u8],
        header_bytes: usize,
        ecc_bytes: usize,
        complen_bytes: usize,
    ) -> ChdResult<Vec<u8>> {
        let base_compressed = deflate_with_reset(&mut self.cdzl_base_deflate, base)?;
        let subcode_compressed = deflate_with_reset(&mut self.cdzl_subcode_deflate, subcode)?;
        Ok(assemble_cd_output(
            ecc_flags,
            header_bytes,
            ecc_bytes,
            complen_bytes,
            &base_compressed,
            &subcode_compressed,
        ))
    }

    fn compress_cdfl(
        &mut self,
        base: &[u8],
        subcode: &[u8],
        ecc_flags: &[u8],
        header_bytes: usize,
        ecc_bytes: usize,
        complen_bytes: usize,
    ) -> ChdResult<Vec<u8>> {
        if !base.len().is_multiple_of(BYTES_PER_STEREO_SAMPLE) {
            return Err(ChdError::InvalidHunkSize);
        }
        let samples = samples_from_bytes(base, Endian::Big);
        let samples_per_channel = samples.len() / CD_CHANNELS;
        let base_compressed =
            encode_flac_samples(&samples, CD_CHANNELS, CD_SAMPLE_RATE, samples_per_channel)?;
        // CDFL reuses the CDZL subcode deflater for subcode (stateless reset)
        let subcode_compressed = deflate_compress(subcode)?;
        Ok(assemble_cd_output(
            ecc_flags,
            header_bytes,
            ecc_bytes,
            complen_bytes,
            &base_compressed,
            &subcode_compressed,
        ))
    }
}

fn deflate_with_reset(compressor: &mut flate2::Compress, data: &[u8]) -> ChdResult<Vec<u8>> {
    compressor.reset();
    // Deflate worst case is slightly larger than input
    let max_out = data.len() + data.len() / 100 + 600;
    let mut output = vec![0u8; max_out];
    let before_out = compressor.total_out();
    let status = compressor
        .compress(data, &mut output, flate2::FlushCompress::Finish)
        .map_err(|e| io::Error::other(format!("deflate compress error: {e}")))?;
    match status {
        flate2::Status::StreamEnd => {}
        _ => {
            return Err(io::Error::other("deflate compression did not finish in one call").into());
        }
    }
    let written = (compressor.total_out() - before_out) as usize;
    output.truncate(written);
    Ok(output)
}

fn assemble_cd_output(
    ecc_flags: &[u8],
    header_bytes: usize,
    ecc_bytes: usize,
    complen_bytes: usize,
    base_compressed: &[u8],
    subcode_compressed: &[u8],
) -> Vec<u8> {
    let mut output = vec![0u8; header_bytes];
    output[..ecc_bytes].copy_from_slice(ecc_flags);
    write_cd_header(&mut output, ecc_bytes, base_compressed.len(), complen_bytes);
    output.extend_from_slice(base_compressed);
    output.extend_from_slice(subcode_compressed);
    output
}

/// Persistent decoder state for CD hunk decompression. Mirrors the
/// encoder-side [`CdCodecSet`]: one instance per worker thread
/// holds a reusable LZMA decoder plus persistent deflate
/// decompressors for cdlz / cdzl base + subcode streams, so every
/// hunk skips the allocator path.
pub(crate) struct CdDecoderSet {
    lzma: crate::chd::compression::lzma::LzmaDecoder,
    deflate: flate2::Decompress,
}

impl CdDecoderSet {
    pub fn new(hunk_bytes: usize) -> ChdResult<Self> {
        Ok(Self {
            lzma: crate::chd::compression::lzma::LzmaDecoder::new(hunk_bytes)?,
            deflate: flate2::Decompress::new(false),
        })
    }

    /// Decompress a CDLZ hunk body: LZMA on the base stream +
    /// deflate on the subcode stream, then ECC restoration + frame
    /// interleave. Matches the shape of [`decompress_cd_hunk`] but
    /// reuses persistent codec state.
    pub fn decompress_cdlz(&mut self, data: &[u8], output_len: usize) -> ChdResult<Vec<u8>> {
        self.decompress_cd_hunk(data, output_len, CdBaseDecoder::Lzma)
    }

    /// Decompress a CDZL hunk body: deflate on both base and
    /// subcode streams.
    pub fn decompress_cdzl(&mut self, data: &[u8], output_len: usize) -> ChdResult<Vec<u8>> {
        self.decompress_cd_hunk(data, output_len, CdBaseDecoder::Deflate)
    }

    /// Decompress a CDFL hunk body: FLAC on the base stream +
    /// deflate on the subcode stream. FLAC doesn't currently have
    /// a persistent-state decoder, so the base call still
    /// allocates, but the fixture rarely hits this path.
    pub fn decompress_cdfl(&mut self, data: &[u8], output_len: usize) -> ChdResult<Vec<u8>> {
        self.decompress_cd_hunk(data, output_len, CdBaseDecoder::Flac)
    }

    fn decompress_cd_hunk(
        &mut self,
        data: &[u8],
        output_len: usize,
        base: CdBaseDecoder,
    ) -> ChdResult<Vec<u8>> {
        let frames = output_len / FRAME_SIZE;
        let (header_bytes, ecc_bytes, complen_bytes) = cd_header_sizes(output_len, frames);

        let ecc_flags = &data[..ecc_bytes];

        let base_length = if complen_bytes == 2 {
            BigEndian::read_u16(&data[ecc_bytes..ecc_bytes + 2]) as usize
        } else {
            BigEndian::read_u24(&data[ecc_bytes..ecc_bytes + 3]) as usize
        };

        let base_compressed = &data[header_bytes..header_bytes + base_length];
        let subcode_compressed = &data[header_bytes + base_length..];

        let expected_base_len = frames * SECTOR_SIZE;
        let expected_subcode_len = frames * SUBCODE_SIZE;

        let mut base_bytes = match base {
            CdBaseDecoder::Lzma => self.lzma.decompress(base_compressed, expected_base_len)?,
            CdBaseDecoder::Deflate => {
                deflate_decompress_with(&mut self.deflate, base_compressed, expected_base_len)?
            }
            CdBaseDecoder::Flac => {
                crate::chd::compression::flac::flac_decompress(base_compressed, expected_base_len)?
            }
        };

        let subcode =
            deflate_decompress_with(&mut self.deflate, subcode_compressed, expected_subcode_len)?;

        for frame in 0..frames {
            if ecc_flags[frame / 8] & (1 << (frame % 8)) != 0 {
                let sector = &mut base_bytes[frame * SECTOR_SIZE..(frame + 1) * SECTOR_SIZE];
                restore_sector_ecc(sector);
            }
        }

        let mut output = Vec::with_capacity(output_len);
        for frame in 0..frames {
            let base_offset = frame * SECTOR_SIZE;
            let subcode_offset = frame * SUBCODE_SIZE;
            output.extend_from_slice(&base_bytes[base_offset..base_offset + SECTOR_SIZE]);
            output.extend_from_slice(&subcode[subcode_offset..subcode_offset + SUBCODE_SIZE]);
        }
        Ok(output)
    }
}

enum CdBaseDecoder {
    Lzma,
    Deflate,
    Flac,
}

fn deflate_decompress_with(
    decompress: &mut flate2::Decompress,
    src: &[u8],
    expected_len: usize,
) -> ChdResult<Vec<u8>> {
    decompress.reset(false);
    let mut output = vec![0u8; expected_len];
    let before_out = decompress.total_out();
    let status = decompress
        .decompress(src, &mut output, flate2::FlushDecompress::Finish)
        .map_err(|e| io::Error::other(format!("deflate decompress error: {e}")))?;
    match status {
        flate2::Status::StreamEnd | flate2::Status::Ok => {}
        flate2::Status::BufError => {
            return Err(io::Error::other("deflate decompress buffer error").into());
        }
    }
    let written = (decompress.total_out() - before_out) as usize;
    output.truncate(written);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_sizes_small_hunk() {
        // data_len < 0x10000 -> complen_bytes = 2
        let (header, ecc, complen) = cd_header_sizes(0x8000, 8);
        assert_eq!(complen, 2);
        assert_eq!(ecc, 1); // 8 / 8 = 1
        assert_eq!(header, ecc + complen);
    }

    #[test]
    fn header_sizes_large_hunk() {
        // data_len >= 0x10000 -> complen_bytes = 3
        let (header, ecc, complen) = cd_header_sizes(0x10000, 8);
        assert_eq!(complen, 3);
        assert_eq!(ecc, 1);
        assert_eq!(header, ecc + complen);
    }

    #[test]
    fn header_sizes_ecc_rounds_up() {
        // 9 frames -> ceil(9/8) = 2 ecc bytes
        let (_, ecc, _) = cd_header_sizes(0x8000, 9);
        assert_eq!(ecc, 2);
    }

    #[test]
    fn split_cd_frames_valid() {
        // Build 2 frames: each FRAME_SIZE bytes (2352 sector + 96 subcode)
        let mut data = vec![0u8; 2 * FRAME_SIZE];
        // Mark sectors and subcodes with distinct values
        data[0] = 0xAA; // sector 0 first byte
        data[SECTOR_SIZE] = 0xBB; // subcode 0 first byte
        data[FRAME_SIZE] = 0xCC; // sector 1 first byte
        data[FRAME_SIZE + SECTOR_SIZE] = 0xDD; // subcode 1 first byte

        let (frames, base, subcode) = split_cd_frames(&data).unwrap();
        assert_eq!(frames, 2);
        assert_eq!(base.len(), 2 * SECTOR_SIZE);
        assert_eq!(subcode.len(), 2 * SUBCODE_SIZE);
        assert_eq!(base[0], 0xAA);
        assert_eq!(base[SECTOR_SIZE], 0xCC);
        assert_eq!(subcode[0], 0xBB);
        assert_eq!(subcode[SUBCODE_SIZE], 0xDD);
    }

    #[test]
    fn split_cd_frames_wrong_size_fails() {
        let data = vec![0u8; FRAME_SIZE + 1];
        assert!(split_cd_frames(&data).is_err());
    }

    #[test]
    fn deflate_round_trip() {
        let original = b"hello world, this is test data for deflate compression!";
        let compressed = deflate_compress(original).unwrap();
        let decompressed = deflate_decompress(&compressed, original.len()).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn deflate_round_trip_empty() {
        let original: &[u8] = b"";
        let compressed = deflate_compress(original).unwrap();
        let decompressed = deflate_decompress(&compressed, 0).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn cd_hunk_deflate_round_trip() {
        // Build a hunk of 8 frames with non-ECC data (no sync header)
        let mut hunk = vec![0u8; 8 * FRAME_SIZE];
        for (i, b) in hunk.iter_mut().enumerate() {
            *b = (i % 253) as u8;
        }

        let compressed = compress_cd_hunk(&hunk, deflate_compress, deflate_compress).unwrap();
        let decompressed = decompress_cd_hunk(
            &compressed,
            hunk.len(),
            deflate_decompress,
            deflate_decompress,
        )
        .unwrap();
        assert_eq!(decompressed, hunk);
    }
}
