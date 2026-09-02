//! Minimal RIFF/AVI reader for laserdisc rips.
//!
//! Covers exactly what a `createld`-style CHD needs: uncompressed
//! YUY2/UYVY/VYUY video handed out as big-endian YUY16 words (Y in the high
//! byte, chroma in the low byte, the layout avhuff encodes), PCM audio
//! de-interleaved per channel, and the derived frame geometry in
//! [`LdParams`] that fixes the CHD hunk size.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

/// Video codecs the reader accepts; everything else is compressed.
const SUPPORTED_VIDEO: [[u8; 4]; 3] = [*b"YUY2", *b"UYVY", *b"VYUY"];

/// Header values read straight out of `avih`, `strh`, and `strf`.
///
/// Field names follow MAME's `avi_file::movie_info`, which is where the
/// laserdisc frame geometry is derived from.
#[derive(Debug, Clone, Default)]
pub struct AviInfo {
    pub video_width: u32,
    pub video_height: u32,
    /// `dwRate` of the video stream.
    pub video_timescale: u32,
    /// `dwScale` of the video stream.
    pub video_sampletime: u32,
    /// `dwLength` of the video stream, in frames.
    pub video_numsamples: u32,
    pub video_format: [u8; 4],
    pub audio_channels: u32,
    pub audio_samplerate: u32,
    pub audio_samplebits: u32,
    pub audio_numsamples: u64,
}

/// Laserdisc frame geometry derived from an AVI's headers.
///
/// `width`/`height` and `frame_count` are post-interlace: for an interlaced
/// source each AVI frame becomes two half-height fields, so `height` is halved
/// and `frame_count` doubled. One field is one CHD hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LdParams {
    pub fps_times_1million: u32,
    pub width: u32,
    pub height: u32,
    pub interlaced: bool,
    pub channels: u32,
    pub rate: u32,
    pub max_samples_per_frame: u32,
    pub bytes_per_frame: u32,
    pub frame_count: u32,
}

impl LdParams {
    /// Derives the laserdisc frame geometry from an AVI's headers.
    ///
    /// # Errors
    ///
    /// Fails when the video stream declares a zero sample time, or when the
    /// resulting frame size does not fit a `u32`.
    pub fn derive(info: &AviInfo) -> Result<Self> {
        if info.video_sampletime == 0 {
            bail!("AVI video stream declares a zero sample time");
        }
        if info.audio_channels > 8 {
            bail!(
                "AVI audio stream declares {} channels; only up to 8 are supported",
                info.audio_channels
            );
        }
        let mut fps_times_1million =
            (u64::from(info.video_timescale) * 1_000_000 / u64::from(info.video_sampletime)) as u32;
        let mut height = info.video_height;
        let mut frame_count = u64::from(info.video_numsamples);

        let interlaced =
            (fps_times_1million / 1_000_000) <= 30 && height.is_multiple_of(2) && height > 288;
        if interlaced {
            fps_times_1million *= 2;
            height /= 2;
            frame_count *= 2;
        }
        if fps_times_1million == 0 {
            bail!("AVI video stream declares a zero frame rate");
        }

        let max_samples_per_frame = (u64::from(info.audio_samplerate) * 1_000_000)
            .div_ceil(u64::from(fps_times_1million)) as u32;
        let bytes_per_frame = 12
            + u64::from(info.audio_channels) * u64::from(max_samples_per_frame) * 2
            + u64::from(info.video_width) * u64::from(height) * 2;

        Ok(Self {
            fps_times_1million,
            width: info.video_width,
            height,
            interlaced,
            channels: info.audio_channels,
            rate: info.audio_samplerate,
            max_samples_per_frame,
            bytes_per_frame: u32::try_from(bytes_per_frame)
                .map_err(|_| anyhow!("AVI frame size {bytes_per_frame} exceeds 4 GiB"))?,
            frame_count: u32::try_from(frame_count)
                .map_err(|_| anyhow!("AVI field count {frame_count} exceeds 2^32"))?,
        })
    }

    /// Renders the CHD `AVAV` metadata string for these parameters.
    pub fn av_metadata(&self) -> String {
        format!(
            "FPS:{}.{:06} WIDTH:{} HEIGHT:{} INTERLACED:{} CHANNELS:{} SAMPLERATE:{}",
            self.fps_times_1million / 1_000_000,
            self.fps_times_1million % 1_000_000,
            self.width,
            self.height,
            u8::from(self.interlaced),
            self.channels,
            self.rate,
        )
    }
}

/// A parsed AVI, indexed by frame and by audio byte position.
pub struct AviFile<R> {
    reader: R,
    info: AviInfo,
    /// `(file offset, byte length)` of each video frame's chunk data.
    video: Vec<(u64, u32)>,
    /// `(file offset, byte length)` of each audio chunk's data.
    audio: Vec<(u64, u32)>,
    /// Byte position of each audio chunk within the concatenated audio stream.
    audio_start: Vec<u64>,
    scratch: Vec<u8>,
}

impl AviFile<File> {
    /// Opens and parses the AVI at `path`.
    ///
    /// # Errors
    ///
    /// Fails on I/O errors, malformed RIFF structure, or non-PCM audio.
    /// Header parsing succeeds for any video codec; a compressed codec only
    /// fails once frame data is actually read (see [`read_video_frame`](AviFile::read_video_frame)).
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open AVI file {}", path.display()))?;
        Self::new(file)
    }
}

impl<R: Read + Seek> AviFile<R> {
    /// Parses the AVI headers and builds the frame and audio chunk index.
    ///
    /// # Errors
    ///
    /// Fails on I/O errors, malformed RIFF structure, or non-PCM audio.
    /// Header parsing succeeds for any video codec; a compressed codec only
    /// fails once frame data is actually read (see [`read_video_frame`](AviFile::read_video_frame)).
    pub fn new(mut reader: R) -> Result<Self> {
        let file_len = reader.seek(SeekFrom::End(0))?;
        let mut streams = Vec::new();
        let mut movi = Vec::new();
        let mut idx1 = None;

        let mut pos = 0u64;
        while pos + 12 <= file_len {
            let riff = read_chunk_header(&mut reader, pos)?;
            if riff.id != *b"RIFF" {
                bail!("input is not a RIFF file");
            }
            if riff.size < 4 {
                break;
            }
            let mut list_type = [0u8; 4];
            reader.read_exact(&mut list_type)?;
            let end = (riff.data_pos + u64::from(riff.size)).min(file_len);
            if list_type == *b"AVI " || list_type == *b"AVIX" {
                walk_riff(
                    &mut reader,
                    riff.data_pos + 4,
                    end,
                    &mut streams,
                    &mut movi,
                    &mut idx1,
                )?;
            }
            pos = riff.data_pos + u64::from(riff.size) + u64::from(riff.size & 1);
        }

        let video_stream = streams
            .iter()
            .position(|s| matches!(s, StreamInfo::Video { .. }))
            .ok_or_else(|| anyhow!("AVI has no video stream"))?;
        let audio_stream = streams
            .iter()
            .position(|s| matches!(s, StreamInfo::Audio { .. }));

        let mut info = AviInfo::default();
        if let StreamInfo::Video {
            timescale,
            sampletime,
            length,
            width,
            height,
            format,
        } = streams[video_stream]
        {
            info.video_timescale = timescale;
            info.video_sampletime = sampletime;
            info.video_numsamples = length;
            info.video_width = width;
            info.video_height = height;
            info.video_format = format;
        }

        if let Some(StreamInfo::Audio {
            format_tag,
            channels,
            samplerate,
            samplebits,
        }) = audio_stream.map(|i| streams[i])
        {
            if format_tag != WAVE_FORMAT_PCM {
                bail!("AVI audio stream is not uncompressed PCM (format tag {format_tag})");
            }
            if samplebits != 8 && samplebits != 16 {
                bail!("AVI audio stream is {samplebits}-bit; only 8- and 16-bit PCM are supported");
            }
            if channels == 0 || channels > MAX_SOUND_CHANNELS {
                bail!("AVI audio stream declares {channels} channels");
            }
            info.audio_channels = channels;
            info.audio_samplerate = samplerate;
            info.audio_samplebits = samplebits;
        }

        let (video, audio) = build_index(
            &mut reader,
            &movi,
            idx1,
            video_stream,
            audio_stream,
            file_len,
        )?;

        // chdman derives the frame count from the walked stream index, not the
        // header's dwLength, so a header that disagrees with (or omits) it never
        // desyncs frame_count/logical size mid-convert.
        info.video_numsamples = u32::try_from(video.len())
            .map_err(|_| anyhow!("AVI holds more than 2^32 video frames"))?;

        let mut audio_start = Vec::with_capacity(audio.len());
        let mut total = 0u64;
        for &(_, size) in &audio {
            audio_start.push(total);
            total += u64::from(size);
        }
        // Per-sample stride is channels * bytes-per-sample, not the header's
        // block_align, which need not match the actual interleaved layout.
        let stride = u64::from(info.audio_channels) * u64::from(info.audio_samplebits / 8);
        info.audio_numsamples = total.checked_div(stride).unwrap_or(0);

        Ok(Self {
            reader,
            info,
            video,
            audio,
            audio_start,
            scratch: Vec::new(),
        })
    }

    /// Returns the header values read from the AVI.
    pub fn info(&self) -> &AviInfo {
        &self.info
    }

    /// Returns the laserdisc frame geometry derived from the headers.
    ///
    /// # Errors
    ///
    /// Propagates [`LdParams::derive`].
    pub fn ld_params(&self) -> Result<LdParams> {
        LdParams::derive(&self.info)
    }

    /// Reads video frame `frame` into `dest` as big-endian YUY16 words.
    ///
    /// Each word holds the luma byte in bits 15..8 and the chroma byte
    /// (`Cb` on even columns, `Cr` on odd) in bits 7..0, which is what
    /// avhuff's `bitmap_yuy16` input expects. `dest` must hold at least
    /// `width * height` words.
    ///
    /// # Errors
    ///
    /// Fails when the video codec is compressed (the fourcc is named in the
    /// message), when `frame` is past the end of the AVI, when `dest` is too
    /// small, or when the frame chunk is short.
    pub fn read_video_frame(&mut self, frame: u32, dest: &mut [u16]) -> Result<()> {
        if !SUPPORTED_VIDEO.contains(&self.info.video_format) {
            bail!(
                "AVI video codec '{}' is compressed; only YUY2, UYVY, and VYUY are supported",
                fourcc_str(&self.info.video_format)
            );
        }
        let width = self.info.video_width as usize;
        let height = self.info.video_height as usize;
        let pixels = width * height;
        if dest.len() < pixels {
            bail!(
                "destination holds {} pixels, frame needs {pixels}",
                dest.len()
            );
        }
        let &(pos, size) = self.video.get(frame as usize).ok_or_else(|| {
            anyhow!(
                "video frame {frame} is past the end of the AVI ({} frames)",
                self.video.len()
            )
        })?;
        let needed = pixels * 2;
        if (size as usize) < needed {
            bail!("video frame {frame} holds {size} bytes, expected at least {needed}");
        }

        self.scratch.resize(needed, 0);
        self.reader.seek(SeekFrom::Start(pos))?;
        self.reader.read_exact(&mut self.scratch)?;

        // YUY2 and VYUY store Y first; UYVY stores chroma first. All three become
        // Y-high words (VYUY differs from YUY2 only in which chroma channel a
        // given word holds, not in byte order, so the two convert identically).
        let y_first = self.info.video_format != *b"UYVY";
        let (pairs, _) = self.scratch.as_chunks::<2>();
        for (word, bytes) in dest[..pixels].iter_mut().zip(pairs) {
            *word = if y_first {
                u16::from_be_bytes(*bytes)
            } else {
                u16::from_be_bytes([bytes[1], bytes[0]])
            };
        }
        Ok(())
    }

    /// Reads `samples` PCM samples of `channel` starting at `first_sample`.
    ///
    /// 8-bit input is upconverted as `(value << 8) - 0x8000`.
    ///
    /// # Errors
    ///
    /// Fails when `channel` does not exist, when `dest` is too small, when
    /// the requested window extends past the audio stream, or on I/O errors.
    pub fn read_sound_samples(
        &mut self,
        channel: u32,
        first_sample: u64,
        samples: u32,
        dest: &mut [i16],
    ) -> Result<()> {
        let count = samples as usize;
        if dest.len() < count {
            bail!("destination holds {} samples, need {count}", dest.len());
        }
        if channel >= self.info.audio_channels {
            bail!(
                "audio channel {channel} does not exist ({} channels)",
                self.info.audio_channels
            );
        }
        if count == 0 {
            return Ok(());
        }
        let last_sample = first_sample
            .checked_add(u64::from(samples))
            .ok_or_else(|| anyhow!("audio sample window overflows"))?;
        if last_sample > self.info.audio_numsamples {
            bail!(
                "audio sample window [{first_sample}, {last_sample}) exceeds the {} available samples",
                self.info.audio_numsamples
            );
        }

        // Stride by channels * bytes-per-sample, not the header's block_align:
        // the two need not match, and the interleaved data is always tightly
        // packed at this stride.
        let width = self.info.audio_samplebits as usize / 8;
        let stride = self.info.audio_channels as usize * width;
        self.read_audio_bytes(first_sample * stride as u64, count * stride)?;

        let base = channel as usize * width;
        for (i, out) in dest[..count].iter_mut().enumerate() {
            let off = i * stride + base;
            *out = if width == 1 {
                (((i32::from(self.scratch[off])) << 8) - 0x8000) as i16
            } else {
                i16::from_le_bytes([self.scratch[off], self.scratch[off + 1]])
            };
        }
        Ok(())
    }

    /// Gathers `len` bytes of the concatenated audio stream into `scratch`,
    /// zero-filling anything past the end.
    fn read_audio_bytes(&mut self, start: u64, len: usize) -> Result<()> {
        self.scratch.clear();
        self.scratch.resize(len, 0);
        let end = start + len as u64;

        let mut idx = match self.audio_start.binary_search(&start) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let mut want = start;
        while idx < self.audio.len() && want < end {
            let chunk_start = self.audio_start[idx];
            let (pos, size) = self.audio[idx];
            let chunk_end = chunk_start + u64::from(size);
            if chunk_end <= want {
                idx += 1;
                continue;
            }
            if chunk_start >= end {
                break;
            }
            let take = ((chunk_end - want).min(end - want)) as usize;
            let dst = (want - start) as usize;
            self.reader
                .seek(SeekFrom::Start(pos + (want - chunk_start)))?;
            self.reader.read_exact(&mut self.scratch[dst..dst + take])?;
            want += take as u64;
            idx += 1;
        }
        Ok(())
    }
}

const WAVE_FORMAT_PCM: u16 = 1;
const MAX_SOUND_CHANNELS: u32 = 16;

#[derive(Debug, Clone, Copy)]
enum StreamInfo {
    Video {
        timescale: u32,
        sampletime: u32,
        length: u32,
        width: u32,
        height: u32,
        format: [u8; 4],
    },
    Audio {
        format_tag: u16,
        channels: u32,
        samplerate: u32,
        samplebits: u32,
    },
    Other,
}

/// A RIFF chunk header. `data_pos` is where the body starts.
struct ChunkHeader {
    id: [u8; 4],
    data_pos: u64,
    size: u32,
}

/// Reads the 8-byte chunk header at `pos`, leaving the cursor on the body.
fn read_chunk_header<R: Read + Seek>(reader: &mut R, pos: u64) -> Result<ChunkHeader> {
    reader.seek(SeekFrom::Start(pos))?;
    let mut hdr = [0u8; 8];
    reader.read_exact(&mut hdr)?;
    Ok(ChunkHeader {
        id: [hdr[0], hdr[1], hdr[2], hdr[3]],
        data_pos: pos + 8,
        size: u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]),
    })
}

fn le_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn le_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn fourcc_str(cc: &[u8; 4]) -> String {
    if cc.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(cc).into_owned()
    } else {
        format!("{:02x}{:02x}{:02x}{:02x}", cc[0], cc[1], cc[2], cc[3])
    }
}

/// Advances past a chunk, honouring RIFF's word alignment padding.
fn next_chunk(chunk: &ChunkHeader) -> u64 {
    chunk.data_pos + u64::from(chunk.size) + u64::from(chunk.size & 1)
}

/// Walks the children of one `RIFF`/`AVIX` body, collecting stream headers,
/// `movi` list extents and the `idx1` chunk.
fn walk_riff<R: Read + Seek>(
    reader: &mut R,
    mut pos: u64,
    end: u64,
    streams: &mut Vec<StreamInfo>,
    movi: &mut Vec<(u64, u64)>,
    idx1: &mut Option<(u64, u32)>,
) -> Result<()> {
    while pos + 8 <= end {
        let chunk = read_chunk_header(reader, pos)?;
        let chunk_end = (chunk.data_pos + u64::from(chunk.size)).min(end);
        if chunk.id == *b"LIST" {
            let mut list_type = [0u8; 4];
            reader.read_exact(&mut list_type)?;
            match &list_type {
                b"hdrl" => parse_hdrl(reader, chunk.data_pos + 4, chunk_end, streams)?,
                // idx1 offsets are relative to the 'movi' fourcc itself.
                b"movi" => movi.push((chunk.data_pos, chunk_end)),
                _ => {}
            }
        } else if chunk.id == *b"idx1" {
            *idx1 = Some((chunk.data_pos, chunk.size));
        }
        pos = next_chunk(&chunk);
    }
    Ok(())
}

fn parse_hdrl<R: Read + Seek>(
    reader: &mut R,
    mut pos: u64,
    end: u64,
    streams: &mut Vec<StreamInfo>,
) -> Result<()> {
    while pos + 8 <= end {
        let chunk = read_chunk_header(reader, pos)?;
        let chunk_end = (chunk.data_pos + u64::from(chunk.size)).min(end);
        if chunk.id == *b"LIST" {
            let mut list_type = [0u8; 4];
            reader.read_exact(&mut list_type)?;
            if list_type == *b"strl" {
                streams.push(parse_strl(reader, chunk.data_pos + 4, chunk_end)?);
            }
        }
        pos = next_chunk(&chunk);
    }
    Ok(())
}

fn parse_strl<R: Read + Seek>(reader: &mut R, mut pos: u64, end: u64) -> Result<StreamInfo> {
    let mut strh: Option<Vec<u8>> = None;
    let mut info = StreamInfo::Other;
    while pos + 8 <= end {
        let chunk = read_chunk_header(reader, pos)?;
        let avail = u64::from(chunk.size).min(end.saturating_sub(chunk.data_pos)) as usize;
        match &chunk.id {
            b"strh" => {
                let mut body = vec![0u8; avail];
                reader.read_exact(&mut body)?;
                strh = Some(body);
            }
            b"strf" => {
                let mut body = vec![0u8; avail];
                reader.read_exact(&mut body)?;
                info = build_stream(strh.as_deref(), &body)?;
            }
            _ => {}
        }
        pos = next_chunk(&chunk);
    }
    Ok(info)
}

fn build_stream(strh: Option<&[u8]>, strf: &[u8]) -> Result<StreamInfo> {
    let Some(strh) = strh else {
        return Ok(StreamInfo::Other);
    };
    if strh.len() < 48 {
        return Ok(StreamInfo::Other);
    }
    match &strh[0..4] {
        b"vids" => {
            if strf.len() < 20 {
                bail!(
                    "AVI video format header is {} bytes, expected 40",
                    strf.len()
                );
            }
            Ok(StreamInfo::Video {
                timescale: le_u32(strh, 24),
                sampletime: le_u32(strh, 20),
                length: le_u32(strh, 32),
                width: le_u32(strf, 4),
                height: (le_u32(strf, 8) as i32).unsigned_abs(),
                format: [strf[16], strf[17], strf[18], strf[19]],
            })
        }
        b"auds" => {
            if strf.len() < 16 {
                bail!(
                    "AVI audio format header is {} bytes, expected 16",
                    strf.len()
                );
            }
            Ok(StreamInfo::Audio {
                format_tag: le_u16(strf, 0),
                channels: u32::from(le_u16(strf, 2)),
                samplerate: le_u32(strf, 4),
                samplebits: u32::from(le_u16(strf, 14)),
            })
        }
        _ => Ok(StreamInfo::Other),
    }
}

/// Returns the stream number of a data chunk id such as `00dc`.
///
/// Only `db`/`dc` (video) and `wb` (audio) suffixes qualify; this keeps
/// palette chunks and OpenDML index chunks out of the frame index.
fn stream_number(id: &[u8; 4]) -> Option<usize> {
    if !id[0].is_ascii_digit() || !id[1].is_ascii_digit() {
        return None;
    }
    if !matches!(&id[2..4], b"db" | b"dc" | b"wb") {
        return None;
    }
    Some(usize::from(id[0] - b'0') * 10 + usize::from(id[1] - b'0'))
}

type ChunkIndex = (Vec<(u64, u32)>, Vec<(u64, u32)>);

/// Builds the per-stream chunk index, preferring `idx1` over a `movi` walk.
fn build_index<R: Read + Seek>(
    reader: &mut R,
    movi: &[(u64, u64)],
    idx1: Option<(u64, u32)>,
    video_stream: usize,
    audio_stream: Option<usize>,
    file_len: u64,
) -> Result<ChunkIndex> {
    // idx1 only ever indexes the first RIFF segment; an OpenDML file with
    // AVIX continuations holds most of its frames past it and must be
    // walked in full.
    if movi.len() <= 1
        && let (Some((pos, size)), Some(&(movi_base, _))) = (idx1, movi.first())
        && let Some(index) = index_from_idx1(
            reader,
            pos,
            size,
            movi_base,
            video_stream,
            audio_stream,
            file_len,
        )?
    {
        return Ok(index);
    }
    scan_movi(reader, movi, video_stream, audio_stream)
}

/// Reads `idx1`, resolving whether its offsets are `movi`-relative or absolute.
///
/// Returns `None` when neither base lands on the first entry's chunk id, so
/// the caller can fall back to walking `movi`.
fn index_from_idx1<R: Read + Seek>(
    reader: &mut R,
    pos: u64,
    size: u32,
    movi_base: u64,
    video_stream: usize,
    audio_stream: Option<usize>,
    file_len: u64,
) -> Result<Option<ChunkIndex>> {
    let count = size as usize / 16;
    if count == 0 {
        return Ok(None);
    }
    let mut raw = vec![0u8; count * 16];
    reader.seek(SeekFrom::Start(pos))?;
    reader.read_exact(&mut raw)?;

    let first_id = [raw[0], raw[1], raw[2], raw[3]];
    let first_offset = u64::from(le_u32(&raw, 8));
    let base = [movi_base, 0]
        .into_iter()
        .find(|base| chunk_id_at(reader, base + first_offset, file_len) == Some(first_id));
    let Some(base) = base else {
        return Ok(None);
    };

    let mut video = Vec::new();
    let mut audio = Vec::new();
    for entry in raw.as_chunks::<16>().0 {
        let Some(number) = stream_number(&[entry[0], entry[1], entry[2], entry[3]]) else {
            continue;
        };
        let data_pos = base + u64::from(le_u32(entry, 8)) + 8;
        let len = le_u32(entry, 12);
        if number == video_stream {
            video.push((data_pos, len));
        } else if Some(number) == audio_stream {
            audio.push((data_pos, len));
        }
    }
    Ok(Some((video, audio)))
}

fn chunk_id_at<R: Read + Seek>(reader: &mut R, pos: u64, file_len: u64) -> Option<[u8; 4]> {
    if pos + 4 > file_len {
        return None;
    }
    reader.seek(SeekFrom::Start(pos)).ok()?;
    let mut id = [0u8; 4];
    reader.read_exact(&mut id).ok()?;
    Some(id)
}

/// Walks every `movi` list chunk by chunk, descending into `rec ` groups.
///
/// Segments and nested groups are visited in document order so the frame
/// index stays chronological across OpenDML `AVIX` continuations.
fn scan_movi<R: Read + Seek>(
    reader: &mut R,
    movi: &[(u64, u64)],
    video_stream: usize,
    audio_stream: Option<usize>,
) -> Result<ChunkIndex> {
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for &(base, end) in movi {
        // The 'movi' fourcc occupies the first four bytes of the list body.
        scan_span(
            reader,
            base + 4,
            end,
            video_stream,
            audio_stream,
            &mut video,
            &mut audio,
        )?;
    }
    Ok((video, audio))
}

/// Walks one chunk span in document order, recursing into nested lists.
fn scan_span<R: Read + Seek>(
    reader: &mut R,
    mut pos: u64,
    end: u64,
    video_stream: usize,
    audio_stream: Option<usize>,
    video: &mut Vec<(u64, u32)>,
    audio: &mut Vec<(u64, u32)>,
) -> Result<()> {
    while pos + 8 <= end {
        let chunk = read_chunk_header(reader, pos)?;
        let chunk_end = (chunk.data_pos + u64::from(chunk.size)).min(end);
        if chunk.id == *b"LIST" {
            scan_span(
                reader,
                chunk.data_pos + 4,
                chunk_end,
                video_stream,
                audio_stream,
                video,
                audio,
            )?;
        } else if let Some(number) = stream_number(&chunk.id) {
            if number == video_stream {
                video.push((chunk.data_pos, chunk.size));
            } else if Some(number) == audio_stream {
                audio.push((chunk.data_pos, chunk.size));
            }
        }
        pos = next_chunk(&chunk);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    /// Description of a synthetic AVI to build.
    pub struct AviSpec<'a> {
        pub width: u32,
        pub height: u32,
        /// `dwRate` of the video stream.
        pub timescale: u32,
        /// `dwScale` of the video stream.
        pub sampletime: u32,
        pub video_format: [u8; 4],
        /// Raw pixel bytes per frame, `width * height * 2` each.
        pub frames: &'a [Vec<u8>],
        pub channels: u16,
        pub sample_rate: u32,
        pub sample_bits: u16,
        /// Interleaved PCM samples.
        pub samples: &'a [i16],
        /// Whether to emit an `idx1` chunk.
        pub index: bool,
        /// Declared `nBlockAlign`, when it should disagree with the natural
        /// `channels * sample_bits/8` interleave the sample data actually uses.
        pub block_align_override: Option<u32>,
        /// Declared video `dwLength`, when it should disagree with `frames.len()`.
        pub video_length_override: Option<u32>,
    }

    /// Builds a deterministic frame of raw YUY pixel bytes.
    pub fn pattern_frame(width: u32, height: u32, seed: u8) -> Vec<u8> {
        (0..(width as usize * height as usize * 2))
            .map(|i| ((i * 31 + usize::from(seed) * 17) % 251) as u8)
            .collect()
    }

    /// Builds `count` deterministic interleaved samples for `channels`.
    pub fn pattern_samples(count: usize, channels: usize) -> Vec<i16> {
        (0..count * channels)
            .map(|i| ((i as i32 * 37) % 30_000 - 15_000) as i16)
            .collect()
    }

    /// Assembles a complete little-endian RIFF/AVI file.
    pub fn build_avi(spec: &AviSpec) -> Vec<u8> {
        // The sample data itself is always tightly packed at the natural
        // stride; `block_align_override` only changes what the header claims.
        let natural_align = u32::from(spec.channels) * u32::from(spec.sample_bits) / 8;
        let declared_align = spec.block_align_override.unwrap_or(natural_align);
        let audio = encode_samples(spec);
        let sample_count = (audio.len() as u32).checked_div(natural_align).unwrap_or(0);

        let mut hdrl = Vec::new();
        chunk(&mut hdrl, b"avih", &avih(spec));
        let mut strl = Vec::new();
        chunk(&mut strl, b"strh", &strh_video(spec));
        chunk(&mut strl, b"strf", &strf_video(spec));
        list(&mut hdrl, b"strl", &strl);
        if spec.channels > 0 {
            let mut strl = Vec::new();
            chunk(
                &mut strl,
                b"strh",
                &strh_audio(spec, declared_align, sample_count),
            );
            chunk(&mut strl, b"strf", &strf_audio(spec, declared_align));
            list(&mut hdrl, b"strl", &strl);
        }

        let mut movi = Vec::new();
        let mut entries: Vec<([u8; 4], u32, u32)> = Vec::new();
        for frame in spec.frames {
            entries.push((*b"00dc", movi.len() as u32, frame.len() as u32));
            chunk(&mut movi, b"00dc", frame);
        }
        if spec.channels > 0 {
            // Two chunks so the audio byte-position index gets exercised.
            let split =
                (audio.len() / 2 / natural_align.max(1) as usize) * natural_align.max(1) as usize;
            for part in [&audio[..split], &audio[split..]] {
                entries.push((*b"01wb", movi.len() as u32, part.len() as u32));
                chunk(&mut movi, b"01wb", part);
            }
        }

        let mut idx1 = Vec::new();
        for (id, offset, size) in &entries {
            idx1.extend_from_slice(id);
            idx1.extend_from_slice(&0x10u32.to_le_bytes());
            idx1.extend_from_slice(&(4 + offset).to_le_bytes());
            idx1.extend_from_slice(&size.to_le_bytes());
        }

        let mut body = Vec::new();
        body.extend_from_slice(b"AVI ");
        list(&mut body, b"hdrl", &hdrl);
        list(&mut body, b"movi", &movi);
        if spec.index {
            chunk(&mut body, b"idx1", &idx1);
        }

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Appends an OpenDML `RIFF AVIX` continuation segment holding more
    /// 16-bit frames and audio, optionally wrapped in a `rec ` group.
    pub fn append_avix_segment(
        out: &mut Vec<u8>,
        frames: &[Vec<u8>],
        samples: &[i16],
        wrap_in_rec: bool,
    ) {
        let audio: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut inner = Vec::new();
        for frame in frames {
            chunk(&mut inner, b"00dc", frame);
        }
        if !audio.is_empty() {
            chunk(&mut inner, b"01wb", &audio);
        }
        let mut movi = Vec::new();
        if wrap_in_rec {
            list(&mut movi, b"rec ", &inner);
        } else {
            movi = inner;
        }
        let mut body = Vec::new();
        body.extend_from_slice(b"AVIX");
        list(&mut body, b"movi", &movi);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
    }

    fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(id);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
    }

    fn list(out: &mut Vec<u8>, list_type: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(list_type);
        out.extend_from_slice(body);
    }

    fn encode_samples(spec: &AviSpec) -> Vec<u8> {
        if spec.sample_bits == 8 {
            spec.samples
                .iter()
                .map(|&v| ((i32::from(v) + 0x8000) >> 8) as u8)
                .collect()
        } else {
            spec.samples.iter().flat_map(|v| v.to_le_bytes()).collect()
        }
    }

    fn avih(spec: &AviSpec) -> Vec<u8> {
        let mut b = vec![0u8; 56];
        let micros = 1_000_000u64 * u64::from(spec.sampletime) / u64::from(spec.timescale.max(1));
        b[0..4].copy_from_slice(&(micros as u32).to_le_bytes());
        b[12..16].copy_from_slice(&0x10u32.to_le_bytes());
        b[16..20].copy_from_slice(&(spec.frames.len() as u32).to_le_bytes());
        b[24..28].copy_from_slice(&u32::from(spec.channels > 0).saturating_add(1).to_le_bytes());
        b[32..36].copy_from_slice(&spec.width.to_le_bytes());
        b[36..40].copy_from_slice(&spec.height.to_le_bytes());
        b
    }

    fn strh_video(spec: &AviSpec) -> Vec<u8> {
        let mut b = vec![0u8; 56];
        b[0..4].copy_from_slice(b"vids");
        b[4..8].copy_from_slice(&spec.video_format);
        b[20..24].copy_from_slice(&spec.sampletime.to_le_bytes());
        b[24..28].copy_from_slice(&spec.timescale.to_le_bytes());
        let length = spec
            .video_length_override
            .unwrap_or(spec.frames.len() as u32);
        b[32..36].copy_from_slice(&length.to_le_bytes());
        b
    }

    fn strf_video(spec: &AviSpec) -> Vec<u8> {
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&40u32.to_le_bytes());
        b[4..8].copy_from_slice(&spec.width.to_le_bytes());
        b[8..12].copy_from_slice(&spec.height.to_le_bytes());
        b[12..14].copy_from_slice(&1u16.to_le_bytes());
        b[14..16].copy_from_slice(&16u16.to_le_bytes());
        b[16..20].copy_from_slice(&spec.video_format);
        b[20..24].copy_from_slice(&(spec.width * spec.height * 2).to_le_bytes());
        b
    }

    fn strh_audio(spec: &AviSpec, block_align: u32, sample_count: u32) -> Vec<u8> {
        let mut b = vec![0u8; 56];
        b[0..4].copy_from_slice(b"auds");
        b[20..24].copy_from_slice(&block_align.to_le_bytes());
        b[24..28].copy_from_slice(&(spec.sample_rate * block_align).to_le_bytes());
        b[32..36].copy_from_slice(&sample_count.to_le_bytes());
        b[44..48].copy_from_slice(&block_align.to_le_bytes());
        b
    }

    fn strf_audio(spec: &AviSpec, block_align: u32) -> Vec<u8> {
        let mut b = vec![0u8; 16];
        b[0..2].copy_from_slice(&1u16.to_le_bytes());
        b[2..4].copy_from_slice(&spec.channels.to_le_bytes());
        b[4..8].copy_from_slice(&spec.sample_rate.to_le_bytes());
        b[8..12].copy_from_slice(&(spec.sample_rate * block_align).to_le_bytes());
        b[12..14].copy_from_slice(&(block_align as u16).to_le_bytes());
        b[14..16].copy_from_slice(&spec.sample_bits.to_le_bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::test_fixtures::*;
    use super::*;

    fn ntsc_info(height: u32) -> AviInfo {
        AviInfo {
            video_width: 720,
            video_height: height,
            video_timescale: 30_000,
            video_sampletime: 1001,
            video_numsamples: 100,
            video_format: *b"YUY2",
            audio_channels: 2,
            audio_samplerate: 48_000,
            audio_samplebits: 16,
            audio_numsamples: 0,
        }
    }

    fn spec<'a>(
        format: [u8; 4],
        frames: &'a [Vec<u8>],
        samples: &'a [i16],
        index: bool,
    ) -> AviSpec<'a> {
        AviSpec {
            width: 8,
            height: 4,
            timescale: 30_000,
            sampletime: 1001,
            video_format: format,
            frames,
            channels: 2,
            sample_rate: 48_000,
            sample_bits: 16,
            samples,
            index,
            block_align_override: None,
            video_length_override: None,
        }
    }

    #[test]
    fn derives_ntsc_720x480_parameters() {
        let params = LdParams::derive(&ntsc_info(480)).expect("derive params");
        assert_eq!(params.fps_times_1million, 59_940_058);
        assert!(params.interlaced);
        assert_eq!(params.height, 240);
        assert_eq!(params.width, 720);
        assert_eq!(params.max_samples_per_frame, 801);
        assert_eq!(params.bytes_per_frame, 348_816);
        assert_eq!(params.frame_count, 200);
        assert_eq!(
            params.av_metadata(),
            "FPS:59.940058 WIDTH:720 HEIGHT:240 INTERLACED:1 CHANNELS:2 SAMPLERATE:48000"
        );
    }

    #[test]
    fn derives_ntsc_720x524_parameters() {
        let params = LdParams::derive(&ntsc_info(524)).expect("derive params");
        assert_eq!(params.height, 262);
        assert_eq!(params.bytes_per_frame, 380_496);
        assert_eq!(
            params.av_metadata(),
            "FPS:59.940058 WIDTH:720 HEIGHT:262 INTERLACED:1 CHANNELS:2 SAMPLERATE:48000"
        );
    }

    #[test]
    fn leaves_progressive_input_unhalved() {
        let mut info = ntsc_info(240);
        info.video_timescale = 60_000;
        let params = LdParams::derive(&info).expect("derive params");
        assert!(!params.interlaced);
        assert_eq!(params.height, 240);
        assert_eq!(params.frame_count, 100);
    }

    #[test]
    fn reads_yuy2_frames_byte_exactly() {
        let frames = vec![pattern_frame(8, 4, 1), pattern_frame(8, 4, 2)];
        let samples = pattern_samples(64, 2);
        let data = build_avi(&spec(*b"YUY2", &frames, &samples, true));
        let mut avi = AviFile::new(Cursor::new(data)).expect("open avi");

        assert_eq!(avi.info().video_numsamples, 2);
        let mut pixels = vec![0u16; 32];
        avi.read_video_frame(1, &mut pixels).expect("read frame");
        for (i, word) in pixels.iter().enumerate() {
            assert_eq!(
                *word,
                u16::from_be_bytes([frames[1][i * 2], frames[1][i * 2 + 1]]),
                "pixel {i}"
            );
        }
    }

    #[test]
    fn reads_uyvy_frames_with_swapped_bytes() {
        let frames = vec![pattern_frame(8, 4, 3)];
        let samples = pattern_samples(32, 2);
        let data = build_avi(&spec(*b"UYVY", &frames, &samples, true));
        let mut avi = AviFile::new(Cursor::new(data)).expect("open avi");

        let mut pixels = vec![0u16; 32];
        avi.read_video_frame(0, &mut pixels).expect("read frame");
        for (i, word) in pixels.iter().enumerate() {
            assert_eq!(
                *word,
                u16::from_be_bytes([frames[0][i * 2 + 1], frames[0][i * 2]]),
                "pixel {i}"
            );
        }
    }

    #[test]
    fn reads_samples_per_channel_byte_exactly() {
        let frames = vec![pattern_frame(8, 4, 1)];
        let samples = pattern_samples(64, 2);
        let data = build_avi(&spec(*b"YUY2", &frames, &samples, true));
        let mut avi = AviFile::new(Cursor::new(data)).expect("open avi");
        assert_eq!(avi.info().audio_numsamples, 64);

        for channel in 0..2u32 {
            let mut out = vec![0i16; 40];
            avi.read_sound_samples(channel, 20, 40, &mut out)
                .expect("read samples");
            for (i, got) in out.iter().enumerate() {
                assert_eq!(
                    *got,
                    samples[(20 + i) * 2 + channel as usize],
                    "channel {channel} sample {i}"
                );
            }
        }
    }

    #[test]
    fn errors_on_reads_past_the_end_of_audio() {
        let frames = vec![pattern_frame(8, 4, 1)];
        let samples = pattern_samples(8, 2);
        let data = build_avi(&spec(*b"YUY2", &frames, &samples, true));
        let mut avi = AviFile::new(Cursor::new(data)).expect("open avi");

        let mut out = vec![0i16; 12];
        let err = avi
            .read_sound_samples(0, 4, 12, &mut out)
            .expect_err("window past the end of the audio stream must error");
        assert!(err.to_string().contains('8'), "{err}");
    }

    #[test]
    fn strides_audio_by_channels_and_bytes_not_block_align() {
        // Header claims 8 bytes/frame; the actual interleave is channels(2) *
        // bytes(2) = 4. The old block_align-strided reader would misread this.
        let frames = vec![pattern_frame(8, 4, 1)];
        let samples = pattern_samples(20, 2);
        let mut s = spec(*b"YUY2", &frames, &samples, true);
        s.block_align_override = Some(8);
        let mut avi = AviFile::new(Cursor::new(build_avi(&s))).expect("open avi");

        for channel in 0..2u32 {
            let mut out = vec![0i16; 20];
            avi.read_sound_samples(channel, 0, 20, &mut out)
                .expect("read samples");
            for (i, got) in out.iter().enumerate() {
                assert_eq!(
                    *got,
                    samples[i * 2 + channel as usize],
                    "channel {channel} sample {i}"
                );
            }
        }
    }

    #[test]
    fn upconverts_eight_bit_audio() {
        let frames = vec![pattern_frame(8, 4, 1)];
        let samples: Vec<i16> = (0..16u32)
            .map(|i| (((i as i32 * 16) << 8) - 0x8000) as i16)
            .collect();
        let mut spec = spec(*b"YUY2", &frames, &samples, true);
        spec.channels = 1;
        spec.sample_bits = 8;
        let mut avi = AviFile::new(Cursor::new(build_avi(&spec))).expect("open avi");

        let mut out = vec![0i16; 16];
        avi.read_sound_samples(0, 0, 16, &mut out)
            .expect("read samples");
        assert_eq!(out, samples);
    }

    #[test]
    fn indexes_frames_without_idx1() {
        let frames = vec![pattern_frame(8, 4, 5), pattern_frame(8, 4, 6)];
        let samples = pattern_samples(32, 2);
        let data = build_avi(&spec(*b"YUY2", &frames, &samples, false));
        let mut avi = AviFile::new(Cursor::new(data)).expect("open avi");

        let mut pixels = vec![0u16; 32];
        avi.read_video_frame(1, &mut pixels).expect("read frame");
        assert_eq!(pixels[0], u16::from_be_bytes([frames[1][0], frames[1][1]]));
    }

    #[test]
    fn rejects_compressed_video_naming_the_fourcc() {
        let frames = vec![pattern_frame(8, 4, 1)];
        let samples = pattern_samples(32, 2);
        let data = build_avi(&spec(*b"HFYU", &frames, &samples, true));
        let mut avi = AviFile::new(Cursor::new(data)).expect("headers parse for any fourcc");
        let mut pixels = vec![0u16; 32];
        let err = avi
            .read_video_frame(0, &mut pixels)
            .expect_err("compressed codec must be rejected");
        assert!(err.to_string().contains("HFYU"), "{err}");
    }

    #[test]
    fn rejects_reads_past_the_last_frame() {
        let frames = vec![pattern_frame(8, 4, 1)];
        let samples = pattern_samples(32, 2);
        let data = build_avi(&spec(*b"YUY2", &frames, &samples, true));
        let mut avi = AviFile::new(Cursor::new(data)).expect("open avi");
        let mut pixels = vec![0u16; 32];
        assert!(avi.read_video_frame(5, &mut pixels).is_err());
    }

    #[test]
    fn reads_vyuy_frames_identically_to_its_yuy2_twin() {
        let frame = pattern_frame(8, 4, 9);
        let samples = pattern_samples(32, 2);
        let yuy2 = build_avi(&spec(
            *b"YUY2",
            std::slice::from_ref(&frame),
            &samples,
            true,
        ));
        let vyuy = build_avi(&spec(*b"VYUY", &[frame], &samples, true));
        let mut yuy2 = AviFile::new(Cursor::new(yuy2)).expect("open yuy2 avi");
        let mut vyuy = AviFile::new(Cursor::new(vyuy)).expect("open vyuy avi");

        let mut yuy2_pixels = vec![0u16; 32];
        let mut vyuy_pixels = vec![0u16; 32];
        yuy2.read_video_frame(0, &mut yuy2_pixels)
            .expect("read yuy2 frame");
        vyuy.read_video_frame(0, &mut vyuy_pixels)
            .expect("read vyuy frame");
        assert_eq!(yuy2_pixels, vyuy_pixels);
    }

    #[test]
    fn reads_frames_across_avix_segments_despite_idx1() {
        // OpenDML layout: idx1 covers only the first RIFF segment, the rest
        // of the frames live in an AVIX continuation (here inside a `rec `
        // group). The reader must ignore idx1 and walk everything in order.
        let frames = vec![pattern_frame(8, 4, 1), pattern_frame(8, 4, 2)];
        let samples = pattern_samples(32, 2);
        let mut data = build_avi(&spec(*b"YUY2", &frames, &samples, true));
        let more = vec![pattern_frame(8, 4, 7), pattern_frame(8, 4, 8)];
        let more_samples: Vec<i16> = (0..64).map(|i| (i as i16) * 111 - 3000).collect();
        append_avix_segment(&mut data, &more, &more_samples, true);
        let mut avi = AviFile::new(Cursor::new(data)).expect("open avi");

        assert_eq!(avi.info().video_numsamples, 4);
        assert_eq!(avi.info().audio_numsamples, 64);

        let mut pixels = vec![0u16; 32];
        avi.read_video_frame(3, &mut pixels).expect("read frame");
        for (i, word) in pixels.iter().enumerate() {
            assert_eq!(
                *word,
                u16::from_be_bytes([more[1][i * 2], more[1][i * 2 + 1]]),
                "pixel {i}"
            );
        }

        // A window spanning the segment boundary must splice both chunks.
        let combined: Vec<i16> = samples.iter().chain(&more_samples).copied().collect();
        let mut out = vec![0i16; 32];
        avi.read_sound_samples(0, 16, 32, &mut out)
            .expect("read samples");
        for (i, got) in out.iter().enumerate() {
            assert_eq!(*got, combined[(16 + i) * 2], "sample {i}");
        }
    }

    #[test]
    fn prefers_indexed_frame_count_over_mismatched_dwlength() {
        let frames = vec![
            pattern_frame(8, 4, 1),
            pattern_frame(8, 4, 2),
            pattern_frame(8, 4, 3),
        ];
        let samples = pattern_samples(96, 2);
        let mut s = spec(*b"YUY2", &frames, &samples, true);
        s.video_length_override = Some(999);
        let avi = AviFile::new(Cursor::new(build_avi(&s))).expect("open avi");
        assert_eq!(avi.info().video_numsamples, 3);
    }

    #[test]
    fn rejects_more_than_eight_audio_channels() {
        let mut info = ntsc_info(480);
        info.audio_channels = 9;
        let err = LdParams::derive(&info).expect_err("more than 8 channels must error");
        assert!(err.to_string().contains('8'), "{err}");
    }
}
