use crate::cd::BYTES_PER_SAMPLE;
use crate::chd::compression::{ChdCompressor, tag_to_bytes};
use crate::chd::error::{ChdError, ChdResult};
use flacenc::bitsink::ByteSink;
use flacenc::component::BitRepr;
use flacenc::config;
use flacenc::error::Verify;
use flacenc::source::MemSource;
use std::io::{self, Cursor};

pub(crate) const CD_SAMPLE_RATE: usize = 44_100;
const FLAC_BITS_PER_SAMPLE: usize = 16;

#[derive(Debug, Clone)]
#[allow(dead_code)] // CHD spec codec: non-CD hunk compression.
pub struct FlacCompressor;

impl ChdCompressor for FlacCompressor {
    fn name(&self) -> &'static str {
        "FLAC Compressor"
    }

    fn tag_bytes(&self) -> [u8; 4] {
        tag_to_bytes("flac")
    }

    fn compress(&self, data: &[u8]) -> ChdResult<Vec<u8>> {
        if !data.len().is_multiple_of(BYTES_PER_SAMPLE) {
            return Err(ChdError::InvalidHunkSize);
        }

        let le_samples = samples_from_bytes(data, Endian::Little);
        let be_samples = samples_from_bytes(data, Endian::Big);
        let samples_per_channel = data.len() / BYTES_PER_SAMPLE;
        let block_size = flac_block_size(samples_per_channel);

        let le_flac = encode_flac_samples(&le_samples, 1, CD_SAMPLE_RATE, block_size)?;
        let be_flac = encode_flac_samples(&be_samples, 1, CD_SAMPLE_RATE, block_size)?;

        let (endian_flag, mut compressed) = if le_flac.len() <= be_flac.len() {
            (0u8, le_flac)
        } else {
            (1u8, be_flac)
        };

        let mut output = Vec::with_capacity(compressed.len() + 1);
        output.push(endian_flag);
        output.append(&mut compressed);
        Ok(output)
    }
}

pub(crate) fn encode_flac_samples(
    samples: &[i32],
    channels: usize,
    sample_rate: usize,
    block_size: usize,
) -> ChdResult<Vec<u8>> {
    if channels == 0 || !samples.len().is_multiple_of(channels) {
        return Err(ChdError::InvalidHunkSize);
    }

    let mut config = config::Encoder::default();
    config.block_size = flac_block_size(block_size);
    let config = config
        .into_verified()
        .map_err(|(_, err)| io::Error::other(err))?;

    let source = MemSource::from_samples(samples, channels, FLAC_BITS_PER_SAMPLE, sample_rate);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|err| io::Error::other(err.to_string()))?;

    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|err| io::Error::other(err.to_string()))?;
    Ok(sink.into_inner())
}

fn flac_block_size(samples_per_channel: usize) -> usize {
    samples_per_channel.clamp(
        flacenc::constant::MIN_BLOCK_SIZE,
        flacenc::constant::MAX_BLOCK_SIZE,
    )
}

pub enum Endian {
    Little,
    Big,
}

pub fn samples_from_bytes(data: &[u8], endian: Endian) -> Vec<i32> {
    let mut samples = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        let value = match endian {
            Endian::Little => i16::from_le_bytes([chunk[0], chunk[1]]),
            Endian::Big => i16::from_be_bytes([chunk[0], chunk[1]]),
        };
        samples.push(value as i32);
    }
    samples
}

pub fn bytes_from_samples(samples: &[i32], endian: &Endian) -> Vec<u8> {
    let mut output = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        let value = sample as i16;
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
    }
    output
}

pub(crate) fn flac_decompress(data: &[u8], _expected_len: usize) -> ChdResult<Vec<u8>> {
    if data.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "FLAC data is empty").into());
    }

    let endian = match data[0] {
        0 => Endian::Little,
        1 => Endian::Big,
        _ => {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "Invalid FLAC endian flag").into(),
            );
        }
    };

    let mut reader = claxon::FlacReader::new(Cursor::new(&data[1..]))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let samples: Result<Vec<i32>, _> = reader.samples().collect();
    let samples = samples.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    Ok(bytes_from_samples(&samples, &endian))
}

/// chdman's FLAC block size for raw `flac` hunks: a quarter of the
/// hunk in samples, halved until at most 2048
/// (`chd_flac_compressor::blocksize`).
pub(crate) fn chd_flac_block_size(hunk_bytes: usize) -> usize {
    let mut block = hunk_bytes / 4;
    while block > 2048 {
        block /= 2;
    }
    block
}

/// The 0x2A-byte stream header MAME's `flac_decoder::reset`
/// synthesizes: chdman stores raw `flac` hunks headerless, so the
/// fLaC magic and STREAMINFO (44100 Hz, 2 channels, 16-bit) must be
/// regenerated before claxon can parse the frames.
fn chd_flac_stream_header(block_size: usize) -> [u8; 42] {
    let mut header = [0u8; 42];
    header[0..4].copy_from_slice(b"fLaC");
    header[4] = 0x80;
    header[7] = 0x22;
    header[8..10].copy_from_slice(&(block_size as u16).to_be_bytes());
    header[10..12].copy_from_slice(&(block_size as u16).to_be_bytes());
    // 20-bit sample rate, 3-bit channels-1, 5-bit bits-1, packed as
    // (44100 << 4) | (1 << 1) | 0xF spread over 4 bytes.
    header[18..22].copy_from_slice(&[0x0A, 0xC4, 0x42, 0xF0]);
    header
}

/// Decode a chdman raw `flac` hunk: a 1-byte 'L'/'B' endian marker,
/// then headerless FLAC frames of interleaved 16-bit stereo samples.
pub(crate) fn flac_decompress_chd_raw(data: &[u8], expected_len: usize) -> ChdResult<Vec<u8>> {
    let endian = match data.first() {
        Some(b'L') => Endian::Little,
        Some(b'B') => Endian::Big,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid CHD FLAC endian marker",
            )
            .into());
        }
    };

    let mut stream = Vec::with_capacity(42 + data.len() - 1);
    stream.extend_from_slice(&chd_flac_stream_header(chd_flac_block_size(expected_len)));
    stream.extend_from_slice(&data[1..]);

    let mut reader = claxon::FlacReader::new(Cursor::new(stream))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let wanted = expected_len / 2;
    let mut samples = Vec::with_capacity(wanted);
    for sample in reader.samples() {
        samples
            .push(sample.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?);
        if samples.len() == wanted {
            break;
        }
    }
    if samples.len() < wanted {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short CHD FLAC stream").into());
    }
    Ok(bytes_from_samples(&samples, &endian))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_stream_header(stream: &[u8]) -> &[u8] {
        assert_eq!(&stream[..4], b"fLaC");
        let mut off = 4;
        loop {
            let last = stream[off] & 0x80 != 0;
            let len =
                u32::from_be_bytes([0, stream[off + 1], stream[off + 2], stream[off + 3]]) as usize;
            off += 4 + len;
            if last {
                break;
            }
        }
        &stream[off..]
    }

    #[test]
    fn chd_raw_flac_round_trips() {
        // Audio-like data so FLAC's predictors have something to do.
        let hunk_bytes = 8192usize;
        let mut data = vec![0u8; hunk_bytes];
        for (i, chunk) in data.chunks_exact_mut(2).enumerate() {
            let v = ((i as f64 / 13.0).sin() * 6000.0) as i16;
            chunk.copy_from_slice(&v.to_le_bytes());
        }

        let block_size = chd_flac_block_size(hunk_bytes);
        let samples = samples_from_bytes(&data, Endian::Little);
        let full = encode_flac_samples(&samples, 2, CD_SAMPLE_RATE, block_size).unwrap();

        let mut chd_hunk = vec![b'L'];
        chd_hunk.extend_from_slice(strip_stream_header(&full));

        let decoded = flac_decompress_chd_raw(&chd_hunk, hunk_bytes).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn chd_raw_flac_rejects_bad_marker() {
        assert!(flac_decompress_chd_raw(&[b'X', 0, 0], 4096).is_err());
    }

    #[test]
    fn chd_block_size_matches_chdman() {
        assert_eq!(chd_flac_block_size(2048), 512);
        assert_eq!(chd_flac_block_size(4096), 1024);
        assert_eq!(chd_flac_block_size(8192), 2048);
        assert_eq!(chd_flac_block_size(16384), 2048);
        assert_eq!(chd_flac_block_size(19584), 1224);
    }

    #[test]
    fn samples_from_bytes_le_known_value() {
        // 0x1234 in LE = [0x34, 0x12]
        let data = [0x34u8, 0x12];
        let samples = samples_from_bytes(&data, Endian::Little);
        assert_eq!(samples, vec![0x1234i32]);
    }

    #[test]
    fn samples_from_bytes_be_known_value() {
        // 0x1234 in BE = [0x12, 0x34]
        let data = [0x12u8, 0x34];
        let samples = samples_from_bytes(&data, Endian::Big);
        assert_eq!(samples, vec![0x1234i32]);
    }

    #[test]
    fn samples_from_bytes_negative() {
        // -1 in LE = [0xFF, 0xFF]
        let data = [0xFFu8, 0xFF];
        let samples = samples_from_bytes(&data, Endian::Little);
        assert_eq!(samples, vec![-1i32]);
    }

    #[test]
    fn samples_from_bytes_empty() {
        let samples = samples_from_bytes(&[], Endian::Little);
        assert!(samples.is_empty());
    }

    #[test]
    fn round_trip_le() {
        let original: Vec<u8> = vec![0x34, 0x12, 0xFF, 0xFF, 0x00, 0x80];
        let samples = samples_from_bytes(&original, Endian::Little);
        let back = bytes_from_samples(&samples, &Endian::Little);
        assert_eq!(back, original);
    }

    #[test]
    fn round_trip_be() {
        let original: Vec<u8> = vec![0x12, 0x34, 0xFF, 0xFF, 0x80, 0x00];
        let samples = samples_from_bytes(&original, Endian::Big);
        let back = bytes_from_samples(&samples, &Endian::Big);
        assert_eq!(back, original);
    }

    #[test]
    fn bytes_from_samples_truncates_to_i16() {
        // Values outside i16 range get truncated
        let samples = vec![0x1234i32];
        let bytes = bytes_from_samples(&samples, &Endian::Little);
        assert_eq!(bytes, vec![0x34, 0x12]);
    }
}
