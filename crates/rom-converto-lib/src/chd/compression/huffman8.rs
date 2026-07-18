//! MAME `huffman_8bit_decoder` port for the CHD `huff` hunk codec.
//!
//! chdman's createdvd/createhd default codec set includes `huff`, so
//! reading arbitrary chdman files needs it even though this crate's writer
//! never emits it. The format (MAME `huffman.cpp`): a 24-code/6-bit
//! "small" huffman tree describes the code lengths of the real
//! 256-code/16-bit tree (lengths shifted by one, value 0 = RLE repeat
//! of the previous length), both trees canonical; the payload is one
//! code per output byte.
//!
//! This is the same canonical-code scheme as the compressed-map
//! decoder in `chd/map.rs` but with different parameters and the
//! huffman (not RLE) tree serialization, and it needs MAME's
//! `bitstream_in` semantics: `peek` past the end of input reads
//! zeroes, only `overflow()` at the end reports truncation.

use std::io;

use crate::chd::error::ChdResult;

/// MSB-first bit reader with zero-padded lookahead, mirroring MAME's
/// `bitstream_in` so 16-bit peeks near the end of the stream behave
/// identically.
struct BitStream<'a> {
    data: &'a [u8],
    offset: usize,
    buffer: u32,
    bits: u8,
}

impl<'a> BitStream<'a> {
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

    fn overflow(&self) -> bool {
        self.offset.saturating_sub(self.bits as usize / 8) > self.data.len()
    }
}

/// Canonical huffman decoder over `lengths.len()` codes with a flat
/// `max_bits`-wide lookup table, matching MAME's
/// `assign_canonical_codes` + `build_lookup_table`.
struct CanonicalDecoder {
    lookup: Vec<(u16, u8)>,
    max_bits: u8,
}

impl CanonicalDecoder {
    fn from_lengths(lengths: &[u8], max_bits: u8) -> ChdResult<Self> {
        let mut bithisto = [0u32; 33];
        for &len in lengths {
            if len > max_bits {
                return Err(huff_err("code length exceeds maximum"));
            }
            bithisto[len as usize] += 1;
        }

        let mut curstart = 0u32;
        for codelen in (1..=32usize).rev() {
            let nextstart = (curstart + bithisto[codelen]) >> 1;
            if codelen != 1 && nextstart * 2 != curstart + bithisto[codelen] {
                return Err(huff_err("inconsistent code lengths"));
            }
            bithisto[codelen] = curstart;
            curstart = nextstart;
        }

        let mut lookup = vec![(0u16, 0u8); 1usize << max_bits];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let code = bithisto[len as usize];
            bithisto[len as usize] += 1;

            let shift = max_bits - len;
            let base = (code as usize) << shift;
            for slot in lookup.iter_mut().skip(base).take(1usize << shift) {
                *slot = (symbol as u16, len);
            }
        }

        Ok(Self { lookup, max_bits })
    }

    fn decode_one(&self, bits: &mut BitStream) -> ChdResult<u16> {
        let value = bits.peek(self.max_bits);
        let (symbol, numbits) = self.lookup[value as usize];
        if numbits == 0 {
            return Err(huff_err("invalid code in stream"));
        }
        bits.remove(numbits);
        Ok(symbol)
    }
}

/// Decode one `huff` hunk: import the huffman-encoded 256-code tree,
/// then decode `dest_len` bytes. Port of
/// `huffman_8bit_decoder::decode`.
pub(crate) fn huffman8_decode(src: &[u8], dest_len: usize) -> ChdResult<Vec<u8>> {
    const NUM_CODES: usize = 256;
    // ceil(log2(NUM_CODES - 9)) per MAME's rlefullbits derivation.
    const RLE_FULL_BITS: u8 = 8;

    let mut bits = BitStream::new(src);

    let mut small_lengths = [0u8; 24];
    small_lengths[0] = bits.read(3) as u8;
    let start = bits.read(3) as usize + 1;
    let mut count = 0u32;
    for (index, length) in small_lengths.iter_mut().enumerate().skip(1) {
        if index < start || count == 7 {
            *length = 0;
        } else {
            count = bits.read(3);
            *length = if count == 7 { 0 } else { count as u8 };
        }
    }
    let small = CanonicalDecoder::from_lengths(&small_lengths, 6)?;

    let mut lengths = [0u8; NUM_CODES];
    let mut last = 0u8;
    let mut curcode = 0usize;
    while curcode < NUM_CODES {
        let value = small.decode_one(&mut bits)?;
        if value != 0 {
            last = (value - 1) as u8;
            lengths[curcode] = last;
            curcode += 1;
        } else {
            let mut repeat = bits.read(3) + 2;
            if repeat == 7 + 2 {
                repeat += bits.read(RLE_FULL_BITS);
            }
            while repeat != 0 && curcode < NUM_CODES {
                lengths[curcode] = last;
                curcode += 1;
                repeat -= 1;
            }
        }
    }

    let decoder = CanonicalDecoder::from_lengths(&lengths, 16)?;
    let mut out = vec![0u8; dest_len];
    for byte in out.iter_mut() {
        *byte = decoder.decode_one(&mut bits)? as u8;
    }
    if bits.overflow() {
        return Err(huff_err("input buffer too small"));
    }
    Ok(out)
}

fn huff_err(msg: &str) -> crate::chd::error::ChdError {
    io::Error::other(format!("huffman hunk decode: {msg}")).into()
}

/// MSB-first bit writer mirroring MAME's `bitstream_out`: `write`
/// emits the low `numbits` of `value` most-significant-bit first, and
/// `finish` left-aligns the trailing partial byte with zero padding.
struct BitWriter {
    out: Vec<u8>,
    accum: u32,
    bits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            accum: 0,
            bits: 0,
        }
    }

    fn write(&mut self, value: u32, numbits: u8) {
        for i in (0..numbits).rev() {
            self.accum = (self.accum << 1) | ((value >> i) & 1);
            self.bits += 1;
            if self.bits == 8 {
                self.out.push(self.accum as u8);
                self.accum = 0;
                self.bits = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push((self.accum << (8 - self.bits)) as u8);
        }
        self.out
    }
}

/// One huffman tree node. Ports MAME's `node_t`; `parent == -1` is the
/// null-parent sentinel (MAME uses a null pointer).
#[derive(Clone, Copy)]
struct Node {
    parent: i32,
    weight: u32,
    bits: u32,
    numbits: u8,
}

impl Default for Node {
    fn default() -> Self {
        Node {
            parent: -1,
            weight: 0,
            bits: 0,
            numbits: 0,
        }
    }
}

/// Port of MAME `build_tree`: build a huffman tree from the histogram
/// with weights scaled by `totalweight / totaldata`, returning the
/// longest resulting code length. Leaves live at `nodes[0..numcodes]`;
/// internal nodes are allocated from `nodes[numcodes..]`.
fn build_tree(
    histo: &[u32],
    numcodes: usize,
    totaldata: u32,
    totalweight: u32,
    nodes: &mut [Node],
) -> u8 {
    for node in nodes[..numcodes].iter_mut() {
        *node = Node::default();
    }

    let mut list: Vec<usize> = Vec::with_capacity(numcodes * 2);
    for curcode in 0..numcodes {
        if histo[curcode] != 0 {
            let mut weight = (histo[curcode] as u64 * totalweight as u64 / totaldata as u64) as u32;
            if weight == 0 {
                weight = 1;
            }
            nodes[curcode].weight = weight;
            list.push(curcode);
        }
    }

    // Largest weight first; ties broken by ascending code index, which
    // matches MAME's qsort key (weight desc, then m_bits == curcode asc)
    // with unique keys, so a stable sort reproduces it exactly.
    list.sort_by(|&a, &b| nodes[b].weight.cmp(&nodes[a].weight).then(a.cmp(&b)));

    let mut nextalloc = numcodes;
    while list.len() > 1 {
        let n1 = list.pop().unwrap();
        let n0 = list.pop().unwrap();
        let newidx = nextalloc;
        nextalloc += 1;
        let weight = nodes[n0].weight + nodes[n1].weight;
        nodes[newidx] = Node {
            parent: -1,
            weight,
            bits: 0,
            numbits: 0,
        };
        nodes[n0].parent = newidx as i32;
        nodes[n1].parent = newidx as i32;

        let mut pos = list.len();
        for (i, &item) in list.iter().enumerate() {
            if weight > nodes[item].weight {
                pos = i;
                break;
            }
        }
        list.insert(pos, newidx);
    }

    let mut maxbits = 0u8;
    for curcode in 0..numcodes {
        let has_weight = nodes[curcode].weight > 0;
        nodes[curcode].numbits = 0;
        nodes[curcode].bits = 0;
        if has_weight {
            let mut nb = 0u8;
            let mut cur = curcode;
            while nodes[cur].parent != -1 {
                nb += 1;
                cur = nodes[cur].parent as usize;
            }
            if nb == 0 {
                nb = 1;
            }
            nodes[curcode].numbits = nb;
            maxbits = maxbits.max(nb);
        }
    }
    maxbits
}

/// Port of MAME `assign_canonical_codes`: turn the per-node code
/// lengths into canonical codes, the same scheme the decoder's
/// [`CanonicalDecoder::from_lengths`] reads back.
fn assign_canonical_codes(nodes: &mut [Node], numcodes: usize, maxbits: u8) -> ChdResult<()> {
    let mut bithisto = [0u32; 33];
    for node in nodes[..numcodes].iter() {
        if node.numbits > maxbits {
            return Err(huff_err(
                "internal inconsistency: code length exceeds maximum",
            ));
        }
        if node.numbits <= 32 {
            bithisto[node.numbits as usize] += 1;
        }
    }

    let mut curstart = 0u32;
    for codelen in (1..=32usize).rev() {
        let nextstart = (curstart + bithisto[codelen]) >> 1;
        if codelen != 1 && nextstart * 2 != curstart + bithisto[codelen] {
            return Err(huff_err(
                "internal inconsistency: inconsistent code lengths",
            ));
        }
        bithisto[codelen] = curstart;
        curstart = nextstart;
    }

    for node in nodes[..numcodes].iter_mut() {
        if node.numbits > 0 {
            node.bits = bithisto[node.numbits as usize];
            bithisto[node.numbits as usize] += 1;
        }
    }
    Ok(())
}

/// Port of MAME `compute_tree_from_histo`: binary-search the weight
/// scale so the tree fits within `maxbits`, then assign canonical codes.
fn compute_tree_from_histo(
    histo: &[u32],
    numcodes: usize,
    maxbits: u8,
    nodes: &mut [Node],
) -> ChdResult<()> {
    let sdatacount: u32 = histo[..numcodes].iter().sum();

    let mut lowerweight = 0u32;
    let mut upperweight = sdatacount * 2;
    loop {
        let curweight = (upperweight + lowerweight) / 2;
        let curmaxbits = build_tree(histo, numcodes, sdatacount, curweight, nodes);
        if curmaxbits <= maxbits {
            lowerweight = curweight;
            if curweight == sdatacount || (upperweight - lowerweight) <= 1 {
                break;
            }
        } else {
            upperweight = curweight;
        }
    }

    assign_canonical_codes(nodes, numcodes, maxbits)
}

/// Port of MAME `export_tree_huffman`: RLE-compress the main tree's code
/// lengths, build a 24-code/6-bit huffman tree over the RLE tokens,
/// then write the small-tree header followed by the huffman-coded RLE
/// stream. Mirrors the decoder's `import_tree_huffman`.
fn export_tree_huffman(w: &mut BitWriter, nodes: &[Node], numcodes: usize) -> ChdResult<()> {
    const SMALL_CODES: usize = 24;
    const SMALL_MAX_BITS: u8 = 6;

    let mut rle_data: Vec<u8> = Vec::with_capacity(numcodes);
    let mut rle_lengths: Vec<u16> = Vec::new();
    let mut small_histo = [0u32; SMALL_CODES];

    let push_run = |rle_data: &mut Vec<u8>,
                    rle_lengths: &mut Vec<u16>,
                    small_histo: &mut [u32; SMALL_CODES],
                    last: i32,
                    repcount: i32| {
        if repcount == 1 {
            let d = (last + 1) as u8;
            small_histo[d as usize] += 1;
            rle_data.push(d);
        } else {
            small_histo[0] += 1;
            rle_data.push(0);
            rle_lengths.push((repcount - 2) as u16);
        }
    };

    let mut last: i32 = -1;
    let mut repcount: i32 = 0;
    for node in nodes[..numcodes].iter() {
        let newval = node.numbits as i32;
        if newval != last && repcount > 0 {
            push_run(
                &mut rle_data,
                &mut rle_lengths,
                &mut small_histo,
                last,
                repcount,
            );
        }
        if newval == last {
            repcount += 1;
        } else {
            let d = (newval + 1) as u8;
            small_histo[d as usize] += 1;
            rle_data.push(d);
            last = newval;
            repcount = 0;
        }
    }
    if repcount > 0 {
        push_run(
            &mut rle_data,
            &mut rle_lengths,
            &mut small_histo,
            last,
            repcount,
        );
    }

    let mut small_nodes = vec![Node::default(); SMALL_CODES * 2];
    compute_tree_from_histo(&small_histo, SMALL_CODES, SMALL_MAX_BITS, &mut small_nodes)?;

    let mut first_non_zero = 31i32;
    let mut last_non_zero = 0i32;
    for index in 1..SMALL_CODES {
        if small_nodes[index].numbits != 0 {
            if first_non_zero == 31 {
                first_non_zero = index as i32;
            }
            last_non_zero = index as i32;
        }
    }
    first_non_zero = first_non_zero.min(8);

    w.write(small_nodes[0].numbits as u32, 3);
    w.write((first_non_zero - 1) as u32, 3);
    for index in first_non_zero..=last_non_zero {
        w.write(small_nodes[index as usize].numbits as u32, 3);
    }
    w.write(7, 3);

    let mut temp = (numcodes - 9) as u32;
    let mut rlefullbits = 0u8;
    while temp != 0 {
        temp >>= 1;
        rlefullbits += 1;
    }

    let mut li = 0usize;
    for &data in rle_data.iter() {
        let node = &small_nodes[data as usize];
        w.write(node.bits, node.numbits);
        if data == 0 {
            let count = rle_lengths[li] as u32;
            li += 1;
            if count < 7 {
                w.write(count, 3);
            } else {
                w.write(7, 3);
                w.write(count - 7, rlefullbits);
            }
        }
    }
    Ok(())
}

/// Encode `src` into the CHD `huff` codec bitstream, appended to `dst`.
/// Port of MAME `huffman_8bit_encoder::encode`; the output round-trips
/// through [`huffman8_decode`]. The caller decides whether the result
/// is worth keeping by comparing sizes, matching the CD/DVD codec trials.
pub(crate) fn huffman8_encode(src: &[u8], dst: &mut Vec<u8>) -> ChdResult<()> {
    const NUM_CODES: usize = 256;
    const MAX_BITS: u8 = 16;

    let mut histo = [0u32; NUM_CODES];
    for &b in src {
        histo[b as usize] += 1;
    }

    let mut nodes = vec![Node::default(); NUM_CODES * 2];
    compute_tree_from_histo(&histo, NUM_CODES, MAX_BITS, &mut nodes)?;

    let mut w = BitWriter::new();
    export_tree_huffman(&mut w, &nodes, NUM_CODES)?;
    for &b in src {
        let node = &nodes[b as usize];
        w.write(node.bits, node.numbits);
    }
    dst.extend_from_slice(&w.finish());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode `payload` with a uniform 8-bit tree (every symbol gets
    /// length 8, so the canonical code of byte `b` is `b` itself).
    /// The tree header writes the length value 9 (= 8 + 1) once via
    /// the small tree, then RLE-repeats it across all 256 codes.
    fn encode_uniform(payload: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::new();
        // Small tree: symbol 0 -> code 0, symbol 9 -> code 1, both
        // 1 bit. Layout: lengths[0]=1, start=8, then per-index
        // 3-bit lengths: index 8 = 0, index 9 = 1, index 10 = 7
        // (sentinel: all remaining are zero).
        w.write(1, 3);
        w.write(7, 3);
        w.write(0, 3);
        w.write(1, 3);
        w.write(7, 3);

        // Tree body: symbol 9 (code 1) sets length 8 for code 0;
        // symbol 0 (code 0) starts RLE: count = 7 + 2 + 246 = 255.
        w.write(1, 1);
        w.write(0, 1);
        w.write(7, 3);
        w.write(246, 8);

        for &b in payload {
            w.write(b as u32, 8);
        }
        w.finish()
    }

    #[test]
    fn decodes_uniform_tree_stream() {
        let payload: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let encoded = encode_uniform(&payload);
        let decoded = huffman8_decode(&encoded, payload.len()).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn truncated_stream_reports_overflow() {
        let payload = vec![0xABu8; 64];
        let mut encoded = encode_uniform(&payload);
        encoded.truncate(encoded.len() - 8);
        assert!(huffman8_decode(&encoded, payload.len()).is_err());
    }

    fn roundtrip(payload: &[u8]) {
        let mut encoded = Vec::new();
        huffman8_encode(payload, &mut encoded).unwrap();
        let decoded = huffman8_decode(&encoded, payload.len()).unwrap();
        assert_eq!(decoded, payload);
    }

    fn xorshift(len: usize) -> Vec<u8> {
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
    fn encode_roundtrips_all_zero() {
        roundtrip(&vec![0u8; 4096]);
    }

    #[test]
    fn encode_roundtrips_uniform_random() {
        roundtrip(&xorshift(4096));
    }

    #[test]
    fn encode_roundtrips_text_like() {
        let seed = b"the quick brown fox jumps over the lazy dog. ";
        let payload: Vec<u8> = seed.iter().copied().cycle().take(8192).collect();
        roundtrip(&payload);
    }

    #[test]
    fn encode_roundtrips_cd_hunk_size() {
        roundtrip(&xorshift(18816));
    }

    #[test]
    fn encode_roundtrips_single_distinct_value() {
        roundtrip(&vec![0x5Au8; 4096]);
    }

    #[test]
    fn encode_roundtrips_all_256_values() {
        let payload: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        roundtrip(&payload);
    }
}
