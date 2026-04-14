//! RVZ packing encoding.
//!
//! A Lagged Fibonacci PRNG (f=XOR, j=32, k=521) generates pseudorandom
//! padding that the spec uses to re-synthesise large runs of Wii partition
//! padding losslessly. Both encoder and decoder live here.
//!
//! This module is a line-by-line port of Dolphin's
//! `Source/Core/DiscIO/LaggedFibonacciGenerator.{h,cpp}` from
//! `dolphin-emu/dolphin`. The in-memory buffer layout matches Dolphin on
//! a little-endian host (the only platform we support), so `get_seed`
//! round-trips any bytes Dolphin's encoder produces.
//!
//! Spec: <https://github.com/dolphin-emu/dolphin/blob/master/docs/WiaAndRvz.md>

use crate::nintendo::rvz::error::{RvzError, RvzResult};

/// Buffer length in u32 words (Dolphin's `LFG_K`).
const LFG_K: usize = 521;
/// Lag (Dolphin's `LFG_J`).
const LFG_J: usize = 32;
/// Seed length in u32 words (Dolphin's `SEED_SIZE`). The on-disc seed is
/// `SEED_SIZE * 4 = 68` bytes.
pub const SEED_SIZE: usize = 17;
/// Buffer length in bytes.
const LFG_BUFFER_BYTES: usize = LFG_K * 4;

/// Lagged Fibonacci generator matching Dolphin's RVZ packing
/// implementation byte-for-byte.
#[derive(Clone)]
pub struct LaggedFibonacci {
    buffer: [u32; LFG_K],
    /// Byte position inside the buffer. Matches Dolphin's
    /// `m_position_bytes`. Can reach `LFG_K * 4 = 2084`.
    position_bytes: usize,
}

impl Default for LaggedFibonacci {
    fn default() -> Self {
        Self {
            buffer: [0; LFG_K],
            position_bytes: 0,
        }
    }
}

impl LaggedFibonacci {
    /// Seed the generator from a 68-byte preamble, matching Dolphin's
    /// `SetSeed(const u8*)` + `Initialize(false)` pair.
    pub fn init(seed: &[u8; 68]) -> Self {
        let mut seed_words = [0u32; SEED_SIZE];
        for i in 0..SEED_SIZE {
            let off = i * 4;
            seed_words[i] =
                u32::from_be_bytes([seed[off], seed[off + 1], seed[off + 2], seed[off + 3]]);
        }
        Self::from_seed_words(&seed_words)
    }

    /// Port of Dolphin's `SetSeed(const u32*)` + `Initialize(false)`.
    pub fn from_seed_words(seed: &[u32; SEED_SIZE]) -> Self {
        let mut lfg = Self {
            buffer: [0; LFG_K],
            position_bytes: 0,
        };
        // SetSeed: copy seed into buffer[0..17]. Dolphin's SetSeed(u8*)
        // reads each 4-byte slice as a big-endian u32 via
        // `Common::swap32(ptr)` and stores that directly. Our caller
        // already did the big-endian read in `init` (from_be_bytes),
        // so we copy as-is; an additional byte-swap here would break
        // round-trips against real Dolphin files.
        lfg.buffer[..SEED_SIZE].copy_from_slice(seed);
        lfg.initialize(false).expect("Initialize(false) cannot fail");
        lfg
    }

    /// Port of Dolphin's `Initialize(bool check_existing_data)`. Returns
    /// `false` if `check_existing_data` is true and the buffer contents
    /// don't match the bit-munge constraint.
    fn initialize(&mut self, check_existing_data: bool) -> Option<()> {
        // Fill buffer[17..521] via the recurrence.
        for i in SEED_SIZE..LFG_K {
            let calculated =
                (self.buffer[i - 17] << 23) ^ (self.buffer[i - 16] >> 9) ^ self.buffer[i - 1];

            if check_existing_data {
                let actual = (self.buffer[i] & 0xFF00FFFF) | (self.buffer[i] << 2 & 0x00FC0000);
                if (calculated & 0xFFFCFFFF) != actual {
                    return None;
                }
            }

            self.buffer[i] = calculated;
        }

        // Bit-munge + swap32 every word.
        for x in self.buffer.iter_mut() {
            *x = ((*x & 0xFF00FFFF) | ((*x >> 2) & 0x00FF0000)).swap_bytes();
        }

        // Four forward steps.
        for _ in 0..4 {
            self.forward();
        }

        self.position_bytes = 0;
        Some(())
    }

    /// Port of Dolphin's `Forward()`.
    fn forward(&mut self) {
        for i in 0..LFG_J {
            self.buffer[i] ^= self.buffer[i + LFG_K - LFG_J];
        }
        for i in LFG_J..LFG_K {
            self.buffer[i] ^= self.buffer[i - LFG_J];
        }
    }

    /// Port of Dolphin's `Backward(size_t start_word, size_t end_word)`.
    fn backward(&mut self, start_word: usize, end_word: usize) {
        let loop_end = LFG_J.max(start_word);
        let mut i = end_word.min(LFG_K);
        while i > loop_end {
            self.buffer[i - 1] ^= self.buffer[i - 1 - LFG_J];
            i -= 1;
        }
        let mut i = end_word.min(LFG_J);
        while i > start_word {
            self.buffer[i - 1] ^= self.buffer[i - 1 + LFG_K - LFG_J];
            i -= 1;
        }
    }

    /// Port of Dolphin's 0-arg `Backward()`. Equivalent to
    /// `Backward(0, LFG_K)`.
    fn backward_all(&mut self) {
        self.backward(0, LFG_K);
    }

    /// Port of Dolphin's `Reinitialize(u32 seed_out[SEED_SIZE])`. Reverses
    /// the four forward steps, undoes the bit-munge, extracts the seed,
    /// and re-initializes with `check_existing_data = true`. Returns
    /// `None` if the validation fails (the observed data isn't a valid
    /// LFG trajectory).
    fn reinitialize(&mut self) -> Option<[u32; SEED_SIZE]> {
        for _ in 0..4 {
            self.backward_all();
        }

        for x in self.buffer.iter_mut() {
            *x = x.swap_bytes();
        }

        // Reconstruct bits 16-17 via the XOR trick from Dolphin. Each
        // seed word's missing 2 bits can be recovered from the later
        // buffer words because the recurrence leaks them.
        for i in 0..SEED_SIZE {
            self.buffer[i] = (self.buffer[i] & 0xFF00FFFF)
                | ((self.buffer[i] << 2) & 0x00FC0000)
                | (((self.buffer[i + 16] ^ self.buffer[i + 15]) << 9) & 0x00030000);
        }

        // Return the seed u32 values as-is: the caller converts them
        // to 68 bytes via `to_be_bytes` per word, which mirrors
        // Dolphin's `SetSeed(u8*)` big-endian read convention. No
        // byte-swap here; see `from_seed_words` for the symmetry.
        let mut seed_out = [0u32; SEED_SIZE];
        seed_out.copy_from_slice(&self.buffer[..SEED_SIZE]);

        self.initialize(true).map(|_| seed_out)
    }

    /// Port of Dolphin's `GetByte()`. Reads the next byte from the LFG
    /// output stream; matches Dolphin on a little-endian host because
    /// both sides read u32 words as their native in-memory bytes.
    pub fn next_byte(&mut self) -> u8 {
        let word_idx = self.position_bytes / 4;
        let byte_in_word = self.position_bytes % 4;
        let result = self.buffer[word_idx].to_le_bytes()[byte_in_word];

        self.position_bytes += 1;
        if self.position_bytes == LFG_BUFFER_BYTES {
            self.forward();
            self.position_bytes = 0;
        }
        result
    }

    /// Fill `out` with LFG output. Port of Dolphin's `GetBytes`.
    pub fn fill(&mut self, out: &mut [u8]) {
        for b in out.iter_mut() {
            *b = self.next_byte();
        }
    }

    /// Port of Dolphin's `Forward(size_t count)`. Advance the generator
    /// by `count` bytes without producing output, triggering buffer-wrap
    /// state updates as needed.
    pub fn forward_bytes(&mut self, count: usize) {
        self.position_bytes += count;
        while self.position_bytes >= LFG_BUFFER_BYTES {
            self.forward();
            self.position_bytes -= LFG_BUFFER_BYTES;
        }
    }

    /// Reverse-derive the LFG seed that would produce `data` starting at
    /// byte position `data_offset` inside the implicit PRNG stream. If
    /// the reconstruction succeeds, returns the recovered seed and the
    /// number of leading bytes of `data` that match the generator's
    /// output. A non-zero match count means `data[..matched]` is LFG
    /// junk with the returned seed.
    ///
    /// Port of Dolphin's public
    /// `LaggedFibonacciGenerator::GetSeed(const u8*, ...)`.
    pub fn get_seed(data: &[u8], data_offset: usize) -> Option<([u32; SEED_SIZE], usize)> {
        // Skip up to 3 leading bytes so we land on a u32 boundary
        // relative to the stream.
        let bytes_to_skip = (data_offset.wrapping_neg()) & 3;
        if data.len() < bytes_to_skip {
            return None;
        }
        let aligned = &data[bytes_to_skip..];
        let u32_count = aligned.len() / 4;
        let u32_data_offset = (data_offset + bytes_to_skip) / 4;

        let mut words = Vec::with_capacity(u32_count);
        for i in 0..u32_count {
            let off = i * 4;
            // On an LE host, Dolphin's `reinterpret_cast<const u32*>`
            // reads bytes as little-endian u32s.
            words.push(u32::from_le_bytes([
                aligned[off],
                aligned[off + 1],
                aligned[off + 2],
                aligned[off + 3],
            ]));
        }

        let mut lfg = Self::default();
        let seed = lfg.get_seed_from_words(&words, u32_data_offset)?;

        // Rewind to the original byte offset and walk.
        lfg.position_bytes = data_offset % LFG_BUFFER_BYTES;

        let mut reconstructed_bytes = 0;
        for &expected in data.iter() {
            if lfg.next_byte() != expected {
                break;
            }
            reconstructed_bytes += 1;
        }
        Some((seed, reconstructed_bytes))
    }

    /// Port of Dolphin's private
    /// `GetSeed(const u32*, size, data_offset, *lfg, seed_out)`.
    fn get_seed_from_words(
        &mut self,
        words: &[u32],
        data_offset_words: usize,
    ) -> Option<[u32; SEED_SIZE]> {
        if words.len() < LFG_K {
            return None;
        }
        // Validation: every word must satisfy the bit constraint that the
        // initialization munge imposes.
        for w in &words[..LFG_K] {
            let sw = w.swap_bytes();
            if (sw & 0x00C00000) != ((sw >> 2) & 0x00C00000) {
                return None;
            }
        }

        let data_offset_mod_k = data_offset_words % LFG_K;
        let data_offset_div_k = data_offset_words / LFG_K;

        // Copy the observed words into the buffer, rotated so the word at
        // byte offset 0 of the stream sits at index data_offset_mod_k.
        for i in 0..(LFG_K - data_offset_mod_k) {
            self.buffer[data_offset_mod_k + i] = words[i];
        }
        for i in 0..data_offset_mod_k {
            self.buffer[i] = words[LFG_K - data_offset_mod_k + i];
        }

        self.backward(0, data_offset_mod_k);
        for _ in 0..data_offset_div_k {
            self.backward_all();
        }

        let seed = self.reinitialize()?;

        for _ in 0..data_offset_div_k {
            self.forward();
        }

        Some(seed)
    }
}

const COMPRESSED_FLAG: u32 = 1 << 31;
const MAX_PLAIN_RUN: u32 = 0x7FFF_FFFF;

/// Wii block size (`VolumeWii::BLOCK_TOTAL_SIZE`). Dolphin's RVZ packing
/// treats this as the LFG stream's period for the `data_offset` modulo
/// used by `forward_bytes` at decode time.
const RVZ_BLOCK_SIZE: u64 = 0x8000;

/// Decode an RVZ-packed byte stream into its original bytes.
///
/// The input format is a sequence of records. Each record starts with a
/// 4-byte big-endian `u32`:
/// * MSB = 0: the next `size` bytes are raw payload.
/// * MSB = 1: the lower 31 bits are the size, followed by 68 bytes of LFG
///   seed. The decoder constructs an LFG from the seed, advances it by
///   `data_offset % 0x8000` bytes (matching Dolphin's
///   `RVZPackDecompressor::Decompress`), and fills `size` bytes of
///   output.
///
/// `data_offset` is the absolute logical byte offset of the chunk's first
/// byte inside the partition (or raw region) being decoded. It's used
/// solely to compute the LFG forward skip per junk record.
pub fn pack_decode(mut src: &[u8], data_offset: u64) -> RvzResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut current_offset = data_offset;
    while !src.is_empty() {
        if src.len() < 4 {
            return Err(RvzError::Custom(
                "truncated RVZ packing record header".to_string(),
            ));
        }
        let size = u32::from_be_bytes([src[0], src[1], src[2], src[3]]);
        src = &src[4..];

        let is_random = size & COMPRESSED_FLAG != 0;
        let size = (size & MAX_PLAIN_RUN) as usize;

        if is_random {
            if src.len() < 68 {
                return Err(RvzError::Custom(
                    "truncated RVZ packing seed".to_string(),
                ));
            }
            let seed: [u8; 68] = src[..68].try_into().unwrap();
            src = &src[68..];
            let mut lfg = LaggedFibonacci::init(&seed);
            lfg.forward_bytes((current_offset % RVZ_BLOCK_SIZE) as usize);
            let start = out.len();
            out.resize(start + size, 0);
            lfg.fill(&mut out[start..]);
        } else {
            if src.len() < size {
                return Err(RvzError::Custom(
                    "truncated RVZ packing payload".to_string(),
                ));
            }
            out.extend_from_slice(&src[..size]);
            src = &src[size..];
        }
        current_offset += size as u64;
    }
    Ok(out)
}

/// Encode a plain byte stream as a single verbatim RVZ packing record.
/// Used by the fallback path and for small inputs where scanning for
/// junk runs isn't worth it.
pub fn pack_encode_verbatim(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() + 4);
    let len = src.len() as u32;
    assert!(len & COMPRESSED_FLAG == 0, "verbatim run too large");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(src);
    out
}

/// A detected LFG junk run inside a chunk, as found by [`scan_junk_runs`].
#[derive(Debug, Clone, Copy)]
struct JunkRun {
    /// Byte offset inside the chunk where the junk starts.
    start: usize,
    /// Length in bytes of the matched junk run.
    len: usize,
    /// Seed recovered by [`LaggedFibonacci::get_seed`].
    seed: [u32; SEED_SIZE],
}

/// Port of Dolphin's `RVZPack` first pass: walk `chunk` left-to-right,
/// at each position try to reverse-derive an LFG seed via
/// [`LaggedFibonacci::get_seed`]. Runs the length-capping logic that
/// stops GetSeed calls at the next `RVZ_BLOCK_SIZE` boundary. Returns
/// the list of discovered junk runs in order.
fn scan_junk_runs(chunk: &[u8], chunk_data_offset: u64) -> Vec<JunkRun> {
    let mut runs = Vec::new();
    let mut position: usize = 0;
    let mut data_offset = chunk_data_offset;
    let total_size = chunk.len();
    while position < total_size {
        // Skip any leading zeros. Zstd compresses zeros better than
        // we can via LFG records, so don't try to re-encode them.
        let mut zeroes = 0;
        while position + zeroes < total_size && chunk[position + zeroes] == 0 {
            zeroes += 1;
        }
        position += zeroes;
        data_offset += zeroes as u64;
        if position >= total_size {
            break;
        }

        // Dolphin caps `bytes_to_read` at the next RVZ_BLOCK_SIZE
        // boundary so one GetSeed call never straddles a sector.
        let next_boundary = ((data_offset / RVZ_BLOCK_SIZE) + 1) * RVZ_BLOCK_SIZE;
        let bytes_to_read = ((next_boundary - data_offset) as usize).min(total_size - position);
        let data_offset_mod = (data_offset % RVZ_BLOCK_SIZE) as usize;

        let window = &chunk[position..position + bytes_to_read];
        if let Some((seed, matched)) = LaggedFibonacci::get_seed(window, data_offset_mod) {
            // Only record runs long enough to pay for their 68-byte
            // seed header (72 bytes total: 4-byte header + 68-byte seed).
            if matched > 72 {
                runs.push(JunkRun {
                    start: position,
                    len: matched,
                    seed,
                });
            }
        }

        position += bytes_to_read;
        data_offset += bytes_to_read as u64;
    }
    runs
}

/// Encode a chunk as a sequence of RVZ packing records, detecting LFG
/// junk runs and emitting packed records for them. Non-junk spans become
/// verbatim records. `data_offset` is the absolute logical byte offset
/// of the chunk's first byte inside the partition or raw region.
///
/// Returns `None` if no junk runs were found. Callers should fall
/// through to using `src` verbatim in that case, and set
/// `rvz_packed_size = 0` in the corresponding `RvzGroup` entry. This
/// matches Dolphin's "first_loop_iteration" special case in `RVZPack`
/// where a chunk with no junk runs is stored without any length header.
///
/// Returns `Some(packed)` when one or more junk runs were found. The
/// caller should zstd-compress `packed` and set `rvz_packed_size` to
/// `packed.len()` so the decoder knows to invoke [`pack_decode`] on the
/// zstd-decompressed output.
///
/// Ports Dolphin's `RVZPack` second pass from `WIABlob.cpp`, simplified
/// for the single-chunk case (`multipart = false`, no group reuse).
pub fn pack_encode(src: &[u8], data_offset: u64) -> Option<Vec<u8>> {
    let runs = scan_junk_runs(src, data_offset);
    if runs.is_empty() {
        return None;
    }

    let mut out = Vec::with_capacity(src.len() + runs.len() * 80);
    let mut cursor = 0usize;
    for run in &runs {
        if run.start > cursor {
            let verbatim_len = run.start - cursor;
            out.extend_from_slice(&(verbatim_len as u32).to_be_bytes());
            out.extend_from_slice(&src[cursor..run.start]);
        }
        let junk_len = run.len as u32;
        debug_assert!(junk_len & COMPRESSED_FLAG == 0);
        out.extend_from_slice(&(junk_len | COMPRESSED_FLAG).to_be_bytes());
        for w in &run.seed {
            out.extend_from_slice(&w.to_be_bytes());
        }
        cursor = run.start + run.len;
    }
    if cursor < src.len() {
        let verbatim_len = (src.len() - cursor) as u32;
        out.extend_from_slice(&verbatim_len.to_be_bytes());
        out.extend_from_slice(&src[cursor..]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_roundtrips() {
        let data: Vec<u8> = (0u8..=255).cycle().take(10_000).collect();
        let encoded = pack_encode_verbatim(&data);
        let decoded = pack_decode(&encoded, 0).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn pack_encode_returns_none_on_non_junk_input() {
        let data = b"alias test bytes".repeat(20);
        assert!(pack_encode(&data, 0).is_none());
    }

    #[test]
    fn pack_encode_detects_lfg_junk_and_round_trips() {
        // Dolphin's RVZPack scanner only probes for LFG junk at
        // RVZ_BLOCK_SIZE (0x8000) boundaries, so junk runs must START
        // at a multiple of 0x8000 relative to the chunk's data_offset.
        // Build a chunk where the first 0x8000 is pure LFG output, then
        // a trailing run of 0xBB filler. Compress and round-trip.
        let seed = [0x5Au8; 68];
        let junk_len = 0x8000usize;
        let mut lfg = LaggedFibonacci::init(&seed);
        let mut junk = vec![0u8; junk_len];
        lfg.fill(&mut junk);

        let mut chunk = Vec::with_capacity(junk_len + 200);
        chunk.extend_from_slice(&junk);
        chunk.extend_from_slice(&[0xBBu8; 200]);

        let packed = pack_encode(&chunk, 0).expect("should detect the LFG run");
        assert!(
            packed.len() < chunk.len(),
            "packed {} should be smaller than raw {}",
            packed.len(),
            chunk.len()
        );

        let decoded = pack_decode(&packed, 0).unwrap();
        assert_eq!(decoded, chunk, "round-trip must be exact");
    }

    #[test]
    fn decoder_handles_mixed_record_stream() {
        // Build a hand-rolled packed stream: verbatim 100 bytes, then an
        // LFG-seeded run of 64 bytes, then verbatim 50 bytes. Round-trip
        // through pack_decode and assert the boundary handling is correct.
        let mut input = Vec::new();
        // First record: verbatim, 100 bytes of 0xAB
        input.extend_from_slice(&100u32.to_be_bytes());
        input.extend_from_slice(&[0xABu8; 100]);
        // Second record: LFG-seeded, 64 bytes
        input.extend_from_slice(&(0x80000040u32).to_be_bytes());
        let seed = [0x11u8; 68];
        input.extend_from_slice(&seed);
        // Third record: verbatim, 50 bytes of 0xCD
        input.extend_from_slice(&50u32.to_be_bytes());
        input.extend_from_slice(&[0xCDu8; 50]);

        let decoded = pack_decode(&input, 0).unwrap();
        assert_eq!(decoded.len(), 100 + 64 + 50);
        assert_eq!(&decoded[..100], &[0xABu8; 100]);
        assert_eq!(&decoded[164..], &[0xCDu8; 50]);

        // The LFG bytes in the middle should match a freshly-seeded LFG
        // that has been forward-advanced by 100 bytes (= data_offset at
        // the junk record's start, mod BLOCK_SIZE = 0x8000).
        let mut lfg = LaggedFibonacci::init(&seed);
        lfg.forward_bytes(100);
        let mut expected = [0u8; 64];
        lfg.fill(&mut expected);
        assert_eq!(&decoded[100..164], &expected);
    }

    #[test]
    fn empty_input_decodes_to_empty_output() {
        assert!(pack_decode(&[], 0).unwrap().is_empty());
        let encoded = pack_encode_verbatim(&[]);
        assert!(pack_decode(&encoded, 0).unwrap().is_empty());
    }

    #[test]
    fn get_seed_recovers_zero_offset_seed() {
        // Seed an LFG, generate 8 KiB of output, then try to reverse-
        // derive the seed from those bytes.
        let seed = [0x5Au8; 68];
        let mut lfg = LaggedFibonacci::init(&seed);
        let mut bytes = vec![0u8; 8192];
        lfg.fill(&mut bytes);

        let (recovered, matched) =
            LaggedFibonacci::get_seed(&bytes, 0).expect("seed should be recoverable");

        // The recovered seed, fed back through init, must produce the
        // same bytes we started with.
        let mut recovered_seed_bytes = [0u8; 68];
        for i in 0..SEED_SIZE {
            recovered_seed_bytes[i * 4..i * 4 + 4].copy_from_slice(&recovered[i].to_be_bytes());
        }
        let mut lfg2 = LaggedFibonacci::init(&recovered_seed_bytes);
        let mut bytes2 = vec![0u8; 8192];
        lfg2.fill(&mut bytes2);
        assert_eq!(bytes, bytes2, "recovered seed must reproduce stream");
        assert_eq!(matched, bytes.len(), "entire stream should match");
    }

    #[test]
    fn get_seed_recovers_nonzero_offset() {
        let seed = [0xC3u8; 68];
        let mut lfg = LaggedFibonacci::init(&seed);
        // Skip 0x3E00 bytes to simulate reading from mid-cluster.
        const SKIP: usize = 0x3E00;
        let mut skipbuf = vec![0u8; SKIP];
        lfg.fill(&mut skipbuf);
        let mut bytes = vec![0u8; 8192];
        lfg.fill(&mut bytes);

        let (recovered, matched) =
            LaggedFibonacci::get_seed(&bytes, SKIP).expect("seed should be recoverable");

        let mut recovered_seed_bytes = [0u8; 68];
        for i in 0..SEED_SIZE {
            recovered_seed_bytes[i * 4..i * 4 + 4].copy_from_slice(&recovered[i].to_be_bytes());
        }
        let mut lfg2 = LaggedFibonacci::init(&recovered_seed_bytes);
        let mut skipbuf2 = vec![0u8; SKIP];
        lfg2.fill(&mut skipbuf2);
        let mut bytes2 = vec![0u8; 8192];
        lfg2.fill(&mut bytes2);
        assert_eq!(bytes, bytes2);
        assert_eq!(matched, bytes.len());
    }

    #[test]
    fn get_seed_rejects_non_lfg_data() {
        // All 0xFF is unlikely to satisfy the validation constraint.
        let data = vec![0xFFu8; 8192];
        assert!(
            LaggedFibonacci::get_seed(&data, 0).is_none(),
            "non-LFG data must not produce a seed"
        );
    }

    #[test]
    fn lfg_output_is_deterministic() {
        let seed = [0xA5u8; 68];
        let mut a = LaggedFibonacci::init(&seed);
        let mut b = LaggedFibonacci::init(&seed);
        let mut buf_a = [0u8; 1024];
        let mut buf_b = [0u8; 1024];
        a.fill(&mut buf_a);
        b.fill(&mut buf_b);
        assert_eq!(buf_a, buf_b);
    }

    #[test]
    fn lfg_different_seeds_produce_different_output() {
        let mut a = LaggedFibonacci::init(&[0x00u8; 68]);
        let mut b = LaggedFibonacci::init(&[0xFFu8; 68]);
        let mut buf_a = [0u8; 1024];
        let mut buf_b = [0u8; 1024];
        a.fill(&mut buf_a);
        b.fill(&mut buf_b);
        assert_ne!(buf_a, buf_b);
    }

    #[test]
    fn packed_record_roundtrip_via_lfg() {
        // Build a synthetic packed record by hand:
        // header = 0x80000100 (random, 256 bytes)
        // seed = 68 bytes of 0x5A
        // decoder should produce 256 bytes of LFG output seeded with 0x5A.
        let seed = [0x5Au8; 68];
        let mut header_and_seed = Vec::new();
        header_and_seed.extend_from_slice(&(0x80000100u32).to_be_bytes());
        header_and_seed.extend_from_slice(&seed);
        let decoded = pack_decode(&header_and_seed, 0).unwrap();
        assert_eq!(decoded.len(), 0x100);

        // Independently verify with a second LFG instance.
        let mut lfg = LaggedFibonacci::init(&seed);
        let mut expected = [0u8; 0x100];
        lfg.fill(&mut expected);
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_errors_on_truncated_header() {
        assert!(matches!(pack_decode(&[0x00, 0x00], 0), Err(RvzError::Custom(_))));
    }

    #[test]
    fn decode_errors_on_truncated_payload() {
        let mut buf = (5u32).to_be_bytes().to_vec();
        buf.extend_from_slice(b"abc"); // only 3 bytes, need 5
        assert!(matches!(pack_decode(&buf, 0), Err(RvzError::Custom(_))));
    }

    #[test]
    fn decode_errors_on_truncated_seed() {
        let mut buf = (0x80000100u32).to_be_bytes().to_vec();
        buf.extend_from_slice(&[0u8; 30]); // need 68 seed bytes
        assert!(matches!(pack_decode(&buf, 0), Err(RvzError::Custom(_))));
    }
}
