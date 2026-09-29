//! XEX2 basefile recovery: AES-128-CBC with an all-zero IV, then the basic
//! or LZX decompression path (xenia `xex_module.cc`).

use lzxd::{Lzxd, WindowSize};
use sha1::{Digest, Sha1};
use std::io::{Read, Seek, SeekFrom};

use super::{Compression, FileFormatInfo, SecurityInfo, read_u32};
use crate::microsoft::read_extent_at;
use crate::util::aes::aes128_cbc_decrypt_nopad;

/// The 360 is end of life and this key is public, so metadata reads work
/// without the user supplying anything.
pub(super) const RETAIL_KEY: [u8; 16] = [
    0x20, 0xB1, 0x85, 0xA5, 0x9D, 0x28, 0xFD, 0xC3, 0x40, 0x58, 0x3F, 0xBB, 0x08, 0x96, 0xBF, 0x91,
];
const DEVKIT_KEY: [u8; 16] = [0u8; 16];

const ENCRYPTION_NONE: u16 = 0;
const ENCRYPTION_NORMAL: u16 = 1;

const NORMAL_BLOCK_MEMORY_LIMIT: u64 = 16 * 1024 * 1024;
const BLOCK_INFO_LEN: usize = 24;
const LZX_CHUNK: usize = 32768;

fn cbc_decrypt_zero_iv(key: &[u8; 16], buf: &mut [u8]) -> Option<()> {
    aes128_cbc_decrypt_nopad(key, &[0u8; 16], buf).ok()
}
fn session_key(base_key: &[u8; 16], encrypted: &[u8; 16]) -> Option<[u8; 16]> {
    let mut key = *encrypted;
    cbc_decrypt_zero_iv(base_key, &mut key)?;
    Some(key)
}
fn window_size(raw: u32) -> Option<WindowSize> {
    Some(match raw {
        0x0000_8000 => WindowSize::KB32,
        0x0001_0000 => WindowSize::KB64,
        0x0002_0000 => WindowSize::KB128,
        0x0004_0000 => WindowSize::KB256,
        0x0008_0000 => WindowSize::KB512,
        0x0010_0000 => WindowSize::MB1,
        0x0020_0000 => WindowSize::MB2,
        0x0040_0000 => WindowSize::MB4,
        0x0080_0000 => WindowSize::MB8,
        0x0100_0000 => WindowSize::MB16,
        0x0200_0000 => WindowSize::MB32,
        _ => return None,
    })
}

/// Stored-source geometry and cipher state for one PE image: where it sits
/// in the file, how much of it is physically stored, its declared logical
/// size, and how to decrypt it.
pub(crate) struct ResourceSource {
    base: u64,
    stored_len: u64,
    image_size: u32,
    key: [u8; 16],
    encrypted: bool,
}

impl ResourceSource {
    /// Derives the retail session key and cipher selection from the file
    /// format and security headers.
    pub(crate) fn new(
        base: u64,
        stored_len: u64,
        fmt: &FileFormatInfo,
        security: &SecurityInfo,
    ) -> Option<Self> {
        let encrypted = match fmt.encryption_type {
            ENCRYPTION_NONE => false,
            ENCRYPTION_NORMAL => true,
            _ => return None,
        };
        Some(Self {
            base,
            stored_len,
            image_size: security.image_size,
            key: session_key(&RETAIL_KEY, &security.aes_key)?,
            encrypted,
        })
    }
}

/// Dispatches resource reads while reusing compression state across ranges.
pub(crate) struct ResourceReader {
    normal: Option<Option<NormalResourceReader>>,
    basic: Option<Option<BasicResourceReader>>,
}
impl ResourceReader {
    pub(crate) fn new() -> Self {
        Self {
            normal: None,
            basic: None,
        }
    }

    pub(crate) fn read_resource_at<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        source: &ResourceSource,
        fmt: &FileFormatInfo,
        security: &SecurityInfo,
        resource_start: u64,
        resource_len: usize,
    ) -> Option<Vec<u8>> {
        let resource_end = resource_start.checked_add(resource_len as u64)?;
        if resource_end > u64::from(source.image_size) {
            return None;
        }
        match &fmt.compression {
            Compression::None => read_plain_resource(reader, source, resource_start, resource_len),
            Compression::BasicAt {
                table_offset,
                count,
            } => {
                if self.basic.is_none() {
                    self.basic = Some(BasicResourceReader::new(
                        reader,
                        *table_offset,
                        *count,
                        source.stored_len,
                    ));
                }
                self.basic.as_mut()?.as_mut()?.read_range(
                    reader,
                    source,
                    resource_start,
                    resource_len,
                )
            }
            Compression::Normal { .. } => {
                if self.normal.is_none() {
                    self.normal = Some(NormalResourceReader::from_xex(fmt, security));
                }
                self.normal.as_mut()?.as_mut()?.read_range(
                    reader,
                    source.base,
                    source.stored_len,
                    resource_start,
                    resource_len,
                )
            }
        }
    }
}

#[cfg(test)]
fn read_resource_at<R: Read + Seek>(
    reader: &mut R,
    source: &ResourceSource,
    fmt: &FileFormatInfo,
    security: &SecurityInfo,
    resource_start: u64,
    resource_len: usize,
) -> Option<Vec<u8>> {
    ResourceReader::new().read_resource_at(
        reader,
        source,
        fmt,
        security,
        resource_start,
        resource_len,
    )
}

fn read_plain_resource<R: Read + Seek>(
    reader: &mut R,
    source: &ResourceSource,
    start: u64,
    length: usize,
) -> Option<Vec<u8>> {
    let end = start.checked_add(length as u64)?;
    let stored_end = end.min(source.stored_len);
    let mut out = vec![0u8; length];
    if start < stored_end {
        let bytes = read_stored_range(reader, source, start, (stored_end - start) as usize)?;
        out[..bytes.len()].copy_from_slice(&bytes);
    }
    Some(out)
}

pub(crate) struct BasicResourceReader {
    descriptors: Vec<(u64, u64, u64)>,
}

impl BasicResourceReader {
    pub(crate) fn new<R: Read + Seek>(
        reader: &mut R,
        table_offset: u64,
        count: u64,
        stored_len: u64,
    ) -> Option<Self> {
        let capacity = usize::try_from(count).ok()?.min(4096);
        let mut descriptors = Vec::with_capacity(capacity);
        let mut total_stored = 0u64;
        let mut logical_offset = 0u64;
        let mut index = 0u64;
        while index < count {
            let batch_count = (count - index).min(512);
            let at = index.checked_mul(8)?;
            let batch_len = usize::try_from(batch_count.checked_mul(8)?).ok()?;
            let pairs = read_extent_at(reader, table_offset, count.checked_mul(8)?, at, batch_len)?;
            for pair in pairs.chunks_exact(8) {
                let data_size = u64::from(read_u32(pair, 0)?);
                let zero_size = u64::from(read_u32(pair, 4)?);
                descriptors.push((logical_offset, total_stored, data_size));
                total_stored = total_stored.checked_add(data_size)?;
                logical_offset = logical_offset
                    .checked_add(data_size)?
                    .checked_add(zero_size)?;
                if total_stored > stored_len {
                    return None;
                }
            }
            index += batch_count;
        }
        Some(Self { descriptors })
    }

    pub(crate) fn read_range<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        source: &ResourceSource,
        resource_start: u64,
        resource_len: usize,
    ) -> Option<Vec<u8>> {
        let resource_end = resource_start.checked_add(resource_len as u64)?;
        let mut output = vec![0u8; resource_len];
        let mut index = self
            .descriptors
            .partition_point(|&(logical_offset, _, data_size)| {
                logical_offset + data_size <= resource_start
            });
        while let Some(&(logical_offset, stored_offset, data_size)) = self.descriptors.get(index) {
            if logical_offset >= resource_end {
                break;
            }
            let data_end = logical_offset.checked_add(data_size)?;
            let overlap_start = resource_start.max(logical_offset);
            let overlap_end = resource_end.min(data_end);
            if overlap_start < overlap_end {
                let range_start = stored_offset.checked_add(overlap_start - logical_offset)?;
                let bytes = read_stored_range(
                    reader,
                    source,
                    range_start,
                    usize::try_from(overlap_end - overlap_start).ok()?,
                )?;
                let out_start = usize::try_from(overlap_start - resource_start).ok()?;
                output[out_start..out_start + bytes.len()].copy_from_slice(&bytes);
            }
            index += 1;
        }
        Some(output)
    }
}

/// Decrypts a content-relative CBC range through [`CbcStream`], so the
/// predecessor-IV and partial-final-block alignment live in one place.
fn read_stored_range<R: Read + Seek>(
    reader: &mut R,
    source: &ResourceSource,
    offset: u64,
    length: usize,
) -> Option<Vec<u8>> {
    let end = offset.checked_add(length as u64)?;
    if end > source.stored_len {
        return None;
    }
    if !source.encrypted {
        return read_extent_at(reader, source.base, source.stored_len, offset, length);
    }
    // Cap the stream at this range's block-aligned end so one bounded read
    // never pulls the whole remaining stored region into the buffer.
    let capped_len = (end.checked_add(15)? / 16 * 16).min(source.stored_len);
    let mut stream = CbcStream::new();
    stream.reset(
        reader,
        source.base,
        capped_len,
        offset,
        &source.key,
        source.encrypted,
    )?;
    let mut out = vec![0u8; length];
    stream.read_exact(reader, &mut out)?;
    Some(out)
}

pub(crate) struct NormalResourceReader {
    basefile_size: usize,
    first_block_size: u32,
    first_block_hash: [u8; 20],
    block_size: u32,
    block_hash: [u8; 20],
    stored_offset: u64,
    block_cursor: u64,
    next_block_size: u32,
    next_block_hash: [u8; 20],
    block_loaded: bool,
    logical_offset: usize,
    frame_start: usize,
    frame: Vec<u8>,
    decoder: Lzxd,
    key: [u8; 16],
    devkit_key: [u8; 16],
    encrypted: bool,
    devkit_active: bool,
    stream: CbcStream,
    block_data: Vec<u8>,
    block_in_source: bool,
    finished_blocks: bool,
    hash_buffer: Vec<u8>,
    chunk_buffer: Vec<u8>,
}

impl NormalResourceReader {
    fn new(
        window: WindowSize,
        first_block_size: u32,
        first_block_hash: [u8; 20],
        image_size: usize,
        retail_key: [u8; 16],
        devkit_key: [u8; 16],
        encrypted: bool,
    ) -> Option<Self> {
        Some(Self {
            basefile_size: image_size,
            first_block_size,
            first_block_hash,
            block_size: first_block_size,
            block_hash: first_block_hash,
            stored_offset: 0,
            block_cursor: 0,
            next_block_size: 0,
            next_block_hash: [0; 20],
            block_loaded: false,
            logical_offset: 0,
            frame_start: 0,
            frame: Vec::new(),
            decoder: Lzxd::new(window),
            key: retail_key,
            devkit_key,
            encrypted,
            devkit_active: false,
            stream: CbcStream::new(),
            block_data: Vec::new(),
            block_in_source: false,
            finished_blocks: false,
            hash_buffer: vec![0; 64 * 1024],
            chunk_buffer: vec![0; u16::MAX as usize],
        })
    }

    pub(crate) fn from_xex(fmt: &FileFormatInfo, security: &SecurityInfo) -> Option<Self> {
        let Compression::Normal {
            window_size: raw_window,
            first_block_size,
            first_block_hash,
        } = &fmt.compression
        else {
            return None;
        };
        let window = window_size(*raw_window)?;
        let encrypted = match fmt.encryption_type {
            ENCRYPTION_NONE => false,
            ENCRYPTION_NORMAL => true,
            _ => return None,
        };
        let retail_key = session_key(&RETAIL_KEY, &security.aes_key)?;
        let devkit_key = session_key(&DEVKIT_KEY, &security.aes_key)?;
        Self::new(
            window,
            *first_block_size,
            *first_block_hash,
            usize::try_from(security.image_size).ok()?,
            retail_key,
            devkit_key,
            encrypted,
        )
    }

    fn reset(&mut self) {
        self.block_size = self.first_block_size;
        self.block_hash = self.first_block_hash;
        self.stored_offset = 0;
        self.block_cursor = 0;
        self.next_block_size = 0;
        self.next_block_hash = [0; 20];
        self.block_loaded = false;
        self.logical_offset = 0;
        self.block_data.clear();
        self.block_in_source = false;
        self.finished_blocks = false;
        self.frame_start = 0;
        self.frame.clear();
        self.decoder.reset();
        self.stream.clear();
    }

    pub(crate) fn read_range<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        base: u64,
        stored_len: u64,
        start: u64,
        length: usize,
    ) -> Option<Vec<u8>> {
        if let Some(bytes) = self.read_range_with_key(reader, base, stored_len, start, length) {
            return Some(bytes);
        }
        if self.devkit_active {
            return None;
        }
        self.key = self.devkit_key;
        self.devkit_active = true;
        self.reset();
        self.read_range_with_key(reader, base, stored_len, start, length)
    }

    fn read_range_with_key<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        base: u64,
        stored_len: u64,
        start: u64,
        length: usize,
    ) -> Option<Vec<u8>> {
        let start = usize::try_from(start).ok()?;
        let end = start.checked_add(length)?;
        if end > self.basefile_size {
            return None;
        }
        if start < self.frame_start {
            self.reset();
        }
        let mut output = vec![0u8; length];
        let mut at = start;
        while at < end {
            while at < self.frame_start || at >= self.frame_start + self.frame.len() {
                self.next_frame(reader, base, stored_len)?;
            }
            let frame_end = self.frame_start + self.frame.len();
            let overlap_end = end.min(frame_end);
            let source_start = at - self.frame_start;
            let output_start = at - start;
            let count = overlap_end - at;
            output[output_start..output_start + count]
                .copy_from_slice(&self.frame[source_start..source_start + count]);
            at = overlap_end;
        }
        Some(output)
    }

    fn read_block_exact<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        output: &mut [u8],
    ) -> Option<()> {
        if self.block_in_source {
            self.stream.read_exact(reader, output)
        } else {
            let start = usize::try_from(self.block_cursor).ok()?;
            output.copy_from_slice(
                self.block_data
                    .get(start..start.checked_add(output.len())?)?,
            );
            Some(())
        }
    }

    /// Hashes a block straight off the stored stream. Blocks over the
    /// memory limit are deliberately read and decrypted twice: the hash
    /// must verify before the first frame is decoded, and verifying
    /// without buffering the block means a second pass.
    fn hash_source_block<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        base: u64,
        stored_len: u64,
        block_len: u64,
    ) -> Option<()> {
        self.stream.reset(
            reader,
            base,
            stored_len,
            self.stored_offset,
            &self.key,
            self.encrypted,
        )?;
        let mut hash = Sha1::new();
        let mut remaining = block_len;
        while remaining > 0 {
            let count = usize::try_from(remaining.min(self.hash_buffer.len() as u64)).ok()?;
            self.stream
                .read_exact(reader, &mut self.hash_buffer[..count])?;
            hash.update(&self.hash_buffer[..count]);
            remaining -= count as u64;
        }
        if hash.finalize().as_slice() != self.block_hash.as_slice() {
            return None;
        }
        // Already verified above; reposition to the block start for
        // consumption instead of re-hashing while reading chunks.
        self.stream.seek_to(reader, self.stored_offset)
    }

    fn finish_block(&mut self) -> Option<()> {
        let block_len = self.block_size as u64;
        self.block_cursor = block_len;
        self.stored_offset = self.stored_offset.checked_add(block_len)?;
        self.block_size = self.next_block_size;
        self.block_hash = self.next_block_hash;
        self.block_loaded = false;
        self.finished_blocks = self.block_size == 0;
        Some(())
    }

    fn next_frame<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        base: u64,
        stored_len: u64,
    ) -> Option<()> {
        if self.logical_offset >= self.basefile_size {
            return None;
        }
        loop {
            if self.finished_blocks {
                let frame_len = LZX_CHUNK.min(self.basefile_size - self.logical_offset);
                self.frame_start = self.logical_offset;
                self.frame.clear();
                self.frame.resize(frame_len, 0);
                self.logical_offset = self.logical_offset.checked_add(frame_len)?;
                return Some(());
            }
            if !self.block_loaded {
                let block_len = self.block_size as u64;
                if block_len < BLOCK_INFO_LEN as u64
                    || self.stored_offset.checked_add(block_len)? > stored_len
                {
                    return None;
                }
                self.block_data.clear();
                self.block_in_source = block_len > NORMAL_BLOCK_MEMORY_LIMIT;
                if self.block_in_source {
                    self.hash_source_block(reader, base, stored_len, block_len)?;
                } else {
                    self.stream.reset(
                        reader,
                        base,
                        stored_len,
                        self.stored_offset,
                        &self.key,
                        self.encrypted,
                    )?;
                    self.block_data
                        .reserve_exact(usize::try_from(block_len).ok()?);
                    let mut hash = Sha1::new();
                    let mut remaining = block_len;
                    while remaining > 0 {
                        let count =
                            usize::try_from(remaining.min(self.hash_buffer.len() as u64)).ok()?;
                        self.stream
                            .read_exact(reader, &mut self.hash_buffer[..count])?;
                        hash.update(&self.hash_buffer[..count]);
                        self.block_data
                            .extend_from_slice(&self.hash_buffer[..count]);
                        remaining -= count as u64;
                    }
                    if hash.finalize().as_slice() != self.block_hash.as_slice() {
                        return None;
                    }
                }
                self.block_cursor = 0;
                let mut info = [0u8; BLOCK_INFO_LEN];
                self.read_block_exact(reader, &mut info)?;
                self.next_block_size = read_u32(&info, 0)?;
                self.next_block_hash = info.get(4..BLOCK_INFO_LEN)?.try_into().ok()?;
                self.block_cursor = BLOCK_INFO_LEN as u64;
                self.block_loaded = true;
            }

            let block_len = self.block_size as u64;
            if self.block_cursor >= block_len {
                self.finish_block()?;
                continue;
            }
            let prefix_end = self.block_cursor.checked_add(2)?;
            if prefix_end > block_len {
                return None;
            }
            let mut prefix = [0u8; 2];
            self.read_block_exact(reader, &mut prefix)?;
            let chunk_size = u16::from_be_bytes(prefix) as usize;
            self.block_cursor = prefix_end;
            if chunk_size == 0 {
                self.finish_block()?;
                continue;
            }
            let chunk_end = self.block_cursor.checked_add(chunk_size as u64)?;
            if chunk_end > block_len {
                return None;
            }
            if self.block_in_source {
                self.stream
                    .read_exact(reader, &mut self.chunk_buffer[..chunk_size])?;
            } else {
                let block_start = usize::try_from(self.block_cursor).ok()?;
                let block_end = block_start.checked_add(chunk_size)?;
                self.chunk_buffer[..chunk_size]
                    .copy_from_slice(self.block_data.get(block_start..block_end)?);
            }
            self.block_cursor = chunk_end;
            let frame_len = LZX_CHUNK.min(self.basefile_size - self.logical_offset);
            let decoded = self
                .decoder
                .decompress_next(&self.chunk_buffer[..chunk_size], frame_len)
                .ok()?;
            // develop tolerated chunks that decode short of the frame and
            // zero-padded the rest of the image, so a short chunk fills its
            // frame with zeros instead of being rejected.
            self.frame_start = self.logical_offset;
            self.frame.clear();
            self.frame.resize(frame_len, 0);
            self.frame[..decoded.len()].copy_from_slice(decoded);
            self.logical_offset = self.logical_offset.checked_add(frame_len)?;
            return Some(());
        }
    }
}

struct CbcStream {
    position: u64,
    source_base: u64,
    cipher_offset: u64,
    iv: [u8; 16],
    key: [u8; 16],
    encrypted: bool,
    stored_len: u64,
    buffer: Vec<u8>,
    buffer_start: usize,
    buffer_end: usize,
}

impl CbcStream {
    fn new() -> Self {
        Self {
            position: 0,
            source_base: 0,
            cipher_offset: 0,
            iv: [0; 16],
            key: [0; 16],
            encrypted: false,
            stored_len: 0,
            buffer: Vec::new(),
            buffer_start: 0,
            buffer_end: 0,
        }
    }

    fn clear(&mut self) {
        self.position = 0;
        self.cipher_offset = 0;
        self.iv = [0; 16];
        self.buffer_start = 0;
        self.buffer_end = 0;
    }

    fn reset<R: Read + Seek>(
        &mut self,
        reader: &mut R,
        base: u64,
        stored_len: u64,
        offset: u64,
        key: &[u8; 16],
        encrypted: bool,
    ) -> Option<()> {
        if offset > stored_len {
            return None;
        }
        self.clear();
        self.source_base = base;
        self.key = *key;
        self.encrypted = encrypted;
        self.stored_len = stored_len;
        self.seek_to(reader, offset)
    }

    /// Repositions to `offset` on the current source/key without decrypting
    /// skipped ciphertext: CBC only needs the immediately preceding
    /// ciphertext block as IV, so this seeks directly and decrypts at most
    /// one alignment block of lead-in.
    fn seek_to<R: Read + Seek>(&mut self, reader: &mut R, offset: u64) -> Option<()> {
        if offset > self.stored_len {
            return None;
        }
        self.buffer_start = 0;
        self.buffer_end = 0;
        let aligned = if self.encrypted {
            offset / 16 * 16
        } else {
            offset
        };
        self.position = aligned;
        self.cipher_offset = aligned;
        if self.encrypted && aligned > 0 {
            self.iv.copy_from_slice(&read_extent_at(
                reader,
                self.source_base,
                self.stored_len,
                aligned - 16,
                16,
            )?);
        }
        reader
            .seek(SeekFrom::Start(self.source_base.checked_add(aligned)?))
            .ok()?;
        self.skip(reader, usize::try_from(offset - aligned).ok()?)
    }

    fn fill<R: Read + Seek>(&mut self, reader: &mut R) -> Option<bool> {
        if self.buffer_start < self.buffer_end {
            return Some(true);
        }
        let remaining = self.stored_len.checked_sub(self.cipher_offset)?;
        if remaining == 0 {
            return Some(false);
        }
        if self.encrypted && remaining < 16 {
            // A trailing partial CBC block cannot be decrypted; develop's
            // whole-buffer decrypt errored rather than emitting ciphertext
            // as plaintext.
            return None;
        }
        if self.buffer.is_empty() {
            self.buffer = vec![0; 64 * 1024];
        }
        self.buffer_start = 0;
        let mut count = usize::try_from(remaining.min(self.buffer.len() as u64)).ok()?;
        if self.encrypted && remaining >= 16 {
            count = count / 16 * 16;
        }
        reader
            .seek(SeekFrom::Start(
                self.source_base.checked_add(self.cipher_offset)?,
            ))
            .ok()?;
        reader.read_exact(&mut self.buffer[..count]).ok()?;
        if self.encrypted && count >= 16 && remaining >= 16 {
            let mut next_iv = [0u8; 16];
            next_iv.copy_from_slice(&self.buffer[count - 16..count]);
            aes128_cbc_decrypt_nopad(&self.key, &self.iv, &mut self.buffer[..count]).ok()?;
            self.iv = next_iv;
        }
        self.buffer_end = count;
        self.cipher_offset = self.cipher_offset.checked_add(count as u64)?;
        Some(true)
    }

    fn read_exact<R: Read + Seek>(&mut self, reader: &mut R, output: &mut [u8]) -> Option<()> {
        let mut written = 0;
        while written < output.len() {
            if !self.fill(reader)? {
                return None;
            }
            let count = (self.buffer_end - self.buffer_start).min(output.len() - written);
            output[written..written + count]
                .copy_from_slice(&self.buffer[self.buffer_start..self.buffer_start + count]);
            self.buffer_start += count;
            self.position = self.position.checked_add(count as u64)?;
            written += count;
        }
        Some(())
    }

    fn skip<R: Read + Seek>(&mut self, reader: &mut R, mut count: usize) -> Option<()> {
        let mut scratch = [0u8; 4096];
        while count > 0 {
            let step = count.min(scratch.len());
            self.read_exact(reader, &mut scratch[..step])?;
            count -= step;
        }
        Some(())
    }
}

#[cfg(test)]
pub(super) fn cbc_encrypt_zero_iv(key: &[u8; 16], buf: &mut [u8]) {
    crate::util::aes::aes128_cbc_encrypt_nopad(key, &[0u8; 16], buf)
        .expect("buffer length is a multiple of the block size");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::xex::xdbf;

    const SESSION_KEY: [u8; 16] = [0xA5; 16];

    fn security(aes_key: [u8; 16], image_size: u32) -> SecurityInfo {
        SecurityInfo {
            image_size,
            load_address: 0x8200_0000,
            aes_key,
            region: 0xFF,
            allowed_media: 0,
        }
    }

    #[test]
    fn cbc_stream_resumes_at_its_stored_offset_after_interleaved_reads() {
        let base = 13u64;
        let payload: Vec<u8> = (0..128 * 1024).map(|i| (i % 251) as u8).collect();
        let mut source = vec![0; base as usize];
        source.extend_from_slice(&payload);
        let mut reader = std::io::Cursor::new(source);
        let mut stream = CbcStream::new();
        stream
            .reset(&mut reader, base, payload.len() as u64, 0, &[0; 16], false)
            .expect("reset stream");

        let mut first = vec![0; 64 * 1024];
        stream
            .read_exact(&mut reader, &mut first)
            .expect("read first buffer");
        assert_eq!(first, payload[..64 * 1024]);
        reader.seek(SeekFrom::Start(0)).expect("interleaved seek");
        let mut next = [0];
        stream
            .read_exact(&mut reader, &mut next)
            .expect("continue stream");
        assert_eq!(next[0], payload[64 * 1024]);
    }
    #[test]
    fn cbc_stream_rejects_reads_into_an_undecryptable_tail() {
        // stored_len = 16*k + 5: reads ending at or before the last full
        // block succeed, while a read reaching the 5-byte tail cannot
        // decrypt it and fails instead of serving ciphertext.
        let payload: Vec<u8> = (0..16 * 4 + 5).map(|i| (i % 251) as u8).collect();
        let stored_len = payload.len() as u64;
        let mut reader = std::io::Cursor::new(payload.clone());
        let mut stream = CbcStream::new();
        stream
            .reset(&mut reader, 0, stored_len, 0, &[0; 16], true)
            .expect("reset stream");
        let mut full_blocks = [0u8; 16 * 4];
        stream
            .read_exact(&mut reader, &mut full_blocks)
            .expect("reads within the full blocks succeed");
        let mut tail = [0u8; 1];
        assert!(stream.read_exact(&mut reader, &mut tail).is_none());

        // A fresh stream reading only the last full block also succeeds.
        let mut reader = std::io::Cursor::new(payload);
        let mut stream = CbcStream::new();
        stream
            .reset(&mut reader, 0, stored_len, 48, &[0; 16], true)
            .expect("reset into the last full block");
        let mut last = [0u8; 16];
        stream
            .read_exact(&mut reader, &mut last)
            .expect("read ending at 16*k succeeds");
    }

    #[test]
    fn session_key_is_one_cbc_block_under_the_base_key() {
        let mut encrypted = SESSION_KEY;
        cbc_encrypt_zero_iv(&RETAIL_KEY, &mut encrypted);
        assert_ne!(encrypted, SESSION_KEY);
        assert_eq!(
            session_key(&RETAIL_KEY, &encrypted).expect("valid key"),
            SESSION_KEY
        );
    }

    #[test]
    fn basic_range_reads_descriptors_without_materializing_the_table() {
        let mut source = Vec::new();
        source.extend_from_slice(&4u32.to_be_bytes());
        source.extend_from_slice(&2u32.to_be_bytes());
        source.extend_from_slice(&4u32.to_be_bytes());
        source.extend_from_slice(&0u32.to_be_bytes());
        source.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::BasicAt {
                table_offset: 0,
                count: 2,
            },
        };
        let security = security([0; 16], 10);
        let mut reader = std::io::Cursor::new(source);
        let source = ResourceSource::new(16, 8, &fmt, &security).expect("source");
        let range =
            read_resource_at(&mut reader, &source, &fmt, &security, 3, 5).expect("basic range");
        assert_eq!(range, [4, 0, 0, 5, 6]);
    }

    #[test]
    fn normal_reader_reuses_decoded_frame_for_sequential_ranges() {
        let chunk = [
            0x00, 0x30, 0x30, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, b'a', b'b', b'c', 0x00,
        ];
        let mut block = vec![0u8; BLOCK_INFO_LEN];
        block.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        block.extend_from_slice(&chunk);
        block.extend_from_slice(&0u16.to_be_bytes());
        let block_hash: [u8; 20] = Sha1::digest(&block).into();
        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::Normal {
                window_size: 0x0000_8000,
                first_block_size: block.len() as u32,
                first_block_hash: block_hash,
            },
        };
        let security = security([0; 16], 3);
        let mut reader = std::io::Cursor::new(block.clone());
        let mut decoder = NormalResourceReader::from_xex(&fmt, &security).expect("decoder");
        assert_eq!(
            decoder
                .read_range(&mut reader, 0, block.len() as u64, 0, 2)
                .expect("first range"),
            b"ab"
        );
        assert_eq!(
            decoder
                .read_range(&mut reader, 0, block.len() as u64, 2, 1)
                .expect("sequential range"),
            b"c"
        );
        assert_eq!(
            decoder
                .read_range(&mut reader, 0, block.len() as u64, 0, 1)
                .expect("backward range resets decoder"),
            b"a"
        );
    }

    #[test]
    fn normal_reader_decodes_large_block() {
        let chunk = [
            0x00, 0x30, 0x30, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, b'a', b'b', b'c', 0x00,
        ];
        let mut block = vec![0u8; BLOCK_INFO_LEN];
        block.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        block.extend_from_slice(&chunk);
        block.extend_from_slice(&0u16.to_be_bytes());
        block.resize((NORMAL_BLOCK_MEMORY_LIMIT + 1) as usize, 0);
        let block_hash: [u8; 20] = Sha1::digest(&block).into();
        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::Normal {
                window_size: 0x0000_8000,
                first_block_size: block.len() as u32,
                first_block_hash: block_hash,
            },
        };
        let security = security([0; 16], 3);
        let mut reader = std::io::Cursor::new(block.clone());
        let mut decoder = NormalResourceReader::from_xex(&fmt, &security).expect("decoder");
        assert_eq!(
            decoder
                .read_range(&mut reader, 0, block.len() as u64, 0, 3)
                .expect("resource from large block"),
            b"abc"
        );
    }

    #[test]
    fn normal_reader_advances_to_resources_beyond_two_frames() {
        fn uncompressed_chunk(first: bool, value: u8) -> Vec<u8> {
            let mut bits = Vec::new();
            if first {
                bits.push(0);
            }
            bits.extend([0, 1, 1]);
            let size = 32_768u32;
            bits.extend((0..24).rev().map(|bit| ((size >> bit) & 1) as u8));
            let mut chunk = Vec::new();
            for word in bits.chunks(16) {
                let value = word
                    .iter()
                    .fold(0u16, |value, bit| (value << 1) | u16::from(*bit));
                chunk.extend_from_slice(&value.to_le_bytes());
            }
            for _ in 0..3 {
                chunk.extend_from_slice(&1u32.to_le_bytes());
            }
            chunk.resize(chunk.len() + size as usize, value);
            chunk
        }

        let mut block = vec![0u8; BLOCK_INFO_LEN];
        for index in 0..4 {
            let chunk = uncompressed_chunk(index == 0, index as u8 + 1);
            block.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
            block.extend_from_slice(&chunk);
        }
        block.extend_from_slice(&0u16.to_be_bytes());
        let block_hash: [u8; 20] = Sha1::digest(&block).into();
        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::Normal {
                window_size: 0x0000_8000,
                first_block_size: block.len() as u32,
                first_block_hash: block_hash,
            },
        };
        let security = security([0; 16], 150_000);
        let mut reader = std::io::Cursor::new(block.clone());
        let mut decoder = NormalResourceReader::from_xex(&fmt, &security).expect("decoder");
        assert_eq!(
            decoder
                .read_range(&mut reader, 0, block.len() as u64, 65_536, 1)
                .expect("range in third LZX frame"),
            [3]
        );
        assert_eq!(
            decoder
                .read_range(&mut reader, 0, block.len() as u64, 140_000, 4)
                .expect("zero-padded basefile tail"),
            [0; 4]
        );
    }

    #[test]
    fn lzx_block_larger_than_stored_extent_is_rejected_before_reading() {
        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::Normal {
                window_size: 0x0000_8000,
                first_block_size: u32::MAX,
                first_block_hash: [0; 20],
            },
        };
        let security = security([0; 16], 3);
        let mut reader = std::io::Cursor::new([0u8; 32]);
        let source = ResourceSource::new(0, 32, &fmt, &security).expect("source");
        assert!(read_resource_at(&mut reader, &source, &fmt, &security, 2, 1).is_none());
    }

    #[test]
    fn xdbf_resource_at_a_logical_offset_past_stored_bytes_still_parses() {
        // Sparse-logical basefile: a zero run pushes the SPA to a logical
        // offset beyond the physically stored bytes. develop still reported
        // title and icon here, so descriptor-backed reads must keep serving
        // the resource regardless of the stored byte count.
        let png = xdbf::tests::build_png(64, 64);
        let payload = xdbf::build_xdbf("Sparse Title", &png);
        let xdbf_len = payload.len() as u32;
        let mut source = Vec::new();
        // Descriptor table: an all-zero run, then the whole SPA as data.
        source.extend_from_slice(&0u32.to_be_bytes());
        source.extend_from_slice(&4096u32.to_be_bytes());
        source.extend_from_slice(&xdbf_len.to_be_bytes());
        source.extend_from_slice(&0u32.to_be_bytes());
        let table_len = source.len() as u64;
        source.extend_from_slice(&payload);
        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::BasicAt {
                table_offset: 0,
                count: 2,
            },
        };
        let security = security([0; 16], 4096 + xdbf_len);
        let mut reader = std::io::Cursor::new(source);
        // Payload reads resolve through the source base, which sits past
        // the descriptor table the BasicAt offset points at.
        let source =
            ResourceSource::new(table_len, u64::from(xdbf_len), &fmt, &security).expect("source");
        let mut resource_reader = ResourceReader::new();
        let meta = xdbf::parse_xdbf_ranges(u64::from(xdbf_len), |offset, size| {
            resource_reader.read_resource_at(
                &mut reader,
                &source,
                &fmt,
                &security,
                4096 + offset,
                size,
            )
        });
        assert_eq!(meta.title_name.as_deref(), Some("Sparse Title"));
        let icon = meta.icon.expect("icon survives the zero run");
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.png_bytes, png);
    }
}
