use crate::nintendo::ctr::z3ds::error::Z3dsResult;

/// Seekable zstd seek table — appended as a ZSTD skippable frame.
///
/// Format (all little-endian):
///   u32  skippable magic  0x184D2A5E
///   u32  frame_size       = 9 + num_frames * 8
///   [per frame]
///     u32  compressed_size
///     u32  decompressed_size
///   u32  number_of_frames
///   u8   seek_table_descriptor  (0x00 — no checksums)
///   u32  seekable magic    0x8F92EAB1
pub const FRAME_SIZE_CIA: usize = 32 * 1024 * 1024; // 32 MB
pub const FRAME_SIZE_DEFAULT: usize = 256 * 1024; // 256 KB

const SKIPPABLE_MAGIC: u32 = 0x184D2A5E;
const SEEKABLE_MAGIC: u32 = 0x8F92EAB1;
const SEEK_TABLE_DESCRIPTOR: u8 = 0x00;

/// Per-frame entry recorded while encoding.
struct FrameEntry {
    compressed_size: u32,
    decompressed_size: u32,
}

/// Compress `data` into seekable-zstd format.
///
/// The output is a sequence of independent ZSTD frames (each at most
/// `max_frame_size` decompressed bytes) followed by a seek table encoded as a
/// ZSTD skippable frame.  The whole output is valid input for the standard zstd
/// library.
pub fn encode_seekable(data: &[u8], max_frame_size: usize, level: i32) -> Z3dsResult<Vec<u8>> {
    let mut output: Vec<u8> = Vec::with_capacity(data.len());
    let mut entries: Vec<FrameEntry> = Vec::new();

    for chunk in data.chunks(max_frame_size) {
        let compressed = zstd::encode_all(chunk, level)?;
        entries.push(FrameEntry {
            compressed_size: compressed.len() as u32,
            decompressed_size: chunk.len() as u32,
        });
        output.extend_from_slice(&compressed);
    }

    // Seek table skippable frame
    let num_frames = entries.len() as u32;
    // frame_size = entries (8 bytes each) + number_of_frames (4) + descriptor (1) + seekable_magic (4)
    let frame_payload_size: u32 = num_frames * 8 + 9;

    output.extend_from_slice(&SKIPPABLE_MAGIC.to_le_bytes());
    output.extend_from_slice(&frame_payload_size.to_le_bytes());

    for entry in &entries {
        output.extend_from_slice(&entry.compressed_size.to_le_bytes());
        output.extend_from_slice(&entry.decompressed_size.to_le_bytes());
    }

    output.extend_from_slice(&num_frames.to_le_bytes());
    output.push(SEEK_TABLE_DESCRIPTOR);
    output.extend_from_slice(&SEEKABLE_MAGIC.to_le_bytes());

    Ok(output)
}

/// Decompress seekable-zstd data back to the original bytes.
///
/// Strips the seek table skippable frame (if present) then decompresses all
/// remaining ZSTD frames sequentially — the standard zstd library handles
/// multiple concatenated frames natively.
pub fn decode_seekable(data: &[u8]) -> Z3dsResult<Vec<u8>> {
    let payload = strip_seek_table(data);
    Ok(zstd::decode_all(payload)?)
}

/// Returns a slice of `data` with the trailing seek table skippable frame
/// removed, or the original slice unchanged if no seek table is present.
fn strip_seek_table(data: &[u8]) -> &[u8] {
    // The last 4 bytes of the seek table are the seekable magic.
    // Walk backwards to find the skippable frame header.
    if data.len() < 13 {
        return data;
    }

    let magic_offset = data.len() - 4;
    let trailing_magic = u32::from_le_bytes([
        data[magic_offset],
        data[magic_offset + 1],
        data[magic_offset + 2],
        data[magic_offset + 3],
    ]);

    if trailing_magic != SEEKABLE_MAGIC {
        return data;
    }

    // Read num_frames from 9 bytes before the end (4 seekable_magic + 1 descriptor + 4 num_frames)
    if data.len() < 13 {
        return data;
    }
    let num_frames_offset = data.len() - 9;
    let num_frames = u32::from_le_bytes([
        data[num_frames_offset],
        data[num_frames_offset + 1],
        data[num_frames_offset + 2],
        data[num_frames_offset + 3],
    ]) as usize;

    // frame_payload_size = num_frames * 8 + 9
    // total skippable frame = 4 (magic) + 4 (size field) + frame_payload_size
    let skippable_frame_total = 8 + num_frames * 8 + 9;

    if data.len() < skippable_frame_total {
        return data;
    }

    let skippable_start = data.len() - skippable_frame_total;

    // Sanity check: verify the skippable magic at that offset
    let skippable_magic = u32::from_le_bytes([
        data[skippable_start],
        data[skippable_start + 1],
        data[skippable_start + 2],
        data[skippable_start + 3],
    ]);

    if skippable_magic == SKIPPABLE_MAGIC {
        &data[..skippable_start]
    } else {
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reads num_frames from the seek table at the end of encoded output.
    fn read_num_frames(data: &[u8]) -> u32 {
        let offset = data.len() - 9;
        u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    }

    // Reads the trailing u32 magic from the encoded output.
    fn read_trailing_magic(data: &[u8]) -> u32 {
        let offset = data.len() - 4;
        u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    }

    // --- Round-trip correctness ---

    #[test]
    fn round_trip_small() {
        let original = b"Hello, Z3DS seekable zstd!".repeat(100);
        let encoded = encode_seekable(&original, 512, 0).unwrap();
        let decoded = decode_seekable(&encoded).unwrap();
        assert_eq!(original.as_slice(), decoded.as_slice());
    }

    #[test]
    fn round_trip_exact_frame_boundary() {
        // Data length is exactly 4x the frame size — produces 4 even frames.
        let original = vec![0xABu8; 1024];
        let encoded = encode_seekable(&original, 256, 0).unwrap();
        let decoded = decode_seekable(&encoded).unwrap();
        assert_eq!(original, decoded);
        assert_eq!(read_num_frames(&encoded), 4);
    }

    #[test]
    fn round_trip_single_frame() {
        let original = b"single frame data";
        let encoded = encode_seekable(original, 1024 * 1024, 0).unwrap();
        let decoded = decode_seekable(&encoded).unwrap();
        assert_eq!(original.as_slice(), decoded.as_slice());
        assert_eq!(read_num_frames(&encoded), 1);
    }

    #[test]
    fn round_trip_multiple_frames() {
        // 300 bytes with a 100-byte frame size → 3 frames.
        let original: Vec<u8> = (0u8..=99).cycle().take(300).collect();
        let encoded = encode_seekable(&original, 100, 0).unwrap();
        let decoded = decode_seekable(&encoded).unwrap();
        assert_eq!(original, decoded);
        assert_eq!(read_num_frames(&encoded), 3);
    }

    #[test]
    fn round_trip_large_patterned_data() {
        // 1 MB of a repeating pattern — exercises multi-frame paths and real compression.
        let original: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024).collect();
        let encoded = encode_seekable(&original, FRAME_SIZE_DEFAULT, 0).unwrap();
        let decoded = decode_seekable(&encoded).unwrap();
        assert_eq!(original, decoded);
        // Should compress well below the original size.
        assert!(encoded.len() < original.len());
    }

    #[test]
    fn round_trip_incompressible_data() {
        // High-entropy bytes that compress poorly — must still round-trip correctly.
        // Built with a simple LCG so the test is deterministic and dependency-free.
        let mut state: u64 = 0xDEADBEEFCAFEBABE;
        let original: Vec<u8> = (0..4096)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 56) as u8
            })
            .collect();
        let encoded = encode_seekable(&original, 512, 0).unwrap();
        let decoded = decode_seekable(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    // --- Seek table structure ---

    #[test]
    fn seek_table_ends_with_seekable_magic() {
        let data = b"some test data".repeat(10);
        let encoded = encode_seekable(&data, 64, 0).unwrap();
        assert_eq!(read_trailing_magic(&encoded), SEEKABLE_MAGIC);
    }

    #[test]
    fn seek_table_skippable_frame_starts_with_skippable_magic() {
        let data = b"test".repeat(50);
        let encoded = encode_seekable(&data, 32, 0).unwrap();
        let num_frames = read_num_frames(&encoded) as usize;
        let skippable_frame_total = 8 + num_frames * 8 + 9;
        let skippable_start = encoded.len() - skippable_frame_total;
        let magic = u32::from_le_bytes([
            encoded[skippable_start],
            encoded[skippable_start + 1],
            encoded[skippable_start + 2],
            encoded[skippable_start + 3],
        ]);
        assert_eq!(magic, SKIPPABLE_MAGIC);
    }

    // --- strip_seek_table ---

    #[test]
    fn strip_seek_table_removes_trailing_table() {
        let data = b"hello world seekable".repeat(20);
        let encoded = encode_seekable(&data, 64, 0).unwrap();
        let stripped = strip_seek_table(&encoded);
        // Stripped output must be decompressible to the original.
        let decoded = zstd::decode_all(stripped).unwrap();
        assert_eq!(data.repeat(1).as_slice(), decoded.as_slice());
    }

    #[test]
    fn strip_seek_table_leaves_plain_zstd_unchanged() {
        // Plain single zstd frame (no seek table) — strip_seek_table must not corrupt it.
        let original = b"plain zstd, no seek table";
        let plain = zstd::encode_all(original.as_slice(), 0).unwrap();
        let stripped = strip_seek_table(&plain);
        let decoded = zstd::decode_all(stripped).unwrap();
        assert_eq!(original.as_slice(), decoded.as_slice());
    }

    #[test]
    fn decode_seekable_handles_plain_zstd_frame() {
        // decode_seekable must work on a plain zstd frame without a seek table.
        let original = b"plain frame, no seek table";
        let plain = zstd::encode_all(original.as_slice(), 0).unwrap();
        let decoded = decode_seekable(&plain).unwrap();
        assert_eq!(original.as_slice(), decoded.as_slice());
    }
}
