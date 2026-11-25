//! Single-member archive outputs: a structured zip, 7z, tar, or tar.gz
//! holding one payload, and the check that an existing 7z or tar output
//! holds exactly the member it should. The 7z and tar writers store no
//! timestamps or owners, so the same payload always yields the same bytes.

use crate::util::archive::{ArchiveKind, CancelRead, cancelled_or, read_member};
use crate::util::verify::env_io_error;
use crate::util::{
    CancelToken, ProgressReporter, ZipFormat, ZipMember, publish_temp, scratch_output_file,
    write_torrentzip,
};
use anyhow::{Result, anyhow, bail};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// The container an archived output is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// A structured zip of the given flavour.
    Zip(ZipFormat),
    /// 7z with LZMA2.
    SevenZ,
    /// Uncompressed tar.
    Tar,
    /// Gzip-compressed tar.
    TarGz,
}

impl ArchiveFormat {
    /// The extension outputs of this format carry, without the leading dot.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Zip(_) => "zip",
            Self::SevenZ => "7z",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
        }
    }

    /// The kind the archive reader detects for this format's outputs.
    pub(crate) fn kind(self) -> ArchiveKind {
        match self {
            Self::Zip(_) => ArchiveKind::Zip,
            Self::SevenZ => ArchiveKind::SevenZ,
            Self::Tar => ArchiveKind::Tar,
            Self::TarGz => ArchiveKind::TarGz,
        }
    }
}

/// Writes `member` as the one entry of a `format` archive at `output`.
///
/// Zip goes through [`write_torrentzip`]. The other formats stream the
/// payload with per-chunk progress and cancellation into a sibling temp
/// file that is renamed into place only once the archive is complete.
///
/// # Errors
/// Errors when the source cannot be read or changes size while it is read,
/// or the archive cannot be written; cancellation surfaces as
/// [`crate::util::Cancelled`].
pub fn write_archive(
    member: &ZipMember,
    output: &Path,
    format: ArchiveFormat,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    if let ArchiveFormat::Zip(zip) = format {
        return write_torrentzip(member, output, zip, progress, cancel).map(drop);
    }
    let len = std::fs::metadata(&member.path)?.len();
    let raw_size = len.checked_sub(member.skip).ok_or_else(|| {
        anyhow!(
            "{}: header strip of {} bytes exceeds the {}-byte file",
            member.path.display(),
            member.skip,
            len
        )
    })? + member.pad;
    let mut source = File::open(&member.path)?;
    source.seek(SeekFrom::Start(member.skip))?;

    let (file, temp) = scratch_output_file(output)?.into_parts();
    progress.start(raw_size, format.ext());
    let mut payload = Payload {
        inner: CancelRead::new(
            BufReader::with_capacity(1 << 20, source)
                .chain(std::io::repeat(member.fill).take(member.pad)),
            cancel,
        ),
        progress,
        count: 0,
        unreported: 0,
    };
    write_entry(file, format, &member.name, raw_size, &mut payload)
        .map_err(|err| cancelled_or(err, cancel))?;
    if payload.count != raw_size {
        bail!(
            "{}: changed while archiving: {} bytes read, {} expected",
            member.path.display(),
            payload.count,
            raw_size
        );
    }
    progress.finish();

    publish_temp(temp, output, true)?;
    Ok(())
}

/// Payload bytes read between progress updates: the archive writers pull a
/// few KiB per read, and one update each keeps the progress channel busy.
const PROGRESS_STEP: u64 = 1 << 20;

/// The member payload as a reader that counts the bytes it yields and
/// advances `progress` by them, every [`PROGRESS_STEP`] bytes and at the end.
struct Payload<'a, R> {
    inner: R,
    progress: &'a dyn ProgressReporter,
    count: u64,
    unreported: u64,
}

impl<R: Read> Read for Payload<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.count += read as u64;
        self.unreported += read as u64;
        if self.unreported >= PROGRESS_STEP || (read == 0 && self.unreported > 0) {
            self.progress.inc(std::mem::take(&mut self.unreported));
        }
        Ok(read)
    }
}

/// Writes the one-entry archive into `file`, closing every layer
/// explicitly: the tar builder and the gzip encoder would otherwise close
/// on drop and swallow any error, publishing a truncated archive.
fn write_entry(
    file: File,
    format: ArchiveFormat,
    name: &str,
    size: u64,
    payload: impl Read,
) -> Result<()> {
    match format {
        ArchiveFormat::SevenZ => {
            // LZMA2 at preset 6 on one thread (8 MiB dictionary) is the
            // intended level; it is pinned so a dependency update cannot
            // change it and with it the output bytes.
            let mut writer = sevenz_rust2::ArchiveWriter::new(file)?;
            writer.set_content_methods(vec![
                sevenz_rust2::encoder_options::Lzma2Options::from_level(6).into(),
            ]);
            writer.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file(name), Some(payload))?;
            writer.finish()?;
        }
        ArchiveFormat::Tar => {
            let out = append_tar(BufWriter::with_capacity(1 << 20, file), name, size, payload)?;
            out.into_inner()
                .map_err(std::io::IntoInnerError::into_error)?;
        }
        ArchiveFormat::TarGz => {
            let gz = flate2::GzBuilder::new()
                .mtime(0)
                .write(file, flate2::Compression::best());
            append_tar(gz, name, size, payload)?.finish()?;
        }
        ArchiveFormat::Zip(_) => unreachable!("zip outputs are written by write_torrentzip"),
    }
    Ok(())
}

/// Writes a tar stream holding one regular file and closes it, returning
/// the writer for the caller to finish.
fn append_tar<W: Write>(out: W, name: &str, size: u64, payload: impl Read) -> std::io::Result<W> {
    let mut builder = tar::Builder::new(out);
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    builder.append_data(&mut header, name, payload)?;
    builder.into_inner()
}

/// The name and size of the entry a 7z or tar archive of `kind` holds, when
/// it holds exactly one raw entry and that entry is a regular file. Every
/// entry counts, directories and tar metadata entries included, so an
/// archive holding anything beside its member is `None`.
///
/// # Errors
/// Errors when the archive cannot be read or its 7z header is refused, or
/// when `kind` is neither 7z nor tar.
pub(crate) fn sole_member(
    path: &Path,
    kind: ArchiveKind,
    cancel: &CancelToken,
) -> Result<Option<(String, u64)>> {
    match kind {
        ArchiveKind::SevenZ => {
            let (archive, _) = crate::util::sevenz_guard::open(path)?;
            Ok(match archive.files.as_slice() {
                // An entry without a stream is an empty file.
                [entry] if !entry.is_directory() && !entry.is_anti_item() => {
                    Some((entry.name().to_string(), entry.size()))
                }
                _ => None,
            })
        }
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            let mut archive = open_sole_tar(path, kind, cancel)?;
            let Some((name, size, ())) = tar_sole_entry(&mut archive, |_| Ok(()))? else {
                return Ok(None);
            };
            // Read to the end like `member_matches` does, so an archive whose
            // gzip stream is broken past the tar end block is never copied
            // through as a sole-member archive that then fails verification.
            std::io::copy(&mut archive.into_inner(), &mut std::io::sink())?;
            Ok(Some((name, size)))
        }
        ArchiveKind::Zip | ArchiveKind::Rar => {
            bail!("{} is not a 7z or tar archive", path.display())
        }
    }
}

/// Opens a tar or tar.gz for the sole-member checks. Every gzip member is
/// decoded, so a second member reads as a second entry, the way standard
/// extractors see it.
fn open_sole_tar(
    path: &Path,
    kind: ArchiveKind,
    cancel: &CancelToken,
) -> std::io::Result<tar::Archive<CancelRead<Box<dyn Read>>>> {
    let file = File::open(path)?;
    let reader: Box<dyn Read> = if kind == ArchiveKind::TarGz {
        Box::new(flate2::read::MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };
    Ok(tar::Archive::new(CancelRead::new(reader, cancel)))
}

/// Longest GNU long name a sole-member tar may carry; a larger one is
/// refused unread.
const MAX_LONG_NAME_BYTES: u64 = 64 << 10;

/// [`sole_member`] over a tar stream, handing the entry's data to `read`.
/// Entries are walked raw, so no extension header is ever read whole: one
/// GNU long name of at most [`MAX_LONG_NAME_BYTES`] may name the file, and
/// any other extension, link or directory entry makes the archive no
/// sole-member archive.
fn tar_sole_entry<R: Read, T>(
    archive: &mut tar::Archive<R>,
    read: impl FnOnce(&mut dyn Read) -> std::io::Result<T>,
) -> std::io::Result<Option<(String, u64, T)>> {
    let mut entries = archive.entries()?.raw(true);
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    let mut entry = entry?;
    let mut long_name = None;
    if entry.header().entry_type().is_gnu_longname() {
        if entry.size() > MAX_LONG_NAME_BYTES {
            return Ok(None);
        }
        let mut name = Vec::new();
        entry.read_to_end(&mut name)?;
        let len = name
            .iter()
            .rposition(|&byte| byte != 0)
            .map_or(0, |last| last + 1);
        name.truncate(len);
        long_name = Some(String::from_utf8_lossy(&name).into_owned());
        let Some(next) = entries.next() else {
            return Ok(None);
        };
        entry = next?;
    }
    if !entry.header().entry_type().is_file() {
        return Ok(None);
    }
    let name = match long_name {
        Some(name) => name,
        None => entry.path()?.to_string_lossy().into_owned(),
    };
    let size = entry.size();
    let data = read(&mut entry)?;
    drop(entry);
    if let Some(next) = entries.next() {
        next?;
        return Ok(None);
    }
    Ok(Some((name, size, data)))
}

/// True when the 7z or tar archive of `kind` at `path` holds exactly one
/// regular file, named `member`, whose `size` bytes decode to CRC-32 `crc`.
/// `Ok(false)` is a verified mismatch, including a payload that fails to
/// decode or a tar.gz whose gzip trailer is broken; `Err` means the check
/// could not run, with the io error kind kept for the caller to classify.
/// Cancellation surfaces as [`crate::util::Cancelled`].
pub(crate) fn member_matches(
    path: &Path,
    kind: ArchiveKind,
    crc: u32,
    size: u64,
    member: &str,
    cancel: &CancelToken,
) -> Result<bool> {
    sole_member_digest(path, kind, size, member, cancel)
        .map(|digest| digest == Some((crc, size)))
        .map_err(|err| cancelled_or(err, cancel))
}

/// The CRC-32 and decoded length of the archive's member when it is the
/// archive's only entry, named `member` and declared `size` bytes; `None`
/// when it is not, or when the archive is broken (a payload that fails to
/// decode, a truncated stream). An environment read failure or a cancel is
/// an error: no verdict was reached.
fn sole_member_digest(
    path: &Path,
    kind: ArchiveKind,
    size: u64,
    member: &str,
    cancel: &CancelToken,
) -> Result<Option<(u32, u64)>> {
    let digest = match kind {
        ArchiveKind::SevenZ => {
            if !matches!(sole_member(path, kind, cancel)?, Some((name, found)) if name == member && found == size)
            {
                return Ok(None);
            }
            read_member(path, kind, member, 0, cancel, |rd| Ok(fold_crc(rd)))?.map(Some)
        }
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            tar_member_digest(path, kind, size, member, cancel)
        }
        ArchiveKind::Zip | ArchiveKind::Rar => {
            bail!("{} is not a 7z or tar archive", path.display())
        }
    };
    match digest {
        Err(err) if !env_io_error(&err) && !cancel.is_cancelled() => Ok(None),
        digest => Ok(digest?),
    }
}

/// [`sole_member_digest`] for a tar or tar.gz, read on to the end of the
/// stream.
fn tar_member_digest(
    path: &Path,
    kind: ArchiveKind,
    size: u64,
    member: &str,
    cancel: &CancelToken,
) -> std::io::Result<Option<(u32, u64)>> {
    let mut archive = open_sole_tar(path, kind, cancel)?;
    let Some((name, found, digest)) = tar_sole_entry(&mut archive, |rd| fold_crc(rd))? else {
        return Ok(None);
    };
    if name != member || found != size {
        return Ok(None);
    }
    // Tar reading stops at the first end-of-archive block; reading on to the
    // end of the stream makes gzip check every member's CRC-32 and length
    // trailer.
    std::io::copy(&mut archive.into_inner(), &mut std::io::sink())?;
    Ok(Some(digest))
}

/// Folds `rd` to its end into CRC-32 and length.
fn fold_crc(rd: &mut dyn Read) -> std::io::Result<(u32, u64)> {
    let mut digest = crate::util::hash::CRC32.digest();
    let mut len = 0u64;
    let mut chunk = vec![0u8; 1 << 20];
    loop {
        let read = rd.read(&mut chunk)?;
        if read == 0 {
            return Ok((digest.finalize(), len));
        }
        digest.update(&chunk[..read]);
        len += read as u64;
    }
}

/// A file name split into stem and extension the way [`Path::file_stem`]
/// and [`Path::extension`] split it, except that a trailing `.tar.gz` (any
/// case) is one extension: `Game.tar.gz` splits into `Game` and `tar.gz`.
pub(crate) fn split_name(path: &Path) -> (Option<&str>, Option<&str>) {
    const TAR_GZ: &[u8] = b".tar.gz";
    if let Some(name) = path.file_name().and_then(OsStr::to_str)
        && name.len() > TAR_GZ.len()
        && name.as_bytes()[name.len() - TAR_GZ.len()..].eq_ignore_ascii_case(TAR_GZ)
    {
        // The suffix is ASCII, so the cut is a character boundary.
        let cut = name.len() - TAR_GZ.len();
        return (Some(&name[..cut]), Some(&name[cut + 1..]));
    }
    (
        path.file_stem().and_then(OsStr::to_str),
        path.extension().and_then(OsStr::to_str),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::verify::unverifiable;
    use crate::util::{Cancelled, NoProgress};

    const FORMATS: [ArchiveFormat; 3] = [
        ArchiveFormat::SevenZ,
        ArchiveFormat::Tar,
        ArchiveFormat::TarGz,
    ];

    /// Varied but compressible bytes, so every format has a real payload
    /// stream to corrupt.
    fn payload(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i.wrapping_mul(31) ^ (i >> 3)) as u8)
            .collect()
    }

    fn crc(bytes: &[u8]) -> u32 {
        crate::util::hash::CRC32.checksum(bytes)
    }

    /// Writes `source` to a file beside the archives and returns the member
    /// that archives it under `name`.
    fn member(dir: &Path, name: &str, source: &[u8], skip: u64, pad: u64, fill: u8) -> ZipMember {
        let path = dir.join("source.bin");
        std::fs::write(&path, source).unwrap();
        ZipMember {
            name: name.to_string(),
            path,
            skip,
            pad,
            fill,
        }
    }

    fn write(member: &ZipMember, output: &Path, format: ArchiveFormat) {
        write_archive(member, output, format, &NoProgress, &CancelToken::new()).unwrap();
    }

    fn matches(
        path: &Path,
        format: ArchiveFormat,
        crc: u32,
        size: u64,
        name: &str,
    ) -> Result<bool> {
        member_matches(path, format.kind(), crc, size, name, &CancelToken::new())
    }

    /// What overwrite-invalid makes of a check: a rejection is a verified
    /// mismatch or a broken archive, never a pass or a check that could not
    /// run.
    fn rejected(result: Result<bool>) -> bool {
        match result {
            Ok(matched) => !matched,
            Err(err) => !unverifiable(err.as_ref()),
        }
    }

    /// Writes an archive of `entries` with another writer, `None` data
    /// marking a directory entry.
    fn write_entries(path: &Path, format: ArchiveFormat, entries: &[(&str, Option<&[u8]>)]) {
        if format == ArchiveFormat::SevenZ {
            let mut writer = sevenz_rust2::ArchiveWriter::create(path).unwrap();
            for (name, data) in entries {
                let entry = match data {
                    Some(_) => sevenz_rust2::ArchiveEntry::new_file(name),
                    None => sevenz_rust2::ArchiveEntry::new_directory(name),
                };
                writer.push_archive_entry(entry, *data).unwrap();
            }
            writer.finish().unwrap();
            return;
        }
        let file = File::create(path).unwrap();
        let out: Box<dyn Write> = if format == ArchiveFormat::TarGz {
            Box::new(flate2::write::GzEncoder::new(
                file,
                flate2::Compression::fast(),
            ))
        } else {
            Box::new(file)
        };
        let mut builder = tar::Builder::new(out);
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_entry_type(match data {
                Some(_) => tar::EntryType::Regular,
                None => tar::EntryType::Directory,
            });
            let data = data.unwrap_or_default();
            header.set_size(data.len() as u64);
            builder.append_data(&mut header, name, data).unwrap();
        }
        builder.into_inner().unwrap();
    }

    /// Each format reads back as the one planned member, with the header
    /// strip and pad applied, and a rewrite of the same member is
    /// byte-identical; a wrong name or CRC is rejected.
    #[test]
    fn formats_round_trip_with_strip_and_pad_and_rewrite_identically() {
        let dir = tempfile::tempdir().unwrap();
        let body = payload(64 * 1024);
        let mut source = b"HDR".to_vec();
        source.extend_from_slice(&body);
        let member = member(dir.path(), "Game.gba", &source, 3, 5, 0xFF);
        let mut expected = body.clone();
        expected.extend_from_slice(&[0xFF; 5]);
        let size = expected.len() as u64;

        for format in FORMATS {
            let output = dir.path().join(format!("Game.{}", format.ext()));
            write(&member, &output, format);
            assert_eq!(
                sole_member(&output, format.kind(), &CancelToken::new()).unwrap(),
                Some(("Game.gba".to_string(), size)),
                "{format:?}"
            );
            assert!(
                matches(&output, format, crc(&expected), size, "Game.gba").unwrap(),
                "{format:?}"
            );
            assert!(rejected(matches(
                &output,
                format,
                crc(&expected) ^ 1,
                size,
                "Game.gba"
            )));
            assert!(rejected(matches(
                &output,
                format,
                crc(&expected),
                size,
                "Other.gba"
            )));

            let again = dir.path().join(format!("Again.{}", format.ext()));
            write(&member, &again, format);
            assert_eq!(
                std::fs::read(&output).unwrap(),
                std::fs::read(&again).unwrap(),
                "{format:?}"
            );
        }
    }

    /// An empty payload and a member name longer than a tar header's name
    /// field both read back unchanged.
    #[test]
    fn empty_payloads_and_long_member_names_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("{}.gba", "N".repeat(120));
        for format in FORMATS {
            for (name, data) in [("Empty.gba", Vec::new()), (long.as_str(), payload(1000))] {
                let member = member(dir.path(), name, &data, 0, 0, 0);
                let output = dir.path().join(format!("out.{}", format.ext()));
                write(&member, &output, format);
                assert_eq!(
                    sole_member(&output, format.kind(), &CancelToken::new()).unwrap(),
                    Some((name.to_string(), data.len() as u64)),
                    "{format:?} {name}"
                );
                assert!(
                    matches(&output, format, crc(&data), data.len() as u64, name).unwrap(),
                    "{format:?} {name}"
                );
            }
        }
    }

    /// Anything beside the member, a second file or a directory entry the
    /// member listing would hide, makes the archive no sole-member archive.
    #[test]
    fn a_second_entry_or_a_directory_entry_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let data = payload(1000);
        let size = data.len() as u64;
        for format in FORMATS {
            let output = dir.path().join(format!("out.{}", format.ext()));
            for entries in [
                [
                    ("Game.gba", Some(data.as_slice())),
                    ("Extra.txt", Some(b"x".as_slice())),
                ],
                [("extra", None), ("Game.gba", Some(data.as_slice()))],
            ] {
                write_entries(&output, format, &entries);
                assert_eq!(
                    sole_member(&output, format.kind(), &CancelToken::new()).unwrap(),
                    None,
                    "{format:?}"
                );
                assert!(rejected(matches(
                    &output,
                    format,
                    crc(&data),
                    size,
                    "Game.gba"
                )));
            }
            write_entries(&output, format, &[("Game.gba", None)]);
            assert_eq!(
                sole_member(&output, format.kind(), &CancelToken::new()).unwrap(),
                None,
                "{format:?}"
            );
        }
    }

    /// A flipped byte inside the stored payload is rejected even though the
    /// archive's listing still names the right member and size.
    #[test]
    fn a_corrupt_payload_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let data = payload(64 * 1024);
        let member = member(dir.path(), "Game.gba", &data, 0, 0, 0);
        for format in FORMATS {
            let output = dir.path().join(format!("Game.{}", format.ext()));
            write(&member, &output, format);
            let mut bytes = std::fs::read(&output).unwrap();
            // Past the 7z signature header, the tar header, and the gzip
            // header: inside each format's payload stream.
            let at = match format {
                ArchiveFormat::TarGz => bytes.len() / 2,
                _ => 512 + 64,
            };
            bytes[at] ^= 0xFF;
            std::fs::write(&output, &bytes).unwrap();
            assert!(
                rejected(matches(
                    &output,
                    format,
                    crc(&data),
                    data.len() as u64,
                    "Game.gba"
                )),
                "{format:?}"
            );
        }
    }

    /// The tar inside a tar.gz ends before the gzip trailer, so a broken
    /// trailer is only caught by reading the stream to its end.
    #[test]
    fn a_broken_gzip_trailer_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let data = payload(4096);
        let member = member(dir.path(), "Game.gba", &data, 0, 0, 0);
        let output = dir.path().join("Game.tar.gz");
        write(&member, &output, ArchiveFormat::TarGz);
        let mut bytes = std::fs::read(&output).unwrap();
        let crc_at = bytes.len() - 8;
        bytes[crc_at] ^= 0xFF;
        std::fs::write(&output, &bytes).unwrap();
        assert!(matches!(
            matches(
                &output,
                ArchiveFormat::TarGz,
                crc(&data),
                data.len() as u64,
                "Game.gba"
            ),
            Ok(false)
        ));
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// A tar.gz whose second gzip member holds another entry is no
    /// sole-member archive, even though its first member ends at an entry
    /// boundary with no end-of-archive blocks.
    #[test]
    fn a_second_gzip_member_is_a_second_entry() {
        let dir = tempfile::tempdir().unwrap();
        let data = payload(1000);
        let tar_of = |close: bool| {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, "Game.gba", data.as_slice())
                .unwrap();
            if close {
                builder.into_inner().unwrap()
            } else {
                builder.get_ref().clone()
            }
        };
        let mut bytes = gzip(&tar_of(false));
        bytes.extend(gzip(&tar_of(true)));
        let output = dir.path().join("Game.tar.gz");
        std::fs::write(&output, bytes).unwrap();

        let format = ArchiveFormat::TarGz;
        assert_eq!(
            sole_member(&output, format.kind(), &CancelToken::new()).unwrap(),
            None
        );
        assert!(matches!(
            matches(&output, format, crc(&data), data.len() as u64, "Game.gba"),
            Ok(false)
        ));
    }

    /// A GNU long name larger than any member name is refused before its
    /// body is read, whatever size it declares.
    #[test]
    fn an_oversized_long_name_header_is_rejected_unread() {
        let dir = tempfile::tempdir().unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::GNULongName);
        header.set_size(8 << 30);
        header.set_cksum();
        for format in [ArchiveFormat::Tar, ArchiveFormat::TarGz] {
            let output = dir.path().join(format!("Game.{}", format.ext()));
            let bytes = match format {
                ArchiveFormat::TarGz => gzip(header.as_bytes()),
                _ => header.as_bytes().to_vec(),
            };
            std::fs::write(&output, bytes).unwrap();
            assert_eq!(
                sole_member(&output, format.kind(), &CancelToken::new()).unwrap(),
                None,
                "{format:?}"
            );
            assert!(
                matches!(matches(&output, format, 0, 0, "Game.gba"), Ok(false)),
                "{format:?}"
            );
        }
    }

    /// A GNU long name over the cap is refused even when a regular file
    /// follows it, which an uncapped read would accept as the sole member.
    #[test]
    fn a_long_name_over_the_cap_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let data = payload(1024);
        let name = format!("{}.gba", "a".repeat(MAX_LONG_NAME_BYTES as usize));
        for format in [ArchiveFormat::Tar, ArchiveFormat::TarGz] {
            let output = dir.path().join(format!("Game.{}", format.ext()));
            write_entries(&output, format, &[(&name, Some(&data))]);
            assert_eq!(
                sole_member(&output, format.kind(), &CancelToken::new()).unwrap(),
                None,
                "{format:?}"
            );
            assert!(
                matches!(
                    matches(&output, format, crc(&data), data.len() as u64, &name),
                    Ok(false)
                ),
                "{format:?}"
            );
        }
    }

    /// A tar.gz cut short, inside its deflate stream or inside its gzip
    /// trailer, is a broken archive: a verified mismatch, not a check that
    /// could not run.
    #[test]
    fn a_truncated_tar_gz_is_a_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let data = payload(64 * 1024);
        let member = member(dir.path(), "Game.gba", &data, 0, 0, 0);
        let output = dir.path().join("Game.tar.gz");
        write(&member, &output, ArchiveFormat::TarGz);
        let bytes = std::fs::read(&output).unwrap();
        for cut in [bytes.len() / 2, bytes.len() - 4] {
            std::fs::write(&output, &bytes[..cut]).unwrap();
            assert!(
                matches!(
                    matches(
                        &output,
                        ArchiveFormat::TarGz,
                        crc(&data),
                        data.len() as u64,
                        "Game.gba"
                    ),
                    Ok(false)
                ),
                "cut at {cut}"
            );
        }
    }

    /// A cancelled write surfaces as the cancel and leaves neither the
    /// output nor its scratch file behind.
    #[test]
    fn a_cancelled_write_removes_its_scratch_file() {
        let source = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let member = member(source.path(), "Game.gba", &payload(4096), 0, 0, 0);
        let cancel = CancelToken::new();
        cancel.cancel();
        for format in FORMATS {
            let output = out.path().join(format!("Game.{}", format.ext()));
            let err = write_archive(&member, &output, format, &NoProgress, &cancel).unwrap_err();
            assert!(err.is::<Cancelled>(), "{format:?}: {err:#}");
            assert_eq!(
                std::fs::read_dir(out.path()).unwrap().count(),
                0,
                "{format:?}"
            );
        }
    }

    #[test]
    fn split_name_keeps_tar_gz_together() {
        let split = |name: &str| {
            let (stem, ext) = split_name(Path::new(name));
            (stem.map(str::to_string), ext.map(str::to_string))
        };
        let owned =
            |stem: &str, ext: Option<&str>| (Some(stem.to_string()), ext.map(str::to_string));
        assert_eq!(split("Game.tar.gz"), owned("Game", Some("tar.gz")));
        assert_eq!(split("Game.TAR.GZ"), owned("Game", Some("TAR.GZ")));
        assert_eq!(split("Game.v1.zip"), owned("Game.v1", Some("zip")));
        assert_eq!(split("Game.gz"), owned("Game", Some("gz")));
        assert_eq!(split("Game"), owned("Game", None));
        assert_eq!(split(".tar.gz"), owned(".tar", Some("gz")));
    }
}
