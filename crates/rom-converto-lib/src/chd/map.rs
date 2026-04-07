use crate::chd::error::{ChdError, ChdResult};
use byteorder::{BigEndian, ByteOrder};
use crc::{CRC_16_IBM_3740, Crc};
use std::cmp;

#[derive(Debug)]
pub(crate) struct MapEntry {
    pub compression: u8,
    pub length: u32,
    pub offset: u64,
    pub crc16: u16,
}

const COMPRESSION_TYPE_0: u8 = 0;
const COMPRESSION_TYPE_1: u8 = 1;
const COMPRESSION_TYPE_2: u8 = 2;
const COMPRESSION_TYPE_3: u8 = 3;
pub(crate) const COMPRESSION_NONE: u8 = 4;
pub(crate) const COMPRESSION_SELF: u8 = 5;
pub(crate) const COMPRESSION_PARENT: u8 = 6;
const COMPRESSION_RLE_SMALL: u8 = 7;
const COMPRESSION_RLE_LARGE: u8 = 8;
const COMPRESSION_SELF_0: u8 = 9;
const COMPRESSION_SELF_1: u8 = 10;
const COMPRESSION_PARENT_SELF: u8 = 11;
const COMPRESSION_PARENT_0: u8 = 12;
const COMPRESSION_PARENT_1: u8 = 13;

const HUFFMAN_CODES: usize = 16;
const HUFFMAN_MAX_BITS: u8 = 8;
const MAP_ENTRY_SIZE: usize = 12;
const MAP_HEADER_SIZE: usize = 16;
const RLE_SMALL_BASE: u32 = 3;
const RLE_SMALL_MAX_EXTRA: u32 = 15;
const RLE_LARGE_BASE: u32 = 3 + 16;
const RLE_LARGE_MAX_EXTRA: u32 = 255;
const RLE_SMALL_DECODE_BASE: u32 = 2;
const RLE_LARGE_DECODE_BASE: u32 = 2 + 16;
const CANONICAL_MAX_BITS: usize = 32;
const BITHISTO_LEN: usize = CANONICAL_MAX_BITS + 1;

pub(crate) fn crc16_ccitt(data: &[u8]) -> u16 {
    let crc = Crc::<u16>::new(&CRC_16_IBM_3740);
    crc.checksum(data)
}

pub(crate) fn compress_v5_map(
    entries: &[MapEntry],
    hunk_bytes: u32,
    unit_bytes: u32,
) -> ChdResult<Vec<u8>> {
    let rawmap = encode_raw_map(entries);
    let hunk_count = entries.len() as u32;

    let mapcrc = crc16_ccitt(&rawmap);
    let mut compression_rle = Vec::with_capacity(hunk_count as usize);
    let mut encoder = HuffmanEncoder::new();

    let mut max_self = 0u32;
    let mut last_self = 0u32;
    let mut max_parent = 0u64;
    let mut last_parent = 0u64;
    let mut max_complen = 0u32;

    let mut lastcomp = 0u8;
    let mut count = 0u32;

    for hunknum in 0..hunk_count {
        let base = (hunknum as usize) * MAP_ENTRY_SIZE;
        let mut curcomp = rawmap[base];

        if curcomp == COMPRESSION_SELF {
            let refhunk = read_u48_be(&rawmap[base + 4..base + 10]) as u32;
            if refhunk == last_self {
                curcomp = COMPRESSION_SELF_0;
            } else if refhunk == last_self + 1 {
                curcomp = COMPRESSION_SELF_1;
            } else {
                max_self = max_self.max(refhunk);
            }
            last_self = refhunk;
        } else if curcomp == COMPRESSION_PARENT {
            let refunit = read_u48_be(&rawmap[base + 4..base + 10]);
            let self_unit = (hunknum as u64 * hunk_bytes as u64) / unit_bytes as u64;
            if refunit == self_unit {
                curcomp = COMPRESSION_PARENT_SELF;
            } else if refunit == last_parent {
                curcomp = COMPRESSION_PARENT_0;
            } else if refunit == last_parent + (hunk_bytes / unit_bytes) as u64 {
                curcomp = COMPRESSION_PARENT_1;
            } else {
                max_parent = max_parent.max(refunit);
            }
            last_parent = refunit;
        } else {
            max_complen = max_complen.max(BigEndian::read_u24(&rawmap[base + 1..base + 4]));
        }

        if curcomp == lastcomp {
            count += 1;
        }

        if curcomp != lastcomp || hunknum == hunk_count - 1 {
            while count != 0 {
                if count < RLE_SMALL_BASE {
                    push_symbol(&mut compression_rle, &mut encoder, lastcomp);
                    count -= 1;
                } else if count <= RLE_SMALL_BASE + RLE_SMALL_MAX_EXTRA {
                    push_symbol(&mut compression_rle, &mut encoder, COMPRESSION_RLE_SMALL);
                    push_symbol(
                        &mut compression_rle,
                        &mut encoder,
                        (count - RLE_SMALL_BASE) as u8,
                    );
                    count = 0;
                } else {
                    let this_count = cmp::min(count, RLE_LARGE_BASE + RLE_LARGE_MAX_EXTRA);
                    let rem = this_count - RLE_LARGE_BASE;
                    push_symbol(&mut compression_rle, &mut encoder, COMPRESSION_RLE_LARGE);
                    push_symbol(&mut compression_rle, &mut encoder, (rem >> 4) as u8);
                    push_symbol(&mut compression_rle, &mut encoder, (rem & 0x0f) as u8);
                    count -= this_count;
                }
            }

            if curcomp != lastcomp {
                push_symbol(&mut compression_rle, &mut encoder, curcomp);
                lastcomp = curcomp;
            }
        }
    }

    let lengthbits = bits_for_value(max_complen as u64);
    let selfbits = bits_for_value(max_self as u64);
    let parentbits = bits_for_value(max_parent);

    encoder.compute_tree_from_histo()?;

    let mut bitbuf = BitWriter::new();
    encoder.export_tree_rle(&mut bitbuf)?;
    for &symbol in &compression_rle {
        encoder.encode_one(&mut bitbuf, symbol);
    }

    let mut src_index = 0usize;
    lastcomp = 0;
    count = 0;
    let mut firstoffs = 0u64;

    for hunknum in 0..hunk_count {
        let base = (hunknum as usize) * MAP_ENTRY_SIZE;
        let length = BigEndian::read_u24(&rawmap[base + 1..base + 4]);
        let offset = read_u48_be(&rawmap[base + 4..base + 10]);
        let crc = BigEndian::read_u16(&rawmap[base + 10..base + 12]);

        if count == 0 {
            let val = compression_rle[src_index];
            src_index += 1;
            if val == COMPRESSION_RLE_SMALL {
                count = RLE_SMALL_DECODE_BASE + compression_rle[src_index] as u32;
                src_index += 1;
            } else if val == COMPRESSION_RLE_LARGE {
                let high = compression_rle[src_index] as u32;
                src_index += 1;
                let low = compression_rle[src_index] as u32;
                src_index += 1;
                count = RLE_LARGE_DECODE_BASE + (high << 4) + low;
            } else {
                lastcomp = val;
            }
        } else {
            count -= 1;
        }

        match lastcomp {
            COMPRESSION_TYPE_0 | COMPRESSION_TYPE_1 | COMPRESSION_TYPE_2 | COMPRESSION_TYPE_3 => {
                bitbuf.write(length, lengthbits);
                bitbuf.write(crc as u32, 16);
                if firstoffs == 0 {
                    firstoffs = offset;
                }
            }
            COMPRESSION_NONE => {
                bitbuf.write(crc as u32, 16);
                if firstoffs == 0 {
                    firstoffs = offset;
                }
            }
            COMPRESSION_SELF => {
                bitbuf.write(offset as u32, selfbits);
            }
            COMPRESSION_PARENT => {
                bitbuf.write(offset as u32, parentbits);
            }
            COMPRESSION_SELF_0
            | COMPRESSION_SELF_1
            | COMPRESSION_PARENT_SELF
            | COMPRESSION_PARENT_0
            | COMPRESSION_PARENT_1 => {}
            _ => return Err(ChdError::MapCompressionError),
        }
    }

    let compressed = bitbuf.finish();
    let mut output = Vec::with_capacity(MAP_HEADER_SIZE + compressed.len());
    output.extend_from_slice(&[0u8; MAP_HEADER_SIZE]);
    output.extend_from_slice(&compressed);

    BigEndian::write_u32(&mut output[0..4], compressed.len() as u32);
    write_u48_be(&mut output[4..10], firstoffs);
    BigEndian::write_u16(&mut output[10..12], mapcrc);
    output[12] = lengthbits;
    output[13] = selfbits;
    output[14] = parentbits;
    output[15] = 0;

    Ok(output)
}

fn push_symbol(list: &mut Vec<u8>, encoder: &mut HuffmanEncoder, value: u8) {
    list.push(value);
    encoder.histo_one(value);
}

fn encode_raw_map(entries: &[MapEntry]) -> Vec<u8> {
    let mut raw = vec![0u8; entries.len() * MAP_ENTRY_SIZE];
    for (idx, entry) in entries.iter().enumerate() {
        let base = idx * MAP_ENTRY_SIZE;
        raw[base] = entry.compression;
        BigEndian::write_u24(&mut raw[base + 1..base + 4], entry.length);
        write_u48_be(&mut raw[base + 4..base + 10], entry.offset);
        BigEndian::write_u16(&mut raw[base + 10..base + 12], entry.crc16);
    }
    raw
}

fn bits_for_value(mut value: u64) -> u8 {
    let mut result = 0u8;
    while value != 0 {
        value >>= 1;
        result += 1;
    }
    result
}

fn read_u48_be(buf: &[u8]) -> u64 {
    let mut bytes = [0u8; 8];
    bytes[2..].copy_from_slice(&buf[..6]);
    u64::from_be_bytes(bytes)
}

fn write_u48_be(buf: &mut [u8], value: u64) {
    let bytes = value.to_be_bytes();
    buf.copy_from_slice(&bytes[2..]);
}

// BitWriter (compression)

#[derive(Debug)]
struct BitWriter {
    data: Vec<u8>,
    accum: u8,
    bits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            accum: 0,
            bits: 0,
        }
    }

    fn write(&mut self, value: u32, num_bits: u8) {
        if num_bits == 0 {
            return;
        }

        for i in (0..num_bits).rev() {
            let bit = ((value >> i) & 1) as u8;
            self.accum = (self.accum << 1) | bit;
            self.bits += 1;
            if self.bits == 8 {
                self.data.push(self.accum);
                self.accum = 0;
                self.bits = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.data.push(self.accum << (8 - self.bits));
            self.bits = 0;
            self.accum = 0;
        }
        self.data
    }
}

// BitReader (decompression)

#[derive(Debug)]
pub(crate) struct BitReader {
    data: Vec<u8>,
    byte_pos: usize,
    bit_pos: u8, // bits consumed in current byte (0-7), MSB first
}

impl BitReader {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    pub fn read(&mut self, num_bits: u8) -> ChdResult<u32> {
        let mut result = 0u32;
        for _ in 0..num_bits {
            if self.byte_pos >= self.data.len() {
                return Err(ChdError::MapDecompressionError);
            }
            let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
            result = (result << 1) | bit as u32;
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }
        Ok(result)
    }

    fn position(&self) -> (usize, u8) {
        (self.byte_pos, self.bit_pos)
    }

    fn set_position(&mut self, pos: (usize, u8)) {
        self.byte_pos = pos.0;
        self.bit_pos = pos.1;
    }
}

// HuffNode

#[derive(Debug, Clone, Copy)]
struct HuffNode {
    parent: Option<usize>,
    weight: u32,
    bits: u32,
    num_bits: u8,
}

// HuffmanEncoder (compression)

#[derive(Debug)]
struct HuffmanEncoder {
    datahisto: [u32; HUFFMAN_CODES],
    nodes: Vec<HuffNode>,
}

impl HuffmanEncoder {
    fn new() -> Self {
        Self {
            datahisto: [0u32; HUFFMAN_CODES],
            nodes: vec![
                HuffNode {
                    parent: None,
                    weight: 0,
                    bits: 0,
                    num_bits: 0,
                };
                HUFFMAN_CODES * 2
            ],
        }
    }

    fn histo_one(&mut self, data: u8) {
        self.datahisto[data as usize] += 1;
    }

    fn compute_tree_from_histo(&mut self) -> ChdResult<()> {
        let totaldata = self.datahisto.iter().copied().sum::<u32>();
        if totaldata == 0 {
            self.nodes[0].num_bits = 1;
            self.nodes[0].bits = 0;
            return Ok(());
        }

        let mut lowerweight = 0u32;
        let mut upperweight = totaldata.saturating_mul(2);
        loop {
            let curweight = (upperweight + lowerweight) / 2;
            let curmaxbits = self.build_tree(totaldata, curweight);
            if curmaxbits <= HUFFMAN_MAX_BITS {
                lowerweight = curweight;
                if curweight == totaldata || upperweight.saturating_sub(lowerweight) <= 1 {
                    break;
                }
            } else {
                upperweight = curweight;
            }
        }

        self.assign_canonical_codes()
    }

    fn build_tree(&mut self, totaldata: u32, totalweight: u32) -> u8 {
        for node in &mut self.nodes {
            *node = HuffNode {
                parent: None,
                weight: 0,
                bits: 0,
                num_bits: 0,
            };
        }

        let mut list: Vec<usize> = Vec::with_capacity(HUFFMAN_CODES * 2);
        for code in 0..HUFFMAN_CODES {
            let count = self.datahisto[code];
            if count != 0 {
                let mut weight = (count as u64 * totalweight as u64) / totaldata as u64;
                if weight == 0 {
                    weight = 1;
                }
                self.nodes[code].weight = weight as u32;
                self.nodes[code].bits = code as u32;
                list.push(code);
            }
        }

        list.sort_by(|&a, &b| {
            let wa = self.nodes[a].weight;
            let wb = self.nodes[b].weight;
            if wa != wb {
                wb.cmp(&wa)
            } else {
                self.nodes[a].bits.cmp(&self.nodes[b].bits)
            }
        });

        let mut nextalloc = HUFFMAN_CODES;
        while list.len() > 1 {
            let node1 = list.pop().unwrap();
            let node0 = list.pop().unwrap();

            let new_index = nextalloc;
            nextalloc += 1;
            self.nodes[new_index].weight = self.nodes[node0].weight + self.nodes[node1].weight;
            self.nodes[node0].parent = Some(new_index);
            self.nodes[node1].parent = Some(new_index);

            let insert_pos = list
                .iter()
                .position(|&idx| self.nodes[new_index].weight > self.nodes[idx].weight)
                .unwrap_or(list.len());
            list.insert(insert_pos, new_index);
        }

        let mut maxbits = 0u8;
        for code in 0..HUFFMAN_CODES {
            if self.nodes[code].weight == 0 {
                continue;
            }
            let mut bits = 0u8;
            let mut current = Some(code);
            while let Some(idx) = current {
                if let Some(parent) = self.nodes[idx].parent {
                    bits += 1;
                    current = Some(parent);
                } else {
                    break;
                }
            }
            if bits == 0 {
                bits = 1;
            }
            self.nodes[code].num_bits = bits;
            maxbits = maxbits.max(bits);
        }

        maxbits
    }

    fn assign_canonical_codes(&mut self) -> ChdResult<()> {
        let mut bithisto = [0u32; BITHISTO_LEN];
        for code in 0..HUFFMAN_CODES {
            let bits = self.nodes[code].num_bits as usize;
            if bits > HUFFMAN_MAX_BITS as usize {
                return Err(ChdError::MapCompressionError);
            }
            if bits <= CANONICAL_MAX_BITS {
                bithisto[bits] += 1;
            }
        }

        let mut curstart = 0u32;
        for codelen in (1..=CANONICAL_MAX_BITS).rev() {
            let nextstart = (curstart + bithisto[codelen]) >> 1;
            if codelen != 1 && nextstart * 2 != curstart + bithisto[codelen] {
                return Err(ChdError::MapCompressionError);
            }
            bithisto[codelen] = curstart;
            curstart = nextstart;
        }

        for code in 0..HUFFMAN_CODES {
            let bits = self.nodes[code].num_bits as usize;
            if bits > 0 {
                self.nodes[code].bits = bithisto[bits];
                bithisto[bits] += 1;
            }
        }

        Ok(())
    }

    fn export_tree_rle(&self, bitbuf: &mut BitWriter) -> ChdResult<()> {
        let num_bits = if HUFFMAN_MAX_BITS >= 16 {
            5
        } else if HUFFMAN_MAX_BITS >= 8 {
            4
        } else {
            3
        };

        let mut lastval = i32::MIN;
        let mut repcount = 0u32;
        for code in 0..HUFFMAN_CODES {
            let newval = self.nodes[code].num_bits as i32;
            if newval == lastval {
                repcount += 1;
            } else {
                if repcount != 0 {
                    write_rle_tree_bits(bitbuf, lastval as u32, repcount, num_bits);
                }
                lastval = newval;
                repcount = 1;
            }
        }
        write_rle_tree_bits(bitbuf, lastval as u32, repcount, num_bits);
        Ok(())
    }

    fn encode_one(&self, bitbuf: &mut BitWriter, data: u8) {
        let node = self.nodes[data as usize];
        bitbuf.write(node.bits, node.num_bits);
    }
}

fn write_rle_tree_bits(bitbuf: &mut BitWriter, value: u32, mut repcount: u32, num_bits: u8) {
    while repcount > 0 {
        if value == 1 {
            bitbuf.write(1, num_bits);
            bitbuf.write(1, num_bits);
            repcount -= 1;
        } else if repcount <= 2 {
            bitbuf.write(value, num_bits);
            repcount -= 1;
        } else {
            let cur_reps = cmp::min(repcount - 3, (1u32 << num_bits) - 1);
            bitbuf.write(1, num_bits);
            bitbuf.write(value, num_bits);
            bitbuf.write(cur_reps, num_bits);
            repcount -= cur_reps + 3;
        }
    }
}

// HuffmanDecoder (decompression)

#[derive(Debug)]
pub(crate) struct HuffmanDecoder {
    /// Lookup table indexed by HUFFMAN_MAX_BITS-width value: (symbol, num_bits)
    lookup: Vec<(u8, u8)>,
}

impl HuffmanDecoder {
    /// Import Huffman tree from RLE-encoded bit-lengths (reverse of HuffmanEncoder::export_tree_rle)
    pub fn import_tree_rle(bits: &mut BitReader) -> ChdResult<Self> {
        let num_bits: u8 = if HUFFMAN_MAX_BITS >= 16 {
            5
        } else if HUFFMAN_MAX_BITS >= 8 {
            4
        } else {
            3
        };

        let mut bit_lengths = [0u8; HUFFMAN_CODES];
        let mut idx = 0;

        while idx < HUFFMAN_CODES {
            let v = bits.read(num_bits)? as u8;
            if v == 1 {
                // Could be literal value 1 (encoded as pair of 1s) or RLE marker
                let next = bits.read(num_bits)? as u8;
                if next == 1 {
                    // Literal value 1 — one occurrence
                    if idx < HUFFMAN_CODES {
                        bit_lengths[idx] = 1;
                        idx += 1;
                    }
                } else {
                    // RLE: the repeated value is `next`, count is read(num_bits)+3
                    let count = bits.read(num_bits)? as usize + 3;
                    for _ in 0..count {
                        if idx >= HUFFMAN_CODES {
                            break;
                        }
                        bit_lengths[idx] = next;
                        idx += 1;
                    }
                }
            } else {
                // Literal value (one occurrence)
                bit_lengths[idx] = v;
                idx += 1;
            }
        }

        Self::from_bit_lengths(&bit_lengths)
    }

    fn from_bit_lengths(bit_lengths: &[u8; HUFFMAN_CODES]) -> ChdResult<Self> {
        // Step 1: Build canonical codes (same algorithm as assign_canonical_codes in encoder)
        let mut bithisto = [0u32; BITHISTO_LEN];
        for &bits in bit_lengths.iter() {
            if (bits as usize) <= CANONICAL_MAX_BITS {
                bithisto[bits as usize] += 1;
            }
        }

        let mut curstart = 0u32;
        for codelen in (1..=CANONICAL_MAX_BITS).rev() {
            let nextstart = (curstart + bithisto[codelen]) >> 1;
            bithisto[codelen] = curstart;
            curstart = nextstart;
        }

        // Step 2: Assign codes to symbols
        let mut codes = [(0u32, 0u8); HUFFMAN_CODES]; // (code, num_bits)
        for symbol in 0..HUFFMAN_CODES {
            let bits = bit_lengths[symbol];
            if bits > 0 {
                codes[symbol] = (bithisto[bits as usize], bits);
                bithisto[bits as usize] += 1;
            }
        }

        // Step 3: Build lookup table (indexed by HUFFMAN_MAX_BITS-width value)
        let table_size = 1usize << HUFFMAN_MAX_BITS;
        let mut lookup = vec![(0u8, 0u8); table_size];

        for (symbol, &(code, num_bits)) in codes.iter().enumerate() {
            if num_bits == 0 {
                continue;
            }
            // Fill all lookup entries where the top `num_bits` bits match `code`
            let shift = HUFFMAN_MAX_BITS - num_bits;
            let base_index = (code as usize) << shift;
            let count = 1usize << shift;
            for i in 0..count {
                lookup[base_index + i] = (symbol as u8, num_bits);
            }
        }

        Ok(Self { lookup })
    }

    pub fn decode_one(&self, bits: &mut BitReader) -> ChdResult<u8> {
        let pos = bits.position();
        let value = bits.read(HUFFMAN_MAX_BITS)?;
        let (symbol, num_bits) = self.lookup[value as usize];
        if num_bits == 0 {
            return Err(ChdError::MapDecompressionError);
        }
        // Restore position and advance only num_bits
        bits.set_position(pos);
        bits.read(num_bits)?;
        Ok(symbol)
    }
}

pub(crate) fn decompress_v5_map(
    map_data: &[u8],
    hunk_count: u32,
    hunk_bytes: u32,
    unit_bytes: u32,
) -> ChdResult<Vec<MapEntry>> {
    if map_data.len() < MAP_HEADER_SIZE {
        return Err(ChdError::MapDecompressionError);
    }

    // Parse 16-byte header
    let compressed_len = BigEndian::read_u32(&map_data[0..4]) as usize;
    let first_offset = read_u48_be(&map_data[4..10]);
    let map_crc = BigEndian::read_u16(&map_data[10..12]);
    let length_bits = map_data[12];
    let self_bits = map_data[13];
    let parent_bits = map_data[14];

    if MAP_HEADER_SIZE + compressed_len > map_data.len() {
        return Err(ChdError::MapDecompressionError);
    }

    let compressed = map_data[MAP_HEADER_SIZE..MAP_HEADER_SIZE + compressed_len].to_vec();
    let mut bits = BitReader::new(compressed);

    // Import Huffman tree
    let decoder = HuffmanDecoder::import_tree_rle(&mut bits)?;

    // Decode all Huffman symbols (compression types with RLE)
    // Decode compression types using the same logic as the encoder's pass 2.
    // The encoder's pass 2 reads compression_rle[] sequentially:
    //   - if count==0: read next symbol. RLE_SMALL/LARGE set count. Other values set lastcomp.
    //   - if count>0: decrement count.
    // In both cases, lastcomp is used for the current hunk.
    let mut compression_types = Vec::with_capacity(hunk_count as usize);
    {
        let mut count = 0u32;
        let mut lastcomp = 0u8;

        for _ in 0..hunk_count {
            if count > 0 {
                count -= 1;
            } else {
                let val = decoder.decode_one(&mut bits)?;
                if val == COMPRESSION_RLE_SMALL {
                    let extra = decoder.decode_one(&mut bits)? as u32;
                    count = RLE_SMALL_DECODE_BASE + extra;
                } else if val == COMPRESSION_RLE_LARGE {
                    let high = decoder.decode_one(&mut bits)? as u32;
                    let low = decoder.decode_one(&mut bits)? as u32;
                    count = RLE_LARGE_DECODE_BASE + (high << 4) + low;
                } else {
                    lastcomp = val;
                }
            }
            compression_types.push(lastcomp);
        }
    }

    // Decode per-hunk data from the bitstream
    let mut entries = Vec::with_capacity(hunk_count as usize);
    let mut cur_offset = first_offset;
    let mut last_self = 0u32;
    let mut last_parent = 0u64;

    for (hunknum, &comp) in compression_types.iter().enumerate() {
        let entry = match comp {
            COMPRESSION_TYPE_0 | COMPRESSION_TYPE_1 | COMPRESSION_TYPE_2 | COMPRESSION_TYPE_3 => {
                let length = bits.read(length_bits)?;
                let crc16 = bits.read(16)? as u16;
                let offset = cur_offset;
                cur_offset += length as u64;
                MapEntry {
                    compression: comp,
                    length,
                    offset,
                    crc16,
                }
            }
            COMPRESSION_NONE => {
                let crc16 = bits.read(16)? as u16;
                let offset = cur_offset;
                cur_offset += hunk_bytes as u64;
                MapEntry {
                    compression: comp,
                    length: hunk_bytes,
                    offset,
                    crc16,
                }
            }
            COMPRESSION_SELF => {
                let ref_hunk = bits.read(self_bits)?;
                last_self = ref_hunk;
                MapEntry {
                    compression: comp,
                    length: 0,
                    offset: ref_hunk as u64,
                    crc16: 0,
                }
            }
            COMPRESSION_SELF_0 => MapEntry {
                compression: COMPRESSION_SELF,
                length: 0,
                offset: last_self as u64,
                crc16: 0,
            },
            COMPRESSION_SELF_1 => {
                last_self += 1;
                MapEntry {
                    compression: COMPRESSION_SELF,
                    length: 0,
                    offset: last_self as u64,
                    crc16: 0,
                }
            }
            COMPRESSION_PARENT => {
                let ref_unit = bits.read(parent_bits)?;
                last_parent = ref_unit as u64;
                MapEntry {
                    compression: comp,
                    length: 0,
                    offset: ref_unit as u64,
                    crc16: 0,
                }
            }
            COMPRESSION_PARENT_SELF => {
                let self_unit = (hunknum as u64 * hunk_bytes as u64) / unit_bytes as u64;
                last_parent = self_unit;
                MapEntry {
                    compression: COMPRESSION_PARENT,
                    length: 0,
                    offset: self_unit,
                    crc16: 0,
                }
            }
            COMPRESSION_PARENT_0 => MapEntry {
                compression: COMPRESSION_PARENT,
                length: 0,
                offset: last_parent,
                crc16: 0,
            },
            COMPRESSION_PARENT_1 => {
                last_parent += (hunk_bytes / unit_bytes) as u64;
                MapEntry {
                    compression: COMPRESSION_PARENT,
                    length: 0,
                    offset: last_parent,
                    crc16: 0,
                }
            }
            _ => return Err(ChdError::MapDecompressionError),
        };
        entries.push(entry);
    }

    // Verify CRC: rebuild raw map and check
    let raw_map = encode_raw_map(&entries);
    let computed_crc = crc16_ccitt(&raw_map);
    if computed_crc != map_crc {
        return Err(ChdError::MapDecompressionError);
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_roundtrip() {
        // Create test map entries with cumulative offsets (as in a real CHD)
        let mut entries: Vec<MapEntry> = Vec::new();
        let mut cur_offset = 229u64;
        for i in 0..100u32 {
            let length = 5000 + i * 10;
            entries.push(MapEntry {
                compression: 0, // COMPRESSION_TYPE_0
                length,
                offset: cur_offset,
                crc16: (i * 7) as u16,
            });
            cur_offset += length as u64;
        }

        let hunk_bytes = 19584u32;
        let unit_bytes = 2448u32;

        // Compress
        let compressed = compress_v5_map(&entries, hunk_bytes, unit_bytes).unwrap();

        // Decompress
        let decompressed = decompress_v5_map(&compressed, 100, hunk_bytes, unit_bytes).unwrap();

        // Compare
        assert_eq!(entries.len(), decompressed.len());
        for (i, (orig, dec)) in entries.iter().zip(decompressed.iter()).enumerate() {
            assert_eq!(
                orig.compression, dec.compression,
                "compression mismatch at hunk {i}"
            );
            assert_eq!(orig.length, dec.length, "length mismatch at hunk {i}");
            assert_eq!(orig.offset, dec.offset, "offset mismatch at hunk {i}");
            assert_eq!(orig.crc16, dec.crc16, "crc16 mismatch at hunk {i}");
        }
        println!("Roundtrip OK!");
    }
}
