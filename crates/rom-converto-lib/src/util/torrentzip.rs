//! TorrentZip and RVZSTD container writing and validation: the structured,
//! deterministic zip flavours ROM managers expect. Hand-rolled with
//! `std::io` so every pinned header field (version-made-by,
//! general-purpose flag, fixed DOS timestamp, member name encoding) is
//! written and checked byte for byte.

use crate::util::hash::{CRC32, PumpSink, pump_skip_pad};
use crate::util::{CancelToken, ProgressReporter, publish_temp, scratch_output_path};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;

/// Max-compression general-purpose bit flag.
const GP_FLAG: u16 = 0x0002;
/// UTF-8 member-name flag (bit 11). Set alongside [`GP_FLAG`] only when a
/// member name has a character no CP437 byte can hold; names CP437 can
/// represent are stored as CP437 with this flag clear.
const GP_FLAG_UTF8: u16 = 0x0800;
/// Version needed to extract 2.0, deflate members.
const VERSION_NEEDED: u16 = 20;
/// Version needed to extract 4.5, zip64.
const VERSION_NEEDED_ZIP64: u16 = 45;
/// Version needed to extract 6.3, the first version that knows zstd;
/// RVZSTD pins zstd members here even when zip64.
const VERSION_NEEDED_ZSTD: u16 = 63;
/// Fixed DOS timestamp TorrentZip pins: 1996-12-24 23:32:00. RVZSTD pins
/// zero instead.
const DOS_TIME: u16 = 0xBC00;
const DOS_DATE: u16 = 0x2198;
const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const METHOD_ZSTD: u16 = 93;
/// 32-bit field sentinel meaning "read the zip64 extra/record instead".
const U32_SENTINEL: u64 = 0xFFFF_FFFF;

/// Fixed local file header size, excluding name and extra field.
const LOCAL_HEADER_BYTES: u64 = 30;
/// Fixed central directory header size, excluding name and extra field.
const CENTRAL_HEADER_BYTES: usize = 46;
/// Fixed end-of-central-directory record size, excluding the comment.
const EOCD_HEADER_BYTES: usize = 22;
/// One-member directory slack: the 16-bit maxima of the name, extra and
/// comment lengths on top of the fixed central header.
const CD_SLOP_MAX: usize = 3 * 65535;
/// Zip64 EOCD locator size.
const ZIP64_LOCATOR_BYTES: usize = 20;
/// Zip64 EOCD record size.
const ZIP64_EOCD_BYTES: usize = 56;
/// How far back from EOF an EOCD may hide: 64 KiB of comment room plus the
/// fixed record, plus the zip64 tail as a margin.
const EOCD_SEARCH_BYTES: u64 = 64 * 1024 + 22 + (ZIP64_LOCATOR_BYTES + ZIP64_EOCD_BYTES) as u64;

/// Which structured zip flavour to write or accept: TorrentZip (DEFLATE)
/// or RVZSTD, a zstd-compressed structured zip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZipFormat {
    TorrentZip,
    RvZstd,
}

impl ZipFormat {
    /// Parse a `zip_format` option value; the accepted spellings are the
    /// ones [`ZipFormat::name`] produces.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "torrentzip" => Some(Self::TorrentZip),
            "rvzstd" => Some(Self::RvZstd),
            _ => None,
        }
    }

    /// Canonical lowercase name of the format.
    pub fn name(self) -> &'static str {
        match self {
            Self::TorrentZip => "torrentzip",
            Self::RvZstd => "rvzstd",
        }
    }

    /// EOCD comment prefix carrying the central-directory checksum.
    fn comment_prefix(self) -> &'static str {
        match self {
            Self::TorrentZip => "TORRENTZIPPED-",
            Self::RvZstd => "RVZSTD-",
        }
    }

    /// Fixed DOS timestamp the format pins.
    fn dos_time(self) -> u16 {
        match self {
            Self::TorrentZip => DOS_TIME,
            Self::RvZstd => 0,
        }
    }

    /// Fixed DOS date the format pins.
    fn dos_date(self) -> u16 {
        match self {
            Self::TorrentZip => DOS_DATE,
            Self::RvZstd => 0,
        }
    }

    /// Zip compression method number for a member of the given uncompressed
    /// size: RVZSTD stores zero-byte members raw.
    fn method(self, raw_size: u64) -> u16 {
        match self {
            Self::TorrentZip => METHOD_DEFLATE,
            Self::RvZstd if raw_size == 0 => METHOD_STORE,
            Self::RvZstd => METHOD_ZSTD,
        }
    }
}

/// Version-needed-to-extract for an entry: 6.3 for RVZSTD zstd members
/// (RVZSTD keeps it even when zip64), 4.5 when the entry carries a
/// zip64 extra, otherwise 2.0.
fn entry_version_needed(format: ZipFormat, raw_size: u64, zip64: bool) -> u16 {
    if format == ZipFormat::RvZstd && raw_size > 0 {
        VERSION_NEEDED_ZSTD
    } else if zip64 {
        VERSION_NEEDED_ZIP64
    } else {
        VERSION_NEEDED
    }
}

/// Zip64 decision for one member: the uncompressed size plus the most
/// deflate can ever expand it (`raw_size/1000` of per-block overhead plus
/// 64 KiB) reaches the 32-bit sentinel.
///
/// The decision deliberately never looks at the actual compressed size, so
/// it is stable before streaming: rewriting the same input stays
/// byte-identical (canonical identity). The edge is a member a little
/// under 4 GiB whose deflate stream happens to fit 32 bits: it still gets
/// a zip64 extra, and its central-directory extra then carries none of the
/// three fields, only the mark itself. Members from 4290611148 up to
/// 4294967294 bytes therefore carry the extra although their size field
/// could still hold the value: the extra marks every size whose deflate
/// output itself might not fit.
fn entry_zip64(raw_size: u64) -> bool {
    raw_size.saturating_add(raw_size / 1000 + 65536) >= U32_SENTINEL
}

/// One input to archive: `path` is read from `skip` bytes in (console
/// header strip), `pad` bytes of `fill` are appended, and the result is
/// stored under `name`.
#[derive(Debug, Clone)]
pub struct ZipMember {
    pub name: String,
    pub path: PathBuf,
    /// Leading bytes omitted from the source file.
    pub skip: u64,
    /// Bytes appended after the source content; 0 = none.
    pub pad: u64,
    /// Byte the appended pad bytes are written with.
    pub fill: u8,
}

/// An archive member's identity as written or read back: name, CRC-32
/// (ISO-HDLC) of the uncompressed content, and its uncompressed size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub crc: u32,
    pub size: u64,
}

/// One member as written: identity plus the layout facts the central
/// directory and zip64 decision need.
struct WrittenEntry {
    name: String,
    name_bytes: Vec<u8>,
    flags: u16,
    crc: u32,
    raw_size: u64,
    comp_size: u64,
    offset: u64,
}

/// Write `member` as a structured TorrentZip/RVZSTD archive holding exactly
/// that one member at `output`.
///
/// The member is streamed from its source file with per-chunk progress and
/// cancellation; the archive is staged on a sibling temp file and renamed
/// into place only on success. Returns the written member's identity.
///
/// # Errors
/// Returns an error when the source cannot be read or the archive cannot
/// be written; cancellation surfaces as [`crate::util::Cancelled`].
pub fn write_torrentzip(
    member: &ZipMember,
    output: &Path,
    format: ZipFormat,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<ZipEntry> {
    let len = std::fs::metadata(&member.path)?.len();
    let raw_size = len.checked_sub(member.skip).ok_or_else(|| {
        anyhow::anyhow!(
            "{}: header strip of {} bytes exceeds the {}-byte file",
            member.path.display(),
            member.skip,
            len
        )
    })? + member.pad;

    let temp = scratch_output_path(output)?;
    let mut file = std::fs::File::create(&temp)?;
    progress.start(raw_size, "zip");

    let entry = write_member(&mut file, member, raw_size, format, progress, cancel)?;

    let cd_offset = file.stream_position()?;
    let mut cd = Vec::new();
    write_central_header(&mut cd, &entry, format);
    let cd_crc = CRC32.checksum(&cd);
    file.write_all(&cd)?;

    let needs_zip64 = cd_offset >= U32_SENTINEL || cd.len() as u64 >= U32_SENTINEL || entry.zip64();
    if needs_zip64 {
        write_zip64_eocd(&mut file, cd_offset, cd.len() as u64)?;
    }

    let comment = format!("{}{:08X}", format.comment_prefix(), cd_crc);
    write_eocd(&mut file, cd_offset, cd.len() as u64, needs_zip64, &comment)?;
    file.flush()?;
    progress.finish();

    publish_temp(temp, output, true)?;
    Ok(ZipEntry {
        name: entry.name,
        crc: entry.crc,
        size: entry.raw_size,
    })
}

/// Stream one member into the archive: placeholder local header, raw
/// deflate/zstd stream, then seek back and patch the true CRC and sizes.
fn write_member(
    mut file: &mut std::fs::File,
    member: &ZipMember,
    raw_size: u64,
    format: ZipFormat,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<WrittenEntry> {
    let offset = file.stream_position()?;
    // Sizes are known up front from the source metadata, so zip64-ness is
    // decidable before streaming (`entry_zip64` uses the deflate worst-case
    // slack, never the compressed output). The local zip64 extra carries
    // only the two sizes; a header offset, when one is ever needed, lives
    // in the central directory's extra alone.
    let zip64 = entry_zip64(raw_size);
    let extra_len: u64 = if zip64 { 20 } else { 0 };
    // A name every character of which has a CP437 byte is stored as CP437;
    // anything else is stored as UTF-8 with the name-encoding flag.
    let (name_bytes, flags) = member_name_encoding(&member.name);
    let name = &name_bytes;

    // Placeholder header; CRC and sizes are patched after streaming.
    file.write_all(&LOCAL_SIG.to_le_bytes())?;
    let version_needed = entry_version_needed(format, raw_size, zip64);
    file.write_all(&version_needed.to_le_bytes())?;
    file.write_all(&flags.to_le_bytes())?;
    file.write_all(&format.method(raw_size).to_le_bytes())?;
    file.write_all(&format.dos_time().to_le_bytes())?;
    file.write_all(&format.dos_date().to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&(name.len() as u16).to_le_bytes())?;
    file.write_all(&(extra_len as u16).to_le_bytes())?;
    file.write_all(name)?;
    if zip64 {
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&16u16.to_le_bytes())?;
        file.write_all(&0u64.to_le_bytes())?; // uncompressed size
        file.write_all(&0u64.to_le_bytes())?; // compressed size
    }

    let data_start = offset + LOCAL_HEADER_BYTES + name.len() as u64 + extra_len;
    let mut crc = CRC32.digest();
    let mut count = 0u64;
    let mut input = std::fs::File::open(&member.path)?;
    match format {
        ZipFormat::TorrentZip => {
            let mut encoder =
                flate2::write::DeflateEncoder::new(&mut file, flate2::Compression::new(9));
            pump_member(
                &mut input,
                &mut encoder,
                member,
                &mut crc,
                &mut count,
                progress,
                cancel,
            )?;
            encoder.finish()?;
        }
        // A zero-byte RVZSTD member is stored raw, no zstd frame at all.
        ZipFormat::RvZstd if raw_size == 0 => {
            pump_member(
                &mut input, &mut file, member, &mut crc, &mut count, progress, cancel,
            )?;
        }
        ZipFormat::RvZstd => {
            let mut encoder = zstd::stream::write::Encoder::new(&mut file, 19)?;
            pump_member(
                &mut input,
                &mut encoder,
                member,
                &mut crc,
                &mut count,
                progress,
                cancel,
            )?;
            encoder.finish()?;
        }
    }
    if count != raw_size {
        anyhow::bail!(
            "{}: changed while archiving: {} bytes read, {} expected",
            member.path.display(),
            count,
            raw_size
        );
    }

    let comp_size = file.stream_position()? - data_start;
    if !zip64 && comp_size >= U32_SENTINEL {
        anyhow::bail!(
            "{}: compressed member of {} bytes overflows a 32-bit size field",
            member.path.display(),
            comp_size
        );
    }

    // Patch the true values into the placeholder header, then return to
    // the end of the member's data for the next one.
    if zip64 {
        let extra_at = offset + LOCAL_HEADER_BYTES + name.len() as u64 + 4;
        file.seek(SeekFrom::Start(extra_at))?;
        file.write_all(&raw_size.to_le_bytes())?;
        file.write_all(&comp_size.to_le_bytes())?;
    }
    let crc = crc.finalize();
    let (comp32, raw32) = if zip64 {
        (U32_SENTINEL as u32, U32_SENTINEL as u32)
    } else {
        (comp_size as u32, raw_size as u32)
    };
    file.seek(SeekFrom::Start(offset + 14))?;
    file.write_all(&crc.to_le_bytes())?;
    file.write_all(&comp32.to_le_bytes())?;
    file.write_all(&raw32.to_le_bytes())?;
    file.seek(SeekFrom::Start(data_start + comp_size))?;

    Ok(WrittenEntry {
        name: member.name.clone(),
        name_bytes,
        flags,
        crc,
        raw_size,
        comp_size,
        offset,
    })
}

/// Folds each pumped chunk into the member CRC and streamed byte count,
/// then writes it into the member's output stream (the deflate/zstd
/// encoder or the raw file).
struct MemberSink<'a> {
    crc: &'a mut crate::util::hash::Crc32Digest,
    count: &'a mut u64,
    out: &'a mut dyn std::io::Write,
}

/// Streams one member's source through `out`, folding the CRC and byte
/// count: the one [`pump_skip_pad`] call site of the writer.
fn pump_member(
    input: &mut std::fs::File,
    out: &mut dyn std::io::Write,
    member: &ZipMember,
    crc: &mut crate::util::hash::Crc32Digest,
    count: &mut u64,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> std::io::Result<()> {
    let mut sink = MemberSink {
        crc,
        count,
        out: &mut *out,
    };
    pump_skip_pad(
        input,
        &mut sink,
        member.skip,
        member.pad,
        member.fill,
        progress,
        cancel,
    )
}

impl PumpSink for MemberSink<'_> {
    fn absorb(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        self.crc.update(chunk);
        *self.count += chunk.len() as u64;
        self.out.write_all(chunk)
    }
}

/// Append one central directory header for `entry` to `cd`.
fn write_central_header(cd: &mut Vec<u8>, entry: &WrittenEntry, format: ZipFormat) {
    let comp32 = u32::try_from(entry.comp_size.min(U32_SENTINEL)).unwrap_or(U32_SENTINEL as u32);
    let raw32 = u32::try_from(entry.raw_size.min(U32_SENTINEL)).unwrap_or(U32_SENTINEL as u32);
    let offset32 = u32::try_from(entry.offset.min(U32_SENTINEL)).unwrap_or(U32_SENTINEL as u32);

    // Zip64 extra payload fields sit in the fixed order uncompressed,
    // compressed, offset, each present exactly when its base field overflows.
    // The extra itself is present for every zip64 entry (a member decided
    // by the slack rule may overflow nothing and carry an empty payload),
    // so the recorded version-needed-to-extract matches what the validator
    // derives from the extra's presence.
    let mut payload = Vec::new();
    if entry.raw_size >= U32_SENTINEL {
        payload.extend_from_slice(&entry.raw_size.to_le_bytes());
    }
    if entry.comp_size >= U32_SENTINEL {
        payload.extend_from_slice(&entry.comp_size.to_le_bytes());
    }
    if entry.offset >= U32_SENTINEL {
        payload.extend_from_slice(&entry.offset.to_le_bytes());
    }
    let mut extra_field = Vec::with_capacity(payload.len() + 4);
    if entry.zip64() {
        extra_field.extend_from_slice(&1u16.to_le_bytes());
        extra_field.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        extra_field.extend_from_slice(&payload);
    }

    cd.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
    cd.extend_from_slice(&0u16.to_le_bytes()); // version made by: MS-DOS
    let version_needed = entry_version_needed(format, entry.raw_size, entry.zip64());
    cd.extend_from_slice(&version_needed.to_le_bytes());
    cd.extend_from_slice(&entry.flags.to_le_bytes());
    cd.extend_from_slice(&format.method(entry.raw_size).to_le_bytes());
    cd.extend_from_slice(&format.dos_time().to_le_bytes());
    cd.extend_from_slice(&format.dos_date().to_le_bytes());
    cd.extend_from_slice(&entry.crc.to_le_bytes());
    cd.extend_from_slice(&comp32.to_le_bytes());
    cd.extend_from_slice(&raw32.to_le_bytes());
    cd.extend_from_slice(&(entry.name_bytes.len() as u16).to_le_bytes());
    cd.extend_from_slice(&(extra_field.len() as u16).to_le_bytes());
    cd.extend_from_slice(&0u16.to_le_bytes()); // file comment length
    cd.extend_from_slice(&0u16.to_le_bytes()); // disk number start
    cd.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
    cd.extend_from_slice(&0u32.to_le_bytes()); // external attributes
    cd.extend_from_slice(&offset32.to_le_bytes());
    cd.extend_from_slice(&entry.name_bytes);
    cd.extend_from_slice(&extra_field);
}

/// Write the zip64 EOCD record and locator for an archive whose plain EOCD
/// fields would overflow.
fn write_zip64_eocd(file: &mut std::fs::File, cd_offset: u64, cd_size: u64) -> std::io::Result<()> {
    let zip64_eocd_offset = file.stream_position()?;
    file.write_all(&ZIP64_EOCD_SIG.to_le_bytes())?;
    file.write_all(&44u64.to_le_bytes())?; // size of this record
    file.write_all(&VERSION_NEEDED_ZIP64.to_le_bytes())?;
    file.write_all(&VERSION_NEEDED_ZIP64.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&1u64.to_le_bytes())?; // entries
    file.write_all(&1u64.to_le_bytes())?;
    file.write_all(&cd_size.to_le_bytes())?;
    file.write_all(&cd_offset.to_le_bytes())?;
    file.write_all(&ZIP64_LOCATOR_SIG.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&zip64_eocd_offset.to_le_bytes())?;
    file.write_all(&1u32.to_le_bytes())?;
    Ok(())
}

/// Write the EOCD record closing the archive, with the checksum comment.
fn write_eocd(
    file: &mut std::fs::File,
    cd_offset: u64,
    cd_size: u64,
    zip64: bool,
    comment: &str,
) -> std::io::Result<()> {
    let entries16 = 1u16;
    let cd_size32 = u32::try_from(cd_size.min(U32_SENTINEL)).unwrap_or(U32_SENTINEL as u32);
    let cd_offset32 = u32::try_from(cd_offset.min(U32_SENTINEL)).unwrap_or(U32_SENTINEL as u32);
    let (cd_size32, cd_offset32) = if zip64 {
        (U32_SENTINEL as u32, U32_SENTINEL as u32)
    } else {
        (cd_size32, cd_offset32)
    };
    file.write_all(&EOCD_SIG.to_le_bytes())?;
    file.write_all(&0u16.to_le_bytes())?;
    file.write_all(&0u16.to_le_bytes())?;
    file.write_all(&entries16.to_le_bytes())?;
    file.write_all(&entries16.to_le_bytes())?;
    file.write_all(&cd_size32.to_le_bytes())?;
    file.write_all(&cd_offset32.to_le_bytes())?;
    file.write_all(&(comment.len() as u16).to_le_bytes())?;
    file.write_all(comment.as_bytes())?;
    Ok(())
}

impl WrittenEntry {
    /// True when this entry carries a zip64 extra, by the same
    /// pre-streaming decision the local header used.
    fn zip64(&self) -> bool {
        entry_zip64(self.raw_size)
    }
}

/// One parsed central directory entry, with the layout facts the local
/// header check needs.
struct ParsedEntry {
    entry: ZipEntry,
    flags: u16,
    comp_size: u64,
    offset: u64,
    name_bytes: Vec<u8>,
}

/// Validate the zip at `path` as TorrentZip/RVZSTD structure: the EOCD
/// comment must carry the right prefix and the uppercase-hex CRC-32 of the
/// central directory bytes, every central header must pin the fields the
/// flavour fixes (version made by, general-purpose flag, timestamp,
/// method), and every local header must repeat its central entry's pinned
/// fields and name.
///
/// Returns `Ok(None)` when the file is not a zip at all or is a plain zip
/// that does not follow either structure; I/O problems are `Err`.
pub fn validate_torrentzip(path: &Path) -> anyhow::Result<Option<(ZipFormat, Vec<ZipEntry>)>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len < EOCD_HEADER_BYTES as u64 {
        return Ok(None);
    }
    // The comment is the last thing in the file, so the EOCD hides within
    // the last window; find the record whose own comment length reaches EOF.
    let window = len.min(EOCD_SEARCH_BYTES) as usize;
    file.seek(SeekFrom::Start(len - window as u64))?;
    let mut tail = vec![0u8; window];
    file.read_exact(&mut tail)?;
    let Some(eocd_at) = find_eocd(&tail) else {
        return Ok(None);
    };
    let eocd = &tail[eocd_at..];
    let entries16 = u16::from_le_bytes(eocd[10..12].try_into()?);
    let cd_size32 = u32::from_le_bytes(eocd[12..16].try_into()?);
    let cd_offset32 = u32::from_le_bytes(eocd[16..20].try_into()?);
    let comment = &eocd[EOCD_HEADER_BYTES..];
    // One archive on one disk: the EOCD disk fields are always zero.
    if u16::from_le_bytes(eocd[4..6].try_into()?) != 0
        || u16::from_le_bytes(eocd[6..8].try_into()?) != 0
    {
        return Ok(None);
    }

    let format = if comment.len() == 22 && comment.starts_with(b"TORRENTZIPPED-") {
        ZipFormat::TorrentZip
    } else if comment.len() == 15 && comment.starts_with(b"RVZSTD-") {
        ZipFormat::RvZstd
    } else {
        return Ok(None);
    };
    let Some(comment_hex) = std::str::from_utf8(&comment[format.comment_prefix().len()..]).ok()
    else {
        return Ok(None);
    };

    // One member: both EOCD entry fields are 1, never a sentinel. The
    // zip64 tail exists exactly when both directory fields are sentinels.
    if entries16 != 1 || u16::from_le_bytes(eocd[8..10].try_into()?) != 1 {
        return Ok(None);
    }
    let tail_pattern = cd_offset32 == U32_SENTINEL as u32 && cd_size32 == U32_SENTINEL as u32;

    // Resolve the real values through the zip64 records when the plain
    // EOCD fields are sentinels.
    let mut entries = entries16 as u64;
    let mut cd_size = cd_size32 as u64;
    let mut cd_offset = cd_offset32 as u64;
    let mut zip64_tail_at: Option<u64> = None;
    if tail_pattern {
        if eocd_at < ZIP64_LOCATOR_BYTES {
            return Ok(None);
        }
        let loc = &tail[eocd_at - ZIP64_LOCATOR_BYTES..eocd_at];
        if u32::from_le_bytes(loc[0..4].try_into()?) != ZIP64_LOCATOR_SIG
            || u32::from_le_bytes(loc[4..8].try_into()?) != 0
            || u32::from_le_bytes(loc[16..20].try_into()?) != 1
        {
            return Ok(None);
        }
        let zip64_eocd_offset = u64::from_le_bytes(loc[8..16].try_into()?);
        // The zip64 EOCD record sits right against the locator: it starts
        // where the directory ends and ends where the locator begins, so a
        // bogus offset can neither point into the file body nor past it.
        let ends_at_locator = zip64_eocd_offset
            .checked_add(ZIP64_EOCD_BYTES as u64)
            .is_some_and(|end| {
                end.checked_add(ZIP64_LOCATOR_BYTES as u64)
                    == Some(eocd_abs_of(len, window, eocd_at))
            });
        if !ends_at_locator {
            return Ok(None);
        }
        let mut record = [0u8; ZIP64_EOCD_BYTES];
        file.seek(SeekFrom::Start(zip64_eocd_offset))?;
        file.read_exact(&mut record)?;
        if u32::from_le_bytes(record[0..4].try_into()?) != ZIP64_EOCD_SIG
            || u64::from_le_bytes(record[4..12].try_into()?) != 44
            || u16::from_le_bytes(record[12..14].try_into()?) != VERSION_NEEDED_ZIP64
            || u16::from_le_bytes(record[14..16].try_into()?) != VERSION_NEEDED_ZIP64
            || u32::from_le_bytes(record[16..20].try_into()?) != 0
            || u32::from_le_bytes(record[20..24].try_into()?) != 0
        {
            return Ok(None);
        }
        entries = u64::from_le_bytes(record[32..40].try_into()?);
        cd_size = u64::from_le_bytes(record[40..48].try_into()?);
        cd_offset = u64::from_le_bytes(record[48..56].try_into()?);
        if u64::from_le_bytes(record[24..32].try_into()?) != entries {
            return Ok(None);
        }
        zip64_tail_at = Some(zip64_eocd_offset);
    }

    // One member: the directory holds exactly one entry and is far smaller
    // than a hostile size field could claim, so a corrupt archive cannot
    // make the validator allocate gigabytes.
    if entries != 1 || cd_size > (CENTRAL_HEADER_BYTES + CD_SLOP_MAX) as u64 {
        return Ok(None);
    }

    // The central directory ends exactly where the zip64 tail or the EOCD
    // begins; junk in between is not a structured zip.
    let eocd_abs = eocd_abs_of(len, window, eocd_at);
    let tail_start = zip64_tail_at.unwrap_or(eocd_abs);
    if cd_offset.checked_add(cd_size) != Some(tail_start) {
        return Ok(None);
    }

    let mut cd_bytes = vec![0u8; cd_size as usize];
    file.seek(SeekFrom::Start(cd_offset))?;
    file.read_exact(&mut cd_bytes)?;
    let expected = format!("{:08X}", CRC32.checksum(&cd_bytes));
    if expected != comment_hex {
        return Ok(None);
    }

    let mut parsed = Vec::new();
    let mut rest = &cd_bytes[..];
    for _ in 0..entries {
        let Some((parsed_entry, remaining)) = parse_central(rest, format) else {
            return Ok(None);
        };
        parsed.push(parsed_entry);
        rest = remaining;
    }
    if !rest.is_empty() {
        return Ok(None);
    }

    // A zip64 tail is present exactly when the writer emits one: some
    // plain EOCD field is a sentinel, or the writer's own rule demanded
    // one (a member size that can overflow the deflate worst case, or a
    // directory offset that can overflow 32 bits). Sentinel fields on an
    // archive whose resolved values need no tail are a forgery.
    let writer_needs_tail = parsed.iter().any(|parsed| entry_zip64(parsed.entry.size))
        || cd_offset >= U32_SENTINEL
        || cd_size >= U32_SENTINEL;
    if writer_needs_tail != zip64_tail_at.is_some() {
        return Ok(None);
    }

    // The local headers must repeat their central entries' pinned fields:
    // a zip whose two records disagree is not structured.
    for parsed_entry in &parsed {
        if check_local(&mut file, parsed_entry, format, cd_offset, eocd_abs)?.is_none() {
            return Ok(None);
        }
    }
    Ok(Some((
        format,
        parsed.into_iter().map(|parsed| parsed.entry).collect(),
    )))
}

/// The EOCD's absolute offset in the file.
fn eocd_abs_of(len: u64, window: usize, eocd_at: usize) -> u64 {
    len - window as u64 + eocd_at as u64
}

/// Locate the EOCD record in a tail window: the last signature whose own
/// comment length runs exactly to the end of the file.
fn find_eocd(tail: &[u8]) -> Option<usize> {
    if tail.len() < EOCD_HEADER_BYTES {
        return None;
    }
    (0..=tail.len() - EOCD_HEADER_BYTES).rev().find(|&i| {
        tail[i..i + 4] == EOCD_SIG.to_le_bytes()
            && i + EOCD_HEADER_BYTES
                + u16::from_le_bytes(tail[i + 20..i + 22].try_into().expect("fixed-size slice"))
                    as usize
                == tail.len()
    })
}

/// Parse one central directory header from the front of `cur`, returning
/// the parsed entry and the remaining bytes. `None` on any structural
/// mismatch.
fn parse_central(cur: &[u8], format: ZipFormat) -> Option<(ParsedEntry, &[u8])> {
    if cur.len() < CENTRAL_HEADER_BYTES {
        return None;
    }
    let u16_at = |o: usize| u16::from_le_bytes(cur[o..o + 2].try_into().expect("fixed-size slice"));
    let u32_at = |o: usize| u32::from_le_bytes(cur[o..o + 4].try_into().expect("fixed-size slice"));
    let flags = u16_at(8);
    if u32_at(0) != CENTRAL_SIG
        || u16_at(4) != 0 // version made by: MS-DOS
        || (flags != GP_FLAG && flags != GP_FLAG | GP_FLAG_UTF8)
        || u16_at(12) != format.dos_time()
        || u16_at(14) != format.dos_date()
        // No file comment, no disk split, no attribute bits: the writer
        // pins all four fields to zero.
        || u16_at(32) != 0
        || u16_at(34) != 0
        || u16_at(36) != 0
        || u32_at(38) != 0
    {
        return None;
    }
    let crc = u32_at(16);
    let mut comp_size = u32_at(20) as u64;
    let mut raw_size = u32_at(24) as u64;
    let name_len = u16_at(28) as usize;
    let extra_len = u16_at(30) as usize;
    let mut offset = u32_at(42) as u64;
    let total = CENTRAL_HEADER_BYTES + name_len + extra_len;
    if cur.len() < total {
        return None;
    }
    let name_bytes = &cur[CENTRAL_HEADER_BYTES..CENTRAL_HEADER_BYTES + name_len];
    // The UTF-8 flag marks name bytes no CP437 byte can hold; a name that
    // CP437 can represent is always stored as CP437.
    let name = if flags & GP_FLAG_UTF8 != 0 {
        let name = std::str::from_utf8(name_bytes).ok()?;
        if cp437_encode(name).is_some() {
            return None;
        }
        name.to_string()
    } else {
        cp437_to_string(name_bytes)
    };

    // Zip64 extras are present exactly when their base field is the
    // sentinel, in the fixed order uncompressed, compressed, offset.
    let mut has_zip64 = false;
    let mut extra = &cur[CENTRAL_HEADER_BYTES + name_len..total];
    while !extra.is_empty() {
        if extra.len() < 4 {
            return None;
        }
        let id = u16::from_le_bytes(extra[0..2].try_into().expect("fixed-size slice"));
        let size = u16::from_le_bytes(extra[2..4].try_into().expect("fixed-size slice")) as usize;
        let data = extra.get(4..4 + size)?;
        if id != 1 {
            // Neither flavour uses another extra field.
            return None;
        }
        // The writer emits the zip64 extra once.
        if has_zip64 {
            return None;
        }
        has_zip64 = true;
        let mut data = data;
        for slot in [&mut raw_size, &mut comp_size, &mut offset] {
            if *slot == U32_SENTINEL {
                if data.len() < 8 {
                    return None;
                }
                *slot = u64::from_le_bytes(data[..8].try_into().expect("fixed-size slice"));
                // A sentinel field is only canonical when the value cannot
                // stay inline: an extra-supplied value is always at or past
                // the 32-bit sentinel.
                if *slot < U32_SENTINEL {
                    return None;
                }
                data = &data[8..];
            }
        }
        if !data.is_empty() {
            return None;
        }
        extra = &extra[4 + size..];
    }

    // RVZSTD pins zstd members at version 6.3 (kept even for zip64)
    // and store zero-byte members raw at 2.0/4.5.
    if u16_at(6) != entry_version_needed(format, raw_size, has_zip64)
        || u16_at(10) != format.method(raw_size)
        // The zip64 extra is present exactly when the member's size can
        // overflow a 32-bit field.
        || has_zip64 != entry_zip64(raw_size)
    {
        return None;
    }

    Some((
        ParsedEntry {
            entry: ZipEntry {
                name,
                crc,
                size: raw_size,
            },
            flags,
            comp_size,
            offset,
            name_bytes: name_bytes.to_vec(),
        },
        &cur[total..],
    ))
}

/// Unicode mapping for CP437 bytes 0x80 to 0xFF; lower bytes are ASCII.
const CP437_HIGH: &[char; 128] = &[
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', //
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', //
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', //
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', //
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// Decodes a member name stored without the UTF-8 flag: bytes below 0x80
/// are ASCII, the high half maps through CP437.
fn cp437_to_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                CP437_HIGH[(b - 0x80) as usize]
            }
        })
        .collect()
}

/// The name's CP437 bytes, when every character maps to one.
fn cp437_encode(name: &str) -> Option<Vec<u8>> {
    name.chars()
        .map(|c| {
            u8::try_from(u32::from(c))
                .ok()
                .filter(|&b| b < 0x80)
                .or_else(|| {
                    CP437_HIGH
                        .iter()
                        .position(|&h| h == c)
                        .map(|i| 0x80 + i as u8)
                })
        })
        .collect()
}

/// The stored bytes and general-purpose flag for a member name: CP437 when
/// every character has a CP437 byte, UTF-8 with the name-encoding flag
/// otherwise.
fn member_name_encoding(name: &str) -> (Vec<u8>, u16) {
    match cp437_encode(name) {
        Some(bytes) => (bytes, GP_FLAG),
        None => (name.as_bytes().to_vec(), GP_FLAG | GP_FLAG_UTF8),
    }
}

/// Checks one local header against its central entry: `None` when the two
/// records disagree or the layout is not the writer's canonical one.
/// `cd_offset` is the central directory's absolute offset and `limit` the
/// EOCD's.
fn check_local(
    file: &mut std::fs::File,
    parsed: &ParsedEntry,
    format: ZipFormat,
    cd_offset: u64,
    limit: u64,
) -> anyhow::Result<Option<()>> {
    // One member, first: the local header opens the file and the member's
    // data runs up to the central directory with nothing between.
    if parsed.offset != 0 {
        return Ok(None);
    }
    let header_end = match parsed.offset.checked_add(LOCAL_HEADER_BYTES) {
        Some(end) if end <= limit => end,
        _ => return Ok(None),
    };
    file.seek(SeekFrom::Start(parsed.offset))?;
    let mut header = [0u8; LOCAL_HEADER_BYTES as usize];
    file.read_exact(&mut header)?;
    let u16_at =
        |o: usize| u16::from_le_bytes(header[o..o + 2].try_into().expect("fixed-size slice"));
    let u32_at =
        |o: usize| u32::from_le_bytes(header[o..o + 4].try_into().expect("fixed-size slice"));
    let name_len = u16_at(26) as u64;
    let extra_len = u16_at(28) as u64;
    let Some(data_start) = header_end
        .checked_add(name_len)
        .and_then(|at| at.checked_add(extra_len))
    else {
        return Ok(None);
    };
    // The member data ends exactly at the central directory, and the local
    // extra field is empty unless the header is zip64, in which case it is
    // exactly the zip64 size pair.
    if data_start
        .checked_add(parsed.comp_size)
        .is_none_or(|end| end != cd_offset || data_start > limit)
    {
        return Ok(None);
    }
    let mut name = vec![0u8; name_len as usize];
    file.read_exact(&mut name)?;
    let mut extra = vec![0u8; extra_len as usize];
    file.read_exact(&mut extra)?;

    let mut comp_size = u32_at(18) as u64;
    let mut raw_size = u32_at(22) as u64;
    let local_zip64 = comp_size == U32_SENTINEL || raw_size == U32_SENTINEL;
    // Both size fields go to the zip64 extra together, never one alone.
    if local_zip64 && (comp_size != U32_SENTINEL || raw_size != U32_SENTINEL) {
        return Ok(None);
    }
    if local_zip64 != entry_zip64(parsed.entry.size) {
        return Ok(None);
    }
    if local_zip64 {
        let Some((local_raw, local_comp)) = local_zip64_sizes(&extra) else {
            return Ok(None);
        };
        raw_size = local_raw;
        comp_size = local_comp;
    } else if !extra.is_empty() {
        return Ok(None);
    }

    let version_needed = entry_version_needed(format, parsed.entry.size, local_zip64);
    if u32_at(0) != LOCAL_SIG
        || u16_at(4) != version_needed
        || u16_at(6) != parsed.flags
        || u16_at(8) != format.method(parsed.entry.size)
        || u16_at(10) != format.dos_time()
        || u16_at(12) != format.dos_date()
        || u32_at(14) != parsed.entry.crc
        || comp_size != parsed.comp_size
        || raw_size != parsed.entry.size
        || name != parsed.name_bytes
    {
        return Ok(None);
    }
    Ok(Some(()))
}

/// The zip64 (uncompressed, compressed) size pair from a local header's
/// extra field, when the extra is exactly that pair.
fn local_zip64_sizes(extra: &[u8]) -> Option<(u64, u64)> {
    if extra.len() != 20
        || u16::from_le_bytes(extra[0..2].try_into().ok()?) != 1
        || u16::from_le_bytes(extra[2..4].try_into().ok()?) != 16
    {
        return None;
    }
    Some((
        u64::from_le_bytes(extra[4..12].try_into().ok()?),
        u64::from_le_bytes(extra[12..20].try_into().ok()?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The TorrentZip EOCD total: the fixed record plus the checksum
    /// comment.
    const EOCD_TOTAL_BYTES: usize = EOCD_HEADER_BYTES + 22;
    use crate::util::NoProgress;
    use std::io::Read;
    use tempfile::tempdir;

    /// Write `bytes` to `dir/name` and return its member description.
    fn member(dir: &Path, name: &str, bytes: &[u8]) -> ZipMember {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write source");
        ZipMember {
            name: name.to_string(),
            path,
            skip: 0,
            pad: 0,
            fill: 0,
        }
    }

    /// Read one member's uncompressed bytes back through the `zip` crate.
    fn read_member(path: &Path, name: &str) -> Vec<u8> {
        let mut archive =
            zip::ZipArchive::new(std::fs::File::open(path).expect("open zip")).expect("archive");
        let index = archive
            .file_names()
            .position(|n| n == name)
            .expect("member name");
        let mut member = archive.by_index(index).expect("member");
        let mut bytes = Vec::new();
        member.read_to_end(&mut bytes).expect("read member");
        bytes
    }

    #[test]
    fn writes_a_torrentzip_the_zip_crate_reads() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");

        let written = write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // The entry comes back with the raw content's CRC and size.
        assert_eq!(written.name, "a.bin");
        assert_eq!(written.size, b"first member bytes".len() as u64);
        assert_eq!(written.crc, CRC32.checksum(b"first member bytes"));

        let (format, validated) = validate_torrentzip(&output)
            .expect("validate succeeds")
            .expect("structured");
        assert_eq!(format, ZipFormat::TorrentZip);
        assert_eq!(validated, vec![written]);
        assert_eq!(read_member(&output, "a.bin"), b"first member bytes");
    }

    #[test]
    fn rewriting_identical_inputs_is_byte_identical() {
        let dir = tempdir().expect("temp dir");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        let one = dir.path().join("one.zip");
        let two = dir.path().join("two.zip");

        for output in [&one, &two] {
            write_torrentzip(
                &a,
                output,
                ZipFormat::TorrentZip,
                &NoProgress,
                &CancelToken::new(),
            )
            .expect("write succeeds");
        }

        assert_eq!(
            std::fs::read(&one).expect("read one"),
            std::fs::read(&two).expect("read two")
        );
    }

    #[test]
    fn rvzstd_round_trips() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");

        write_torrentzip(
            &a,
            &output,
            ZipFormat::RvZstd,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // RVZSTD pins members at version 6.3 with a zero timestamp.
        let raw = std::fs::read(&output).expect("read archive");
        let version = u16::from_le_bytes(raw[4..6].try_into().expect("version"));
        let time = u16::from_le_bytes(raw[10..12].try_into().expect("time"));
        let date = u16::from_le_bytes(raw[12..14].try_into().expect("date"));
        assert_eq!((version, time, date), (63, 0, 0));

        let (format, entries) = validate_torrentzip(&output)
            .expect("validate succeeds")
            .expect("structured");
        assert_eq!(format, ZipFormat::RvZstd);
        assert_eq!(entries.len(), 1);
        assert_eq!(read_member(&output, "a.bin"), b"first member bytes");
    }

    /// A zero-byte RVZSTD member is stored raw, no zstd frame at all.
    #[test]
    fn rvzstd_stores_a_zero_byte_member_raw() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let zero = member(dir.path(), "zero.bin", b"");

        write_torrentzip(
            &zero,
            &output,
            ZipFormat::RvZstd,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let (format, entries) = validate_torrentzip(&output)
            .expect("validate succeeds")
            .expect("structured");
        assert_eq!(format, ZipFormat::RvZstd);
        assert_eq!(entries.len(), 1);
        assert_eq!(read_member(&output, "zero.bin"), b"");
    }

    #[test]
    fn skip_and_zero_pad_account_into_size_and_crc() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let source = dir.path().join("cart.bin");
        let content = b"0123456789abcdefpayload-payload";
        std::fs::write(&source, content).expect("write source");
        let stripped: Vec<u8> = content[16..]
            .iter()
            .copied()
            .chain(std::iter::repeat_n(0, 8))
            .collect();

        let entries = write_torrentzip(
            &ZipMember {
                name: "cart.bin".to_string(),
                path: source,
                skip: 16,
                pad: 8,
                fill: 0,
            },
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        assert_eq!(entries.size, (content.len() - 16 + 8) as u64);
        assert_eq!(entries.size, stripped.len() as u64);
        assert_eq!(entries.crc, CRC32.checksum(&stripped));
        assert_eq!(read_member(&output, "cart.bin"), stripped);
    }

    /// The zip64 decision uses the deflate worst-case slack
    /// (`raw + raw/1000 + 64 KiB`), never the actual compressed size, so a
    /// deflate output over 32 bits never hard-errors.
    #[test]
    fn zip64_decision_uses_worst_case_slack() {
        assert!(!entry_zip64(0));
        assert!(!entry_zip64(4_000_000_000));
        // One below the exact boundary: raw + raw/1000 + 64 KiB = 0xFFFFFFFF-1.
        assert!(!entry_zip64(4_290_611_147));
        // Exactly on it: raw + raw/1000 + 64 KiB = 0xFFFFFFFF.
        assert!(entry_zip64(4_290_611_148));
        assert!(entry_zip64(u64::from(u32::MAX) + 1));
    }

    #[test]
    fn plain_zip_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let source = dir.path().join("cart.gba");
        std::fs::write(&source, b"cartridge bytes").expect("write source");
        let output = dir.path().join("plain.zip");
        crate::util::write_zip(
            &source,
            "cart.gba",
            &output,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write zip");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    #[test]
    fn corrupted_comment_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // Flip one comment hex digit; the central-directory checksum no
        // longer matches.
        let mut bytes = std::fs::read(&output).expect("read archive");
        let last = bytes.len() - 1;
        assert!(bytes[last].is_ascii_hexdigit());
        bytes[last] = if bytes[last] == b'0' { b'1' } else { b'0' };
        std::fs::write(&output, bytes).expect("write corrupted archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A lowercase comment checksum is rejected even though it names the
    /// right CRC.
    #[test]
    fn lowercase_comment_checksum_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"member bytes for the lowercase check");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let crc = CRC32.checksum(&bytes[cd_offset as usize..(cd_offset + cd_size) as usize]);
        let comment = format!("TORRENTZIPPED-{crc:08x}");
        assert!(
            comment != comment.to_uppercase(),
            "need a lowercase hex digit"
        );
        let start = bytes.len() - comment.len();
        bytes[start..].copy_from_slice(comment.as_bytes());
        std::fs::write(&output, bytes).expect("write lowercase archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A name that is not pure ASCII but every character of which has a
    /// CP437 byte is stored as CP437 without the UTF-8 flag, and readers
    /// decode it back.
    #[test]
    fn cp437_name_is_stored_without_the_utf8_flag_and_round_trips() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "Pok\u{e9}mon.gb", b"first member bytes");

        let written = write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // Local header flag: max compression only, name bytes CP437.
        let raw = std::fs::read(&output).expect("read archive");
        let local_flags = u16::from_le_bytes(raw[6..8].try_into().expect("flags"));
        assert_eq!(local_flags, GP_FLAG);
        assert_eq!(
            &raw[30..40],
            [b'P', b'o', b'k', 0x82, b'm', b'o', b'n', b'.', b'g', b'b']
        );

        let (format, validated) = validate_torrentzip(&output)
            .expect("validate succeeds")
            .expect("structured");
        assert_eq!(format, ZipFormat::TorrentZip);
        assert_eq!(validated, vec![written]);
        assert_eq!(validated[0].name, "Pok\u{e9}mon.gb");
        assert_eq!(
            read_member(&output, "Pok\u{e9}mon.gb"),
            b"first member bytes"
        );
    }

    /// A name with characters outside CP437 carries the UTF-8 flag in both
    /// headers, and readers decode the name as UTF-8.
    #[test]
    fn non_cp437_name_sets_the_utf8_flag_and_round_trips() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let name = "\u{30b9}\u{30fc}\u{30d1}\u{30fc}.gb";
        let a = member(dir.path(), name, b"first member bytes");

        let written = write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // Local header flag: max compression plus the UTF-8 bit.
        let raw = std::fs::read(&output).expect("read archive");
        let local_flags = u16::from_le_bytes(raw[6..8].try_into().expect("flags"));
        assert_eq!(local_flags, GP_FLAG | GP_FLAG_UTF8);

        let (format, validated) = validate_torrentzip(&output)
            .expect("validate succeeds")
            .expect("structured");
        assert_eq!(format, ZipFormat::TorrentZip);
        assert_eq!(validated, vec![written]);
        assert_eq!(validated[0].name, name);
        assert_eq!(read_member(&output, name), b"first member bytes");
    }

    /// A local header whose CRC disagrees with its central entry is not a
    /// structured zip.
    #[test]
    fn local_header_mismatch_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        bytes[14] ^= 0xff; // local header CRC
        std::fs::write(&output, bytes).expect("write corrupted archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// The UTF-8 name flag is never set on a pure-ASCII name: an archive
    /// that carries it anyway is not structured.
    #[test]
    fn utf8_flag_on_an_ascii_name_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        restamp_with_flags(&output, GP_FLAG | GP_FLAG_UTF8);

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// The member's data ends exactly where the central directory begins:
    /// a gap in between is not a structured zip.
    #[test]
    fn gap_before_the_central_directory_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        let (_, cd_offset) = eocd_fields(&bytes);
        let cd_at = cd_offset as usize;
        bytes.insert(cd_at, 0x00); // one junk byte between member and CD
        let eocd = bytes.len() - 44;
        bytes[eocd + 16..eocd + 20].copy_from_slice(&(cd_offset + 1).to_le_bytes());
        std::fs::write(&output, bytes).expect("write gapped archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// The zip64 EOCD record carries the 4-byte signature and 56-byte
    /// layout the format pins, followed by the locator.
    #[test]
    fn zip64_eocd_record_has_the_pinned_layout() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("tail.bin");
        let mut file = std::fs::File::create(&path).expect("create file");

        write_zip64_eocd(&mut file, 0x1234, 0x5678).expect("write zip64 tail");

        let bytes = std::fs::read(&path).expect("read tail");
        assert_eq!(bytes.len(), ZIP64_EOCD_BYTES + ZIP64_LOCATOR_BYTES);
        assert_eq!(&bytes[0..4], &ZIP64_EOCD_SIG.to_le_bytes());
        assert_eq!(&bytes[4..12], &44u64.to_le_bytes());
        assert_eq!(&bytes[12..14], &VERSION_NEEDED_ZIP64.to_le_bytes());
        assert_eq!(&bytes[14..16], &VERSION_NEEDED_ZIP64.to_le_bytes());
        assert_eq!(&bytes[24..32], &1u64.to_le_bytes());
        assert_eq!(&bytes[32..40], &1u64.to_le_bytes());
        assert_eq!(&bytes[40..48], &0x5678u64.to_le_bytes());
        assert_eq!(&bytes[48..56], &0x1234u64.to_le_bytes());
        // The locator follows and points back at the record.
        assert_eq!(&bytes[56..60], &ZIP64_LOCATOR_SIG.to_le_bytes());
        assert_eq!(&bytes[64..72], &0u64.to_le_bytes());
    }

    /// Sentinel EOCD fields on an archive whose resolved values need no
    /// zip64 tail are a forgery: the tail must match the writer's rule.
    #[test]
    fn sentinel_zip64_tail_without_need_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let bytes = spliced_with_zip64_tail(
            std::fs::read(&output).expect("read archive"),
            VERSION_NEEDED_ZIP64,
            0,
        );
        std::fs::write(&output, bytes).expect("write zip64 archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A sentinel EOCD entry count with inline directory fields is not
    /// structured: the writer writes 1 in both entry fields and reserves
    /// the sentinel pattern for the directory fields alone.
    #[test]
    fn sentinel_entry_count_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        let eocd = bytes.len() - 44;
        bytes[eocd + 8..eocd + 12].copy_from_slice(&[0xFF; 4]);
        std::fs::write(&output, bytes).expect("write sentinel-count archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A zip64 EOCD record whose declared size is not the pinned 44 bytes
    /// is not structured.
    #[test]
    fn zip64_tail_record_size_mismatch_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = spliced_with_zip64_tail(
            oversize_member_bytes(
                std::fs::read(&output).expect("read archive"),
                U32_SENTINEL + 1,
            ),
            VERSION_NEEDED_ZIP64,
            0,
        );
        let record_at = bytes.len() - (ZIP64_EOCD_BYTES + ZIP64_LOCATOR_BYTES + EOCD_TOTAL_BYTES);
        bytes[record_at + 4..record_at + 12].copy_from_slice(&0u64.to_le_bytes());
        std::fs::write(&output, bytes).expect("write sized archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A lone sentinel directory field with the other inline is not
    /// structured: the sentinel offset cannot resolve, so the directory
    /// does not end where the EOCD begins.
    #[test]
    fn lone_sentinel_directory_field_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // A valid zip64 tail, then the directory size field written back
        // inline while the offset stays a sentinel.
        let mut bytes = spliced_with_zip64_tail(
            oversize_member_bytes(
                std::fs::read(&output).expect("read archive"),
                U32_SENTINEL + 1,
            ),
            VERSION_NEEDED_ZIP64,
            0,
        );
        let record_at = bytes.len() - (ZIP64_EOCD_BYTES + ZIP64_LOCATOR_BYTES + EOCD_TOTAL_BYTES);
        let cd_size = u64::from_le_bytes(
            bytes[record_at + 40..record_at + 48]
                .try_into()
                .expect("fixed-size slice"),
        );
        let eocd = bytes.len() - 44;
        bytes[eocd + 12..eocd + 16].copy_from_slice(&(cd_size as u32).to_le_bytes());
        std::fs::write(&output, bytes).expect("write lone-sentinel archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }
    /// A central zip64 extra value below the 32-bit sentinel is a forgery:
    /// a raw size that still fits the field is stored inline with an empty
    /// extra, never as sentinel plus value.
    #[test]
    fn sub_sentinel_zip64_extra_value_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // The zip64 line value: over the slack-band edge, under the
        // sentinel, so the writer would keep it inline.
        let bytes = spliced_with_zip64_tail(
            oversize_member_bytes(std::fs::read(&output).expect("read archive"), 4_290_611_148),
            VERSION_NEEDED_ZIP64,
            0,
        );
        std::fs::write(&output, bytes).expect("write sub-sentinel archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A local header on a zip64 member with only one size sentinel (the
    /// other written inline) is not structured: both go together.
    #[test]
    fn half_sentinel_local_on_zip64_member_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // The compressed size is small: writing it inline while the raw
        // size stays a sentinel splits the pair.
        let bytes = oversize_member_bytes(
            std::fs::read(&output).expect("read archive"),
            U32_SENTINEL + 1,
        );
        let (_, cd_offset) = eocd_fields(&bytes);
        let real_comp = u32::from_le_bytes(
            bytes[cd_offset as usize + 20..cd_offset as usize + 24]
                .try_into()
                .expect("fixed-size slice"),
        );
        let mut bytes = spliced_with_zip64_tail(bytes, VERSION_NEEDED_ZIP64, 0);
        bytes[18..22].copy_from_slice(&real_comp.to_le_bytes());
        std::fs::write(&output, bytes).expect("write half-sentinel archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A member over the zip64 line writes the full zip64 tail and reads
    /// back through the validator with its padded size intact.
    #[test]
    fn zip64_member_round_trips() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let source = dir.path().join("cart.bin");
        std::fs::write(&source, b"cart payload").expect("write source");

        let written = write_torrentzip(
            &ZipMember {
                name: "cart.bin".to_string(),
                path: source,
                skip: 0,
                pad: U32_SENTINEL, // one step past the 32-bit sentinel
                fill: 0,
            },
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        assert_eq!(written.size, U32_SENTINEL + b"cart payload".len() as u64);
        let (format, validated) = validate_torrentzip(&output)
            .expect("validate succeeds")
            .expect("structured");
        assert_eq!(format, ZipFormat::TorrentZip);
        assert_eq!(validated, vec![written]);
    }

    /// A central header with any of the four tail fields nonzero (here the
    /// external attributes) is not structured.
    #[test]
    fn nonzero_central_attribute_fields_are_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let cd_at = cd_offset as usize;
        bytes[cd_at + 38..cd_at + 42].copy_from_slice(&0x20u32.to_le_bytes());
        let crc = CRC32.checksum(&bytes[cd_at..cd_at + cd_size as usize]);
        let comment = format!("TORRENTZIPPED-{crc:08X}");
        let start = bytes.len() - comment.len();
        bytes[start..].copy_from_slice(comment.as_bytes());
        std::fs::write(&output, bytes).expect("write attributed archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A local header whose flag word differs from its central entry is
    /// not structured.
    #[test]
    fn local_flag_mismatch_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        bytes[6..8].copy_from_slice(&GP_FLAG_UTF8.to_le_bytes()); // local only
        std::fs::write(&output, bytes).expect("write flagged archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A local header whose name differs by one byte from its central
    /// entry is not structured.
    #[test]
    fn local_name_mismatch_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        bytes[30] = b'x'; // local name, central directory untouched
        std::fs::write(&output, bytes).expect("write renamed archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A locator whose record offset overflows the bound is not a
    /// structured zip, and the check must not panic on it.
    #[test]
    fn zip64_locator_offset_overflow_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let bytes = std::fs::read(&output).expect("read archive");
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let eocd_at = bytes.len() - 44;

        let mut tail = zip64_tail(
            cd_size,
            u64::from(cd_offset),
            u64::MAX - 56,
            VERSION_NEEDED_ZIP64,
            0,
        );
        let mut eocd = bytes[eocd_at..].to_vec();
        eocd[8..12].copy_from_slice(&[1, 0, 1, 0]);
        eocd[12..16].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        eocd[16..20].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        let mut spliced = bytes[..eocd_at].to_vec();
        spliced.append(&mut tail);
        spliced.extend_from_slice(&eocd);
        std::fs::write(&output, spliced).expect("write overflowing archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A zip64 EOCD record and locator closing an archive whose central
    /// directory is `cd_size` bytes at `cd_offset`, with the locator's
    /// record offset, record version and locator disk overridden.
    fn zip64_tail(
        cd_size: u32,
        cd_offset: u64,
        locator_offset: u64,
        record_version: u16,
        locator_disk: u32,
    ) -> Vec<u8> {
        let mut tail = Vec::new();
        tail.extend_from_slice(&ZIP64_EOCD_SIG.to_le_bytes());
        tail.extend_from_slice(&44u64.to_le_bytes());
        tail.extend_from_slice(&record_version.to_le_bytes());
        tail.extend_from_slice(&record_version.to_le_bytes());
        tail.extend_from_slice(&0u32.to_le_bytes());
        tail.extend_from_slice(&0u32.to_le_bytes());
        tail.extend_from_slice(&1u64.to_le_bytes());
        tail.extend_from_slice(&1u64.to_le_bytes());
        tail.extend_from_slice(&u64::from(cd_size).to_le_bytes());
        tail.extend_from_slice(&cd_offset.to_le_bytes());
        tail.extend_from_slice(&ZIP64_LOCATOR_SIG.to_le_bytes());
        tail.extend_from_slice(&locator_disk.to_le_bytes());
        tail.extend_from_slice(&locator_offset.to_le_bytes());
        tail.extend_from_slice(&1u32.to_le_bytes());
        tail
    }

    /// A zip64 tail on an archive that needs none is not structured: the
    /// directory must end exactly where the EOCD begins.
    #[test]
    fn unneeded_zip64_tail_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let bytes = std::fs::read(&output).expect("read archive");
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let eocd_at = bytes.len() - 44;
        let mut tail = zip64_tail(
            cd_size,
            u64::from(cd_offset),
            eocd_at as u64,
            VERSION_NEEDED_ZIP64,
            0,
        );
        let mut spliced = bytes[..eocd_at].to_vec();
        spliced.append(&mut tail);
        spliced.extend_from_slice(&bytes[eocd_at..]);
        std::fs::write(&output, spliced).expect("write spliced archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A zip64 EOCD record with eight junk bytes between it and its
    /// locator is not structured: the record must sit right against the
    /// locator that names it.
    #[test]
    fn displaced_zip64_record_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // A member that needs the tail, so the rejection cannot come from
        // the tail-requirement rule, with eight junk bytes between the
        // record and the locator.
        let mut bytes = spliced_with_zip64_tail(
            oversize_member_bytes(
                std::fs::read(&output).expect("read archive"),
                U32_SENTINEL + 1,
            ),
            VERSION_NEEDED_ZIP64,
            0,
        );
        let locator_at = bytes.len() - (ZIP64_LOCATOR_BYTES + EOCD_TOTAL_BYTES);
        bytes.splice(locator_at..locator_at, [0u8; 8]);
        std::fs::write(&output, bytes).expect("write displaced archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// Junk between the end of the central directory and the EOCD is not a
    /// structured zip.
    #[test]
    fn junk_after_the_central_directory_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let cd_end = cd_offset as usize + cd_size as usize;
        bytes.insert(cd_end, 0x00);
        std::fs::write(&output, bytes).expect("write padded archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A local header with only one 32-bit sentinel size is not
    /// structured: both sizes go to the zip64 extra together.
    #[test]
    fn half_zip64_local_header_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        bytes[18..22].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        std::fs::write(&output, bytes).expect("write half-zip64 archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A name CP437 can represent is never stored as UTF-8 with the flag:
    /// such a name is not structured.
    #[test]
    fn utf8_stored_cp437_name_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        // "x\u{2603}yyyyyyy" and "Pok\u{e9}mon.gb" are both 11 UTF-8
        // bytes; only the first needs the flag.
        let a = member(dir.path(), "x\u{2603}yyyyyyy", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        restamp_name(&output, "Pok\u{e9}mon.gb");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A sentinel EOCD entry count with a zip64 tail is not structured:
    /// the tail resolves the entry count from the record, and both plain
    /// entry fields stay 1.
    #[test]
    fn sentinel_entry_count_with_a_zip64_tail_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = spliced_with_zip64_tail(
            oversize_member_bytes(
                std::fs::read(&output).expect("read archive"),
                U32_SENTINEL + 1,
            ),
            VERSION_NEEDED_ZIP64,
            0,
        );
        let eocd = bytes.len() - EOCD_TOTAL_BYTES;
        bytes[eocd + 8..eocd + 12].copy_from_slice(&[0xFF; 4]);
        std::fs::write(&output, bytes).expect("write sentinel-count archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A central header carrying the zip64 extra twice is not structured:
    /// the writer emits it once.
    #[test]
    fn duplicated_central_zip64_extra_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        // A slack-band member whose central extra is doubled.
        let mut surgered =
            slack_band_member_bytes(std::fs::read(&output).expect("read archive"), 4_290_611_148);
        let (cd_size, cd_offset) = eocd_fields(&surgered);
        let cd_at = cd_offset as usize;
        let name_len = u16::from_le_bytes(
            surgered[cd_at + 28..cd_at + 30]
                .try_into()
                .expect("name length"),
        );
        let cd_extra_at = cd_at + CENTRAL_HEADER_BYTES + name_len as usize;
        surgered[cd_at + 30..cd_at + 32].copy_from_slice(&8u16.to_le_bytes());
        surgered.splice(cd_extra_at + 4..cd_extra_at + 4, [0x01, 0x00, 0x00, 0x00]);
        let cd_size = cd_size + 4;
        let eocd = surgered.len() - EOCD_TOTAL_BYTES;
        surgered[eocd + 12..eocd + 16].copy_from_slice(&cd_size.to_le_bytes());
        let crc = CRC32.checksum(&surgered[cd_at..cd_at + cd_size as usize]);
        let comment = format!("TORRENTZIPPED-{crc:08X}");
        let start = surgered.len() - comment.len();
        surgered[start..].copy_from_slice(comment.as_bytes());
        let bytes = spliced_with_zip64_tail(surgered, VERSION_NEEDED_ZIP64, 0);
        std::fs::write(&output, bytes).expect("write doubled-extra archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A slack-band member (over the deflate worst-case line, under the
    /// 32-bit sentinel) stores both sizes inline with an empty central
    /// zip64 extra, and reads back through the validator with its size.
    #[test]
    fn slack_band_member_round_trips() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let bytes = spliced_with_zip64_tail(
            slack_band_member_bytes(std::fs::read(&output).expect("read archive"), 4_290_611_148),
            VERSION_NEEDED_ZIP64,
            0,
        );
        std::fs::write(&output, bytes).expect("write slack-band archive");

        let (format, validated) = validate_torrentzip(&output)
            .expect("validate succeeds")
            .expect("structured");
        assert_eq!(format, ZipFormat::TorrentZip);
        assert_eq!(validated.len(), 1);
        assert_eq!(validated[0].size, 4_290_611_148);
    }

    /// Rewrites a written archive into a slack-band zip64 member: both
    /// headers carry zip64 versions, the raw size stays inline because it
    /// fits the 32-bit field, and the central zip64 extra is the empty
    /// mark the writer emits for that band. The EOCD is restamped without
    /// a zip64 tail.
    fn slack_band_member_bytes(mut bytes: Vec<u8>, raw_size: u64) -> Vec<u8> {
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let comp = u32::from_le_bytes(bytes[18..22].try_into().expect("fixed-size slice"));
        let name_len = u16::from_le_bytes(bytes[26..28].try_into().expect("fixed-size slice"));

        // Local header: zip64 version, both size fields as sentinels, and
        // the zip64 size pair as the extra field.
        bytes[4..6].copy_from_slice(&45u16.to_le_bytes());
        bytes[18..22].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        bytes[22..26].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        let mut local_extra = Vec::new();
        local_extra.extend_from_slice(&1u16.to_le_bytes());
        local_extra.extend_from_slice(&16u16.to_le_bytes());
        local_extra.extend_from_slice(&raw_size.to_le_bytes());
        local_extra.extend_from_slice(&u64::from(comp).to_le_bytes());
        let data_at = 30 + name_len as usize;
        let local_extra_len = local_extra.len();
        bytes[28..30].copy_from_slice(&(local_extra_len as u16).to_le_bytes());
        bytes.splice(data_at..data_at, local_extra);

        // Central header: zip64 version, both sizes inline, and the empty
        // zip64 extra marking the member.
        let cd_at = cd_offset as usize + local_extra_len;
        bytes[cd_at + 6..cd_at + 8].copy_from_slice(&45u16.to_le_bytes());
        bytes[cd_at + 20..cd_at + 24].copy_from_slice(&comp.to_le_bytes());
        bytes[cd_at + 24..cd_at + 28].copy_from_slice(&(raw_size as u32).to_le_bytes());
        let cd_name_at = cd_at + CENTRAL_HEADER_BYTES + name_len as usize;
        bytes[cd_at + 30..cd_at + 32].copy_from_slice(&4u16.to_le_bytes());
        bytes.splice(cd_name_at..cd_name_at, [0x01, 0x00, 0x00, 0x00]);

        // Restamp the EOCD fields and comment for the shifted directory.
        let cd_offset = cd_offset + local_extra_len as u32;
        let cd_size = cd_size + 4;
        let eocd = bytes.len() - EOCD_TOTAL_BYTES;
        bytes[eocd + 12..eocd + 16].copy_from_slice(&cd_size.to_le_bytes());
        bytes[eocd + 16..eocd + 20].copy_from_slice(&cd_offset.to_le_bytes());
        let crc = CRC32.checksum(&bytes[cd_offset as usize..(cd_offset + cd_size) as usize]);
        let comment = format!("TORRENTZIPPED-{crc:08X}");
        let start = bytes.len() - comment.len();
        bytes[start..].copy_from_slice(comment.as_bytes());
        bytes
    }

    /// Rewrites a written single-member archive into a zip64 member: both
    /// headers carry zip64 versions, the raw size moves into a sentinel
    /// plus zip64 extras (any value; below the sentinel the layout is
    /// forged), and the EOCD size, offset and comment are restamped
    /// without adding a zip64 tail.
    fn oversize_member_bytes(mut bytes: Vec<u8>, raw_size: u64) -> Vec<u8> {
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let comp = u32::from_le_bytes(bytes[18..22].try_into().expect("fixed-size slice"));
        let name_len = u16::from_le_bytes(bytes[26..28].try_into().expect("fixed-size slice"));

        // Local header: zip64 version, both size fields as sentinels, and
        // the zip64 size pair as the extra field.
        bytes[4..6].copy_from_slice(&45u16.to_le_bytes());
        bytes[18..22].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        bytes[22..26].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        let mut local_extra = Vec::new();
        local_extra.extend_from_slice(&1u16.to_le_bytes());
        local_extra.extend_from_slice(&16u16.to_le_bytes());
        local_extra.extend_from_slice(&raw_size.to_le_bytes());
        local_extra.extend_from_slice(&u64::from(comp).to_le_bytes());
        let data_at = 30 + name_len as usize;
        let local_extra_len = local_extra.len();
        bytes[28..30].copy_from_slice(&(local_extra_len as u16).to_le_bytes());
        bytes.splice(data_at..data_at, local_extra);

        // Central header: zip64 version, raw size as the sentinel filled
        // by a zip64 extra carrying only that field.
        let cd_at = cd_offset as usize + local_extra_len;
        bytes[cd_at + 6..cd_at + 8].copy_from_slice(&45u16.to_le_bytes());
        bytes[cd_at + 24..cd_at + 28].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        let mut central_extra = Vec::new();
        central_extra.extend_from_slice(&1u16.to_le_bytes());
        central_extra.extend_from_slice(&8u16.to_le_bytes());
        central_extra.extend_from_slice(&raw_size.to_le_bytes());
        let cd_name_at = cd_at + CENTRAL_HEADER_BYTES + name_len as usize;
        let central_extra_len = central_extra.len();
        bytes[cd_at + 30..cd_at + 32].copy_from_slice(&(central_extra_len as u16).to_le_bytes());
        bytes.splice(cd_name_at..cd_name_at, central_extra);

        // Restamp the EOCD fields and comment for the shifted directory.
        let cd_offset = cd_offset + local_extra_len as u32;
        let cd_size = cd_size + central_extra_len as u32;
        let eocd = bytes.len() - 44;
        bytes[eocd + 12..eocd + 16].copy_from_slice(&cd_size.to_le_bytes());
        bytes[eocd + 16..eocd + 20].copy_from_slice(&cd_offset.to_le_bytes());
        let crc = CRC32.checksum(&bytes[cd_offset as usize..(cd_offset + cd_size) as usize]);
        let comment = format!("TORRENTZIPPED-{crc:08X}");
        let start = bytes.len() - comment.len();
        bytes[start..].copy_from_slice(comment.as_bytes());
        bytes
    }

    /// Splices a zip64 EOCD record and locator in front of the EOCD and
    /// turns the plain EOCD fields into sentinels. `record_version` and
    /// `locator_disk` go into the tail's pinned fields.
    fn spliced_with_zip64_tail(bytes: Vec<u8>, record_version: u16, locator_disk: u32) -> Vec<u8> {
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let eocd_at = bytes.len() - 44;
        let mut tail = zip64_tail(
            cd_size,
            u64::from(cd_offset),
            eocd_at as u64,
            record_version,
            locator_disk,
        );
        let mut eocd = bytes[eocd_at..].to_vec();
        // The writer's EOCD: both entry fields 1, both directory fields
        // sentinels.
        eocd[8..12].copy_from_slice(&[1, 0, 1, 0]);
        eocd[12..16].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        eocd[16..20].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        let mut spliced = bytes[..eocd_at].to_vec();
        spliced.append(&mut tail);
        spliced.extend_from_slice(&eocd);
        spliced
    }

    /// A member whose raw size can overflow a 32-bit field needs the
    /// zip64 tail even when both size fields still hold the value.
    #[test]
    fn oversize_member_without_a_zip64_tail_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let bytes = oversize_member_bytes(
            std::fs::read(&output).expect("read archive"),
            U32_SENTINEL + 1, // one step past the 32-bit sentinel
        );
        std::fs::write(&output, bytes).expect("write oversize archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// The same zip64 member with its zip64 tail reads back through the
    /// validator with the extra-supplied size.
    #[test]
    fn oversize_member_with_a_zip64_tail_round_trips() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let bytes = spliced_with_zip64_tail(
            oversize_member_bytes(
                std::fs::read(&output).expect("read archive"),
                U32_SENTINEL + 1,
            ),
            VERSION_NEEDED_ZIP64,
            0,
        );
        std::fs::write(&output, bytes).expect("write zip64 archive");

        let (format, validated) = validate_torrentzip(&output)
            .expect("validate succeeds")
            .expect("structured");
        assert_eq!(format, ZipFormat::TorrentZip);
        assert_eq!(validated.len(), 1);
        assert_eq!(validated[0].size, U32_SENTINEL + 1);
    }

    /// A zip64 EOCD record whose version fields disagree with the writer's
    /// pins is not structured.
    #[test]
    fn zip64_tail_record_field_mismatch_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let bytes = spliced_with_zip64_tail(
            oversize_member_bytes(
                std::fs::read(&output).expect("read archive"),
                U32_SENTINEL + 1,
            ),
            VERSION_NEEDED_ZIP64 - 1,
            0,
        );
        std::fs::write(&output, bytes).expect("write mismatched archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A zip64 locator naming a nonzero disk is not structured.
    #[test]
    fn zip64_tail_locator_field_mismatch_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let bytes = spliced_with_zip64_tail(
            oversize_member_bytes(
                std::fs::read(&output).expect("read archive"),
                U32_SENTINEL + 1,
            ),
            VERSION_NEEDED_ZIP64,
            1,
        );
        std::fs::write(&output, bytes).expect("write mismatched archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A central zip64 extra on a member whose size cannot overflow a
    /// 32-bit field is not structured: the extra is present exactly when
    /// the size needs it.
    #[test]
    fn stray_central_zip64_extra_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let cd_at = cd_offset as usize;
        // The zip64 version the extra's presence would pin.
        bytes[cd_at + 6..cd_at + 8].copy_from_slice(&45u16.to_le_bytes());
        let name_len = u16::from_le_bytes(
            bytes[cd_at + 28..cd_at + 30]
                .try_into()
                .expect("name length"),
        );
        let cd_name_at = cd_at + CENTRAL_HEADER_BYTES + name_len as usize;
        bytes[cd_at + 30..cd_at + 32].copy_from_slice(&4u16.to_le_bytes());
        bytes.splice(cd_name_at..cd_name_at, [0x01, 0x00, 0x00, 0x00]);
        let cd_size = cd_size + 4;
        let eocd = bytes.len() - 44;
        bytes[eocd + 12..eocd + 16].copy_from_slice(&cd_size.to_le_bytes());
        let crc = CRC32.checksum(&bytes[cd_at..cd_at + cd_size as usize]);
        let comment = format!("TORRENTZIPPED-{crc:08X}");
        let start = bytes.len() - comment.len();
        bytes[start..].copy_from_slice(comment.as_bytes());
        std::fs::write(&output, bytes).expect("write stray-extra archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// A local zip64 size pair on a member whose size cannot overflow a
    /// 32-bit field is not structured.
    #[test]
    fn stray_local_zip64_extra_is_not_structured() {
        let dir = tempdir().expect("temp dir");
        let output = dir.path().join("game.zip");
        let a = member(dir.path(), "a.bin", b"first member bytes");
        write_torrentzip(
            &a,
            &output,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write succeeds");

        let mut bytes = std::fs::read(&output).expect("read archive");
        let (_, cd_offset) = eocd_fields(&bytes);
        let comp = u32::from_le_bytes(bytes[18..22].try_into().expect("fixed-size slice"));
        let raw = u32::from_le_bytes(bytes[22..26].try_into().expect("fixed-size slice"));
        let name_len = u16::from_le_bytes(bytes[26..28].try_into().expect("fixed-size slice"));
        bytes[4..6].copy_from_slice(&45u16.to_le_bytes());
        bytes[18..22].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        bytes[22..26].copy_from_slice(&(U32_SENTINEL as u32).to_le_bytes());
        let mut extra = Vec::new();
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.extend_from_slice(&16u16.to_le_bytes());
        extra.extend_from_slice(&u64::from(raw).to_le_bytes());
        extra.extend_from_slice(&u64::from(comp).to_le_bytes());
        let extra_len = extra.len();
        bytes[28..30].copy_from_slice(&(extra_len as u16).to_le_bytes());
        bytes.splice(30 + name_len as usize..30 + name_len as usize, extra);

        let cd_offset = cd_offset + extra_len as u32;
        let eocd = bytes.len() - 44;
        bytes[eocd + 16..eocd + 20].copy_from_slice(&cd_offset.to_le_bytes());
        std::fs::write(&output, bytes).expect("write stray-local archive");

        assert_eq!(
            validate_torrentzip(&output).expect("validate succeeds"),
            None
        );
    }

    /// Re-stamps a written archive's member name with `name` (same UTF-8
    /// length) in both headers and the comment checksum.
    fn restamp_name(output: &Path, name: &str) {
        let mut bytes = std::fs::read(output).expect("read archive");
        let old_len =
            u16::from_le_bytes(bytes[26..28].try_into().expect("fixed-size slice")) as usize;
        assert_eq!(old_len, name.len(), "test needs an equal-length name");
        bytes[30..30 + old_len].copy_from_slice(name.as_bytes());
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let cd_at = cd_offset as usize;
        let cd_name_len = u16::from_le_bytes(
            bytes[cd_at + 28..cd_at + 30]
                .try_into()
                .expect("fixed-size slice"),
        ) as usize;
        assert_eq!(cd_name_len, old_len);
        let cd_name_at = cd_at + CENTRAL_HEADER_BYTES;
        bytes[cd_name_at..cd_name_at + cd_name_len].copy_from_slice(name.as_bytes());
        let crc = CRC32.checksum(&bytes[cd_at..cd_at + cd_size as usize]);
        let comment = format!("TORRENTZIPPED-{crc:08X}");
        let start = bytes.len() - comment.len();
        bytes[start..].copy_from_slice(comment.as_bytes());
        std::fs::write(output, bytes).expect("write restamped archive");
    }

    /// Re-stamps both header flags and the comment checksum, keeping the
    /// archive's structure otherwise intact.
    fn restamp_with_flags(output: &Path, flags: u16) {
        let mut bytes = std::fs::read(output).expect("read archive");
        bytes[6..8].copy_from_slice(&flags.to_le_bytes());
        let (cd_size, cd_offset) = eocd_fields(&bytes);
        let cd_at = cd_offset as usize;
        bytes[cd_at + 8..cd_at + 10].copy_from_slice(&flags.to_le_bytes());
        let crc = CRC32.checksum(&bytes[cd_at..cd_at + cd_size as usize]);
        let comment = format!("TORRENTZIPPED-{crc:08X}");
        let start = bytes.len() - comment.len();
        bytes[start..].copy_from_slice(comment.as_bytes());
        std::fs::write(output, bytes).expect("write restamped archive");
    }

    /// The EOCD's (central directory size, central directory offset).
    fn eocd_fields(bytes: &[u8]) -> (u32, u32) {
        let eocd = bytes.len() - 44;
        (
            u32::from_le_bytes(
                bytes[eocd + 12..eocd + 16]
                    .try_into()
                    .expect("fixed-size slice"),
            ),
            u32::from_le_bytes(
                bytes[eocd + 16..eocd + 20]
                    .try_into()
                    .expect("fixed-size slice"),
            ),
        )
    }
}
