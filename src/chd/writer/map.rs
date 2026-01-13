use crate::chd::error::{ChdError, ChdResult};
use crc::{Crc, CRC_16_IBM_3740};
use std::cmp;

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
const COMPRESSION_NONE: u8 = 4;
const COMPRESSION_SELF: u8 = 5;
const COMPRESSION_PARENT: u8 = 6;
const COMPRESSION_RLE_SMALL: u8 = 7;
const COMPRESSION_RLE_LARGE: u8 = 8;
const COMPRESSION_SELF_0: u8 = 9;
const COMPRESSION_SELF_1: u8 = 10;
const COMPRESSION_PARENT_SELF: u8 = 11;
const COMPRESSION_PARENT_0: u8 = 12;
const COMPRESSION_PARENT_1: u8 = 13;

const HUFFMAN_CODES: usize = 16;
const HUFFMAN_MAX_BITS: u8 = 8;

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
        let base = (hunknum as usize) * 12;
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
            max_complen = max_complen.max(read_u24_be(&rawmap[base + 1..base + 4]));
        }

        if curcomp == lastcomp {
            count += 1;
        }

        if curcomp != lastcomp || hunknum == hunk_count - 1 {
            while count != 0 {
                if count < 3 {
                    push_symbol(&mut compression_rle, &mut encoder, lastcomp);
                    count -= 1;
                } else if count <= 3 + 15 {
                    push_symbol(&mut compression_rle, &mut encoder, COMPRESSION_RLE_SMALL);
                    push_symbol(&mut compression_rle, &mut encoder, (count - 3) as u8);
                    count = 0;
                } else {
                    let this_count = cmp::min(count, 3 + 16 + 255);
                    let rem = this_count - 3 - 16;
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
        let base = (hunknum as usize) * 12;
        let length = read_u24_be(&rawmap[base + 1..base + 4]);
        let offset = read_u48_be(&rawmap[base + 4..base + 10]);
        let crc = read_u16_be(&rawmap[base + 10..base + 12]);

        if count == 0 {
            let val = compression_rle[src_index];
            src_index += 1;
            if val == COMPRESSION_RLE_SMALL {
                count = 2 + compression_rle[src_index] as u32;
                src_index += 1;
            } else if val == COMPRESSION_RLE_LARGE {
                let high = compression_rle[src_index] as u32;
                src_index += 1;
                let low = compression_rle[src_index] as u32;
                src_index += 1;
                count = 2 + 16 + (high << 4) + low;
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
    let mut output = Vec::with_capacity(16 + compressed.len());
    output.extend_from_slice(&[0u8; 16]);
    output.extend_from_slice(&compressed);

    write_u32_be(&mut output[0..4], compressed.len() as u32);
    write_u48_be(&mut output[4..10], firstoffs);
    write_u16_be(&mut output[10..12], mapcrc);
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
    let mut raw = vec![0u8; entries.len() * 12];
    for (idx, entry) in entries.iter().enumerate() {
        let base = idx * 12;
        raw[base] = entry.compression;
        write_u24_be(&mut raw[base + 1..base + 4], entry.length);
        write_u48_be(&mut raw[base + 4..base + 10], entry.offset);
        write_u16_be(&mut raw[base + 10..base + 12], entry.crc16);
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

fn read_u16_be(buf: &[u8]) -> u16 {
    ((buf[0] as u16) << 8) | (buf[1] as u16)
}

fn write_u16_be(buf: &mut [u8], value: u16) {
    buf[0] = (value >> 8) as u8;
    buf[1] = value as u8;
}

fn write_u32_be(buf: &mut [u8], value: u32) {
    buf[0] = (value >> 24) as u8;
    buf[1] = (value >> 16) as u8;
    buf[2] = (value >> 8) as u8;
    buf[3] = value as u8;
}

fn read_u24_be(buf: &[u8]) -> u32 {
    ((buf[0] as u32) << 16) | ((buf[1] as u32) << 8) | (buf[2] as u32)
}

fn write_u24_be(buf: &mut [u8], value: u32) {
    let value = value & 0x00ff_ffff;
    buf[0] = (value >> 16) as u8;
    buf[1] = (value >> 8) as u8;
    buf[2] = value as u8;
}

fn read_u48_be(buf: &[u8]) -> u64 {
    ((buf[0] as u64) << 40)
        | ((buf[1] as u64) << 32)
        | ((buf[2] as u64) << 24)
        | ((buf[3] as u64) << 16)
        | ((buf[4] as u64) << 8)
        | (buf[5] as u64)
}

fn write_u48_be(buf: &mut [u8], value: u64) {
    buf[0] = (value >> 40) as u8;
    buf[1] = (value >> 32) as u8;
    buf[2] = (value >> 24) as u8;
    buf[3] = (value >> 16) as u8;
    buf[4] = (value >> 8) as u8;
    buf[5] = value as u8;
}

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

    fn write(&mut self, value: u32, numbits: u8) {
        if numbits == 0 {
            return;
        }

        for i in (0..numbits).rev() {
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

#[derive(Clone, Copy)]
struct HuffNode {
    parent: Option<usize>,
    weight: u32,
    bits: u32,
    numbits: u8,
}

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
                    numbits: 0,
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
            self.nodes[0].numbits = 1;
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
                numbits: 0,
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
            self.nodes[new_index].weight =
                self.nodes[node0].weight + self.nodes[node1].weight;
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
            self.nodes[code].numbits = bits;
            maxbits = maxbits.max(bits);
        }

        maxbits
    }

    fn assign_canonical_codes(&mut self) -> ChdResult<()> {
        let mut bithisto = [0u32; 33];
        for code in 0..HUFFMAN_CODES {
            let bits = self.nodes[code].numbits as usize;
            if bits > HUFFMAN_MAX_BITS as usize {
                return Err(ChdError::MapCompressionError);
            }
            if bits <= 32 {
                bithisto[bits] += 1;
            }
        }

        let mut curstart = 0u32;
        for codelen in (1..=32).rev() {
            let nextstart = (curstart + bithisto[codelen]) >> 1;
            if codelen != 1 && nextstart * 2 != curstart + bithisto[codelen] {
                return Err(ChdError::MapCompressionError);
            }
            bithisto[codelen] = curstart;
            curstart = nextstart;
        }

        for code in 0..HUFFMAN_CODES {
            let bits = self.nodes[code].numbits as usize;
            if bits > 0 {
                self.nodes[code].bits = bithisto[bits];
                bithisto[bits] += 1;
            }
        }

        Ok(())
    }

    fn export_tree_rle(&self, bitbuf: &mut BitWriter) -> ChdResult<()> {
        let numbits = if HUFFMAN_MAX_BITS >= 16 {
            5
        } else if HUFFMAN_MAX_BITS >= 8 {
            4
        } else {
            3
        };

        let mut lastval = i32::MIN;
        let mut repcount = 0u32;
        for code in 0..HUFFMAN_CODES {
            let newval = self.nodes[code].numbits as i32;
            if newval == lastval {
                repcount += 1;
            } else {
                if repcount != 0 {
                    write_rle_tree_bits(bitbuf, lastval as u32, repcount, numbits);
                }
                lastval = newval;
                repcount = 1;
            }
        }
        write_rle_tree_bits(bitbuf, lastval as u32, repcount, numbits);
        Ok(())
    }

    fn encode_one(&self, bitbuf: &mut BitWriter, data: u8) {
        let node = self.nodes[data as usize];
        bitbuf.write(node.bits, node.numbits);
    }
}

fn write_rle_tree_bits(bitbuf: &mut BitWriter, value: u32, mut repcount: u32, numbits: u8) {
    while repcount > 0 {
        if value == 1 {
            bitbuf.write(1, numbits);
            bitbuf.write(1, numbits);
            repcount -= 1;
        } else if repcount <= 2 {
            bitbuf.write(value, numbits);
            repcount -= 1;
        } else {
            let cur_reps = cmp::min(repcount - 3, (1u32 << numbits) - 1);
            bitbuf.write(1, numbits);
            bitbuf.write(value, numbits);
            bitbuf.write(cur_reps, numbits);
            repcount -= cur_reps + 3;
        }
    }
}
