//! MAME `avhuff` codec port: the A/V hunk format `chdman createld`
//! writes for laserdisc CHDs (MAME `avhuff.cpp`).
//!
//! A hunk is one video frame (one field, when interlaced) plus that
//! frame's slice of audio. Raw hunks carry a `'chav'` header, planar
//! big-endian audio and big-endian YUY16 video; the compressed form is
//! a small header, one headerless FLAC stream per audio channel, then a
//! lossless delta-RLE + huffman video stream.
//!
//! Everything multibyte is big-endian. The huffman core (tree building
//! and canonical code assignment) is shared with
//! [`super::huffman8`]; the tree serialization is the RLE form also
//! used by the compressed map in `chd/map.rs`.

use crate::chd::compression::flac::{
    Endian, bytes_from_samples, encode_flac_samples, samples_from_bytes, strip_flac_stream_header,
};
use crate::chd::compression::huffman8::canonical_codes;
use crate::chd::error::{ChdError, ChdResult};
use std::io::{self, Cursor};

/// Bytes of `'chav'` header ahead of the audio and video payloads.
const RAW_HEADER_SIZE: usize = 12;
/// 256 delta codes plus 16 RLE run-length codes.
const VIDEO_CODES: usize = 256 + 16;
const VIDEO_MAX_BITS: u8 = 16;
/// Marker in the audio-tree-size field meaning the streams are FLAC.
const FLAC_TREE_MARKER: u16 = 0xFFFF;
/// avhuff declares this rate in every audio stream regardless of the
/// source's real rate; the CHD's `AVAV` metadata carries the truth.
const FLAC_SAMPLE_RATE: usize = 48_000;
/// First byte of the video stream, marking the lossless encoding.
const LOSSLESS_MARKER: u32 = 0x80;

fn avhuff_err(msg: &str) -> ChdError {
    io::Error::other(format!("avhuff: {msg}")).into()
}

/// Size of the raw (uncompressed) frame for these dimensions, MAME's
/// `avhuff_encoder::raw_data_size`. This is also the CHD hunk and unit
/// size for a laserdisc image.
pub fn raw_data_size(width: u32, height: u32, channels: u8, samples: u32) -> usize {
    RAW_HEADER_SIZE
        + channels as usize * samples as usize * 2
        + width as usize * height as usize * 2
}

/// Assemble one raw avhuff frame: the `'chav'` header, each audio
/// channel's samples as big-endian 16-bit planes, then the YUY16 video
/// words big-endian. Every channel must supply the same sample count.
pub fn assemble_raw_frame(
    width: u16,
    height: u16,
    video: &[u16],
    audio: &[&[i16]],
) -> ChdResult<Vec<u8>> {
    if video.len() != width as usize * height as usize {
        return Err(avhuff_err("video size does not match the frame dimensions"));
    }
    let channels = u8::try_from(audio.len())
        .map_err(|_| avhuff_err("more than 255 audio channels in a frame"))?;
    let samples = audio.first().map_or(0, |channel| channel.len());
    if audio.iter().any(|channel| channel.len() != samples) {
        return Err(avhuff_err("audio channels have differing sample counts"));
    }
    let samples =
        u16::try_from(samples).map_err(|_| avhuff_err("more than 65535 samples in a frame"))?;

    let mut out = Vec::with_capacity(raw_data_size(
        width as u32,
        height as u32,
        channels,
        samples as u32,
    ));
    out.extend_from_slice(b"chav");
    out.push(0); // metadata size; chdman never emits frame metadata
    out.push(channels);
    out.extend_from_slice(&samples.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    for channel in audio {
        for &sample in *channel {
            out.extend_from_slice(&sample.to_be_bytes());
        }
    }
    for &word in video {
        out.extend_from_slice(&word.to_be_bytes());
    }
    Ok(out)
}

/// Compress one raw avhuff frame into a CHD `avhu` hunk. `raw` must
/// start with a `'chav'` header and may carry trailing zero padding up
/// to the hunk size, which is not encoded.
pub fn encode(raw: &[u8]) -> ChdResult<Vec<u8>> {
    if raw.len() < RAW_HEADER_SIZE || &raw[0..4] != b"chav" {
        return Err(avhuff_err("raw frame is missing its 'chav' header"));
    }
    if raw[4] != 0 {
        return Err(avhuff_err("frame metadata is not supported"));
    }
    let channels = raw[5] as usize;
    let samples = u16::from_be_bytes([raw[6], raw[7]]) as usize;
    let width = u16::from_be_bytes([raw[8], raw[9]]) as usize;
    let height = u16::from_be_bytes([raw[10], raw[11]]) as usize;

    let audio_bytes = channels * samples * 2;
    let video_bytes = width * height * 2;
    if raw.len() < RAW_HEADER_SIZE + audio_bytes + video_bytes {
        return Err(avhuff_err("raw frame is shorter than its header describes"));
    }

    let mut dest = vec![0u8; 10 + 2 * channels];
    dest[1] = raw[5];
    dest[2..8].copy_from_slice(&raw[6..12]);

    if channels > 0 {
        dest[8..10].copy_from_slice(&FLAC_TREE_MARKER.to_be_bytes());
        for channel in 0..channels {
            let start = RAW_HEADER_SIZE + channel * samples * 2;
            let stream = encode_audio(&raw[start..start + samples * 2])?;
            let size = u16::try_from(stream.len())
                .map_err(|_| avhuff_err("compressed audio stream exceeds 65535 bytes"))?;
            dest[10 + 2 * channel..12 + 2 * channel].copy_from_slice(&size.to_be_bytes());
            dest.extend_from_slice(&stream);
        }
    }

    if width > 0 && height > 0 {
        let video = &raw[RAW_HEADER_SIZE + audio_bytes..][..video_bytes];
        dest.extend_from_slice(&encode_video(video, width, height)?);
    }
    Ok(dest)
}

/// Decompress a CHD `avhu` hunk back to its raw `'chav'` frame,
/// zero-padded to `out_len` (the CHD's hunk size).
pub fn decode(compressed: &[u8], out_len: usize) -> ChdResult<Vec<u8>> {
    if compressed.len() < 10 {
        return Err(avhuff_err("compressed hunk is too short for its header"));
    }
    if compressed[0] != 0 {
        return Err(avhuff_err("frame metadata is not supported"));
    }
    let channels = compressed[1] as usize;
    let samples = u16::from_be_bytes([compressed[2], compressed[3]]) as usize;
    let width = u16::from_be_bytes([compressed[4], compressed[5]]) as usize;
    let height = u16::from_be_bytes([compressed[6], compressed[7]]) as usize;

    let header_size = 10 + 2 * channels;
    if compressed.len() < header_size {
        return Err(avhuff_err("compressed hunk is too short for its header"));
    }
    let tree_size = u16::from_be_bytes([compressed[8], compressed[9]]);
    if channels > 0 && tree_size != FLAC_TREE_MARKER {
        return Err(avhuff_err("huffman-coded audio is not supported"));
    }

    let sizes: Vec<usize> = (0..channels)
        .map(|channel| {
            let at = 10 + 2 * channel;
            u16::from_be_bytes([compressed[at], compressed[at + 1]]) as usize
        })
        .collect();
    let total: usize = header_size + sizes.iter().sum::<usize>();
    if total >= compressed.len() {
        return Err(avhuff_err("compressed hunk is shorter than its streams"));
    }

    if out_len < raw_data_size(width as u32, height as u32, compressed[1], samples as u32) {
        return Err(avhuff_err("hunk size is too small for the decoded frame"));
    }

    let mut out = vec![0u8; out_len];
    out[0..4].copy_from_slice(b"chav");
    out[5..12].copy_from_slice(&compressed[1..8]);

    let mut src = header_size;
    for (channel, &size) in sizes.iter().enumerate() {
        let dest = RAW_HEADER_SIZE + channel * samples * 2;
        out[dest..dest + samples * 2]
            .copy_from_slice(&decode_audio(&compressed[src..src + size], samples)?);
        src += size;
    }

    if width > 0 && height > 0 {
        let dest = RAW_HEADER_SIZE + channels * samples * 2;
        out[dest..dest + width * height * 2].copy_from_slice(&decode_video(
            &compressed[src..],
            width,
            height,
        )?);
    }
    Ok(out)
}

// -- audio ------------------------------------------------------------

/// FLAC block size for a frame's sample count. avhuff sets the block
/// size to the exact sample count so each frame is a single block; the
/// clamp only guards degenerate frames flacenc would reject.
fn flac_block_size(samples: usize) -> usize {
    samples.clamp(
        flacenc::constant::MIN_BLOCK_SIZE,
        flacenc::constant::MAX_BLOCK_SIZE,
    )
}

/// The 42-byte stream header MAME's decoder synthesizes for avhuff
/// audio: `fLaC` plus a STREAMINFO of 48 kHz, mono, 16-bit. avhuff
/// stores the FLAC frames alone, with magic and metadata stripped.
fn flac_stream_header(block_size: usize) -> [u8; 42] {
    let mut header = [0u8; 42];
    header[0..4].copy_from_slice(b"fLaC");
    header[4] = 0x80; // last metadata block, type STREAMINFO
    header[7] = 0x22; // STREAMINFO length (34)
    let block_size = block_size as u16;
    header[8..10].copy_from_slice(&block_size.to_be_bytes());
    header[10..12].copy_from_slice(&block_size.to_be_bytes());
    // 20-bit sample rate, 3-bit channels-1, 5-bit bits-per-sample-1.
    let packed = ((FLAC_SAMPLE_RATE as u32) << 12) | (15 << 4);
    header[18..22].copy_from_slice(&packed.to_be_bytes());
    header
}

/// Encode one channel's big-endian 16-bit samples as a headerless
/// mono FLAC stream.
fn encode_audio(samples_be: &[u8]) -> ChdResult<Vec<u8>> {
    if samples_be.is_empty() {
        return Ok(Vec::new());
    }
    let samples = samples_from_bytes(samples_be, Endian::Big);
    let stream = encode_flac_samples(
        &samples,
        1,
        FLAC_SAMPLE_RATE,
        flac_block_size(samples.len()),
    )?;
    Ok(strip_flac_stream_header(&stream).to_vec())
}

/// Decode one channel's headerless mono FLAC stream back to
/// big-endian 16-bit samples.
fn decode_audio(stream: &[u8], samples: usize) -> ChdResult<Vec<u8>> {
    if samples == 0 {
        return Ok(Vec::new());
    }
    let header = flac_stream_header(flac_block_size(samples));
    let mut full = Vec::with_capacity(header.len() + stream.len());
    full.extend_from_slice(&header);
    full.extend_from_slice(stream);

    let mut reader = claxon::FlacReader::new(Cursor::new(full))
        .map_err(|e| avhuff_err(&format!("audio stream header: {e}")))?;
    let mut decoded = Vec::with_capacity(samples);
    for sample in reader.samples() {
        decoded.push(sample.map_err(|e| avhuff_err(&format!("audio stream: {e}")))?);
        if decoded.len() == samples {
            break;
        }
    }
    if decoded.len() < samples {
        return Err(avhuff_err("audio stream is short"));
    }
    Ok(bytes_from_samples(&decoded, &Endian::Big))
}

// -- bit I/O ----------------------------------------------------------

/// MSB-first bit writer mirroring MAME's `bitstream_out`: writes past
/// `limit` are dropped but still counted, and [`BitWriter::flush`]
/// left-aligns the trailing partial byte and returns the cumulative
/// byte count, which avhuff uses to size its sections.
struct BitWriter {
    out: Vec<u8>,
    limit: usize,
    written: usize,
    accum: u32,
    bits: u8,
}

impl BitWriter {
    fn new(limit: usize) -> Self {
        Self {
            out: Vec::with_capacity(limit.min(1 << 20)),
            limit,
            written: 0,
            accum: 0,
            bits: 0,
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.written < self.limit {
            self.out.push(byte);
        }
        self.written += 1;
    }

    fn write(&mut self, value: u32, numbits: u8) {
        for i in (0..numbits).rev() {
            self.accum = (self.accum << 1) | ((value >> i) & 1);
            self.bits += 1;
            if self.bits == 8 {
                self.push_byte(self.accum as u8);
                self.accum = 0;
                self.bits = 0;
            }
        }
    }

    fn flush(&mut self) -> usize {
        if self.bits > 0 {
            let byte = (self.accum << (8 - self.bits)) as u8;
            self.push_byte(byte);
            self.accum = 0;
            self.bits = 0;
        }
        self.written
    }

    fn overflow(&self) -> bool {
        self.written > self.limit
    }
}

/// MSB-first bit reader mirroring MAME's `bitstream_in`: reads past the
/// end yield zeroes, and [`BitReader::flush`] rewinds to the next byte
/// boundary so a byte-flushed section can be followed exactly.
struct BitReader<'a> {
    data: &'a [u8],
    offset: usize,
    buffer: u32,
    bits: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            offset: 0,
            buffer: 0,
            bits: 0,
        }
    }

    fn peek(&mut self, numbits: u8) -> u32 {
        if numbits > self.bits {
            while self.bits <= 24 {
                if self.offset < self.data.len() {
                    self.buffer |= (self.data[self.offset] as u32) << (24 - self.bits);
                }
                self.offset += 1;
                self.bits += 8;
            }
        }
        self.buffer >> (32 - numbits)
    }

    fn remove(&mut self, numbits: u8) {
        self.buffer <<= numbits;
        self.bits -= numbits;
    }

    fn read(&mut self, numbits: u8) -> u32 {
        let value = self.peek(numbits);
        self.remove(numbits);
        value
    }

    fn flush(&mut self) {
        self.offset -= (self.bits / 8) as usize;
        self.buffer = 0;
        self.bits = 0;
    }

    fn overflow(&self) -> bool {
        self.offset.saturating_sub(self.bits as usize / 8) > self.data.len()
    }
}

// -- huffman tree serialization ---------------------------------------

/// Bits per code-length field in the RLE tree form. `VIDEO_MAX_BITS`
/// is 16, so MAME picks 5.
const TREE_LENGTH_BITS: u8 = 5;

/// Port of MAME `write_rle_tree_bits`: length 1 is the escape value and
/// is always written twice; three or more equal lengths become
/// escape + value + (count - 3).
fn write_rle_tree_bits(w: &mut BitWriter, value: u32, mut repcount: u32) {
    while repcount > 0 {
        if value == 1 {
            w.write(1, TREE_LENGTH_BITS);
            w.write(1, TREE_LENGTH_BITS);
            repcount -= 1;
        } else if repcount <= 2 {
            w.write(value, TREE_LENGTH_BITS);
            repcount -= 1;
        } else {
            let reps = (repcount - 3).min((1 << TREE_LENGTH_BITS) - 1);
            w.write(1, TREE_LENGTH_BITS);
            w.write(value, TREE_LENGTH_BITS);
            w.write(reps, TREE_LENGTH_BITS);
            repcount -= reps + 3;
        }
    }
}

/// Port of MAME `export_tree_rle`: RLE the per-code lengths into the
/// bitstream. The caller byte-flushes afterwards.
fn export_tree_rle(w: &mut BitWriter, lengths: &[u8]) {
    let mut lastval = u32::MAX;
    let mut repcount = 0u32;
    for &length in lengths {
        let newval = length as u32;
        if newval == lastval {
            repcount += 1;
        } else {
            if repcount != 0 {
                write_rle_tree_bits(w, lastval, repcount);
            }
            lastval = newval;
            repcount = 1;
        }
    }
    write_rle_tree_bits(w, lastval, repcount);
}

/// Port of MAME `import_tree_rle`, reading `VIDEO_CODES` code lengths.
fn import_tree_rle(r: &mut BitReader) -> [u8; VIDEO_CODES] {
    let mut lengths = [0u8; VIDEO_CODES];
    let mut index = 0usize;
    while index < VIDEO_CODES {
        let value = r.read(TREE_LENGTH_BITS) as u8;
        if value != 1 {
            lengths[index] = value;
            index += 1;
            continue;
        }
        let next = r.read(TREE_LENGTH_BITS) as u8;
        if next == 1 {
            lengths[index] = 1;
            index += 1;
        } else {
            let count = r.read(TREE_LENGTH_BITS) as usize + 3;
            for _ in 0..count {
                if index >= VIDEO_CODES {
                    break;
                }
                lengths[index] = next;
                index += 1;
            }
        }
    }
    lengths
}

/// Canonical huffman decoder over `VIDEO_CODES` symbols with a flat
/// `VIDEO_MAX_BITS`-wide lookup table, matching MAME's
/// `build_lookup_table`.
struct CanonicalDecoder {
    lookup: Vec<(u16, u8)>,
}

impl CanonicalDecoder {
    fn from_lengths(lengths: &[u8]) -> ChdResult<Self> {
        let mut bithisto = [0u32; 33];
        for &len in lengths {
            if len > VIDEO_MAX_BITS {
                return Err(avhuff_err("video tree code length exceeds maximum"));
            }
            bithisto[len as usize] += 1;
        }

        let mut curstart = 0u32;
        for codelen in (1..=32usize).rev() {
            let nextstart = (curstart + bithisto[codelen]) >> 1;
            if codelen != 1 && nextstart * 2 != curstart + bithisto[codelen] {
                return Err(avhuff_err("video tree has inconsistent code lengths"));
            }
            bithisto[codelen] = curstart;
            curstart = nextstart;
        }

        let mut lookup = vec![(0u16, 0u8); 1usize << VIDEO_MAX_BITS];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let code = bithisto[len as usize];
            bithisto[len as usize] += 1;
            let shift = VIDEO_MAX_BITS - len;
            let base = (code as usize) << shift;
            for slot in lookup.iter_mut().skip(base).take(1usize << shift) {
                *slot = (symbol as u16, len);
            }
        }
        Ok(Self { lookup })
    }

    fn decode_one(&self, r: &mut BitReader) -> ChdResult<u16> {
        let value = r.peek(VIDEO_MAX_BITS);
        let (symbol, numbits) = self.lookup[value as usize];
        if numbits == 0 {
            return Err(avhuff_err("invalid code in video stream"));
        }
        r.remove(numbits);
        Ok(symbol)
    }
}

// -- delta RLE --------------------------------------------------------

/// Number of values an RLE code stands for. Codes below 0x100 are
/// literal deltas and always cover a single value.
fn code_to_rlecount(code: usize) -> usize {
    if code == 0x00 {
        1
    } else if code <= 0x107 {
        8 + (code - 0x100)
    } else {
        16 << (code - 0x108)
    }
}

/// Largest RLE code whose run fits in `rlecount` values.
fn rlecount_to_code(rlecount: usize) -> usize {
    match rlecount {
        n if n >= 2048 => 0x10f,
        n if n >= 1024 => 0x10e,
        n if n >= 512 => 0x10d,
        n if n >= 256 => 0x10c,
        n if n >= 128 => 0x10b,
        n if n >= 64 => 0x10a,
        n if n >= 32 => 0x109,
        n if n >= 16 => 0x108,
        n if n >= 8 => 0x100 + (n - 8),
        _ => 0x00,
    }
}

/// One video plane's delta-RLE context: token generation with
/// histogramming, then huffman-coded emission. Ports MAME's
/// `avhuff_encoder::deltarle_encoder`.
struct DeltaRleEncoder {
    histo: [u32; VIDEO_CODES],
    tokens: Vec<u16>,
    codes: Vec<(u32, u8)>,
    rlecount: u32,
    next: usize,
}

impl DeltaRleEncoder {
    /// Port of `rle_and_histo_bitmap`. The delta predictor runs
    /// continuously across rows; runs are bounded by the row, and a run
    /// reaching the row's end with eight or more values is maximized so
    /// it codes as "rest of row".
    fn analyze(
        source: &[u8],
        start: usize,
        items_per_row: usize,
        item_advance: usize,
        rows: usize,
    ) -> Self {
        let mut histo = [0u32; VIDEO_CODES];
        let mut tokens = Vec::with_capacity(items_per_row * rows);

        let mut prevdata = 0u8;
        let mut row_start = start;
        for _ in 0..rows {
            let end = row_start + items_per_row * item_advance;
            let mut pos = row_start;
            while pos < end {
                let curdelta = source[pos].wrapping_sub(prevdata);
                prevdata = source[pos];

                let code = if curdelta == 0 {
                    let mut zerocount = 1usize;
                    let mut scan = pos + item_advance;
                    while scan < end && source[scan] == prevdata {
                        zerocount += 1;
                        scan += item_advance;
                    }
                    if scan >= end && zerocount >= 8 {
                        zerocount = 100_000;
                    }
                    let code = rlecount_to_code(zerocount);
                    pos += (code_to_rlecount(code) - 1) * item_advance;
                    code
                } else {
                    curdelta as usize
                };
                histo[code] += 1;
                tokens.push(code as u16);
                pos += item_advance;
            }
            row_start = end;
        }

        Self {
            histo,
            tokens,
            codes: Vec::new(),
            rlecount: 0,
            next: 0,
        }
    }

    fn build_tree(&mut self) -> ChdResult<()> {
        self.codes = canonical_codes(&self.histo, VIDEO_CODES, VIDEO_MAX_BITS)?;
        Ok(())
    }

    fn export_tree(&self, w: &mut BitWriter) {
        let lengths: Vec<u8> = self.codes.iter().map(|&(_, numbits)| numbits).collect();
        export_tree_rle(w, &lengths);
    }

    fn flush_rle(&mut self) {
        self.rlecount = 0;
    }

    fn encode_one(&mut self, w: &mut BitWriter) {
        if self.rlecount != 0 {
            self.rlecount -= 1;
            return;
        }
        let data = self.tokens[self.next] as usize;
        self.next += 1;
        let (bits, numbits) = self.codes[data];
        w.write(bits, numbits);
        if data >= 0x100 {
            self.rlecount = code_to_rlecount(data) as u32 - 1;
        }
    }
}

/// Decoding counterpart of [`DeltaRleEncoder`], MAME's
/// `deltarle_decoder`.
struct DeltaRleDecoder {
    decoder: CanonicalDecoder,
    prevdata: u8,
    rlecount: u32,
}

impl DeltaRleDecoder {
    fn new(r: &mut BitReader) -> ChdResult<Self> {
        let decoder = CanonicalDecoder::from_lengths(&import_tree_rle(r))?;
        r.flush();
        Ok(Self {
            decoder,
            prevdata: 0,
            rlecount: 0,
        })
    }

    fn flush_rle(&mut self) {
        self.rlecount = 0;
    }

    fn decode_one(&mut self, r: &mut BitReader) -> ChdResult<u8> {
        if self.rlecount != 0 {
            self.rlecount -= 1;
            return Ok(self.prevdata);
        }
        let data = self.decoder.decode_one(r)? as usize;
        if data < 0x100 {
            self.prevdata = self.prevdata.wrapping_add(data as u8);
        } else {
            self.rlecount = code_to_rlecount(data) as u32 - 1;
        }
        Ok(self.prevdata)
    }
}

// -- video ------------------------------------------------------------

/// Port of `encode_video_lossless`: the `0x80` marker, the Y/Cb/Cr
/// trees each byte-flushed, then one interleaved code stream over the
/// three planes.
fn encode_video(source: &[u8], width: usize, height: usize) -> ChdResult<Vec<u8>> {
    let mut w = BitWriter::new(width * height * 2);
    w.write(LOSSLESS_MARKER, 8);

    // Plane addressing over the big-endian YUY16 bytes: Y at every
    // even byte, Cb at byte 1 and Cr at byte 3 of each pixel pair.
    let mut y = DeltaRleEncoder::analyze(source, 0, width, 2, height);
    let mut cb = DeltaRleEncoder::analyze(source, 1, width / 2, 4, height);
    let mut cr = DeltaRleEncoder::analyze(source, 3, width / 2, 4, height);
    for plane in [&mut y, &mut cb, &mut cr] {
        plane.build_tree()?;
        plane.export_tree(&mut w);
        w.flush();
    }

    for _ in 0..height {
        y.flush_rle();
        cb.flush_rle();
        cr.flush_rle();
        for _ in 0..width / 2 {
            y.encode_one(&mut w);
            cb.encode_one(&mut w);
            y.encode_one(&mut w);
            cr.encode_one(&mut w);
        }
    }

    w.flush();
    if w.overflow() {
        // MAME lets the truncated stream through and relies on the CHD
        // layer rejecting the oversized hunk; failing here is the same
        // outcome without ever emitting a corrupt stream.
        return Err(avhuff_err("video stream does not compress"));
    }
    Ok(w.out)
}

/// Port of `decode_video_lossless`, reconstructing the big-endian
/// YUY16 bytes.
fn decode_video(source: &[u8], width: usize, height: usize) -> ChdResult<Vec<u8>> {
    let mut r = BitReader::new(source);
    if r.read(8) != LOSSLESS_MARKER {
        return Err(avhuff_err("video stream is not lossless-encoded"));
    }

    let mut y = DeltaRleDecoder::new(&mut r)?;
    let mut cb = DeltaRleDecoder::new(&mut r)?;
    let mut cr = DeltaRleDecoder::new(&mut r)?;

    let mut dest = vec![0u8; width * height * 2];
    for row in 0..height {
        y.flush_rle();
        cb.flush_rle();
        cr.flush_rle();
        let mut at = row * width * 2;
        for _ in 0..width / 2 {
            dest[at] = y.decode_one(&mut r)?;
            dest[at + 1] = cb.decode_one(&mut r)?;
            dest[at + 2] = y.decode_one(&mut r)?;
            dest[at + 3] = cr.decode_one(&mut r)?;
            at += 4;
        }
    }
    if r.overflow() {
        return Err(avhuff_err("video stream is truncated"));
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yuy16_frame(width: usize, height: usize, pixel: impl Fn(usize, usize) -> u16) -> Vec<u16> {
        let mut frame = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                frame.push(pixel(x, y));
            }
        }
        frame
    }

    /// Noisy but video-like: a slow ramp with small per-pixel jitter.
    /// Uniform random bytes are incompressible and would legitimately
    /// overflow the codec's output budget, as they would in chdman.
    fn noisy_frame(len: usize) -> Vec<u16> {
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        (0..len)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let jitter = (state & 0x0707) as u16;
                (((i / 8) as u16) << 8 | ((i % 251) as u16)) ^ jitter
            })
            .collect()
    }

    fn video_round_trips(width: usize, height: usize, video: &[u16]) {
        let raw = assemble_raw_frame(width as u16, height as u16, video, &[]).unwrap();
        let source = &raw[RAW_HEADER_SIZE..];
        let encoded = encode_video(source, width, height).unwrap();
        assert_eq!(encoded[0], 0x80);
        let decoded = decode_video(&encoded, width, height).unwrap();
        assert_eq!(decoded, source);
    }

    #[test]
    fn rlecount_code_table_matches_mame() {
        assert_eq!(rlecount_to_code(1), 0x00);
        assert_eq!(rlecount_to_code(7), 0x00);
        assert_eq!(rlecount_to_code(8), 0x100);
        assert_eq!(rlecount_to_code(15), 0x107);
        assert_eq!(rlecount_to_code(16), 0x108);
        assert_eq!(rlecount_to_code(31), 0x108);
        assert_eq!(rlecount_to_code(32), 0x109);
        assert_eq!(rlecount_to_code(64), 0x10a);
        assert_eq!(rlecount_to_code(128), 0x10b);
        assert_eq!(rlecount_to_code(256), 0x10c);
        assert_eq!(rlecount_to_code(512), 0x10d);
        assert_eq!(rlecount_to_code(1024), 0x10e);
        assert_eq!(rlecount_to_code(2048), 0x10f);
        assert_eq!(rlecount_to_code(100_000), 0x10f);

        assert_eq!(code_to_rlecount(0x00), 1);
        assert_eq!(code_to_rlecount(0x100), 8);
        assert_eq!(code_to_rlecount(0x107), 15);
        assert_eq!(code_to_rlecount(0x108), 16);
        assert_eq!(code_to_rlecount(0x109), 32);
        assert_eq!(code_to_rlecount(0x10a), 64);
        assert_eq!(code_to_rlecount(0x10b), 128);
        assert_eq!(code_to_rlecount(0x10c), 256);
        assert_eq!(code_to_rlecount(0x10d), 512);
        assert_eq!(code_to_rlecount(0x10e), 1024);
        assert_eq!(code_to_rlecount(0x10f), 2048);

        // Every RLE code round-trips through the count it stands for.
        for code in 0x100..0x110 {
            assert_eq!(rlecount_to_code(code_to_rlecount(code)), code);
        }
    }

    #[test]
    fn tree_rle_round_trips() {
        // A histogram spanning short and long codes, plus unused
        // symbols, so the exported lengths exercise literals, escaped
        // 1s and long runs of zeros.
        let mut histo = [0u32; VIDEO_CODES];
        for (index, slot) in histo.iter_mut().enumerate().take(40) {
            *slot = (40 - index) as u32 * 17;
        }
        histo[0x10f] = 1;
        let codes = canonical_codes(&histo, VIDEO_CODES, VIDEO_MAX_BITS).unwrap();
        let lengths: Vec<u8> = codes.iter().map(|&(_, numbits)| numbits).collect();

        let mut w = BitWriter::new(4096);
        export_tree_rle(&mut w, &lengths);
        w.flush();
        let mut r = BitReader::new(&w.out);
        assert_eq!(import_tree_rle(&mut r).as_slice(), lengths.as_slice());
    }

    #[test]
    fn video_round_trips_flat() {
        for width in (48..=64).step_by(2) {
            video_round_trips(width, 6, &vec![0x8010; width * 6]);
        }
    }

    #[test]
    fn video_round_trips_gradient() {
        for width in (48..=64).step_by(2) {
            let frame = yuy16_frame(width, 6, |x, y| ((x as u16) << 8) | (y as u16 * 7));
            video_round_trips(width, 6, &frame);
        }
    }

    #[test]
    fn video_round_trips_noise() {
        for width in (48..=64).step_by(2) {
            video_round_trips(width, 6, &noisy_frame(width * 6));
        }
    }

    #[test]
    fn video_round_trips_row_end_runs() {
        // Each row starts with varying data and ends in a long
        // constant run, exercising the end-of-row run maximization.
        for width in (48..=64).step_by(2) {
            let frame = yuy16_frame(width, 6, |x, y| {
                if x < 8 {
                    ((x as u16 * 31) << 8) | (y as u16 * 11)
                } else {
                    0x4020
                }
            });
            video_round_trips(width, 6, &frame);
        }
    }

    fn audio_channel(samples: usize, seed: i32) -> Vec<i16> {
        (0..samples)
            .map(|i| (((i as f64 + seed as f64) / 19.0).sin() * 7000.0) as i16)
            .collect()
    }

    fn hunk_round_trips(channels: usize, samples: usize, width: usize, height: usize, pad: usize) {
        let audio: Vec<Vec<i16>> = (0..channels)
            .map(|ch| audio_channel(samples, ch as i32 * 5))
            .collect();
        let audio_refs: Vec<&[i16]> = audio.iter().map(|c| c.as_slice()).collect();
        let video = noisy_frame(width * height);

        let mut raw = assemble_raw_frame(width as u16, height as u16, &video, &audio_refs).unwrap();
        assert_eq!(
            raw.len(),
            raw_data_size(width as u32, height as u32, channels as u8, samples as u32)
        );
        raw.resize(raw.len() + pad, 0);

        let compressed = encode(&raw).unwrap();
        assert_eq!(decode(&compressed, raw.len()).unwrap(), raw);
    }

    #[test]
    fn hunk_round_trips_without_audio() {
        hunk_round_trips(0, 0, 64, 8, 0);
    }

    #[test]
    fn hunk_round_trips_mono() {
        hunk_round_trips(1, 801, 64, 8, 0);
        hunk_round_trips(1, 800, 64, 8, 0);
    }

    #[test]
    fn hunk_round_trips_stereo() {
        hunk_round_trips(2, 801, 64, 8, 0);
        hunk_round_trips(2, 800, 64, 8, 0);
    }

    #[test]
    fn hunk_round_trips_short_frame_zero_padded() {
        // A 800-sample frame stored in a 801-sample hunk: the tail is
        // zero padding that must survive the round trip untouched.
        hunk_round_trips(2, 800, 64, 8, 2 * 2);
    }

    #[test]
    fn compressed_header_fields() {
        let audio: Vec<Vec<i16>> = (0..2).map(|ch| audio_channel(801, ch)).collect();
        let audio_refs: Vec<&[i16]> = audio.iter().map(|c| c.as_slice()).collect();
        let video = noisy_frame(64 * 8);
        let raw = assemble_raw_frame(64, 8, &video, &audio_refs).unwrap();
        let compressed = encode(&raw).unwrap();

        assert_eq!(compressed[0], 0, "metasize");
        assert_eq!(compressed[1], 2, "channels");
        assert_eq!(&compressed[2..4], &801u16.to_be_bytes(), "samples");
        assert_eq!(&compressed[4..6], &64u16.to_be_bytes(), "width");
        assert_eq!(&compressed[6..8], &8u16.to_be_bytes(), "height");
        assert_eq!(&compressed[8..10], &[0xFF, 0xFF], "FLAC audio tree marker");

        let size0 = u16::from_be_bytes([compressed[10], compressed[11]]) as usize;
        let size1 = u16::from_be_bytes([compressed[12], compressed[13]]) as usize;
        assert!(size0 > 0 && size1 > 0);
        // Header is 10 + 2 per channel, then the streams, then the
        // video block which runs unsized to the end of the hunk.
        let video_at = 14 + size0 + size1;
        assert!(video_at < compressed.len());
        assert_eq!(compressed[video_at], 0x80, "video lossless marker");
    }

    #[test]
    fn encode_rejects_bad_magic() {
        let mut raw = assemble_raw_frame(8, 2, &[0; 16], &[]).unwrap();
        raw[0] = b'x';
        assert!(encode(&raw).is_err());
    }

    #[test]
    fn decode_rejects_huffman_audio_tree() {
        let audio = audio_channel(64, 0);
        let raw = assemble_raw_frame(64, 8, &vec![0; 64 * 8], &[&audio]).unwrap();
        let mut compressed = encode(&raw).unwrap();
        compressed[8] = 0x00;
        compressed[9] = 0x10;
        assert!(decode(&compressed, raw.len()).is_err());
    }
}
