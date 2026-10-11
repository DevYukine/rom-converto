//! Decoder limits for 7z archives. The 7z reader has no public bound on what
//! an archive's declared decoders allocate: a declared dictionary is
//! zero-filled whole on first read, a block with several packed streams
//! builds its coders once per bound input, and the decode chain follows bind
//! pairs with no check that each coder is visited once, so a tiny file can
//! force a multi-gigabyte allocation while it is only being listed. The
//! guard admits only what real 7z writers produce for a plain archive: one
//! packed stream per block, a short chain of coders whose methods have a
//! fixed memory model, and no encryption. [`open`] checks the header before
//! it is decoded and every parsed data block before listing. [`check_block`]
//! checks each block again just before decoding; everything else is refused.

use anyhow::{Result, anyhow, bail};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Largest decoder memory a folder or block may declare, summed over its
/// coders: the guard refuses anything above 1.5 GiB.
const MAX_DECODER_BYTES: u64 = 1536 << 20;
/// Room above the cap for the fixed tables next to a dictionary (an LZMA
/// literal table is at most 6 MiB), so a dictionary exactly at the cap,
/// which the reference implementation writes for large inputs, still passes.
const DECODER_SLACK_BYTES: u64 = 8 << 20;
/// Largest encoded header this guard inspects. The reader preallocates
/// roughly 100 to 200 bytes per header bit that declares a file, block or
/// coder (an all-set `kEmptyStream` vector costs one bit per file), from
/// counts bounded only by the header length, so 4 MiB of decoded header
/// keeps the header phase under about 1 GiB.
const MAX_HEADER_BYTES: u64 = 4 << 20;
/// An encoded header's LZMA2 dictionary is zero-filled even when the header
/// behind it is tiny, so a dictionary more than twice the stream it decodes
/// (and past this floor, where the reference encoder stops shrinking) is
/// refused. Data blocks are not held to this: the encoder sizes the
/// dictionary to the largest file or the solid total, not to each block.
const LZMA2_DICT_FLOOR_BYTES: u64 = 64 << 20;
/// Coders per folder or block. Real writers chain a filter or two in front
/// of a compressor; every coder adds a nested reader layer.
const MAX_CODERS: usize = 4;

const COPY_ID: &[u8] = &[0x00];
const LZMA_ID: &[u8] = &[0x03, 0x01, 0x01];
const LZMA2_ID: &[u8] = &[0x21];
const AES_ID: &[u8] = &[0x06, 0xF1, 0x07, 0x01];

/// Delta and the BCJ filters (x86, PPC, IA64, ARM, ARM Thumb, SPARC, ARM64,
/// RISC-V): they keep no dictionary.
fn is_filter(id: &[u8]) -> bool {
    matches!(
        id,
        [0x03]
            | [0x03, 0x03, 0x01, 0x03]
            | [0x03, 0x03, 0x02, 0x05]
            | [0x03, 0x03, 0x04, 0x01]
            | [0x03, 0x03, 0x05, 0x01]
            | [0x03, 0x03, 0x07, 0x01]
            | [0x03, 0x03, 0x08, 0x05]
            | [0x0A]
            | [0x0B]
    )
}

/// What an encoded header's folder may hold: what the reference
/// implementation and the crate's writer emit there.
fn is_header_method(id: &[u8]) -> bool {
    id == COPY_ID || id == LZMA_ID || id == LZMA2_ID || is_filter(id)
}

/// The dictionary an LZMA2 coder declares, from its one property byte.
fn lzma2_dictionary(properties: &[u8]) -> Option<u64> {
    let bits = *properties.first()? & 0x3F;
    if bits > 40 {
        // The reader rejects this dictionary itself.
        return None;
    }
    if bits == 40 {
        return Some(0xFFFF_FFFF);
    }
    Some(u64::from(2 | (bits & 1)) << (bits / 2 + 11))
}

/// Memory a coder allocates for its declared parameters, decoding a stream
/// of `unpack_size` bytes, or `None` when the method is not one the reader
/// decodes with a known footprint or its properties are unusable. Methods
/// outside this table (BCJ2, Brotli, encryption, anything else) are refused.
fn coder_memory(method_id: &[u8], properties: &[u8], unpack_size: u64) -> Option<u64> {
    match method_id {
        // Stored data and Deflate keep no dictionary to size.
        [0x00] | [0x04, 0x01, 0x08] => Some(0),
        id if is_filter(id) => Some(0),
        // LZ4: two frame buffers.
        [0x04, 0xF7, 0x11, 0x04] => Some(2 * ((12 << 20) + (64 << 10))),
        // LZMA: five property bytes, the lc/lp/pb byte then the dictionary
        // little-endian. The reader caps the dictionary at the stream size
        // and allocates its literal probabilities by lc + lp.
        [0x03, 0x01, 0x01] => {
            let props = *properties.first()?;
            if props >= 225 {
                return None;
            }
            let dict: [u8; 4] = properties.get(1..5)?.try_into().ok()?;
            let dict = u64::from(u32::from_le_bytes(dict))
                .min(unpack_size)
                .max(4096);
            let lc_lp = props % 9 + props / 9 % 5;
            Some(dict + (1536u64 << lc_lp))
        }
        [0x21] => lzma2_dictionary(properties),
        // PPMd: five property bytes, model size little-endian at [1..5].
        [0x03, 0x04, 0x01] => {
            let bytes: [u8; 4] = properties.get(1..5)?.try_into().ok()?;
            Some(u64::from(u32::from_le_bytes(bytes)))
        }
        // BZip2 block buffers and Zstandard's window plus stream buffers.
        [0x04, 0x02, 0x02] => Some(4 << 20),
        [0x04, 0xF7, 0x11, 0x01] => Some((128 << 20) + (1 << 20)),
        _ => None,
    }
}

/// True when an LZMA2 dictionary is large next to the stream it decodes:
/// the reference encoder shrinks the dictionary to the stream size, so a
/// legitimate header stays well inside this rule.
fn lzma2_dict_too_large(dict: u64, unpack_size: u64) -> bool {
    dict > LZMA2_DICT_FLOOR_BYTES && dict > unpack_size.saturating_mul(2)
}

/// One coder of a folder or block as declared.
struct CoderRef<'a> {
    id: &'a [u8],
    properties: &'a [u8],
    /// Declared size of the stream the coder produces.
    unpack_size: u64,
}

/// The name of a known method, for refusal messages.
fn method_name(id: &[u8]) -> Option<&'static str> {
    match id {
        [0x00] => Some("Copy"),
        [0x03] => Some("Delta"),
        [0x03, 0x01, 0x01] => Some("LZMA"),
        [0x21] => Some("LZMA2"),
        [0x03, 0x04, 0x01] => Some("PPMd"),
        [0x04, 0x02, 0x02] => Some("BZip2"),
        [0x04, 0x01, 0x08] => Some("Deflate"),
        [0x04, 0x01, 0x09] => Some("Deflate64"),
        [0x04, 0xF7, 0x11, 0x04] => Some("LZ4"),
        [0x04, 0xF7, 0x11, 0x01] => Some("Zstd"),
        [0x04, 0xF7, 0x11, 0x02] => Some("Brotli"),
        [0x03, 0x03, 0x01, 0x1B] => Some("BCJ2"),
        [0x06, 0xF1, 0x07, 0x01] => Some("AES"),
        [0x03, 0x03, 0x01, 0x03] => Some("BCJ x86"),
        [0x03, 0x03, 0x02, 0x05] => Some("BCJ PPC"),
        [0x03, 0x03, 0x04, 0x01] => Some("BCJ IA64"),
        [0x03, 0x03, 0x05, 0x01] => Some("BCJ ARM"),
        [0x03, 0x03, 0x07, 0x01] => Some("BCJ ARM Thumb"),
        [0x03, 0x03, 0x08, 0x05] => Some("BCJ SPARC"),
        [0x0A] => Some("BCJ ARM64"),
        [0x0B] => Some("BCJ RISC-V"),
        _ => None,
    }
}

/// Memory the coders of one folder or block allocate together, or why the
/// layout is refused.
fn chain_memory(coders: &[CoderRef<'_>]) -> Result<u64, String> {
    if coders.len() > MAX_CODERS {
        return Err(format!("chains more than {MAX_CODERS} coders"));
    }
    let mut sum = 0u64;
    for coder in coders {
        if coder.id == AES_ID {
            return Err("uses the AES method, which is not supported".to_string());
        }
        let memory =
            coder_memory(coder.id, coder.properties, coder.unpack_size).ok_or_else(|| {
                match method_name(coder.id) {
                    Some(name) => format!("uses the {name} method, which is not supported"),
                    None => format!(
                        "uses an unknown method {:02X?}, which is not supported",
                        coder.id
                    ),
                }
            })?;
        sum = sum.saturating_add(memory);
    }
    Ok(sum)
}

/// Refuses a coder layout the whitelist does not cover, or one whose summed
/// decoder memory is above the cap.
fn check_chain(path: &Path, coders: &[CoderRef<'_>]) -> Result<()> {
    let needed = chain_memory(coders).map_err(|why| anyhow!("{} {why}", path.display()))?;
    if needed > MAX_DECODER_BYTES + DECODER_SLACK_BYTES {
        bail!(
            "{} declares decoders needing {} MiB, above the {} MiB limit",
            path.display(),
            needed >> 20,
            MAX_DECODER_BYTES >> 20
        );
    }
    Ok(())
}

/// True when a decode chain visits each of `coders` coders exactly once.
/// The reader follows bind pairs with no revisit check, so a pair list that
/// reuses an index instantiates one coder many times.
fn visits_each_once(visited: impl Iterator<Item = usize>, coders: usize) -> bool {
    let mut seen = vec![false; coders];
    let mut count = 0;
    for index in visited {
        match seen.get_mut(index) {
            Some(slot) if !*slot => *slot = true,
            _ => return false,
        }
        count += 1;
    }
    count == coders
}

/// How many packed streams block `index` reads, or `None` when the archive
/// does not map the block. The reader builds its multi-input decode path
/// (every coder once per bound input) exactly when a block has more than
/// one.
fn packed_stream_count(archive: &sevenz_rust2::Archive, index: usize) -> Option<usize> {
    let firsts = archive.stream_map.block_first_pack_stream_index();
    let first = *firsts.get(index)?;
    let next = firsts
        .get(index + 1)
        .copied()
        .unwrap_or(archive.pack_sizes().len());
    next.checked_sub(first)
}

fn read_byte(cursor: &mut &[u8]) -> std::io::Result<u8> {
    let Some((&byte, rest)) = cursor.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "truncated 7z header",
        ));
    };
    *cursor = rest;
    Ok(byte)
}

fn read_bytes<'a>(cursor: &mut &'a [u8], len: usize) -> std::io::Result<&'a [u8]> {
    if cursor.len() < len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "truncated 7z header",
        ));
    }
    let (head, rest) = cursor.split_at(len);
    *cursor = rest;
    Ok(head)
}

/// Reads a header number exactly as the 7z reader does, so every encoding
/// the reader accepts is parsed here too.
fn read_number(cursor: &mut &[u8]) -> std::io::Result<u64> {
    let first = u64::from(read_byte(cursor)?);
    let mut mask = 0x80_u64;
    let mut value = 0;
    for i in 0..8 {
        if first & mask == 0 {
            return Ok(value | ((first & (mask - 1)) << (8 * i)));
        }
        value |= u64::from(read_byte(cursor)?) << (8 * i);
        mask >>= 1;
    }
    Ok(value)
}

/// What an encoded header holds for this guard.
enum Folder0<'a> {
    /// A plain header: its coders are checked from the parsed archive.
    NotEncoded,
    /// An encoded header this parser cannot read and therefore refuses.
    Unreadable,
    /// A declared shape the guard refuses outright.
    Refused(String),
    Parsed {
        coders: Vec<CoderRef<'a>>,
    },
}

/// Parses just enough of an encoded header to inspect its one folder's
/// coders and unpack sizes.
fn folder0_coders(header: &[u8]) -> Folder0<'_> {
    // A plain header carries no decoder parameters of its own.
    if header.first() != Some(&0x17) {
        return Folder0::NotEncoded;
    }
    match folder0_streams(&header[1..]) {
        Ok(folder) | Err(folder) => folder,
    }
}

/// Parses the streams info that follows the encoded-header tag.
fn folder0_streams(streams: &[u8]) -> Result<Folder0<'_>, Folder0<'_>> {
    // Local readers turn every parse problem into `Unreadable`, so an
    // encoded header this parser cannot read is refused.
    fn byte(cursor: &mut &[u8]) -> Result<u8, Folder0<'static>> {
        read_byte(cursor).map_err(|_| Folder0::Unreadable)
    }
    fn number(cursor: &mut &[u8]) -> Result<u64, Folder0<'static>> {
        read_number(cursor).map_err(|_| Folder0::Unreadable)
    }
    fn bytes<'a>(cursor: &mut &'a [u8], len: u64) -> Result<&'a [u8], Folder0<'static>> {
        let len = usize::try_from(len).map_err(|_| Folder0::Unreadable)?;
        read_bytes(cursor, len).map_err(|_| Folder0::Unreadable)
    }

    let mut cursor = streams;
    // The streams info starts with PackInfo directly.
    if byte(&mut cursor)? != 0x06 {
        return Err(Folder0::Unreadable);
    }
    let _pack_pos = number(&mut cursor)?;
    let num_pack_streams = number(&mut cursor)?;
    let mut nid = byte(&mut cursor)?;
    if nid == 0x09 {
        for _ in 0..num_pack_streams {
            let _ = number(&mut cursor)?;
        }
        nid = byte(&mut cursor)?;
    }
    if nid == 0x0A {
        skip_digests(&mut cursor, num_pack_streams).ok_or(Folder0::Unreadable)?;
        nid = byte(&mut cursor)?;
    }
    if nid != 0x00 {
        return Err(Folder0::Unreadable);
    }
    // UnpackInfo, folder 0.
    if byte(&mut cursor)? != 0x07 || byte(&mut cursor)? != 0x0B {
        return Err(Folder0::Unreadable);
    }
    let num_folders = number(&mut cursor)?;
    if byte(&mut cursor)? != 0 {
        return Err(Folder0::Unreadable);
    }
    // Only a single-folder header is checked here; a header with more or
    // no folders would be parsed past folder 0 on a guess.
    if num_folders != 1 {
        return Err(Folder0::Refused(format!(
            "declares {num_folders} folders in its encoded header, which is not supported"
        )));
    }
    let num_coders = number(&mut cursor)?;
    if num_coders > MAX_CODERS as u64 {
        return Err(Folder0::Refused(format!(
            "declares {num_coders} coders in its header folder, above the {MAX_CODERS} coder limit, which is not supported"
        )));
    }
    let mut coders = Vec::new();
    for _ in 0..num_coders {
        let flag = byte(&mut cursor)?;
        let id = bytes(&mut cursor, u64::from(flag & 0x0F))?;
        // The reference implementation writes only single-stream coders in
        // a header folder.
        if flag & 0x10 != 0 {
            return Err(Folder0::Refused(
                "declares a multi-stream coder in its header folder, which is not supported"
                    .to_string(),
            ));
        }
        let properties: &[u8] = if flag & 0x20 != 0 {
            let props_size = number(&mut cursor)?;
            bytes(&mut cursor, props_size)?
        } else {
            &[]
        };
        if id == AES_ID {
            return Err(Folder0::Refused(
                "uses the AES method, which is not supported".to_string(),
            ));
        }
        if !is_header_method(id) {
            return Err(Folder0::Refused(match method_name(id) {
                Some(name) => {
                    format!("uses the {name} method in its header, which is not supported")
                }
                None => format!(
                    "uses an unknown method {id:02X?} in its header, which is not supported"
                ),
            }));
        }
        coders.push(CoderRef {
            id,
            properties,
            unpack_size: 0,
        });
    }
    // A chain of single-stream coders has one bind pair fewer than coders,
    // and no stream may be bound twice.
    let num_bind_pairs = num_coders.checked_sub(1).ok_or(Folder0::Unreadable)?;
    let mut bound_in = Vec::new();
    let mut bound_out = Vec::new();
    for _ in 0..num_bind_pairs {
        let in_index = number(&mut cursor)?;
        let out_index = number(&mut cursor)?;
        if bound_in.contains(&in_index) || bound_out.contains(&out_index) {
            return Err(Folder0::Refused(
                "binds a coder stream twice in its header folder, which is not supported"
                    .to_string(),
            ));
        }
        bound_in.push(in_index);
        bound_out.push(out_index);
    }
    if byte(&mut cursor)? != 0x0C {
        return Err(Folder0::Unreadable);
    }
    for coder in &mut coders {
        coder.unpack_size = number(&mut cursor)?;
    }
    Ok(Folder0::Parsed { coders })
}

/// Steps over a digest block: an all-defined byte, a bit vector when not
/// all are defined, then four bytes per defined digest. Only the first
/// `count` bits of the vector count; the padding bits past it are ignored.
fn skip_digests(cursor: &mut &[u8], count: u64) -> Option<()> {
    let all_defined = read_byte(cursor).ok()?;
    let mut defined = count;
    if all_defined == 0 {
        let bits = read_bytes(cursor, usize::try_from(count.div_ceil(8)).ok()?).ok()?;
        defined = (0..count)
            .filter(|i| {
                usize::try_from(i / 8)
                    .ok()
                    .and_then(|byte| bits.get(byte))
                    .is_some_and(|bits| bits & (0x80 >> (i % 8)) != 0)
            })
            .count() as u64;
    }
    for _ in 0..defined {
        let _ = read_bytes(cursor, 4).ok()?;
    }
    Some(())
}

/// Checks the decoder parameters a 7z declares, so opening it cannot trigger
/// an unbounded allocation. Reads from `file`, leaving the position moved.
///
/// # Errors
/// Errors when the archive's start header is blank or declares a header past
/// the end of the file, its header or decoder parameters exceed the limits,
/// its encoded header cannot be checked, or reading the header fails.
fn check_file(file: &mut BufReader<File>, path: &Path) -> Result<()> {
    let mut signature = [0u8; 32];
    if file.read_exact(&mut signature).is_err() {
        // Shorter than a signature header: the reader reports it.
        return Ok(());
    }
    let chunks = signature[12..28].as_chunks::<8>().0;
    let next_header_offset = u64::from_le_bytes(chunks[0]);
    let next_header_size = u64::from_le_bytes(chunks[1]);
    if signature[..6] != [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C] {
        // Not a 7z by magic: the reader names the real problem.
        return Ok(());
    }
    if signature[8..32].iter().all(|byte| *byte == 0) {
        // The reader would scan the file tail for a header and decode it
        // without verifying its checksum.
        bail!(
            "{} has no start header, which is not supported",
            path.display()
        );
    }
    if next_header_size > MAX_HEADER_BYTES {
        bail!(
            "{} declares a {} MiB header, above the {} MiB limit",
            path.display(),
            next_header_size >> 20,
            MAX_HEADER_BYTES >> 20
        );
    }
    let header_pos = 32u64
        .checked_add(next_header_offset)
        .ok_or_else(|| anyhow!("{} declares an overflowing header offset", path.display()))?;
    // A header that lies past the end is refused here, not left to the
    // reader: a file that grows between this check and the parse must not
    // slip an unchecked header through.
    if header_pos.saturating_add(next_header_size) > file.get_ref().metadata()?.len() {
        bail!(
            "{} declares a header past the end of the file",
            path.display()
        );
    }
    file.seek(SeekFrom::Start(header_pos))?;
    let mut header = vec![0u8; next_header_size as usize];
    file.read_exact(&mut header)?;
    match folder0_coders(&header) {
        Folder0::NotEncoded => Ok(()),
        // The reader is more lenient than this parser; a header it would
        // accept but this check cannot read must not reach it unchecked.
        Folder0::Unreadable => bail!("{} has a 7z header that cannot be read", path.display()),
        Folder0::Refused(reason) => bail!("{} {reason}", path.display()),
        Folder0::Parsed { coders } => {
            let total = coders
                .iter()
                .fold(0u64, |sum, coder| sum.saturating_add(coder.unpack_size));
            if total > MAX_HEADER_BYTES {
                bail!(
                    "{} declares a header decoding to {} MiB, above the {} MiB limit",
                    path.display(),
                    total >> 20,
                    MAX_HEADER_BYTES >> 20
                );
            }
            for coder in &coders {
                if coder.id == LZMA2_ID
                    && let Some(dict) = lzma2_dictionary(coder.properties)
                    && lzma2_dict_too_large(dict, coder.unpack_size)
                {
                    bail!(
                        "{} declares a {} MiB LZMA2 dictionary for a {} MiB header stream",
                        path.display(),
                        dict >> 20,
                        coder.unpack_size >> 20
                    );
                }
            }
            check_chain(path, &coders)
        }
    }
}

/// Checks one parsed data block: a single packed stream, a whitelisted
/// method set within the coder cap and the summed memory cap, and a chain
/// that visits each coder once. Opening checks all blocks without decoding;
/// the caller checks again just before a block's decoder is built.
pub(crate) fn check_block(
    archive: &sevenz_rust2::Archive,
    index: usize,
    path: &Path,
) -> Result<()> {
    if packed_stream_count(archive, index) != Some(1) {
        bail!(
            "{} uses more than one packed stream per block (for example BCJ2), which is not supported",
            path.display()
        );
    }
    let block = &archive.blocks[index];
    let coders: Vec<CoderRef<'_>> = block
        .coders
        .iter()
        .enumerate()
        .take(MAX_CODERS + 1)
        .map(|(coder_index, coder)| CoderRef {
            id: coder.encoder_method_id(),
            properties: coder.properties(),
            unpack_size: block.get_unpack_size_at_index(coder_index),
        })
        .collect();
    check_chain(path, &coders)?;
    if !visits_each_once(
        block
            .ordered_coder_iter()
            .map(|(coder_index, _)| coder_index),
        coders.len(),
    ) {
        bail!(
            "{} binds its coders in an unsupported layout",
            path.display()
        );
    }
    Ok(())
}

/// Opens a 7z archive after checking its header and all data block decoder
/// parameters. Returns the parsed archive and the buffered file, so one
/// handle serves the checks and the reads.
///
/// # Errors
/// Errors when the archive is refused or its header cannot be parsed.
pub(crate) fn open(path: &Path) -> Result<(sevenz_rust2::Archive, BufReader<File>)> {
    let mut file = BufReader::with_capacity(1 << 16, File::open(path)?);
    check_file(&mut file, path)?;
    file.seek(SeekFrom::Start(0))?;
    let archive = sevenz_rust2::Archive::read(&mut file, &sevenz_rust2::Password::empty())?;
    for index in 0..archive.blocks.len() {
        check_block(&archive, index, path)?;
    }
    Ok((archive, file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const PPMD: &[u8] = &[0x03, 0x04, 0x01];
    /// PPMd order 6 with a 1 GiB model.
    const PPMD_1GIB: &[u8] = &[6, 0, 0, 0, 0x40];
    const BCJ2: &[u8] = &[0x03, 0x03, 0x01, 0x1B];
    const BROTLI: &[u8] = &[0x04, 0xF7, 0x11, 0x02];
    const DEFLATE64: &[u8] = &[0x04, 0x01, 0x09];

    /// A real archive of one file stored with BCJ2 over three LZMA
    /// streams and a plain header: its block reads four packed streams.
    /// Embedded because the crate's writer cannot produce BCJ2.
    const BCJ2_FIXTURE: &[u8] = &[
        0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c, 0x00, 0x04, 0xf7, 0x4c, 0x90, 0x75, 0x60, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x82, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x83,
        0xb8, 0x32, 0x00, 0x31, 0x18, 0xc9, 0x91, 0x14, 0x83, 0x0d, 0x46, 0xc0, 0x4d, 0x12, 0x66,
        0xc7, 0x86, 0x7c, 0xa6, 0xe2, 0xaf, 0x1d, 0x1d, 0x4f, 0x39, 0x91, 0x4a, 0x1e, 0xd2, 0x48,
        0xd2, 0xf7, 0xcf, 0x2b, 0x60, 0x97, 0xda, 0xc2, 0xbb, 0x05, 0x0d, 0x7b, 0x3c, 0x76, 0x93,
        0xbc, 0x2a, 0x60, 0x4d, 0x98, 0x7d, 0x50, 0xa3, 0x36, 0x17, 0x6b, 0xaf, 0xf8, 0x4e, 0x22,
        0x2d, 0x0d, 0x73, 0xdb, 0xbe, 0x9a, 0xc6, 0xb3, 0x7c, 0x1a, 0x41, 0x6e, 0x1b, 0x07, 0x46,
        0xd0, 0x37, 0x80, 0x74, 0xa5, 0x3a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x04, 0x06, 0x00, 0x04, 0x09, 0x51,
        0x05, 0x05, 0x05, 0x00, 0x07, 0x0b, 0x01, 0x00, 0x04, 0x23, 0x03, 0x01, 0x01, 0x05, 0x5d,
        0x00, 0x10, 0x00, 0x00, 0x23, 0x03, 0x01, 0x01, 0x05, 0x5d, 0x00, 0x10, 0x00, 0x00, 0x23,
        0x03, 0x01, 0x01, 0x05, 0x5d, 0x00, 0x10, 0x00, 0x00, 0x14, 0x03, 0x03, 0x01, 0x1b, 0x04,
        0x01, 0x05, 0x00, 0x04, 0x01, 0x03, 0x02, 0x02, 0x06, 0x01, 0x00, 0x0c, 0x00, 0x00, 0x82,
        0x68, 0x82, 0x68, 0x00, 0x08, 0x0a, 0x01, 0x13, 0x9a, 0x65, 0xbc, 0x00, 0x00, 0x05, 0x01,
        0x19, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x0d, 0x00, 0x66,
        0x00, 0x2e, 0x00, 0x74, 0x00, 0x78, 0x00, 0x74, 0x00, 0x00, 0x00, 0x14, 0x0a, 0x01, 0x00,
        0x00, 0x25, 0xc3, 0xf7, 0x50, 0x59, 0xdd, 0x01, 0x15, 0x06, 0x01, 0x00, 0x20, 0x80, 0xa4,
        0x81, 0x00, 0x00,
    ];

    fn coder<'a>(id: &'a [u8], properties: &'a [u8], unpack_size: u64) -> CoderRef<'a> {
        CoderRef {
            id,
            properties,
            unpack_size,
        }
    }

    #[test]
    fn coder_memory_models_the_reader_allocation() {
        // LZMA2 dictionary bits.
        assert_eq!(coder_memory(LZMA2_ID, &[40], 0), Some(0xFFFF_FFFF));
        assert_eq!(coder_memory(LZMA2_ID, &[24], 0), Some(16 << 20));
        assert_eq!(coder_memory(LZMA2_ID, &[], 0), None);
        // LZMA: the dictionary is capped at the stream size (and floored at
        // 4 KiB), plus the literal probabilities for its lc and lp.
        let props = [0x5D, 0x00, 0x00, 0x00, 0x04];
        assert_eq!(
            coder_memory(LZMA_ID, &props, 1 << 40),
            Some((64 << 20) + (1536 << 3))
        );
        assert_eq!(
            coder_memory(LZMA_ID, &props, 1024),
            Some(4096 + (1536 << 3))
        );
        assert_eq!(coder_memory(LZMA_ID, &props[..2], 1024), None);
        // lc/lp/pb byte beyond the reader's range.
        assert_eq!(coder_memory(LZMA_ID, &[225, 0, 0, 1, 0], 1 << 20), None);
        // Filters and stored data keep no dictionary.
        assert_eq!(coder_memory(COPY_ID, &[], 0), Some(0));
        assert_eq!(coder_memory(&[0x03, 0x03, 0x01, 0x03], &[], 0), Some(0));
        // Windowed decoders have a fixed model.
        assert_eq!(coder_memory(&[0x04, 0x02, 0x02], &[], 0), Some(4 << 20));
        assert_eq!(
            coder_memory(&[0x04, 0xF7, 0x11, 0x01], &[], 0),
            Some((128 << 20) + (1 << 20))
        );
        assert_eq!(
            coder_memory(&[0x04, 0xF7, 0x11, 0x04], &[], 0),
            Some(2 * ((12 << 20) + (64 << 10)))
        );
        assert_eq!(coder_memory(PPMD, PPMD_1GIB, 1024), Some(1 << 30));
        // Methods without a safe model are refused.
        for id in [BCJ2, BROTLI, DEFLATE64, AES_ID, &[0x7F]] {
            assert_eq!(coder_memory(id, &[], 0), None);
        }
    }

    #[test]
    fn chain_memory_sums_coders_and_refuses_unknown_layouts() {
        let small_lzma: &[u8] = &[0x5D, 0, 0, 0x10, 0];
        let chain = [
            coder(&[0x03, 0x03, 0x01, 0x03], &[], 1 << 30),
            coder(LZMA_ID, small_lzma, 1 << 30),
        ];
        assert_eq!(chain_memory(&chain).unwrap(), (1 << 20) + (1536 << 3));
        let five: Vec<_> = (0..5).map(|_| coder(COPY_ID, &[], 1)).collect();
        assert!(chain_memory(&five).is_err());
        // Two PPMd models of 1 GiB sum past the cap; one does not.
        let two = [coder(PPMD, PPMD_1GIB, 1), coder(PPMD, PPMD_1GIB, 1)];
        assert!(check_chain(Path::new("x.7z"), &two).is_err());
        assert!(check_chain(Path::new("x.7z"), &two[..1]).is_ok());
        // BCJ2 is no whitelisted coder.
        assert!(check_chain(Path::new("x.7z"), &[coder(BCJ2, &[], 1)]).is_err());
        // Encryption has its own refusal, apart from an unknown method.
        let encrypted = chain_memory(&[coder(AES_ID, &[], 1)]).unwrap_err();
        let unknown = chain_memory(&[coder(&[0x7F], &[], 1)]).unwrap_err();
        assert_ne!(encrypted, unknown);
    }

    #[test]
    fn visits_each_once_refuses_a_chain_that_repeats_or_falls_short() {
        assert!(visits_each_once([0, 1, 2].into_iter(), 3));
        assert!(visits_each_once([2, 0, 1].into_iter(), 3));
        // A bind pair list that reuses an index revisits a coder.
        assert!(!visits_each_once([0, 1, 1].into_iter(), 3));
        // A chain that stops early leaves a coder unreached.
        assert!(!visits_each_once([0, 1].into_iter(), 3));
        assert!(!visits_each_once([0, 5].into_iter(), 2));
    }

    /// Writes a signature header naming a header blob at `32 + offset`.
    fn write_7z_shell(path: &Path, offset: u64, size: u64, crc: u32, header: &[u8]) {
        let mut file = File::create(path).unwrap();
        let mut signature = [0u8; 32];
        signature[..6].copy_from_slice(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]);
        signature[12..20].copy_from_slice(&offset.to_le_bytes());
        signature[20..28].copy_from_slice(&size.to_le_bytes());
        signature[28..32].copy_from_slice(&crc.to_le_bytes());
        file.write_all(&signature).unwrap();
        file.seek(SeekFrom::Start(32 + offset)).unwrap();
        file.write_all(header).unwrap();
    }

    fn check_shell(path: &Path) -> Result<()> {
        check_file(&mut BufReader::new(File::open(path).unwrap()), path)
    }

    /// Checks an archive made of just this encoded header.
    fn check_header(header: &[u8]) -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell.7z");
        write_7z_shell(&path, 0, header.len() as u64, 0, header);
        check_shell(&path)
    }

    /// An encoded header whose one folder holds `coders` (method id and
    /// properties) bound by `pairs` (in index, out index), laid out as the
    /// reader parses it: streams info with PackInfo, then UnpackInfo, then
    /// the unpack sizes (64 bytes each).
    fn encoded_header(coders: &[(&[u8], &[u8])], pairs: &[(u8, u8)]) -> Vec<u8> {
        let mut header = vec![
            0x17,
            0x06,
            0x00,
            0x01,
            0x09,
            0x01,
            0x00,
            0x07,
            0x0B,
            0x01,
            0x00,
            coders.len() as u8,
        ];
        for (id, properties) in coders {
            let flag = id.len() as u8 | if properties.is_empty() { 0 } else { 0x20 };
            header.push(flag);
            header.extend(*id);
            if !properties.is_empty() {
                header.push(properties.len() as u8);
                header.extend(*properties);
            }
        }
        for (in_index, out_index) in pairs {
            header.extend([*in_index, *out_index]);
        }
        header.push(0x0C);
        header.extend(std::iter::repeat_n(0x40, coders.len()));
        header.extend([0x00, 0x00]);
        header
    }

    #[test]
    fn check_file_refuses_an_oversized_decoder_dictionary() {
        assert!(check_header(&encoded_header(&[(LZMA2_ID, &[40])], &[])).is_err());
    }

    #[test]
    fn check_file_accepts_a_modest_decoder_dictionary() {
        assert!(check_header(&encoded_header(&[(LZMA2_ID, &[26])], &[])).is_ok());
        // A plain chain of up to four coders is a real layout.
        let chain = [
            (COPY_ID, &[][..]),
            (COPY_ID, &[]),
            (COPY_ID, &[]),
            (COPY_ID, &[]),
        ];
        assert!(check_header(&encoded_header(&chain, &[(0, 1), (1, 2), (2, 3)])).is_ok());
    }

    /// A dictionary within the cap but far above the tiny stream it
    /// decodes is refused all the same: it is zero-filled regardless.
    #[test]
    fn check_file_refuses_a_large_dictionary_for_a_tiny_stream() {
        assert!(check_header(&encoded_header(&[(LZMA2_ID, &[30])], &[])).is_err());
    }

    #[test]
    fn check_file_refuses_more_coders_than_the_limit() {
        let five = [(COPY_ID, &[][..]); 5];
        assert!(check_header(&encoded_header(&five, &[(0, 1), (1, 2), (2, 3), (3, 4)])).is_err());
    }

    /// Bind pairs that reuse a stream make one coder instantiate many times.
    #[test]
    fn check_file_refuses_bind_pairs_that_reuse_a_stream() {
        let three = [(COPY_ID, &[][..]), (COPY_ID, &[]), (COPY_ID, &[])];
        assert!(check_header(&encoded_header(&three, &[(1, 0), (1, 1)])).is_err());
        assert!(check_header(&encoded_header(&three, &[(0, 1), (2, 1)])).is_err());
        assert!(check_header(&encoded_header(&three, &[(0, 1), (1, 2)])).is_ok());
    }

    /// A header folder holds only what the reference implementation and
    /// the crate's writer emit there: stored data, filters, LZMA and LZMA2.
    #[test]
    fn check_file_refuses_an_unsupported_header_method() {
        for id in [AES_ID, BCJ2, PPMD, BROTLI, DEFLATE64] {
            assert!(check_header(&encoded_header(&[(id, &[])], &[])).is_err());
        }
        assert!(check_header(&encoded_header(&[(LZMA_ID, &[0x5D, 0, 0, 1, 0])], &[])).is_ok());
    }

    /// A header coder with several streams (a BCJ2-style input list) is
    /// refused before its counts are read.
    #[test]
    fn check_file_refuses_a_multi_stream_header_coder() {
        let header = [
            0x17, 0x06, 0x00, 0x02, 0x09, 0x01, 0x01, 0x00, // two packed streams
            0x07, 0x0B, 0x01, 0x00, 0x01, // one folder, one coder
            0x31, 0x21, 0x02, 0x01, 0x01, 26, // complex LZMA2 coder, two inputs
            0x0C, 0x40, 0x00, 0x00,
        ];
        assert!(check_header(&header).is_err());
    }

    /// A multi-folder encoded header is refused rather than read as folder
    /// 0 with the wrong coders.
    #[test]
    fn check_file_refuses_a_multi_folder_header() {
        let mut header = encoded_header(&[(LZMA2_ID, &[26])], &[]);
        // The folder count byte sits after the kFolder tag.
        header[9] = 2;
        assert!(check_header(&header).is_err());
    }

    #[test]
    fn check_file_refuses_a_blank_start_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blank.7z");
        let mut file = File::create(&path).unwrap();
        let mut signature = [0u8; 32];
        signature[..6].copy_from_slice(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]);
        file.write_all(&signature).unwrap();
        assert!(check_shell(&path).is_err());
    }

    /// A header that lies past the end of the file is refused, so a file
    /// that grows after the check cannot slip an unchecked header through.
    #[test]
    fn check_file_refuses_a_header_past_the_end_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.7z");
        let mut signature = [0u8; 32];
        signature[..6].copy_from_slice(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]);
        signature[20..28].copy_from_slice(&100u64.to_le_bytes());
        std::fs::write(&path, signature).unwrap();
        assert!(check_shell(&path).is_err());
    }

    /// A digest bit vector whose padding bits past the stream count are
    /// set must not desync the parse: the coder check after it still runs.
    #[test]
    fn check_file_parses_a_digest_block_with_padding_bits() {
        let header = [
            0x17, 0x06, 0x00, 0x01, 0x09, 0x01, // pack pos, one stream, sizes
            0x0A, 0x00, 0xFF, 1, 2, 3, 4, 0x00, // digest block, end
            0x07, 0x0B, 0x01, 0x00, 0x01, 0x21, 0x21, 0x01, 40, // one LZMA2 coder, bits 40
            0x0C, 0x40, 0x00, 0x00, // unpack size, end
        ];
        assert!(check_header(&header).is_err());
    }

    #[test]
    fn check_file_refuses_a_header_above_the_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bighdr.7z");
        let mut signature = [0u8; 32];
        signature[..6].copy_from_slice(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]);
        signature[20..28].copy_from_slice(&((MAX_HEADER_BYTES + 1).to_le_bytes()));
        std::fs::write(&path, signature).unwrap();
        assert!(check_shell(&path).is_err());
    }

    #[test]
    fn open_passes_a_real_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("real.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&path).unwrap();
        writer
            .push_archive_entry::<&[u8]>(
                sevenz_rust2::ArchiveEntry::new_file("game.iso"),
                Some(b"payload"),
            )
            .unwrap();
        writer.finish().unwrap();
        let (archive, _) = open(&path).unwrap();
        assert_eq!(archive.files.len(), 1);
        assert_eq!(archive.files[0].name(), "game.iso");
        assert_eq!(packed_stream_count(&archive, 0), Some(1));
    }

    /// A block that reads several packed streams takes the reader's
    /// multi-input path, which builds its coders once per bound input.
    /// Opening refuses it before it can be listed or decoded.
    #[test]
    fn open_refuses_a_bcj2_archive_before_listing_or_decoding() {
        let archive = sevenz_rust2::Archive::read(
            &mut std::io::Cursor::new(BCJ2_FIXTURE),
            &sevenz_rust2::Password::empty(),
        )
        .unwrap();
        assert!(packed_stream_count(&archive, 0).is_some_and(|count| count > 1));
        assert!(check_block(&archive, 0, Path::new("bcj2.7z")).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bcj2.7z");
        std::fs::write(&path, BCJ2_FIXTURE).unwrap();
        assert!(open(&path).is_err());
    }

    /// The parser must read a header the writer actually produced: a
    /// desync here would silently pass every real archive. Enough members
    /// that the writer compresses the header instead of storing it plain,
    /// which is the only way it emits an encoded one.
    #[test]
    fn folder0_coders_reads_a_real_encoded_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("real.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&path).unwrap();
        for index in 0..64u32 {
            writer
                .push_archive_entry::<&[u8]>(
                    sevenz_rust2::ArchiveEntry::new_file(&format!("disc{index}/track01.bin")),
                    Some(b"track".as_slice()),
                )
                .unwrap();
        }
        writer.finish().unwrap();
        let mut file = File::open(&path).unwrap();
        let mut signature = [0u8; 32];
        file.read_exact(&mut signature).unwrap();
        let offset = u64::from_le_bytes(signature[12..20].try_into().unwrap());
        let size = u64::from_le_bytes(signature[20..28].try_into().unwrap());
        file.seek(SeekFrom::Start(32 + offset)).unwrap();
        let mut header = vec![0u8; size as usize];
        file.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 0x17);
        let Folder0::Parsed { coders } = folder0_coders(&header) else {
            panic!("a real encoded header must parse");
        };
        assert_eq!(coders.len(), 1);
        // The writer encodes headers with LZMA.
        assert_eq!(coders[0].id, LZMA_ID);
        assert!(open(&path).is_ok());

        // Breaking the PackInfo marker leaves a header this check cannot
        // read, which is refused rather than handed to the reader.
        let mut bytes = std::fs::read(&path).unwrap();
        let header_start = 32 + usize::try_from(offset).unwrap();
        bytes[header_start + 1] = 0x00;
        assert!(matches!(
            folder0_coders(&bytes[header_start..]),
            Folder0::Unreadable
        ));
        let broken_path = dir.path().join("broken.7z");
        std::fs::write(&broken_path, bytes).unwrap();
        let mut broken = BufReader::new(File::open(&broken_path).unwrap());
        assert!(check_file(&mut broken, &broken_path).is_err());
    }
}
