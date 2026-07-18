use crate::cd::ecc::{has_valid_ecc, restore_sector_ecc, strip_sector_ecc};
use crate::cd::{FRAME_SIZE, SECTOR_SIZE, SUBCODE_SIZE};
use crate::chd::compression::cdfl::CD_SYNC_HEADER;
use crate::chd::compression::flac::FlacCompressor;
use crate::chd::compression::huffman8::huffman8_encode;
use crate::chd::compression::lzma::LzmaEncoder;
use crate::chd::error::{ChdError, ChdResult};
use byteorder::{BigEndian, ByteOrder};
#[cfg(test)]
use flate2::Compression;
#[cfg(test)]
use flate2::read::DeflateDecoder;
#[cfg(test)]
use flate2::write::DeflateEncoder;
use std::fmt::Debug;
use std::io;
#[cfg(test)]
use std::io::{Read, Write};

const CD_SHORT_HUNK_LIMIT: usize = 0x1_0000;
const CD_ECC_DIVISOR: usize = 8;

pub mod cdfl;
pub mod dvd;
pub mod flac;
pub mod huffman8;
pub mod lzma;

pub const fn tag_to_bytes(tag: &str) -> [u8; 4] {
    let bytes = tag.as_bytes();
    assert!(bytes.len() == 4, "tag must be exactly 4 bytes");
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

/// A single CHD hunk compressor, one of the codecs chdman implements.
/// The `Cd*` variants only decode/encode CD frame hunks (base + subcode
/// split); the rest apply to a hunk's raw bytes directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdCodec {
    Zlib,
    Zstd,
    Lzma,
    Huff,
    Flac,
    Cdzl,
    Cdzs,
    Cdlz,
    Cdfl,
}

impl ChdCodec {
    /// The chdman fourcc tag stored in the CHD header's compressor slots.
    pub const fn tag(self) -> [u8; 4] {
        tag_to_bytes(match self {
            ChdCodec::Zlib => "zlib",
            ChdCodec::Zstd => "zstd",
            ChdCodec::Lzma => "lzma",
            ChdCodec::Huff => "huff",
            ChdCodec::Flac => "flac",
            ChdCodec::Cdzl => "cdzl",
            ChdCodec::Cdzs => "cdzs",
            ChdCodec::Cdlz => "cdlz",
            ChdCodec::Cdfl => "cdfl",
        })
    }

    /// Look up the codec a header compressor slot's fourcc names.
    pub fn from_tag(tag: [u8; 4]) -> Option<ChdCodec> {
        match &tag {
            b"zlib" => Some(ChdCodec::Zlib),
            b"zstd" => Some(ChdCodec::Zstd),
            b"lzma" => Some(ChdCodec::Lzma),
            b"huff" => Some(ChdCodec::Huff),
            b"flac" => Some(ChdCodec::Flac),
            b"cdzl" => Some(ChdCodec::Cdzl),
            b"cdzs" => Some(ChdCodec::Cdzs),
            b"cdlz" => Some(ChdCodec::Cdlz),
            b"cdfl" => Some(ChdCodec::Cdfl),
            _ => None,
        }
    }

    fn is_cd_only(self) -> bool {
        matches!(
            self,
            ChdCodec::Cdzl | ChdCodec::Cdzs | ChdCodec::Cdlz | ChdCodec::Cdfl
        )
    }
}

impl std::fmt::Display for ChdCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(std::str::from_utf8(&self.tag()).expect("codec tags are ASCII"))
    }
}

impl std::str::FromStr for ChdCodec {
    type Err = ChdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "zlib" => Ok(ChdCodec::Zlib),
            "zstd" => Ok(ChdCodec::Zstd),
            "lzma" => Ok(ChdCodec::Lzma),
            "huff" => Ok(ChdCodec::Huff),
            "flac" => Ok(ChdCodec::Flac),
            "cdzl" => Ok(ChdCodec::Cdzl),
            "cdzs" => Ok(ChdCodec::Cdzs),
            "cdlz" => Ok(ChdCodec::Cdlz),
            "cdfl" => Ok(ChdCodec::Cdfl),
            other => Err(ChdError::UnknownCodecName(other.to_string())),
        }
    }
}

/// Deflate (zlib) compression level for a user-supplied `--level` in
/// `1..=22`. Unset uses `flate2`'s best-compression preset (9); a set
/// level is clamped to deflate's 0..=9 range.
pub fn deflate_level(level: Option<i32>) -> flate2::Compression {
    match level {
        None => flate2::Compression::best(),
        Some(l) => flate2::Compression::new(l.min(9) as u32),
    }
}

/// LZMA compression level for a user-supplied `--level` in `1..=22`.
/// Unset defaults to chdman's level 8; a set level is clamped to
/// LZMA's 0..=9 range.
pub fn lzma_level(level: Option<i32>) -> u32 {
    match level {
        None => 8,
        Some(l) => l.min(9) as u32,
    }
}

/// Zstd compression level for a user-supplied `--level` in `1..=22`.
/// Unset defaults to chdman's max level 19; unlike deflate/lzma, zstd
/// accepts the full requested range unclamped.
pub fn zstd_level(level: Option<i32>) -> i32 {
    level.unwrap_or(19)
}

/// Parse a comma-separated chdman-style codec list, e.g. `"cdlz,cdzl,cdfl"`.
pub fn parse_codec_list(s: &str) -> Result<Vec<ChdCodec>, ChdError> {
    s.split(',').map(|part| part.trim().parse()).collect()
}

/// Validate a codec list for CHD header use: non-empty, at most the 4
/// header compressor slots, no duplicates, and (for DVD-mode CHDs) no
/// CD-only codec since DVD hunks are never CD frame-split.
pub fn validate_codecs(codecs: &[ChdCodec], dvd: bool) -> Result<(), ChdError> {
    if codecs.is_empty() {
        return Err(ChdError::EmptyCodecList);
    }
    if codecs.len() > 4 {
        return Err(ChdError::TooManyCodecs(codecs.len()));
    }
    for (i, codec) in codecs.iter().enumerate() {
        if codecs[..i].contains(codec) {
            return Err(ChdError::DuplicateCodec(*codec));
        }
        if dvd && codec.is_cd_only() {
            return Err(ChdError::CdCodecOnDvd(*codec));
        }
    }
    Ok(())
}

/// chdman `createcd`'s default codec pack.
pub fn default_cd_codecs() -> Vec<ChdCodec> {
    vec![ChdCodec::Cdlz, ChdCodec::Cdzl, ChdCodec::Cdfl]
}

/// chdman `createdvd`'s default codec pack.
pub fn default_dvd_codecs() -> Vec<ChdCodec> {
    vec![
        ChdCodec::Lzma,
        ChdCodec::Zlib,
        ChdCodec::Huff,
        ChdCodec::Flac,
    ]
}

/// Fill the CHD header's four compressor slots from a resolved codec
/// list, in user order; unused slots stay zero.
pub(crate) fn codec_header_slots(codecs: &[ChdCodec]) -> [[u8; 4]; 4] {
    let mut slots = [[0u8; 4]; 4];
    for (slot, codec) in slots.iter_mut().zip(codecs.iter()) {
        *slot = codec.tag();
    }
    slots
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

// The plain one-shot compress_cd_hunk/decompress_cd_hunk/deflate_*
// pair below is only exercised by tests now: production hunks go
// through CdCodecSet/CdDecoderSet's persistent-state codecs
// (deflate_with_reset/RawDecoders) instead.
#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn deflate_compress(data: &[u8]) -> ChdResult<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

#[cfg(test)]
pub(crate) fn deflate_decompress(data: &[u8], _expected_len: usize) -> ChdResult<Vec<u8>> {
    let mut decoder = DeflateDecoder::new(data);
    let mut output = Vec::new();
    decoder.read_to_end(&mut output)?;
    Ok(output)
}

#[cfg(test)]
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
/// The trial runs the resolved header codec list in slot order; the CD frame
/// codecs (cd*) split base + subcode, the generic codecs compress the raw hunk
/// buffer as chdman does.
pub(crate) struct CdCodecSet {
    codecs: Vec<ChdCodec>,
    lzma: Option<LzmaEncoder>,
    deflate: flate2::Compress,
    zstd: Option<::zstd::bulk::Compressor<'static>>,
}

impl CdCodecSet {
    pub fn new(hunk_bytes: usize, codecs: Vec<ChdCodec>, level: Option<i32>) -> ChdResult<Self> {
        let needs_lzma = codecs
            .iter()
            .any(|c| matches!(c, ChdCodec::Lzma | ChdCodec::Cdlz));
        let needs_zstd = codecs
            .iter()
            .any(|c| matches!(c, ChdCodec::Zstd | ChdCodec::Cdzs));
        Ok(Self {
            lzma: needs_lzma
                .then(|| LzmaEncoder::new(hunk_bytes, lzma_level(level) as i32))
                .transpose()?,
            deflate: flate2::Compress::new(deflate_level(level), false),
            zstd: needs_zstd
                .then(|| ::zstd::bulk::Compressor::new(zstd_level(level)))
                .transpose()?,
            codecs,
        })
    }

    /// Compress a CD hunk trying every resolved codec in slot order,
    /// return `(compressed_data, slot_index)` for the smallest result,
    /// or the raw hunk with [`ChdCompression::None`] when all lose.
    pub fn compress_hunk(&mut self, hunk: &[u8]) -> ChdResult<(Vec<u8>, u8)> {
        let (frames, mut base, subcode) = split_cd_frames(hunk)?;
        let (header_bytes, ecc_bytes, complen_bytes) = cd_header_sizes(hunk.len(), frames);

        // MAME's cd_flac copies the audio straight from the source and
        // never strips ECC or writes ecc-flag/complen header bytes; it
        // only ever wins on all-audio hunks. Mirror that: offer cdfl
        // only when no frame carries a data-sector sync header, and run
        // it on the raw (pre-strip) base so ECC-stripped bytes can never
        // reach the FLAC path.
        let cdfl_candidate =
            (0..frames).all(|f| base[f * SECTOR_SIZE..f * SECTOR_SIZE + 12] != CD_SYNC_HEADER);
        let mut cdfl_result = if cdfl_candidate && self.codecs.contains(&ChdCodec::Cdfl) {
            self.compress_cdfl(&base, &subcode).ok()
        } else {
            None
        };

        let ecc_flags = strip_ecc_from_base(&mut base, frames, ecc_bytes);

        let mut best: Option<Vec<u8>> = None;
        let mut best_slot = ChdCompression::None as u8;
        let best_len = |best: &Option<Vec<u8>>| best.as_ref().map_or(hunk.len(), |b| b.len());

        for slot in 0..self.codecs.len() {
            let candidate = match self.codecs[slot] {
                ChdCodec::Cdlz => self
                    .compress_cdlz(
                        &base,
                        &subcode,
                        &ecc_flags,
                        header_bytes,
                        ecc_bytes,
                        complen_bytes,
                    )
                    .ok(),
                ChdCodec::Cdzl => self
                    .compress_cdzl(
                        &base,
                        &subcode,
                        &ecc_flags,
                        header_bytes,
                        ecc_bytes,
                        complen_bytes,
                    )
                    .ok(),
                ChdCodec::Cdfl => cdfl_result.take(),
                ChdCodec::Cdzs => self
                    .compress_cdzs(
                        &base,
                        &subcode,
                        &ecc_flags,
                        header_bytes,
                        ecc_bytes,
                        complen_bytes,
                    )
                    .ok(),
                codec => self.compress_generic(codec, hunk),
            };
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

    fn compress_cdlz(
        &mut self,
        base: &[u8],
        subcode: &[u8],
        ecc_flags: &[u8],
        header_bytes: usize,
        ecc_bytes: usize,
        complen_bytes: usize,
    ) -> ChdResult<Vec<u8>> {
        let base_compressed = self.lzma.as_ref().unwrap().compress(base)?;
        let subcode_compressed = deflate_with_reset(&mut self.deflate, subcode)?;
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
        let base_compressed = deflate_with_reset(&mut self.deflate, base)?;
        let subcode_compressed = deflate_with_reset(&mut self.deflate, subcode)?;
        Ok(assemble_cd_output(
            ecc_flags,
            header_bytes,
            ecc_bytes,
            complen_bytes,
            &base_compressed,
            &subcode_compressed,
        ))
    }

    /// MAME's headerless cdfl layout: FLAC frames of the audio data
    /// immediately followed by the raw-deflate subcode stream. No
    /// ecc-flag bytes, no complen field; the FLAC stream is self-
    /// delimiting so the decoder finds the subcode offset from it.
    fn compress_cdfl(&mut self, base: &[u8], subcode: &[u8]) -> ChdResult<Vec<u8>> {
        let mut output = flac::cdfl_compress(base)?;
        let subcode_compressed = deflate_with_reset(&mut self.deflate, subcode)?;
        output.extend_from_slice(&subcode_compressed);
        Ok(output)
    }

    fn compress_cdzs(
        &mut self,
        base: &[u8],
        subcode: &[u8],
        ecc_flags: &[u8],
        header_bytes: usize,
        ecc_bytes: usize,
        complen_bytes: usize,
    ) -> ChdResult<Vec<u8>> {
        let zstd = self.zstd.as_mut().unwrap();
        let base_compressed = zstd.compress(base)?;
        let subcode_compressed = zstd.compress(subcode)?;
        Ok(assemble_cd_output(
            ecc_flags,
            header_bytes,
            ecc_bytes,
            complen_bytes,
            &base_compressed,
            &subcode_compressed,
        ))
    }

    /// Generic (non-CD-frame) codec on the raw hunk buffer, matching
    /// chdman's plain codecs. Returns `None` when the codec is not
    /// applicable so the trial simply skips it.
    fn compress_generic(&mut self, codec: ChdCodec, hunk: &[u8]) -> Option<Vec<u8>> {
        compress_raw_codec(
            codec,
            hunk,
            self.lzma.as_ref(),
            &mut self.deflate,
            self.zstd.as_mut(),
        )
    }
}

/// Compress `hunk` with one generic codec using the caller's persistent
/// encoder state. Shared by the CD and DVD codec sets.
fn compress_raw_codec(
    codec: ChdCodec,
    hunk: &[u8],
    lzma: Option<&LzmaEncoder>,
    deflate: &mut flate2::Compress,
    zstd: Option<&mut ::zstd::bulk::Compressor<'static>>,
) -> Option<Vec<u8>> {
    match codec {
        ChdCodec::Lzma => lzma.unwrap().compress(hunk).ok(),
        ChdCodec::Zlib => deflate_with_reset(deflate, hunk).ok(),
        ChdCodec::Zstd => zstd.unwrap().compress(hunk).ok(),
        ChdCodec::Huff => {
            let mut out = Vec::new();
            huffman8_encode(hunk, &mut out).ok().map(|()| out)
        }
        ChdCodec::Flac => FlacCompressor.compress(hunk).ok(),
        ChdCodec::Cdlz | ChdCodec::Cdzl | ChdCodec::Cdfl | ChdCodec::Cdzs => None,
    }
}

pub(crate) fn deflate_with_reset(
    compressor: &mut flate2::Compress,
    data: &[u8],
) -> ChdResult<Vec<u8>> {
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

/// Persistent generic-codec decoder state shared by the CD and DVD
/// reader worker sets. Each decoder inflates a whole hunk buffer the
/// way chdman's plain codecs do (no CD frame split), and the same
/// state also drives the base/subcode streams of the CD frame codecs.
pub(crate) struct RawDecoders {
    lzma: lzma::LzmaDecoder,
    deflate: flate2::Decompress,
    zstd: ::zstd::bulk::Decompressor<'static>,
}

impl RawDecoders {
    pub(crate) fn new(hunk_bytes: usize) -> ChdResult<Self> {
        Ok(Self {
            lzma: lzma::LzmaDecoder::new(hunk_bytes)?,
            deflate: flate2::Decompress::new(false),
            zstd: ::zstd::bulk::Decompressor::new()?,
        })
    }

    /// Decode a whole hunk with one generic codec. The CD frame codecs
    /// (`cd*`) are not generic and must be routed by the caller; they
    /// come back as an explicit unsupported-codec error naming the tag.
    pub(crate) fn decode(
        &mut self,
        codec: ChdCodec,
        data: &[u8],
        output_len: usize,
    ) -> ChdResult<Vec<u8>> {
        match codec {
            ChdCodec::Lzma => self.lzma.decompress(data, output_len),
            ChdCodec::Zlib => deflate_decompress_with(&mut self.deflate, data, output_len),
            ChdCodec::Zstd => Ok(self.zstd.decompress(data, output_len)?),
            ChdCodec::Huff => huffman8::huffman8_decode(data, output_len),
            ChdCodec::Flac => flac::flac_decompress_chd_raw(data, output_len),
            cd @ (ChdCodec::Cdlz | ChdCodec::Cdzl | ChdCodec::Cdfl | ChdCodec::Cdzs) => {
                Err(ChdError::UnknownCompressionCodec(cd.tag()))
            }
        }
    }

    fn decode_cd_stream(
        &mut self,
        stream: CdStream,
        data: &[u8],
        expected_len: usize,
    ) -> ChdResult<Vec<u8>> {
        match stream {
            CdStream::Lzma => self.lzma.decompress(data, expected_len),
            CdStream::Deflate => deflate_decompress_with(&mut self.deflate, data, expected_len),
            CdStream::Zstd => Ok(self.zstd.decompress(data, expected_len)?),
        }
    }
}

/// One CD frame codec's base or subcode stream codec.
#[derive(Debug, Clone, Copy)]
enum CdStream {
    Lzma,
    Deflate,
    Zstd,
}

/// Persistent decoder state for CD hunk decompression. Each map entry's
/// slot index resolves against the header compressor tags, so any codec
/// combination decodes correctly: the CD frame codecs (`cdlz`/`cdzl`/
/// `cdfl`/`cdzs`) split base + subcode and interleave with ECC restore,
/// the generic codecs inflate the whole hunk buffer as chdman does.
pub(crate) struct CdDecoderSet {
    slots: [Option<ChdCodec>; 4],
    raw: RawDecoders,
}

impl CdDecoderSet {
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

    /// Decompress the hunk stored under map `slot`, resolving the slot
    /// to its header codec first.
    pub fn decompress(&mut self, slot: u8, data: &[u8], output_len: usize) -> ChdResult<Vec<u8>> {
        let codec = self
            .slots
            .get(slot as usize)
            .copied()
            .flatten()
            .ok_or(ChdError::UnknownCompressionCodec([slot, 0, 0, 0]))?;
        match codec {
            ChdCodec::Cdlz => {
                self.decompress_cd_hunk(data, output_len, CdStream::Lzma, CdStream::Deflate)
            }
            ChdCodec::Cdzl => {
                self.decompress_cd_hunk(data, output_len, CdStream::Deflate, CdStream::Deflate)
            }
            ChdCodec::Cdfl => self.decompress_cdfl(data, output_len),
            ChdCodec::Cdzs => {
                self.decompress_cd_hunk(data, output_len, CdStream::Zstd, CdStream::Zstd)
            }
            generic => self.raw.decode(generic, data, output_len),
        }
    }

    fn decompress_cd_hunk(
        &mut self,
        data: &[u8],
        output_len: usize,
        base: CdStream,
        sub: CdStream,
    ) -> ChdResult<Vec<u8>> {
        let frames = output_len / FRAME_SIZE;
        let (header_bytes, ecc_bytes, complen_bytes) = cd_header_sizes(output_len, frames);

        if data.len() < header_bytes {
            return Err(ChdError::MalformedHunk);
        }
        let ecc_flags = &data[..ecc_bytes];

        let base_length = if complen_bytes == 2 {
            BigEndian::read_u16(&data[ecc_bytes..ecc_bytes + 2]) as usize
        } else {
            BigEndian::read_u24(&data[ecc_bytes..ecc_bytes + 3]) as usize
        };

        let base_end = header_bytes
            .checked_add(base_length)
            .filter(|&e| e <= data.len())
            .ok_or(ChdError::MalformedHunk)?;
        let base_compressed = &data[header_bytes..base_end];
        let subcode_compressed = &data[base_end..];

        let expected_base_len = frames * SECTOR_SIZE;
        let expected_subcode_len = frames * SUBCODE_SIZE;

        let mut base_bytes = self
            .raw
            .decode_cd_stream(base, base_compressed, expected_base_len)?;
        let subcode = self
            .raw
            .decode_cd_stream(sub, subcode_compressed, expected_subcode_len)?;

        for frame in 0..frames {
            if ecc_flags[frame / 8] & (1 << (frame % 8)) != 0 {
                let sector = &mut base_bytes[frame * SECTOR_SIZE..(frame + 1) * SECTOR_SIZE];
                restore_sector_ecc(sector);
            }
        }

        interleave_cd_hunk(output_len, frames, &base_bytes, &subcode)
    }

    /// Decode MAME's headerless cdfl hunk: FLAC audio frames followed by
    /// the raw-deflate subcode. The FLAC stream is self-delimiting, so
    /// its consumed length locates the subcode. No ecc-flag header and
    /// no ECC restore, matching `chd_cd_flac_decompressor`.
    fn decompress_cdfl(&mut self, data: &[u8], output_len: usize) -> ChdResult<Vec<u8>> {
        let frames = output_len / FRAME_SIZE;
        let expected_base_len = frames * SECTOR_SIZE;
        let expected_subcode_len = frames * SUBCODE_SIZE;

        let (base_bytes, consumed) = flac::cdfl_decompress(data, expected_base_len)?;
        let subcode_compressed = data.get(consumed..).ok_or(ChdError::MalformedHunk)?;
        let subcode = deflate_decompress_with(
            &mut self.raw.deflate,
            subcode_compressed,
            expected_subcode_len,
        )?;

        interleave_cd_hunk(output_len, frames, &base_bytes, &subcode)
    }
}

/// Reassemble a CD hunk from decoded base sectors and subcode by
/// interleaving one sector then its subcode per frame.
fn interleave_cd_hunk(
    output_len: usize,
    frames: usize,
    base_bytes: &[u8],
    subcode: &[u8],
) -> ChdResult<Vec<u8>> {
    let mut output = Vec::with_capacity(output_len);
    for frame in 0..frames {
        let base_offset = frame * SECTOR_SIZE;
        let subcode_offset = frame * SUBCODE_SIZE;
        output.extend_from_slice(&base_bytes[base_offset..base_offset + SECTOR_SIZE]);
        output.extend_from_slice(&subcode[subcode_offset..subcode_offset + SUBCODE_SIZE]);
    }
    Ok(output)
}

pub(crate) fn deflate_decompress_with(
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
    fn parse_codec_list_valid() {
        let codecs = parse_codec_list("cdlz,cdzl,cdfl").unwrap();
        assert_eq!(codecs, vec![ChdCodec::Cdlz, ChdCodec::Cdzl, ChdCodec::Cdfl]);
    }

    #[test]
    fn parse_codec_list_unknown_name() {
        assert!(matches!(
            parse_codec_list("bogus"),
            Err(ChdError::UnknownCodecName(name)) if name == "bogus"
        ));
    }

    #[test]
    fn validate_codecs_rejects_empty() {
        assert!(matches!(
            validate_codecs(&[], false),
            Err(ChdError::EmptyCodecList)
        ));
    }

    #[test]
    fn validate_codecs_rejects_too_many() {
        let codecs = parse_codec_list("zlib,zstd,lzma,huff,flac").unwrap();
        assert!(matches!(
            validate_codecs(&codecs, false),
            Err(ChdError::TooManyCodecs(5))
        ));
    }

    #[test]
    fn validate_codecs_rejects_duplicates() {
        let codecs = parse_codec_list("cdlz,cdlz").unwrap();
        assert!(matches!(
            validate_codecs(&codecs, false),
            Err(ChdError::DuplicateCodec(ChdCodec::Cdlz))
        ));
    }

    #[test]
    fn validate_codecs_rejects_cd_codec_on_dvd() {
        let codecs = default_cd_codecs();
        assert!(matches!(
            validate_codecs(&codecs, true),
            Err(ChdError::CdCodecOnDvd(ChdCodec::Cdlz))
        ));
    }

    #[test]
    fn validate_codecs_accepts_defaults() {
        assert!(validate_codecs(&default_cd_codecs(), false).is_ok());
        assert!(validate_codecs(&default_dvd_codecs(), true).is_ok());
    }

    #[test]
    fn level_mapping_defaults_when_unset() {
        assert_eq!(deflate_level(None).level(), 9);
        assert_eq!(lzma_level(None), 8);
        assert_eq!(zstd_level(None), 19);
    }

    #[test]
    fn level_mapping_passes_through_mid_range() {
        assert_eq!(deflate_level(Some(5)).level(), 5);
        assert_eq!(lzma_level(Some(5)), 5);
        assert_eq!(zstd_level(Some(5)), 5);
    }

    #[test]
    fn level_mapping_clamps_deflate_and_lzma_at_high_levels() {
        assert_eq!(deflate_level(Some(22)).level(), 9);
        assert_eq!(lzma_level(Some(22)), 9);
        assert_eq!(zstd_level(Some(22)), 22);
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

    fn cd_hunk(frames: usize) -> Vec<u8> {
        // Frame-shaped, compressible, and no Mode-1 sync header so the
        // codec trial runs every candidate without ECC stripping.
        let mut hunk = vec![0u8; frames * FRAME_SIZE];
        for (i, b) in hunk.iter_mut().enumerate() {
            *b = (i / 64) as u8;
        }
        hunk
    }

    fn cd_roundtrip(codecs: Vec<ChdCodec>) {
        cd_roundtrip_leveled(codecs, None);
    }

    fn cd_roundtrip_leveled(codecs: Vec<ChdCodec>, level: Option<i32>) {
        let hunk = cd_hunk(8);
        let mut enc = CdCodecSet::new(hunk.len(), codecs.clone(), level).unwrap();
        let (data, slot) = enc.compress_hunk(&hunk).unwrap();
        let mut dec = CdDecoderSet::new(codec_header_slots(&codecs), hunk.len()).unwrap();
        let decoded = if slot == ChdCompression::None as u8 {
            data
        } else {
            dec.decompress(slot, &data, hunk.len()).unwrap()
        };
        assert_eq!(decoded, hunk, "codec set {codecs:?} level {level:?}");
    }

    #[test]
    fn cd_default_set_round_trips() {
        cd_roundtrip(default_cd_codecs());
    }

    #[test]
    fn cd_cdzs_set_round_trips() {
        cd_roundtrip(vec![ChdCodec::Cdzs]);
    }

    #[test]
    fn cd_generic_zlib_only_round_trips() {
        cd_roundtrip(vec![ChdCodec::Zlib]);
    }

    #[test]
    fn cd_mixed_generic_and_cd_set_round_trips() {
        cd_roundtrip(vec![ChdCodec::Zstd, ChdCodec::Cdfl]);
    }

    /// Audio-like hunk (interleaved 16-bit sine, big-endian sample bytes
    /// so cdfl's FLAC predictors beat the lzma/deflate base codecs).
    /// Proves cdfl actually wins the trial and round-trips byte-exact.
    #[test]
    fn cd_cdfl_wins_on_audio_and_round_trips() {
        let frames = 8usize;
        let mut hunk = vec![0u8; frames * FRAME_SIZE];
        let mut n = 0usize;
        for frame in 0..frames {
            let sector = &mut hunk[frame * FRAME_SIZE..frame * FRAME_SIZE + SECTOR_SIZE];
            for pair in sector.chunks_exact_mut(2) {
                let v = ((n as f64 / 20.0).sin() * 8000.0) as i16;
                pair.copy_from_slice(&v.to_be_bytes());
                n += 1;
            }
        }

        let codecs = vec![ChdCodec::Cdlz, ChdCodec::Cdzl, ChdCodec::Cdfl];
        let mut enc = CdCodecSet::new(hunk.len(), codecs.clone(), None).unwrap();
        let (data, slot) = enc.compress_hunk(&hunk).unwrap();
        assert_ne!(slot, ChdCompression::None as u8, "no codec candidate won");
        assert_eq!(codecs[slot as usize], ChdCodec::Cdfl, "cdfl did not win");

        let mut dec = CdDecoderSet::new(codec_header_slots(&codecs), hunk.len()).unwrap();
        let decoded = dec.decompress(slot, &data, hunk.len()).unwrap();
        assert_eq!(decoded, hunk);
    }

    /// A leveled codec (`cdzs`, backed by zstd's per-level knob) must
    /// round-trip byte-for-byte at both the lowest and highest
    /// `--level` values chdman accepts.
    #[test]
    fn cd_leveled_codec_round_trips_at_level_extremes() {
        for level in [Some(1), Some(22)] {
            cd_roundtrip_leveled(vec![ChdCodec::Cdzs], level);
        }
    }

    #[test]
    fn cd_decoder_rejects_unsupported_tag() {
        let result = CdDecoderSet::new([*b"avhu", [0; 4], [0; 4], [0; 4]], 8 * FRAME_SIZE);
        assert!(matches!(
            result,
            Err(ChdError::UnknownCompressionCodec(tag)) if &tag == b"avhu"
        ));
    }
}
