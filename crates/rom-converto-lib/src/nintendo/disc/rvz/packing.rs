//! RVZ packing encoding.
//!
//! A Lagged Fibonacci PRNG (f=XOR, j=32, k=521) generates pseudorandom
//! padding that the spec uses to re-synthesize large runs of Wii partition
//! padding losslessly. Both encoder and decoder live here.
//!
//! The generator keeps its 521-word state as native-endian `u32` words
//! and emits the in-memory bytes of those words in order, so on a
//! little-endian host (the only platform supported here) the byte
//! stream is the little-endian reading of the state words. `get_seed`
//! inverts that layout, so packing records produced by existing RVZ
//! encoders decode here byte-for-byte and vice versa.

use crate::nintendo::disc::rvz::error::{RvzError, RvzResult};

/// Buffer length in u32 words.
const LFG_K: usize = 521;
/// Lag of the recurrence.
const LFG_J: usize = 32;
/// Seed length in u32 words. The on-disc seed is
/// `SEED_SIZE * 4 = 68` bytes.
pub const SEED_SIZE: usize = 17;
/// Buffer length in bytes.
const LFG_BUFFER_BYTES: usize = LFG_K * 4;

/// Lagged Fibonacci generator over a 521-word state whose output is
/// byte-identical to the RVZ packing format's.
#[derive(Clone)]
pub struct LaggedFibonacci {
    buffer: [u32; LFG_K],
    /// Byte position inside the buffer. Can reach `LFG_K * 4 = 2084`.
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
    /// Seed the generator from a 68-byte preamble: each big-endian
    /// u32 slice lands in the state verbatim, then the state is
    /// expanded without the validation pass.
    pub fn init(seed: &[u8; 68]) -> Self {
        let mut seed_words = [0u32; SEED_SIZE];
        for (word, chunk) in seed_words.iter_mut().zip(seed.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*chunk);
        }
        Self::from_seed_words(&seed_words)
    }

    /// Seed the generator from words read big-endian on disc, then
    /// expand the state without the validation pass.
    pub fn from_seed_words(seed: &[u32; SEED_SIZE]) -> Self {
        let mut lfg = Self {
            buffer: [0; LFG_K],
            position_bytes: 0,
        };
        // The seed words are stored verbatim in the state head: the
        // on-disc bytes were read big-endian by `init`, and every
        // consumer reads them back big-endian, so no byte-swap here.
        lfg.buffer[..SEED_SIZE].copy_from_slice(seed);
        lfg.initialize(false)
            .expect("Initialize(false) cannot fail");
        lfg
    }

    /// Expand the state from its first 17 words via the recurrence.
    /// With `check_existing_data`, returns `None` when the existing
    /// buffer contents do not match the bit-munge constraint (the
    /// observed data is not a valid trajectory).
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

    /// Step the recurrence once: XOR the lag tail into the head, then
    /// each word into its lag-predecessor.
    fn forward(&mut self) {
        for i in 0..LFG_J {
            self.buffer[i] ^= self.buffer[i + LFG_K - LFG_J];
        }
        for i in LFG_J..LFG_K {
            self.buffer[i] ^= self.buffer[i - LFG_J];
        }
    }

    /// Undo one recurrence step over the word range
    /// `start_word..end_word`.
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

    /// Undo one recurrence step over the whole state.
    fn backward_all(&mut self) {
        self.backward(0, LFG_K);
    }

    /// Recover the seed from an expanded state: undo the four forward
    /// steps, undo the bit-munge, reconstruct the two missing seed bits
    /// per word from the later state words, and re-initialize with
    /// validation. Returns `None` if the validation fails (the observed
    /// data isn't a valid LFG trajectory).
    fn reinitialize(&mut self) -> Option<[u32; SEED_SIZE]> {
        for _ in 0..4 {
            self.backward_all();
        }

        for x in self.buffer.iter_mut() {
            *x = x.swap_bytes();
        }

        // Each seed word's missing 2 bits can be recovered from the
        // later buffer words because the recurrence leaks them.
        for i in 0..SEED_SIZE {
            self.buffer[i] = (self.buffer[i] & 0xFF00FFFF)
                | ((self.buffer[i] << 2) & 0x00FC0000)
                | (((self.buffer[i + 16] ^ self.buffer[i + 15]) << 9) & 0x00030000);
        }

        // Return the seed u32 values as-is: the caller converts them
        // to 68 bytes via `to_be_bytes` per word, the inverse of
        // `init`'s big-endian read. No byte-swap here; see
        // `from_seed_words` for the symmetry.
        let mut seed_out = [0u32; SEED_SIZE];
        seed_out.copy_from_slice(&self.buffer[..SEED_SIZE]);

        self.initialize(true).map(|_| seed_out)
    }

    /// Read the next byte of the generator's output stream: the
    /// in-memory bytes of the state words in order, wrapping the state
    /// with a forward step after each full buffer.
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

    /// Fill `out` with successive generator output bytes.
    pub fn fill(&mut self, out: &mut [u8]) {
        for b in out.iter_mut() {
            *b = self.next_byte();
        }
    }

    /// Advance the generator by `count` bytes without producing output,
    /// wrapping the state as needed.
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
    pub fn get_seed(data: &[u8], data_offset: usize) -> Option<([u32; SEED_SIZE], usize)> {
        // Skip up to 3 leading bytes to land on a u32 boundary
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
            // The state words are the little-endian reading of the
            // output bytes on this host.
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

    /// Recover the seed from the first `LFG_K` state words of an
    /// unexpanded buffer, validating the reconstruction against the
    /// remaining words.
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
        let head_len = LFG_K - data_offset_mod_k;
        self.buffer[data_offset_mod_k..data_offset_mod_k + head_len]
            .copy_from_slice(&words[..head_len]);
        self.buffer[..data_offset_mod_k].copy_from_slice(&words[head_len..LFG_K]);

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

impl LaggedFibonacci {
    /// Derive the seed the disc mastering process used for junk data in
    /// the 32 KiB sector containing `offset`, then position the
    /// generator at `offset` within that sector. `offset` is a disc
    /// byte offset for GameCube, or a partition-data offset (hashes
    /// excluded) for Wii partition contents.
    ///
    /// Matches nod's `LaggedFibonacci::init_with_seed`, validated
    /// against the published vector (see tests). Junk reseeds every
    /// 32 KiB; use [`Self::fill_junk`] to cross sector boundaries.
    pub fn with_junk_position(disc_id: &[u8; 4], disc_num: u8, offset: u64) -> Self {
        let sector = (offset / RVZ_BLOCK_SIZE) as u32;
        let seed = junk_seed(disc_id, disc_num, sector);
        let mut lfg = Self::from_seed_words(&seed);
        lfg.forward_bytes((offset % RVZ_BLOCK_SIZE) as usize);
        lfg
    }

    /// Fill `out` with junk data starting at `offset`, reseeding at
    /// every 32 KiB sector boundary as the hardware generator does.
    pub fn fill_junk(disc_id: &[u8; 4], disc_num: u8, mut offset: u64, out: &mut [u8]) {
        let mut out = &mut out[..];
        while !out.is_empty() {
            let mut lfg = Self::with_junk_position(disc_id, disc_num, offset);
            let in_sector = (offset % RVZ_BLOCK_SIZE) as usize;
            let take = out.len().min(RVZ_BLOCK_SIZE as usize - in_sector);
            let (head, tail) = out.split_at_mut(take);
            lfg.fill(head);
            out = tail;
            offset += take as u64;
        }
    }
}

/// Forward seed derivation for GameCube/Wii junk data: 17 seed words
/// from the disc ID, disc number, and 32 KiB sector index. Port of
/// nod's `generate_seed` (`nod/src/util/lfg.rs`), itself derived from
/// the generator the disc mastering process used. The reverse path
/// ([`LaggedFibonacci::get_seed`]) recovers seeds from observed bytes;
/// this is the forward path NKit relies on to regenerate stripped junk.
pub fn junk_seed(disc_id: &[u8; 4], disc_num: u8, sector: u32) -> [u32; SEED_SIZE] {
    let base = u32::from_be_bytes([
        disc_id[2],
        disc_id[1],
        disc_id[3].wrapping_add(disc_id[2]),
        disc_id[0].wrapping_add(disc_id[1]),
    ]) ^ (disc_num as u32);
    let mut n = base.wrapping_mul(0x0260_BCD5) ^ sector.wrapping_mul(0x1EF2_9123);

    let mut seed = [0u32; SEED_SIZE];
    for word in seed.iter_mut() {
        let mut v = 0u32;
        for _ in 0..LFG_J {
            n = n.wrapping_mul(0x5D58_8B65).wrapping_add(1);
            v = (v >> 1) | (n & 0x8000_0000);
        }
        *word = v;
    }
    seed[16] ^= (seed[0] >> 9) ^ (seed[16] << 23);
    seed
}

const COMPRESSED_FLAG: u32 = 1 << 31;
const MAX_PLAIN_RUN: u32 = 0x7FFF_FFFF;

/// Wii block size. The packing format treats this as the LFG stream's
/// period for the `data_offset` modulo used by `forward_bytes` at
/// decode time.
const RVZ_BLOCK_SIZE: u64 = 0x8000;

/// State of the record a [`PackedDecoder`] is currently walking.
#[derive(Clone, Copy)]
enum RecordState {
    /// Between records; the next `read` reads a record header.
    Idle,
    /// Verbatim record with `remaining` payload bytes still in the reader.
    Plain { size: usize, remaining: usize },
    /// LFG record with `remaining` output bytes still in the decoder's `lfg`.
    Random { size: usize, remaining: usize },
}

/// Streaming decoder for RVZ packing records. [`PackedDecoder::read`]
/// decodes the next window of output and resumes mid-record on the next
/// call, so oversized packed chunks can be decoded in bounded windows
/// instead of materializing the whole record stream.
pub struct PackedDecoder<R> {
    reader: R,
    /// Packed input bytes consumed so far.
    input_len: usize,
    /// Absolute logical offset of the in-progress record's first output
    /// byte, advanced by each record's full size as it completes.
    current_offset: u64,
    /// Output bytes produced by completed records so far.
    produced: usize,
    /// Ceiling on the record stream's total output: the chunk size the
    /// container declares. A record starting past it is a corrupt
    /// stream, not up to 2^31 bytes of LFG output to synthesise.
    max_output: usize,
    state: RecordState,
    /// Generator for the current `Random` record; re-seeded per record.
    lfg: LaggedFibonacci,
}

impl<R: std::io::Read> PackedDecoder<R> {
    /// Wraps `reader`, whose records decode output starting at disc
    /// offset `data_offset`. `max_output` is the output size the record
    /// stream may not exceed: the chunk size the container declares.
    pub fn new(reader: R, data_offset: u64, max_output: usize) -> Self {
        Self {
            reader,
            input_len: 0,
            current_offset: data_offset,
            produced: 0,
            max_output,
            state: RecordState::Idle,
            lfg: LaggedFibonacci {
                buffer: [0; LFG_K],
                position_bytes: 0,
            },
        }
    }

    /// The wrapped record reader, for callers that must inspect the
    /// stream they cut (for example a `Take`'s remaining limit).
    pub fn get_ref(&self) -> &R {
        &self.reader
    }

    /// Mutable access to the wrapped record reader.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Decode the next window of packed output into `out`, continuing
    /// across calls. Returns `Ok(0)` once the record stream ends; an
    /// empty `out` consumes and discards any remaining records.
    pub fn read(&mut self, out: &mut [u8]) -> RvzResult<usize> {
        if out.is_empty() {
            self.drain()?;
            return Ok(0);
        }
        let mut written = 0;
        while written < out.len() {
            match self.state {
                RecordState::Idle => {
                    if !self.start_record()? {
                        break;
                    }
                }
                RecordState::Plain { size, remaining } => {
                    let want = remaining.min(out.len() - written);
                    self.reader
                        .read_exact(&mut out[written..written + want])
                        .map_err(|error| {
                            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                                RvzError::Custom("truncated RVZ packing payload".into())
                            } else {
                                error.into()
                            }
                        })?;
                    written += want;
                    self.state = if want == remaining {
                        self.input_len += size;
                        self.current_offset += size as u64;
                        self.produced += size;
                        RecordState::Idle
                    } else {
                        RecordState::Plain {
                            size,
                            remaining: remaining - want,
                        }
                    };
                }
                RecordState::Random { size, remaining } => {
                    let want = remaining.min(out.len() - written);
                    self.lfg.fill(&mut out[written..written + want]);
                    written += want;
                    self.state = if want == remaining {
                        self.current_offset += size as u64;
                        self.produced += size;
                        RecordState::Idle
                    } else {
                        RecordState::Random {
                            size,
                            remaining: remaining - want,
                        }
                    };
                }
            }
        }
        Ok(written)
    }

    /// Reads the next record header into the state. Returns `false` at
    /// end of stream.
    fn start_record(&mut self) -> RvzResult<bool> {
        let mut header = [0u8; 4];
        loop {
            match self.reader.read(&mut header[..1]) {
                Ok(0) => return Ok(false),
                Ok(1) => break,
                Ok(_) => unreachable!("single-byte read cannot return more than one byte"),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        self.reader.read_exact(&mut header[1..]).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                RvzError::Custom("truncated RVZ packing record header".into())
            } else {
                error.into()
            }
        })?;
        self.input_len += 4;
        let encoded_size = u32::from_be_bytes(header);
        let size = (encoded_size & MAX_PLAIN_RUN) as usize;
        // A record whose output would run past the chunk's declared size
        // is corrupt: reject it here instead of generating (or draining
        // through) up to 0x7FFF_FFFF bytes of verbatim or LFG output.
        let total = self.produced.saturating_add(size);
        if total > self.max_output {
            return Err(RvzError::DecompressedSizeMismatch {
                expected: self.max_output as u64,
                actual: total as u64,
            });
        }
        if encoded_size & COMPRESSED_FLAG != 0 {
            let mut seed = [0u8; 68];
            self.reader.read_exact(&mut seed).map_err(|error| {
                if error.kind() == std::io::ErrorKind::UnexpectedEof {
                    RvzError::Custom("truncated RVZ packing seed".into())
                } else {
                    error.into()
                }
            })?;
            self.input_len += seed.len();
            self.lfg = LaggedFibonacci::init(&seed);
            self.lfg
                .forward_bytes((self.current_offset % RVZ_BLOCK_SIZE) as usize);
            self.state = RecordState::Random {
                size,
                remaining: size,
            };
        } else {
            self.state = RecordState::Plain {
                size,
                remaining: size,
            };
        }
        Ok(true)
    }

    /// Consumes every remaining record, discarding their output.
    fn drain(&mut self) -> RvzResult<()> {
        let mut discard = [0u8; 8192];
        while self.read(&mut discard)? != 0 {}
        Ok(())
    }
}

/// Decode RVZ packing records from `reader` directly into a bounded
/// output slice, returning the output and packed-input byte counts.
///
/// The input format is a sequence of records. Each record starts with a
/// 4-byte big-endian `u32`:
/// * MSB = 0: the next `size` bytes are raw payload.
/// * MSB = 1: the lower 31 bits are the size, followed by 68 bytes of LFG
///   seed. The decoder constructs an LFG from the seed, advances it by
///   `data_offset % 0x8000` bytes, and fills `size` bytes of output.
///
/// `data_offset` is the absolute logical byte offset of the chunk's first
/// byte inside the partition (or raw region) being decoded. It's used
/// solely to compute the LFG forward skip per junk record.
///
/// # Errors
/// Returns an error for truncated records; a record that would push the
/// output past `output.len()` (the chunk size the caller declared) is
/// a [`RvzError::DecompressedSizeMismatch`], on the initial decode and
/// on the final drain alike. The drain only consumes records that
/// produce no output: a non-empty record past the bound errors, so the
/// reader cannot be made to walk an unbounded record stream.
pub fn pack_decode_reader<R: std::io::Read>(
    reader: &mut R,
    data_offset: u64,
    output: &mut [u8],
) -> RvzResult<(usize, usize)> {
    let mut decoder = PackedDecoder::new(reader, data_offset, output.len());
    let mut output_len = 0;
    while output_len < output.len() {
        let count = decoder.read(&mut output[output_len..])?;
        if count == 0 {
            break;
        }
        output_len += count;
    }
    // Consume trailing records so truncation and input counts cover the
    // whole declared stream; only zero-output records can remain.
    decoder.drain()?;
    Ok((output_len, decoder.input_len))
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

/// Walk `chunk` left-to-right, at each position try to reverse-derive
/// an LFG seed via [`LaggedFibonacci::get_seed`]. Seed recovery never
/// reads past the next `RVZ_BLOCK_SIZE` boundary. Returns the list of
/// discovered junk runs in order.
fn scan_junk_runs(chunk: &[u8], chunk_data_offset: u64) -> Vec<JunkRun> {
    let mut runs = Vec::new();
    let mut position: usize = 0;
    let mut data_offset = chunk_data_offset;
    let total_size = chunk.len();
    while position < total_size {
        // Skip any leading zeros. Zstd compresses zeros better than
        // LFG records can, so don't try to re-encode them.
        let mut zeroes = 0;
        while position + zeroes < total_size && chunk[position + zeroes] == 0 {
            zeroes += 1;
        }
        position += zeroes;
        data_offset += zeroes as u64;
        if position >= total_size {
            break;
        }

        // Cap the recovery window at the next RVZ_BLOCK_SIZE boundary
        // so one seed recovery never straddles a sector.
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
/// `rvz_packed_size = 0` in the corresponding `RvzGroup` entry: a chunk
/// with no junk runs is stored without any length header.
///
/// Returns `Some(packed)` when one or more junk runs were found. The
/// caller should zstd-compress `packed` and set `rvz_packed_size` to
/// `packed.len()` so the decoder knows to invoke [`pack_decode_reader`] on the
/// zstd-decompressed output.
///
/// Encodes the single-chunk case only (no multipart grouping).
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

    fn decode(input: &[u8], data_offset: u64, max_output: usize) -> RvzResult<Vec<u8>> {
        let mut output = vec![0; max_output];
        let (decoded_len, _) =
            pack_decode_reader(&mut std::io::Cursor::new(input), data_offset, &mut output)?;
        output.truncate(decoded_len);
        Ok(output)
    }

    #[test]
    fn verbatim_roundtrips() {
        let data: Vec<u8> = (0u8..=255).cycle().take(10_000).collect();
        let encoded = pack_encode_verbatim(&data);
        let decoded = decode(&encoded, 0, data.len()).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn pack_encode_returns_none_on_non_junk_input() {
        let data = b"alias test bytes".repeat(20);
        assert!(pack_encode(&data, 0).is_none());
    }

    #[test]
    fn pack_encode_detects_lfg_junk_and_round_trips() {
        // The scanner only probes for LFG junk at RVZ_BLOCK_SIZE
        // (0x8000) boundaries, so junk runs must START at a multiple of
        // 0x8000 relative to the chunk's data_offset.
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

        let decoded = decode(&packed, 0, chunk.len()).unwrap();
        assert_eq!(decoded, chunk, "round-trip must be exact");
    }

    #[test]
    fn decoder_handles_mixed_record_stream() {
        // Build a hand-rolled packed stream: verbatim 100 bytes, then an
        // LFG-seeded run of 64 bytes, then verbatim 50 bytes. Round-trip
        // through the streaming decoder and assert the boundary handling is correct.
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

        let mut decoded = [0u8; 214];
        let (decoded_len, input_len) =
            pack_decode_reader(&mut std::io::Cursor::new(&input), 0, &mut decoded).unwrap();
        assert_eq!(decoded_len, 100 + 64 + 50);
        assert_eq!(input_len, input.len());
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

    /// A record whose declared output would run past the decoder's
    /// `max_output` (the chunk size the container declares) must be
    /// rejected in `start_record`, before any of its up-to-0x7FFF_FFFF
    /// bytes of LFG output are generated.
    #[test]
    fn record_past_max_output_errors_immediately() {
        let mut input = Vec::new();
        input.extend_from_slice(&(0x7FFF_FFFFu32 | COMPRESSED_FLAG).to_be_bytes());
        input.extend_from_slice(&[0x5Au8; 68]);
        let mut decoder = PackedDecoder::new(std::io::Cursor::new(&input), 0, 16);
        let mut out = [0u8; 64];
        let err = decoder.read(&mut out).unwrap_err();
        assert!(
            matches!(
                err,
                RvzError::DecompressedSizeMismatch {
                    expected: 16,
                    actual: 0x7FFF_FFFF
                }
            ),
            "{err}"
        );
    }

    /// The post-decode drain must reject a further record rather than
    /// silently consuming output the chunk size cannot back.
    #[test]
    fn drain_rejects_record_past_max_output() {
        let mut input = Vec::new();
        input.extend_from_slice(&4u32.to_be_bytes());
        input.extend_from_slice(b"abcd");
        input.extend_from_slice(&4u32.to_be_bytes());
        input.extend_from_slice(b"wxyz");
        let mut output = [0u8; 4];
        let err =
            pack_decode_reader(&mut std::io::Cursor::new(&input), 0, &mut output).unwrap_err();
        assert!(
            matches!(
                err,
                RvzError::DecompressedSizeMismatch {
                    expected: 4,
                    actual: 8
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn packed_decoder_windows_match_one_shot_across_lfg_block_boundary() {
        // The verbatim lead ends 100 bytes short of the 0x8000 boundary,
        // so the LFG record's forward skip is 0x7F9C and its 200 output
        // bytes cross the boundary. Windowed reads of an awkward size
        // must reproduce the one-shot walk exactly.
        let seed = [0x7Bu8; 68];
        let lead_len = 0x7F9Cusize;
        const JUNK_LEN: usize = 200;
        let mut input = Vec::new();
        input.extend_from_slice(&(lead_len as u32).to_be_bytes());
        input.extend_from_slice(&vec![0x41u8; lead_len]);
        input.extend_from_slice(&((JUNK_LEN as u32) | COMPRESSED_FLAG).to_be_bytes());
        input.extend_from_slice(&seed);
        input.extend_from_slice(&12u32.to_be_bytes());
        input.extend_from_slice(b"tail-payload");

        let total = lead_len + JUNK_LEN + 12;
        let mut one_shot = vec![0u8; total];
        let (decoded_len, input_len) =
            pack_decode_reader(&mut std::io::Cursor::new(&input), 0, &mut one_shot).unwrap();
        assert_eq!(decoded_len, total);
        assert_eq!(input_len, input.len());
        // Independent check of the LFG span across the 0x8000 boundary.
        let mut lfg = LaggedFibonacci::init(&seed);
        lfg.forward_bytes(lead_len % 0x8000);
        let mut expected_junk = [0u8; JUNK_LEN];
        lfg.fill(&mut expected_junk);
        assert_eq!(&one_shot[lead_len..lead_len + JUNK_LEN], &expected_junk);

        let mut decoder = PackedDecoder::new(std::io::Cursor::new(&input), 0, total);
        let mut windowed = Vec::new();
        // 32668 % 995 = 828, so the 200-byte LFG record straddles a window
        // edge and must resume mid-record on the next call.
        let mut window = [0u8; 995];
        loop {
            let count = decoder.read(&mut window).unwrap();
            if count == 0 {
                break;
            }
            windowed.extend_from_slice(&window[..count]);
        }
        assert_eq!(windowed, one_shot.as_slice());
        assert_eq!(decoder.input_len, input.len());
    }

    #[test]
    fn empty_input_decodes_to_empty_output() {
        assert!(decode(&[], 0, 0).unwrap().is_empty());
        let encoded = pack_encode_verbatim(&[]);
        assert!(decode(&encoded, 0, 0).unwrap().is_empty());
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
        // same bytes the test started with.
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
        let decoded = decode(&header_and_seed, 0, 0x100).unwrap();
        assert_eq!(decoded.len(), 0x100);

        // Independently verify with a second LFG instance.
        let mut lfg = LaggedFibonacci::init(&seed);
        let mut expected = [0u8; 0x100];
        lfg.fill(&mut expected);
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decode_errors_on_truncated_header() {
        assert!(matches!(
            decode(&[0x00, 0x00], 0, 0x100),
            Err(RvzError::Custom(_))
        ));
    }

    #[test]
    fn decode_errors_on_truncated_payload() {
        let mut buf = (5u32).to_be_bytes().to_vec();
        buf.extend_from_slice(b"abc"); // only 3 bytes, need 5
        assert!(matches!(decode(&buf, 0, 0x100), Err(RvzError::Custom(_))));
    }

    #[test]
    fn decode_errors_on_truncated_seed() {
        let mut buf = (0x80000100u32).to_be_bytes().to_vec();
        buf.extend_from_slice(&[0u8; 30]); // need 68 seed bytes
        assert!(matches!(decode(&buf, 0, 0x100), Err(RvzError::Custom(_))));
    }

    /// Published junk vector cross-checked against nod's `lfg.rs` test
    /// suite: disc "GALE" (Super Smash Bros. Melee), disc 0, offset
    /// 0x600000.
    #[test]
    fn junk_seed_matches_known_vector() {
        let mut out = [0u8; 16];
        let mut lfg = LaggedFibonacci::with_junk_position(b"GALE", 0, 0x600000);
        lfg.fill(&mut out);
        assert_eq!(
            out,
            [
                0xE9, 0x47, 0x67, 0xBD, 0x41, 0x50, 0x4D, 0x5D, 0x61, 0x48, 0xB1, 0x99, 0xA0, 0x12,
                0x0C, 0xBA
            ]
        );
    }

    #[test]
    fn fill_junk_reseeds_per_sector() {
        // Filling across a sector boundary must equal two independent
        // per-sector fills.
        let mut joined = vec![0u8; 0x100];
        LaggedFibonacci::fill_junk(b"GALE", 0, 0x8000 - 0x80, &mut joined);

        let mut first = vec![0u8; 0x80];
        LaggedFibonacci::fill_junk(b"GALE", 0, 0x8000 - 0x80, &mut first);
        let mut second = vec![0u8; 0x80];
        LaggedFibonacci::fill_junk(b"GALE", 0, 0x8000, &mut second);

        assert_eq!(&joined[..0x80], &first[..]);
        assert_eq!(&joined[0x80..], &second[..]);
    }

    #[test]
    fn junk_seed_round_trips_through_get_seed() {
        // Forward-generated junk must be recoverable by the reverse
        // derivation used by the RVZ pack encoder.
        let mut junk = vec![0u8; 0x8000];
        LaggedFibonacci::fill_junk(b"RMCE", 0, 0x40000, &mut junk);
        let (seed, matched) =
            LaggedFibonacci::get_seed(&junk, 0).expect("forward junk must be a valid trajectory");
        assert_eq!(matched, junk.len());
        assert_eq!(seed, junk_seed(b"RMCE", 0, 8));
    }
}
