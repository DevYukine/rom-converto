//! Transparent archive input. Read-side commands accept a zip, 7z, rar, tar,
//! or tar.gz container holding a supported image and operate on the first
//! member matching the caller's extension set. The member (and, for a cue
//! sheet, the bin tracks it references) is extracted to a temp dir that is
//! removed when the returned [`ResolvedInput`] is dropped. Rar support is
//! read-only and binds the vendored unrar sources, which are free to use but
//! not OSI-permissive.

use crate::util::fs::{has_any_extension, is_os_junk_file};
use crate::util::{DEFAULT_SPACE_HEADROOM, available_space, format_bytes, space_shortfall};
use anyhow::{Result, anyhow, bail};
use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};

/// Extensions the recursive walker treats as archives. Matches the form
/// [`Path::extension`] returns, so `foo.tar.gz` is caught by `gz` and refined
/// to a gzipped tar by [`kind_of`].
pub const ARCHIVE_EXTS: &[&str] = &["zip", "7z", "tar", "tgz", "gz", "rar"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    Zip,
    SevenZ,
    Tar,
    TarGz,
    Rar,
}

/// A single non-junk file inside an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveMember {
    /// Member path as stored in the archive (forward-slash separated).
    pub name: String,
    /// Uncompressed size in bytes.
    pub size: u64,
}

/// True when `path` carries an extension the archive layer recognizes. A bare
/// `.gz` (a single gzipped file with no tar container) matches here but is
/// reported unsupported once opened.
pub fn is_archive_path(path: &Path) -> bool {
    has_any_extension(path, ARCHIVE_EXTS)
}

fn kind_of(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return Some(ArchiveKind::TarGz);
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "zip" => Some(ArchiveKind::Zip),
        "7z" => Some(ArchiveKind::SevenZ),
        "tar" => Some(ArchiveKind::Tar),
        "rar" => Some(ArchiveKind::Rar),
        _ => None,
    }
}

fn basename(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// Basename of a member path, rejecting names that would escape the extraction
/// directory (empty, `.`, `..`).
fn safe_basename(name: &str) -> Result<String> {
    let base = basename(name);
    if base.is_empty() || base == "." || base == ".." {
        bail!("unsafe archive member name: {name}");
    }
    Ok(base.to_string())
}

/// Case-insensitive extension match against a member name, mirroring
/// [`has_any_extension`] but operating on an in-archive path string.
fn name_has_ext(name: &str, exts: &[&str]) -> bool {
    match basename(name).rsplit_once('.') {
        Some((_, e)) => exts.iter().any(|want| e.eq_ignore_ascii_case(want)),
        None => false,
    }
}

fn keep_member(name: &str, size: u64, is_dir: bool) -> Option<ArchiveMember> {
    if is_dir {
        return None;
    }
    let base = basename(name);
    if base.is_empty() || is_os_junk_file(base) {
        return None;
    }
    // Nested archives are hidden the same way the recursive walker hides them.
    if name_has_ext(name, ARCHIVE_EXTS) {
        return None;
    }
    Some(ArchiveMember {
        name: name.to_string(),
        size,
    })
}

/// List the convertible members of an archive: regular files only, with OS
/// junk and nested archives filtered out, sorted by name for deterministic
/// first-match selection. Bare gzip returns a clear unsupported error.
pub fn list_members(path: &Path) -> Result<Vec<ArchiveMember>> {
    let kind = kind_of(path).ok_or_else(|| {
        anyhow!(
            "gzip archives without a tar container are not supported, extract the file first: {}",
            path.display()
        )
    })?;
    let mut out = match kind {
        ArchiveKind::Zip => list_zip(path)?,
        ArchiveKind::SevenZ => list_7z(path)?,
        ArchiveKind::Tar => list_tar(path, false)?,
        ArchiveKind::TarGz => list_tar(path, true)?,
        ArchiveKind::Rar => list_rar(path)?,
    };
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn list_zip(path: &Path) -> Result<Vec<ArchiveMember>> {
    let mut zip = zip::ZipArchive::new(File::open(path)?)?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        // enclosed_name is None for absolute or parent-traversing paths.
        if entry.enclosed_name().is_none() {
            continue;
        }
        if let Some(m) = keep_member(entry.name(), entry.size(), entry.is_dir()) {
            out.push(m);
        }
    }
    Ok(out)
}

fn list_7z(path: &Path) -> Result<Vec<ArchiveMember>> {
    let reader = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())?;
    let mut out = Vec::new();
    for entry in &reader.archive().files {
        if let Some(m) = keep_member(entry.name(), entry.size(), entry.is_directory()) {
            out.push(m);
        }
    }
    Ok(out)
}

fn list_rar(path: &Path) -> Result<Vec<ArchiveMember>> {
    let archive = unrar::Archive::new(path).open_for_listing()?;
    let mut out = Vec::new();
    for entry in archive {
        let entry = entry?;
        let name = entry.filename.to_string_lossy().replace('\\', "/");
        if let Some(m) = keep_member(&name, entry.unpacked_size, entry.is_directory()) {
            out.push(m);
        }
    }
    Ok(out)
}

fn open_tar(path: &Path, gz: bool) -> Result<tar::Archive<Box<dyn std::io::Read>>> {
    let file = File::open(path)?;
    let reader: Box<dyn std::io::Read> = if gz {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    Ok(tar::Archive::new(reader))
}

fn list_tar(path: &Path, gz: bool) -> Result<Vec<ArchiveMember>> {
    let mut archive = open_tar(path, gz)?;
    let mut out = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        let is_file = entry.header().entry_type().is_file();
        let name = entry.path()?.to_string_lossy().replace('\\', "/");
        if let Some(m) = keep_member(&name, entry.size(), !is_file) {
            out.push(m);
        }
    }
    Ok(out)
}

/// Extract one member to `dest_dir`, returning the written path. Streams so a
/// multi-gigabyte member never buffers in memory.
fn extract_one(
    path: &Path,
    kind: ArchiveKind,
    member_name: &str,
    dest_dir: &Path,
) -> Result<PathBuf> {
    match kind {
        ArchiveKind::Zip => {
            let mut zip = zip::ZipArchive::new(File::open(path)?)?;
            let mut entry = zip.by_name(member_name)?;
            let out = dest_dir.join(safe_basename(member_name)?);
            let mut writer = File::create(&out)?;
            std::io::copy(&mut entry, &mut writer)?;
            Ok(out)
        }
        ArchiveKind::SevenZ => {
            let mut reader =
                sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())?;
            let out = dest_dir.join(safe_basename(member_name)?);
            let mut written: Option<PathBuf> = None;
            let mut io_err: Option<std::io::Error> = None;
            reader.for_each_entries(|entry, rd| {
                if entry.name() == member_name {
                    match File::create(&out).and_then(|mut f| std::io::copy(rd, &mut f).map(|_| ()))
                    {
                        Ok(()) => written = Some(out.clone()),
                        Err(e) => io_err = Some(e),
                    }
                    return Ok(false);
                }
                Ok(true)
            })?;
            if let Some(e) = io_err {
                return Err(e.into());
            }
            written.ok_or_else(|| anyhow!("member {member_name} not found in {}", path.display()))
        }
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            let mut archive = open_tar(path, kind == ArchiveKind::TarGz)?;
            for entry in archive.entries()? {
                let mut entry = entry?;
                let name = entry.path()?.to_string_lossy().replace('\\', "/");
                if name == member_name {
                    let out = dest_dir.join(safe_basename(member_name)?);
                    let mut writer = File::create(&out)?;
                    std::io::copy(&mut entry, &mut writer)?;
                    return Ok(out);
                }
            }
            bail!("member {member_name} not found in {}", path.display())
        }
        ArchiveKind::Rar => {
            let out = dest_dir.join(safe_basename(member_name)?);
            let mut archive = unrar::Archive::new(path).open_for_processing()?;
            while let Some(header) = archive.read_header()? {
                let name = header.entry().filename.to_string_lossy().replace('\\', "/");
                if name == member_name {
                    header.extract_to(&out)?;
                    return Ok(out);
                }
                archive = header.skip()?;
            }
            bail!("member {member_name} not found in {}", path.display())
        }
    }
}

/// A read input resolved to a real file on disk. For a plain file this is a
/// passthrough; for an archive it holds the extracted member and a temp dir
/// that is deleted when this value drops.
#[derive(Debug)]
pub struct ResolvedInput {
    path: PathBuf,
    output_basis: PathBuf,
    _tmp: Option<tempfile::TempDir>,
}

impl ResolvedInput {
    /// Real file the pipeline should read.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Path to derive default output names from. For an archive this points
    /// next to the archive (parent dir + member basename) so results land
    /// beside the archive rather than in the temp dir.
    pub fn output_basis(&self) -> &Path {
        &self.output_basis
    }
}

fn parse_cue_file_line(rest: &str) -> Option<String> {
    let rest = rest.trim();
    if let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let end = after.find('"')?;
        return Some(after[..end].to_string());
    }
    rest.split_whitespace().next().map(str::to_string)
}

/// Basenames referenced by `FILE` lines in an extracted cue sheet.
fn cue_referenced_basenames(cue_path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(cue_path).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let upper = trimmed.get(..5).map(str::to_ascii_uppercase);
            if upper.as_deref() == Some("FILE ") {
                parse_cue_file_line(&trimmed[4..]).map(|n| basename(&n).to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Resolve a read input for `exts`. Plain files pass through unchanged. For an
/// archive, extract the first member matching `exts` (plus the bin tracks a cue
/// references) to a temp dir and point the pipeline at it.
pub fn resolve_input(path: &Path, exts: &[&str]) -> Result<ResolvedInput> {
    if !is_archive_path(path) {
        return Ok(ResolvedInput {
            path: path.to_path_buf(),
            output_basis: path.to_path_buf(),
            _tmp: None,
        });
    }

    let kind = kind_of(path).ok_or_else(|| {
        anyhow!(
            "gzip archives without a tar container are not supported, extract the file first: {}",
            path.display()
        )
    })?;
    let members = list_members(path)?;
    let matches: Vec<&ArchiveMember> = members
        .iter()
        .filter(|m| name_has_ext(&m.name, exts))
        .collect();
    let member = match matches.as_slice() {
        [] => bail!(
            "archive {} contains no matching image ({:?})",
            path.display(),
            exts
        ),
        [first, rest @ ..] => {
            if !rest.is_empty() {
                log::warn!(
                    "{} contains {} matching members; using {}",
                    path.display(),
                    matches.len(),
                    first.name
                );
            }
            *first
        }
    };

    let tmp = tempfile::tempdir()?;
    if let Ok(available) = available_space(tmp.path())
        && space_shortfall(available, member.size, DEFAULT_SPACE_HEADROOM).is_some()
    {
        bail!(
            "not enough space on the temp volume at {} to extract {}: need about {}, only {} free. Point TMPDIR at a larger volume or extract the archive first.",
            tmp.path().display(),
            format_bytes(member.size),
            format_bytes(member.size.saturating_add(DEFAULT_SPACE_HEADROOM)),
            format_bytes(available)
        );
    }
    let extracted = extract_one(path, kind, &member.name, tmp.path())?;

    if name_has_ext(&member.name, &["cue"]) {
        let wanted: HashSet<String> = cue_referenced_basenames(&extracted)
            .into_iter()
            .map(|n| n.to_ascii_lowercase())
            .collect();
        for other in &members {
            let base = basename(&other.name).to_ascii_lowercase();
            if other.name != member.name && wanted.contains(&base) {
                extract_one(path, kind, &other.name, tmp.path())?;
            }
        }
    }

    let output_basis = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(safe_basename(&member.name)?);

    Ok(ResolvedInput {
        path: extracted,
        output_basis,
        _tmp: Some(tmp),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    fn write_tar(path: &Path, gz: bool, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let writer: Box<dyn std::io::Write> = if gz {
            Box::new(flate2::write::GzEncoder::new(
                file,
                flate2::Compression::fast(),
            ))
        } else {
            Box::new(file)
        };
        let mut builder = tar::Builder::new(writer);
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap();
    }

    fn write_7z(path: &Path, entries: &[(&str, &[u8])]) {
        let mut writer = sevenz_rust2::ArchiveWriter::create(path).unwrap();
        for (name, data) in entries {
            writer
                .push_archive_entry::<&[u8]>(sevenz_rust2::ArchiveEntry::new_file(name), Some(data))
                .unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn lists_members_filtering_junk_and_nested_archives() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("lib.zip");
        write_zip(
            &zip,
            &[
                ("game.iso", b"iso-bytes"),
                ("readme.txt", b"hi"),
                (".DS_Store", b"junk"),
                ("box/art.png", b"img"),
                ("inner.zip", b"nested"),
            ],
        );
        let members = list_members(&zip).unwrap();
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"game.iso"));
        assert!(names.contains(&"readme.txt"));
        assert!(names.contains(&"box/art.png"));
        assert!(!names.contains(&".DS_Store"));
        assert!(!names.contains(&"inner.zip"));
    }

    #[test]
    fn zip_round_trip_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("g.zip");
        write_zip(&zip, &[("readme.txt", b"junk"), ("game.iso", b"payload")]);
        let resolved = resolve_input(&zip, &["iso"]).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"payload");
        assert_eq!(resolved.path().file_name().unwrap(), "game.iso");
        // Output lands next to the archive, named after the member.
        assert_eq!(resolved.output_basis(), dir.path().join("game.iso"));
    }

    #[test]
    fn tar_round_trip_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let tar = dir.path().join("g.tar");
        write_tar(&tar, false, &[("game.chd", b"chd-data")]);
        let resolved = resolve_input(&tar, &["chd"]).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"chd-data");
    }

    #[test]
    fn targz_detection_and_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let targz = dir.path().join("g.tar.gz");
        write_tar(&targz, true, &[("game.iso", b"gz-payload")]);
        assert!(is_archive_path(&targz));
        let resolved = resolve_input(&targz, &["iso"]).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"gz-payload");
    }

    #[test]
    fn sevenz_round_trip_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let sz = dir.path().join("g.7z");
        write_7z(&sz, &[("junk.txt", b"x"), ("game.iso", b"seven-payload")]);
        let members = list_members(&sz).unwrap();
        assert!(members.iter().any(|m| m.name == "game.iso"));
        let resolved = resolve_input(&sz, &["iso"]).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"seven-payload");
    }

    #[test]
    fn rejects_traversal_member_names_in_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("evil.zip");
        // A parent-traversing name is dropped by enclosed_name.
        write_zip(&zip, &[("../escape.iso", b"nope"), ("ok.iso", b"good")]);
        let members = list_members(&zip).unwrap();
        assert!(members.iter().all(|m| !m.name.contains("..")));
        let resolved = resolve_input(&zip, &["iso"]).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"good");
    }

    /// A RAR5 archive holding `game.iso` (`rar-payload`) and `readme.txt`
    /// (`hi`), stored uncompressed. Embedded because rar cannot be produced
    /// at test time: no permissively licensed encoder exists.
    const RAR_FIXTURE: &[u8] = &[
        82, 97, 114, 33, 26, 7, 1, 0, 51, 146, 181, 229, 10, 1, 5, 6, 0, 5, 1, 1, 128, 128, 0, 73,
        41, 127, 148, 36, 2, 3, 11, 139, 0, 4, 139, 0, 32, 47, 28, 138, 144, 128, 0, 0, 8, 103, 97,
        109, 101, 46, 105, 115, 111, 10, 3, 2, 233, 77, 17, 63, 141, 12, 221, 1, 114, 97, 114, 45,
        112, 97, 121, 108, 111, 97, 100, 108, 5, 180, 188, 38, 2, 3, 11, 130, 0, 4, 130, 0, 32,
        172, 42, 147, 216, 128, 0, 0, 10, 114, 101, 97, 100, 109, 101, 46, 116, 120, 116, 10, 3, 2,
        52, 117, 17, 63, 141, 12, 221, 1, 104, 105, 29, 119, 86, 81, 3, 5, 4, 0,
    ];

    #[test]
    fn rar_lists_and_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let rar = dir.path().join("g.rar");
        std::fs::write(&rar, RAR_FIXTURE).unwrap();
        let members = list_members(&rar).unwrap();
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["game.iso", "readme.txt"]);
        let resolved = resolve_input(&rar, &["iso"]).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"rar-payload");
        assert_eq!(resolved.output_basis(), dir.path().join("game.iso"));
    }

    #[test]
    fn bare_gzip_reports_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let gz = dir.path().join("game.iso.gz");
        std::fs::write(&gz, b"\x1f\x8b\x08\x00").unwrap();
        let err = resolve_input(&gz, &["iso"]).unwrap_err().to_string();
        assert!(
            err.contains("gzip archives without a tar container"),
            "{err}"
        );
    }

    #[test]
    fn no_match_is_hard_error_as_direct_input() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("docs.zip");
        write_zip(&zip, &[("readme.txt", b"x")]);
        assert!(resolve_input(&zip, &["iso"]).is_err());
    }

    #[test]
    fn multi_match_uses_first_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("multi.zip");
        write_zip(&zip, &[("b.iso", b"second"), ("a.iso", b"first")]);
        let resolved = resolve_input(&zip, &["iso"]).unwrap();
        assert_eq!(resolved.path().file_name().unwrap(), "a.iso");
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"first");
    }

    #[test]
    fn cue_pulls_referenced_bin_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("disc.zip");
        let cue = b"FILE \"disc.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";
        write_zip(
            &zip,
            &[
                ("disc.cue", cue.as_slice()),
                ("disc.bin", b"bin-track-bytes"),
                ("unrelated.bin", b"other"),
            ],
        );
        let resolved = resolve_input(&zip, &["cue"]).unwrap();
        let dir = resolved.path().parent().unwrap();
        assert_eq!(
            std::fs::read(dir.join("disc.bin")).unwrap(),
            b"bin-track-bytes"
        );
        // Only referenced bins are pulled, not every bin in the archive.
        assert!(!dir.join("unrelated.bin").exists());
    }

    #[test]
    fn plain_file_passes_through() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, b"x").unwrap();
        let resolved = resolve_input(&iso, &["iso"]).unwrap();
        assert_eq!(resolved.path(), iso);
        assert_eq!(resolved.output_basis(), iso);
    }
}
