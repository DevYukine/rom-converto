//! Transparent archive input. Read-side commands accept a zip, 7z, rar, tar,
//! or tar.gz container holding a supported image and operate on the first
//! member matching the caller's extension set. The member (and, for a cue
//! sheet, the bin tracks it references) is extracted to a temp dir that is
//! removed when the returned [`ResolvedInput`] is dropped. Rar support is
//! read-only and binds the vendored unrar sources, which are free to use but
//! not OSI-permissive.

use crate::util::fs::{has_any_extension, is_os_junk_file};
use crate::util::{
    DEFAULT_SPACE_HEADROOM, available_space, format_bytes, is_safe_dirent_name, space_shortfall,
};
use anyhow::{Result, anyhow, bail};
use std::collections::HashSet;
use std::fs::File;
use std::io::BufReader;
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

/// Listing and selected member retained from an archive probe until staging.
#[derive(Debug, Clone)]
pub struct ArchiveSelection {
    members: Vec<ArchiveMember>,
    member_index: usize,
    /// How many members matched the probe's extensions.
    candidates: usize,
}

impl ArchiveSelection {
    /// The [`ResolvedInput::output_basis`] an archive would resolve to for
    /// this selection, read from the member listing without extracting.
    pub fn output_basis(&self, archive: &Path) -> Result<PathBuf> {
        basis_for(archive, &self.members[self.member_index])
    }

    /// The picked member's in-archive name, for callers that must re-check
    /// it: a collection walk keeps an archive only when its picked member
    /// carries one of the plain extensions.
    fn member_name(&self) -> &str {
        &self.members[self.member_index].name
    }

    /// Logs the several-members warning for an archive a walk kept: the walk
    /// probes quietly, and the child reuses this selection instead of
    /// probing again, so nothing else would report it.
    fn warn_if_ambiguous(&self, archive: &Path) {
        if self.candidates > 1 {
            log::warn!(
                "{} contains {} matching members; using {}",
                archive.display(),
                self.candidates,
                self.member_name()
            );
        }
    }
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
/// directory: empty, `.`, `..`, NULs, and on Windows drive-relative or
/// alternate data stream names.
fn safe_basename(name: &str) -> Result<String> {
    let base = basename(name);
    if !is_safe_dirent_name(base) {
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

fn is_kept(name: &str, is_dir: bool) -> bool {
    let base = basename(name);
    // Nested archives are hidden the same way the recursive walker hides them.
    !is_dir && !base.is_empty() && !is_os_junk_file(base) && !name_has_ext(name, ARCHIVE_EXTS)
}

fn keep_member(name: &str, size: u64, is_dir: bool) -> Option<ArchiveMember> {
    if !is_kept(name, is_dir) {
        return None;
    }
    Some(ArchiveMember {
        name: name.to_string(),
        size,
    })
}

/// List the convertible members of an archive in archive order: regular files
/// only, with OS junk and nested archives filtered out. Bare gzip returns a
/// clear unsupported error.
pub fn list_members(path: &Path) -> Result<Vec<ArchiveMember>> {
    list_members_with(path, &crate::util::CancelToken::new())
}

/// [`list_members`] that stops a long tar listing when `cancel` fires: a
/// `.tar.gz` listing is a full decompression pass.
fn list_members_with(path: &Path, cancel: &crate::util::CancelToken) -> Result<Vec<ArchiveMember>> {
    let kind = kind_of(path).ok_or_else(|| {
        anyhow!(
            "gzip archives without a tar container are not supported, extract the file first: {}",
            path.display()
        )
    })?;
    Ok(match kind {
        ArchiveKind::Zip => list_zip(path)?,
        ArchiveKind::SevenZ => list_7z(path)?,
        ArchiveKind::Tar => list_tar(path, false, cancel)?,
        ArchiveKind::TarGz => list_tar(path, true, cancel)?,
        ArchiveKind::Rar => list_rar(path)?,
    })
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

/// Index of the zip entry whose decoded name is `member_name`. The zip
/// crate decodes a stored name by its flag: a CP437-stored non-ASCII name
/// is only visible through the decoded [`zip::read::ZipFile::name`], never
/// through `by_name`'s raw-name lookup.
fn zip_member_index<R>(zip: &mut zip::ZipArchive<R>, member_name: &str) -> Result<usize>
where
    R: std::io::Read + std::io::Seek,
{
    for index in 0..zip.len() {
        if zip.name_for_index(index) == Some(member_name) {
            return Ok(index);
        }
    }
    bail!("member {member_name} not found")
}

/// The zip central directory's CRC32 and uncompressed size for `member_name`,
/// read without decompressing the member. Backs dat verify/scan's `--quick`
/// mode, which trusts a zip's own checksum for an eligible cartridge image
/// instead of extracting and hashing it.
pub(crate) fn zip_member_crc32(path: &Path, member_name: &str) -> Result<(u32, u64)> {
    let mut zip = zip::ZipArchive::new(File::open(path)?)?;
    let index = zip_member_index(&mut zip, member_name)?;
    let entry = zip.by_index(index)?;
    Ok((entry.crc32(), entry.size()))
}

/// Read up to `len` leading bytes of a zip member without extracting the whole
/// thing, so the `--quick` path can magic-sniff a member's head cheaply. A
/// member shorter than `len` yields a shorter buffer.
pub(crate) fn zip_member_head(path: &Path, member_name: &str, len: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(File::open(path)?)?;
    let index = zip_member_index(&mut zip, member_name)?;
    let entry = zip.by_index(index)?;
    let mut buf = Vec::with_capacity(len);
    entry.take(len as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Lists an archive once, picks the member a probe would, and reads up to
/// `len` leading bytes of it without extracting the whole member. The head is
/// `None` for a RAR, whose reader has no partial extraction, so those
/// callers keep gating after staging. `None` overall for a plain file. The
/// selection carries over to staging so the archive is not listed again.
///
/// # Errors
/// Errors when listing the archive, picking its member, or reading it fails.
pub(crate) fn probe_with_head(
    path: &Path,
    exts: &[&str],
    len: usize,
    cancel: &crate::util::CancelToken,
) -> Result<Option<(ArchiveSelection, Option<Vec<u8>>)>> {
    use std::io::Read;
    if !is_archive_path(path) {
        return Ok(None);
    }
    let kind = kind_of(path).ok_or_else(|| {
        anyhow!(
            "gzip archives without a tar container are not supported: {}",
            path.display()
        )
    })?;
    let selection = probe_selection(path, exts, false, cancel)?;
    if kind == ArchiveKind::Rar {
        return Ok(Some((selection, None)));
    }
    let name = selection.member_name().to_string();
    let head = read_member(path, kind, &name, selection.member_index, |rd| {
        let mut buf = Vec::with_capacity(len);
        rd.take(len as u64).read_to_end(&mut buf)?;
        Ok(buf)
    })?;
    Ok(Some((selection, Some(head))))
}

/// Streams one member of a zip, 7z, or tar archive into `read`. Tar uses
/// `member_index`, the member's position among the listing's entries in
/// archive order, and checks the lossy decoded name; distinct raw names can
/// share that name. Zip and 7z resolve by exact decoded name.
fn read_member<T>(
    path: &Path,
    kind: ArchiveKind,
    member_name: &str,
    member_index: usize,
    read: impl FnOnce(&mut dyn std::io::Read) -> std::io::Result<T>,
) -> Result<T> {
    match kind {
        ArchiveKind::Zip => {
            let mut zip = zip::ZipArchive::new(File::open(path)?)?;
            let index = zip_member_index(&mut zip, member_name)?;
            let mut entry = zip.by_index(index)?;
            Ok(read(&mut entry)?)
        }
        ArchiveKind::SevenZ => {
            let (archive, file) = crate::util::sevenz_guard::open(path)?;
            for block in 0..archive.blocks.len() {
                crate::util::sevenz_guard::check_block(&archive, block, path)?;
            }
            // One thread: the multithreaded LZMA2 reader allocates one
            // dictionary per worker and buffers whole work units.
            let mut reader = sevenz_rust2::ArchiveReader::from_archive(
                archive,
                file,
                sevenz_rust2::Password::empty(),
            );
            reader.set_thread_count(1);
            let mut read = Some(read);
            let mut result: Option<std::io::Result<T>> = None;
            reader.for_each_entries(|entry, rd| {
                if entry.name() == member_name
                    && let Some(read) = read.take()
                {
                    result = Some(read(rd));
                    return Ok(false);
                }
                Ok(true)
            })?;
            result
                .ok_or_else(|| anyhow!("member {member_name} not found in {}", path.display()))?
                .map_err(Into::into)
        }
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            let mut archive = open_tar(path, kind == ArchiveKind::TarGz)?;
            // Count members exactly as list_tar does, so member_index
            // addresses the sequence the selection was built from.
            let mut kept = 0;
            for entry in archive.entries()? {
                let mut entry = entry?;
                let is_file = entry.header().entry_type().is_file();
                let name = entry.path()?.to_string_lossy().replace('\\', "/");
                if is_kept(&name, !is_file) {
                    if kept == member_index {
                        if name != member_name {
                            bail!(
                                "archive member at index {member_index} changed: expected {member_name}, found {name}"
                            );
                        }
                        return Ok(read(&mut entry)?);
                    }
                    kept += 1;
                }
            }
            bail!("member {member_name} not found in {}", path.display())
        }
        ArchiveKind::Rar => bail!("rar members are extracted, not streamed"),
    }
}

fn list_7z(path: &Path) -> Result<Vec<ArchiveMember>> {
    let (archive, _) = crate::util::sevenz_guard::open(path)?;
    let mut out = Vec::new();
    for entry in &archive.files {
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

fn list_tar(
    path: &Path,
    gz: bool,
    cancel: &crate::util::CancelToken,
) -> Result<Vec<ArchiveMember>> {
    let mut archive = open_tar(path, gz)?;
    let mut out = Vec::new();
    for entry in archive.entries()? {
        if cancel.is_cancelled() {
            return Err(crate::util::Cancelled.into());
        }
        let entry = entry?;
        let is_file = entry.header().entry_type().is_file();
        let name = entry.path()?.to_string_lossy().replace('\\', "/");
        if let Some(m) = keep_member(&name, entry.size(), !is_file) {
            out.push(m);
        }
    }
    Ok(out)
}

/// The error for a member that decoded to `written` bytes when its archive
/// declares `declared_size`.
fn declared_size_error(
    path: &Path,
    member_name: &str,
    written: u64,
    declared_size: u64,
) -> anyhow::Error {
    if written > declared_size {
        anyhow!(
            "{} member {member_name} decodes past its declared {declared_size} bytes",
            path.display()
        )
    } else {
        anyhow!(
            "{} member {member_name} decoded only {written} of {declared_size} bytes",
            path.display()
        )
    }
}

/// Extract one member to `dest_dir`, returning the written path. Streams so a
/// multi-gigabyte member never buffers in memory. See [`read_member`] for how
/// each format finds its member.
fn extract_one(
    path: &Path,
    kind: ArchiveKind,
    member_name: &str,
    member_index: usize,
    dest_dir: &Path,
    declared_size: u64,
) -> Result<PathBuf> {
    match kind {
        ArchiveKind::Zip | ArchiveKind::SevenZ | ArchiveKind::Tar | ArchiveKind::TarGz => {
            let out = dest_dir.join(safe_basename(member_name)?);
            // A member that decodes to other bytes than the size its
            // archive declares must not silently stage the difference:
            // hold the copy to the declared size and refuse a mismatch.
            let copied = read_member(path, kind, member_name, member_index, |rd| {
                let mut writer = File::create(&out)?;
                std::io::copy(
                    &mut std::io::Read::take(&mut *rd, declared_size.saturating_add(1)),
                    &mut writer,
                )
            });
            match copied {
                Ok(written) if written == declared_size => Ok(out),
                Ok(written) => {
                    let _ = std::fs::remove_file(&out);
                    Err(declared_size_error(
                        path,
                        member_name,
                        written,
                        declared_size,
                    ))
                }
                Err(err) => {
                    let _ = std::fs::remove_file(&out);
                    Err(err)
                }
            }
        }
        ArchiveKind::Rar => {
            let out = dest_dir.join(safe_basename(member_name)?);
            let mut archive = unrar::Archive::new(path).open_for_processing()?;
            // Count members exactly as list_rar does, so member_index
            // addresses the sequence the selection was built from.
            let mut kept = 0;
            while let Some(header) = archive.read_header()? {
                let entry = header.entry();
                // Processing mode re-reports a split file's continuation
                // header in every volume part, while list mode hides it;
                // skipping it keeps the kept counter aligned with the list.
                if entry.is_split_before() {
                    archive = header.skip()?;
                    continue;
                }
                let name = entry.filename.to_string_lossy().replace('\\', "/");
                if is_kept(&name, entry.is_directory()) {
                    if kept == member_index {
                        if name != member_name {
                            bail!(
                                "archive member at index {member_index} changed: expected {member_name}, found {name}"
                            );
                        }
                        header.extract_to(&out)?;
                        let staged = std::fs::metadata(&out)?.len();
                        if staged != declared_size {
                            let _ = std::fs::remove_file(&out);
                            return Err(declared_size_error(
                                path,
                                member_name,
                                staged,
                                declared_size,
                            ));
                        }
                        return Ok(out);
                    }
                    kept += 1;
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
    staged_bytes: u64,
    sole_member: bool,
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
    /// Uncompressed bytes occupying the staging directory, including cue sidecars.
    pub fn staged_bytes(&self) -> u64 {
        self.staged_bytes
    }

    /// The archive lists exactly one convertible member, so every member
    /// pick, whatever extensions it accepts, selects this one.
    pub fn sole_member(&self) -> bool {
        self.sole_member
    }
}

/// An archive holds no member with one of the extensions a caller accepts.
/// Carried in the error chain so a library scan can treat such an archive
/// as unrecognized rather than as a read failure.
#[derive(Debug, thiserror::Error)]
#[error("archive contains no matching image")]
pub struct NoMatchingMember;

/// The temp volume has too little free space to extract a member. Carried
/// in the error chain so a caller holding other extractions can free them
/// and retry.
#[derive(Debug, thiserror::Error)]
#[error("not enough space on the temp volume")]
pub struct TempSpaceShortfall;

/// How many non-directory entries `path` holds, counted from the raw
/// listing with no filtering: an archive holding several entries is never
/// removed wholesale, because every entry this run did not place (a sidecar
/// or a nested archive) would be destroyed with it.
pub fn entry_count(path: &Path) -> Result<usize> {
    let kind = kind_of(path).ok_or_else(|| {
        anyhow!(
            "gzip archives without a tar container are not supported, extract the file first: {}",
            path.display()
        )
    })?;
    Ok(match kind {
        ArchiveKind::Zip => {
            let mut zip = zip::ZipArchive::new(File::open(path)?)?;
            (0..zip.len())
                .filter(|index| {
                    !zip.by_index_raw(*index)
                        .map(|e| e.is_dir())
                        .unwrap_or(false)
                })
                .count()
        }
        ArchiveKind::SevenZ => crate::util::sevenz_guard::open(path)?
            .0
            .files
            .iter()
            .filter(|entry| !entry.is_directory())
            .count(),
        ArchiveKind::Rar => {
            let mut count = 0;
            for entry in unrar::Archive::new(path).open_for_listing()? {
                if !entry?.is_directory() {
                    count += 1;
                }
            }
            count
        }
        ArchiveKind::Tar | ArchiveKind::TarGz => {
            // Only entries that hold data count: a pax global header (what
            // `git archive` writes) or another metadata header is no file
            // the archive's removal would destroy.
            let mut count = 0;
            for entry in open_tar(path, kind == ArchiveKind::TarGz)?.entries()? {
                let kind = entry?.header().entry_type();
                if kind.is_file()
                    || kind.is_symlink()
                    || kind.is_hard_link()
                    || kind.is_gnu_sparse()
                {
                    count += 1;
                }
            }
            count
        }
    })
}

/// Extensions a cue or gdi sheet's tracks carry: when a sheet is a candidate
/// these are its tracks, not images of their own (a 2048-byte data track is a
/// plain `.iso`).
const SHEET_TRACK_EXTS: &[&str] = &["iso", "bin", "raw"];

/// The member [`probe_archive`] extracts for `exts`: the first match by sorted
/// name, except that a sheet candidate displaces the track-like candidates
/// (`.iso`, `.bin`, `.raw`) it would reference, and `.md` ranks last, where a
/// readme competes with the Mega Drive extension. A `.chd` next to a sheet
/// still wins by name, as it always did. `quiet` drops the several-members
/// warning, which a collection walk must not log once per candidate archive;
/// it logs the ones it keeps. `members` is in archive order and the returned
/// position addresses that sequence, with the count of matches.
fn pick_member(
    path: &Path,
    members: &[ArchiveMember],
    exts: &[&str],
    quiet: bool,
) -> Result<(usize, usize)> {
    let candidates: Vec<(usize, &ArchiveMember)> = members
        .iter()
        .enumerate()
        .filter(|(_, member)| name_has_ext(&member.name, exts))
        .collect();
    let total = candidates.len();
    let has_sheet = candidates
        .iter()
        .any(|(_, member)| name_has_ext(&member.name, &["cue", "gdi"]));
    let rank = |name: &str| u8::from(name_has_ext(name, &["md"]));
    let picked = candidates
        .iter()
        .filter(|(_, member)| !(has_sheet && name_has_ext(&member.name, SHEET_TRACK_EXTS)))
        .min_by(|a, b| (rank(&a.1.name), &a.1.name).cmp(&(rank(&b.1.name), &b.1.name)))
        .map(|(index, _)| *index);
    match picked {
        Some(index) => {
            if total > 1 && !quiet {
                log::warn!(
                    "{} contains {total} matching members; using {}",
                    path.display(),
                    members[index].name
                );
            }
            Ok((index, total))
        }
        None => Err(anyhow::Error::new(NoMatchingMember).context(format!(
            "archive {} contains no matching image ({:?})",
            path.display(),
            exts
        ))),
    }
}

/// Default-output basis for `member` of `path`: next to the archive, named
/// after the member.
fn basis_for(path: &Path, member: &ArchiveMember) -> Result<PathBuf> {
    Ok(path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(safe_basename(&member.name)?))
}

/// Lists an archive once and retains its selected member for later staging.
/// Plain files return `None`; archives without a matching member return an error.
pub fn probe_archive(
    path: &Path,
    exts: &[&str],
    cancel: &crate::util::CancelToken,
) -> Result<Option<ArchiveSelection>> {
    if !is_archive_path(path) {
        return Ok(None);
    }
    Ok(Some(probe_selection(path, exts, false, cancel)?))
}

/// Lists an archive and retains its selected member. Callers pass the path
/// through [`is_archive_path`] first, so the selection always exists.
fn probe_selection(
    path: &Path,
    exts: &[&str],
    quiet: bool,
    cancel: &crate::util::CancelToken,
) -> Result<ArchiveSelection> {
    let members = list_members_with(path, cancel)?;
    let (member_index, candidates) = pick_member(path, &members, exts, quiet)?;
    Ok(ArchiveSelection {
        members,
        member_index,
        candidates,
    })
}

/// True for the second and later volumes of a split rar set
/// (`name.part2.rar`, in any letter case): opening `part1` reads the whole
/// set, so the others are no inputs of their own.
fn is_rar_continuation(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    crate::util::fs::has_ext(path, "rar")
        && stem
            .rsplit_once('.')
            .and_then(|(_, tag)| {
                if tag.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("part")) {
                    tag[4..].parse::<u32>().ok()
                } else {
                    None
                }
            })
            .is_some_and(|n| n > 1)
}

/// Stages the member a hash should read, or `None` when the archive bytes
/// themselves are the input: a plain file, no member with one of `exts`, a
/// continuation volume of a split rar set, or an archive that cannot be
/// listed. An extraction failure after a member was picked, and a temp-space
/// shortfall, are errors.
///
/// # Errors
/// Errors when staging a picked archive member fails; an archive whose
/// listing or member search fails is logged and returned as `None`.
pub fn resolve_for_hash(path: &Path, exts: &[&str]) -> anyhow::Result<Option<ResolvedInput>> {
    if !is_archive_path(path) {
        return Ok(None);
    }
    // A bare gzip or a later rar volume is no container to open.
    if kind_of(path).is_none() || is_rar_continuation(path) {
        return Ok(None);
    }
    match probe_selection(path, exts, false, &crate::util::CancelToken::new()) {
        Err(err) if err.downcast_ref::<NoMatchingMember>().is_some() => {
            log::debug!("hashing {} as a file: {err:#}", path.display());
            Ok(None)
        }
        Err(err) => {
            log::warn!("hashing {} as a file: {err:#}", path.display());
            Ok(None)
        }
        Ok(selection) => resolve_input_with_selection(path, exts, Some(selection)).map(Some),
    }
}

/// What a recursive walk collects.
pub struct InputScan<'a> {
    /// Extensions of the plain files to collect.
    pub exts: &'a [&'a str],
    /// Extensions the member pick considers, the same set staging picks
    /// with; `None` reuses `exts`. The archive is an input only when the
    /// picked member carries one of `exts`.
    pub member_exts: Option<&'a [&'a str]>,
    /// How deep the walk descends; `None` for unlimited.
    pub max_depth: Option<usize>,
    /// Leave out an archive whose member also sits unpacked beside it (both would write the same output).
    pub skip_unpacked_twins: bool,
}

/// One input a [`collect_inputs`] walk kept.
#[derive(Debug, Clone)]
pub struct CollectedInput {
    /// The walked path: a plain file, or an archive the run opens.
    pub path: PathBuf,
    /// Default-output basis for the run: the file itself, or for an archive
    /// the picked member's name placed beside the archive. An archive kept
    /// despite a failed listing has no member to name and reports its own
    /// path.
    pub output_basis: PathBuf,
    /// The listing and pick the walk already made for an archive, so the
    /// child run does not list it again (a `.tar.gz` listing is a full
    /// decompression).
    pub selection: Option<ArchiveSelection>,
}

/// [`collect_input_entries`] reduced to the walked paths.
///
/// # Errors
/// Same as [`collect_input_entries`].
pub fn collect_inputs(
    dir: &Path,
    scan: &InputScan<'_>,
    cancel: &crate::util::CancelToken,
) -> std::io::Result<Vec<PathBuf>> {
    Ok(collect_input_entries(dir, scan, cancel)?
        .into_iter()
        .map(|input| input.path)
        .collect())
}

/// Every file under `dir` carrying one of `scan.exts`, plus every archive
/// under `dir` whose picked member (picked with `scan.member_exts`, or
/// `scan.exts` when that is `None`) carries one of `scan.exts`, in one
/// sorted walk up to `scan.max_depth`. An archive without such a member is
/// left out, as is one whose member also sits unpacked beside it when
/// `scan.skip_unpacked_twins`. One that cannot be listed stays in, so the
/// run reports it as a failed item rather than silently passing over it.
///
/// # Errors
/// Cancellation stops the walk, or stops before the next archive is listed,
/// and a failing directory walk is an error.
pub fn collect_input_entries(
    dir: &Path,
    scan: &InputScan<'_>,
    cancel: &crate::util::CancelToken,
) -> std::io::Result<Vec<CollectedInput>> {
    let member_exts = scan.member_exts.unwrap_or(scan.exts);
    let mut wanted: Vec<&str> = scan.exts.to_vec();
    wanted.extend(ARCHIVE_EXTS);
    let candidates =
        crate::util::fs::collect_files_with_exts(dir, &wanted, scan.max_depth, cancel)?;
    // Twin detection folds case, so a member and a plain file that differ
    // only in case are one output on case-insensitive volumes.
    let plain: HashSet<String> = candidates
        .iter()
        .filter(|path| !is_archive_path(path))
        .map(|path| path.to_string_lossy().to_lowercase())
        .collect();
    let mut inputs = Vec::with_capacity(candidates.len());
    for path in &candidates {
        if !is_archive_path(path) {
            inputs.push(CollectedInput {
                path: path.clone(),
                output_basis: path.clone(),
                selection: None,
            });
            continue;
        }
        // A bare gzip is no container the batch could open.
        if kind_of(path).is_none() || is_rar_continuation(path) {
            log::debug!("skipping {}: not an archive to open", path.display());
            continue;
        }
        if cancel.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                crate::util::Cancelled,
            ));
        }
        match probe_selection(path, member_exts, true, cancel) {
            Ok(selection) if name_has_ext(selection.member_name(), scan.exts) => {
                let basis = selection
                    .output_basis(path)
                    .unwrap_or_else(|_| path.clone());
                if scan.skip_unpacked_twins
                    && plain.contains(&basis.to_string_lossy().to_lowercase())
                {
                    log::warn!(
                        "skipping {}: its member is also present unpacked as {}",
                        path.display(),
                        basis.display()
                    );
                    continue;
                }
                selection.warn_if_ambiguous(path);
                inputs.push(CollectedInput {
                    path: path.clone(),
                    output_basis: basis,
                    selection: Some(selection),
                });
            }
            Ok(_) => {
                log::debug!(
                    "skipping {}: its picked member carries none of {:?}",
                    path.display(),
                    scan.exts
                );
            }
            Err(err) if err.downcast_ref::<NoMatchingMember>().is_some() => {
                log::debug!(
                    "skipping {}: no member matches {member_exts:?}",
                    path.display()
                );
            }
            // A damaged archive stays in the batch: the item fails on
            // staging and is reported, instead of vanishing from the tally.
            Err(_) if cancel.is_cancelled() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    crate::util::Cancelled,
                ));
            }
            Err(err) => {
                log::debug!("keeping {} for the run to report: {err:#}", path.display());
                inputs.push(CollectedInput {
                    path: path.clone(),
                    output_basis: path.clone(),
                    selection: None,
                });
            }
        }
    }
    Ok(inputs)
}

/// Directory portion of an in-archive member path (empty at the archive
/// root), tolerating either separator.
fn parent_dir(name: &str) -> &str {
    match name.rsplit_once(['/', '\\']) {
        Some((dir, _)) => dir,
        None => "",
    }
}

fn cue_sidecar_members(
    path: &Path,
    members: &[ArchiveMember],
    cue_name: &str,
    references: &HashSet<String>,
) -> Result<Vec<usize>> {
    let cue_dir = parent_dir(cue_name);
    let mut indices = Vec::new();
    for reference in references {
        // A multi-disc archive repeats track names per disc folder, so the
        // sheet's own directory disambiguates; only a reference that matches
        // nothing there falls back to the whole archive.
        let hits: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(_, member)| {
                member.name != cue_name && basename(&member.name).to_ascii_lowercase() == *reference
            })
            .map(|(index, _)| index)
            .collect();
        let in_cue_dir: Vec<usize> = hits
            .iter()
            .copied()
            .filter(|&index| parent_dir(&members[index].name).eq_ignore_ascii_case(cue_dir))
            .collect();
        let matched: &[usize] = match in_cue_dir.as_slice() {
            [] => &hits,
            found => found,
        };
        match matched {
            [] => {}
            [index] => indices.push(*index),
            _ => bail!(
                "referenced archive basename {reference} appears multiple times in {}",
                path.display()
            ),
        }
    }
    indices.sort_unstable();
    Ok(indices)
}

/// Resolve a read input, reusing an archive probe if available.
///
/// # Errors
/// Errors on unsupported or corrupt archives, 7z guard refusals, listing
/// caps, no matching image, unsafe member names, ambiguous track references,
/// insufficient temp space, declared-size mismatches, or extraction I/O failures.
pub fn resolve_input(path: &Path, exts: &[&str]) -> Result<ResolvedInput> {
    resolve_input_with_selection(path, exts, None)
}

/// Same as [`resolve_input`], but reuses a previously probed
/// [`ArchiveSelection`] instead of listing the archive again.
///
/// # Errors
/// Same as [`resolve_input`], plus a member name changing after the probe.
pub fn resolve_input_with_selection(
    path: &Path,
    exts: &[&str],
    selection: Option<ArchiveSelection>,
) -> Result<ResolvedInput> {
    if !is_archive_path(path) {
        return Ok(ResolvedInput {
            path: path.to_path_buf(),
            output_basis: path.to_path_buf(),
            staged_bytes: 0,
            sole_member: false,
            _tmp: None,
        });
    }

    let kind = kind_of(path).ok_or_else(|| {
        anyhow!(
            "gzip archives without a tar container are not supported, extract the file first: {}",
            path.display()
        )
    })?;
    let selection = match selection {
        Some(selection) => selection,
        None => probe_selection(path, exts, false, &crate::util::CancelToken::new())?,
    };
    let ArchiveSelection {
        members,
        member_index,
        ..
    } = selection;
    let member = &members[member_index];

    let tmp = tempfile::tempdir()?;
    let is_sheet = name_has_ext(&member.name, &["cue", "gdi"]);
    let mut needed = member.size;
    let mut referenced = Vec::new();
    let extracted_sheet = if is_sheet {
        if let Ok(available) = available_space(tmp.path())
            && space_shortfall(available, member.size, DEFAULT_SPACE_HEADROOM).is_some()
        {
            return Err(anyhow::Error::new(TempSpaceShortfall).context(format!(
                "not enough space on the temp volume at {} to extract {}: need about {}, only {} free. Point TMPDIR at a larger volume or extract the archive first.",
                tmp.path().display(),
                format_bytes(member.size),
                format_bytes(member.size.saturating_add(DEFAULT_SPACE_HEADROOM)),
                format_bytes(available)
            )));
        }
        Some(extract_one(
            path,
            kind,
            &member.name,
            member_index,
            tmp.path(),
            member.size,
        )?)
    } else {
        None
    };
    if let Some(sheet_path) = extracted_sheet.as_ref() {
        // Best-effort: a non-UTF-8 or nonstandard sheet yields no sidecars
        // rather than failing extraction of the primary member.
        let wanted: HashSet<String> = if name_has_ext(&member.name, &["gdi"]) {
            crate::sega::katana::parse_gdi_index(sheet_path)
                .map(|index| {
                    index
                        .tracks
                        .iter()
                        .map(|track| basename(&track.filename).to_ascii_lowercase())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            crate::disc::cue::CueParser::new(sheet_path)
                .parse_reader(BufReader::new(File::open(sheet_path)?))
                .map(|cue| cue.files)
                .unwrap_or_default()
                .into_iter()
                .map(|file| basename(&file.filename).to_ascii_lowercase())
                .collect()
        };
        referenced = cue_sidecar_members(path, &members, &member.name, &wanted)?;
        for &index in &referenced {
            needed = needed
                .checked_add(members[index].size)
                .ok_or_else(|| anyhow!("archive extraction size overflow"))?;
        }
    }
    if let Ok(available) = available_space(tmp.path()) {
        let available_before_sheet =
            available.saturating_add(if is_sheet { member.size } else { 0 });
        if space_shortfall(available_before_sheet, needed, DEFAULT_SPACE_HEADROOM).is_some() {
            return Err(anyhow::Error::new(TempSpaceShortfall).context(format!(
                "not enough space on the temp volume at {} to extract {}: need about {}, only {} free. Point TMPDIR at a larger volume or extract the archive first.",
                tmp.path().display(),
                format_bytes(needed),
                format_bytes(needed.saturating_add(DEFAULT_SPACE_HEADROOM)),
                format_bytes(available_before_sheet)
            )));
        }
    }
    let extracted = match extracted_sheet {
        Some(path) => path,
        None => extract_one(
            path,
            kind,
            &member.name,
            member_index,
            tmp.path(),
            member.size,
        )?,
    };
    for index in referenced {
        extract_one(
            path,
            kind,
            &members[index].name,
            index,
            tmp.path(),
            members[index].size,
        )?;
    }

    Ok(ResolvedInput {
        path: extracted,
        output_basis: basis_for(path, member)?,
        staged_bytes: needed,
        sole_member: members.len() == 1,
        _tmp: Some(tmp),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{CancelToken, NoProgress, ZipFormat, ZipMember};

    #[test]
    fn safe_basename_refuses_unsafe_names_and_keeps_plain_basenames() {
        for name in ["..", ".", "", "nul\0.bin"] {
            assert!(safe_basename(name).is_err(), "{name:?}");
        }
        for name in ["C:x.iso", "C:", "ab:c"] {
            assert_eq!(safe_basename(name).is_err(), cfg!(windows), "{name:?}");
        }

        for (name, expected) in [
            ("Game (USA).iso", "Game (USA).iso"),
            ("a.b.c.bin", "a.b.c.bin"),
            ("Pokémon.iso", "Pokémon.iso"),
            ("dir/file.bin", "file.bin"),
        ] {
            assert_eq!(safe_basename(name).unwrap(), expected);
        }
    }

    /// A zip whose non-ASCII member name is stored CP437 without the UTF-8
    /// flag stages through the decoded name: the member is found, extracted,
    /// and its bytes come back whole.
    #[test]
    fn a_cp437_stored_member_stages_by_its_decoded_name() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("Pok\u{e9}mon.gba");
        let bytes = b"pokemon rom bytes";
        std::fs::write(&raw, bytes).unwrap();
        let zip_path = dir.path().join("Pokemon.zip");
        crate::util::write_torrentzip(
            &ZipMember {
                name: "Pok\u{e9}mon.gba".to_string(),
                path: raw,
                skip: 0,
                pad: 0,
                fill: 0,
            },
            &zip_path,
            ZipFormat::TorrentZip,
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        // The writer stored the name without the UTF-8 flag, so by_name's
        // raw-name lookup cannot find it.
        let mut zip = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        assert!(zip.by_name("Pok\u{e9}mon.gba").is_err());

        let resolved = resolve_input(&zip_path, &["gba"]).unwrap();
        let extracted = std::fs::read(resolved.path()).unwrap();
        assert_eq!(extracted, bytes);
        assert_eq!(
            resolved.path().file_name().and_then(|name| name.to_str()),
            Some("Pok\u{e9}mon.gba")
        );
    }

    use std::io::Write;

    fn write_zip(path: &Path, method: zip::CompressionMethod, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(method);
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

    /// Like [`write_tar`], but stores raw, possibly non-UTF-8 member name
    /// bytes. Unix only: raw byte names only exist as an OsStr extension.
    #[cfg(unix)]
    fn write_tar_raw_names(path: &Path, entries: &[(&[u8], &[u8])]) {
        use std::os::unix::ffi::OsStringExt;
        let file = File::create(path).unwrap();
        let mut builder = tar::Builder::new(file);
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    std::ffi::OsString::from_vec(name.to_vec()),
                    *data,
                )
                .unwrap();
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
            zip::CompressionMethod::Stored,
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
        write_zip(
            &zip,
            zip::CompressionMethod::Stored,
            &[("readme.txt", b"junk"), ("game.iso", b"payload")],
        );
        let resolved = resolve_input(&zip, &["iso"]).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"payload");
        assert_eq!(resolved.path().file_name().unwrap(), "game.iso");
        // Output lands next to the archive, named after the member.
        assert_eq!(resolved.output_basis(), dir.path().join("game.iso"));
    }

    #[test]
    fn collect_inputs_keeps_plain_files_and_archives_with_a_matching_member() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("sub");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(dir.path().join("plain.iso"), b"iso").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"txt").unwrap();
        write_zip(
            &nested.join("packed.zip"),
            zip::CompressionMethod::Stored,
            &[("readme.txt", b"x"), ("game.iso", b"iso")],
        );
        write_zip(
            &dir.path().join("docs.zip"),
            zip::CompressionMethod::Stored,
            &[("readme.txt", b"x")],
        );
        std::fs::write(dir.path().join("broken.7z"), b"not an archive").unwrap();
        std::fs::write(dir.path().join("log.gz"), b"gz").unwrap();
        std::fs::write(dir.path().join("set.part2.rar"), b"rar").unwrap();
        std::fs::write(dir.path().join("Game.Part2.rar"), b"rar").unwrap();
        write_tar(&dir.path().join("disc.tar"), false, &[("disc.cue", b"cue")]);
        // The same image unpacked beside its archive: the plain file wins.
        write_zip(
            &dir.path().join("plain.zip"),
            zip::CompressionMethod::Stored,
            &[("plain.iso", b"iso")],
        );

        let scan = InputScan {
            exts: &["iso", "cue"],
            member_exts: None,
            max_depth: None,
            skip_unpacked_twins: true,
        };
        let found = collect_input_entries(dir.path(), &scan, &CancelToken::new()).unwrap();
        assert_eq!(
            collected_paths(found),
            vec![
                dir.path().join("broken.7z"),
                dir.path().join("disc.tar"),
                dir.path().join("plain.iso"),
                nested.join("packed.zip"),
            ]
        );

        let shallow = collect_input_entries(
            dir.path(),
            &InputScan {
                exts: &["iso"],
                member_exts: None,
                max_depth: Some(1),
                skip_unpacked_twins: true,
            },
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(
            collected_paths(shallow),
            vec![dir.path().join("broken.7z"), dir.path().join("plain.iso")]
        );

        // `Game.Part2.rar` is a continuation volume in any letter case, and
        // without the twin rule the unpacked archive stays an input too.
        let keep_twin = collect_input_entries(
            dir.path(),
            &InputScan {
                exts: &["iso"],
                member_exts: None,
                max_depth: None,
                skip_unpacked_twins: false,
            },
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(
            collected_paths(keep_twin),
            vec![
                dir.path().join("broken.7z"),
                dir.path().join("plain.iso"),
                dir.path().join("plain.zip"),
                nested.join("packed.zip"),
            ]
        );
    }

    #[test]
    fn collect_inputs_probes_archives_with_their_own_member_extensions() {
        let dir = tempfile::tempdir().unwrap();
        // The pick considers `member_exts`, but the archive is kept only when
        // the picked member carries one of the plain `exts`: a `.chd`-only
        // member leaves it out.
        write_zip(
            &dir.path().join("chd_only.zip"),
            zip::CompressionMethod::Stored,
            &[("game.chd", b"chd")],
        );
        // Holding both, the pick goes by name order and the archive is kept
        // because the picked member carries an `exts` extension.
        write_zip(
            &dir.path().join("both.zip"),
            zip::CompressionMethod::Stored,
            &[("game.iso", b"iso"), ("readme.chd", b"chd")],
        );

        let found = collect_input_entries(
            dir.path(),
            &InputScan {
                exts: &["iso"],
                member_exts: Some(&["iso", "chd"]),
                max_depth: None,
                skip_unpacked_twins: false,
            },
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(collected_paths(found), vec![dir.path().join("both.zip")]);

        // With `member_exts: None` the pick considers the plain `exts` only,
        // so the keep rule never rejects what the pick chose.
        let none = collect_input_entries(
            dir.path(),
            &InputScan {
                exts: &["iso"],
                member_exts: None,
                max_depth: None,
                skip_unpacked_twins: false,
            },
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(collected_paths(none), vec![dir.path().join("both.zip")]);
    }

    #[test]
    fn a_cancelled_tar_listing_stops_with_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let tar = dir.path().join("set.tar.gz");
        write_tar(&tar, true, &[("game.iso", b"iso"), ("b.iso", b"iso")]);
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = list_members_with(&tar, &cancel).unwrap_err();
        assert!(crate::util::Cancelled::in_chain(&err));
        assert_eq!(list_members(&tar).unwrap().len(), 2);
    }

    /// The walked paths of a [`collect_inputs`] result.
    fn collected_paths(inputs: Vec<CollectedInput>) -> Vec<PathBuf> {
        inputs.into_iter().map(|input| input.path).collect()
    }

    #[test]
    fn member_pick_prefers_a_sheet_over_its_tracks_and_ranks_md_last() {
        let dir = tempfile::tempdir().unwrap();
        // A 2048-byte data track named like a disc image sorts before the
        // sheet; the sheet must still win.
        let gdi = b"2\n1 0 4 2048 track01.iso 0\n2 450 0 2352 track02.raw 0\n";
        let set = dir.path().join("set.zip");
        write_zip(
            &set,
            zip::CompressionMethod::Stored,
            &[
                ("track01.iso", b"track"),
                ("track02.raw", b"audio"),
                ("zombie.gdi", gdi.as_slice()),
            ],
        );
        let selection = probe_archive(&set, &["iso", "cue", "gdi"], &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(selection.member_name(), "zombie.gdi");

        // A `.chd` beside a cue sheet still wins by name, as it did before
        // sheets ranked first for tracks.
        let mixed = dir.path().join("mixed.zip");
        write_zip(
            &mixed,
            zip::CompressionMethod::Stored,
            &[("Game.chd", b"chd"), ("Game.cue", b"cue")],
        );
        let all = ["chd", "cue", "iso"];
        let selection = probe_archive(&mixed, &all, &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(selection.member_name(), "Game.chd");

        // A readme must not beat a ROM through the Mega Drive extension.
        let rom = dir.path().join("rom.zip");
        write_zip(
            &rom,
            zip::CompressionMethod::Stored,
            &[("README.md", b"notes"), ("zelda.nes", b"rom")],
        );
        let selection = probe_archive(&rom, &["md", "nes"], &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(selection.member_name(), "zelda.nes");
        // With nothing else, the md member is still the pick.
        let only = dir.path().join("only.zip");
        write_zip(
            &only,
            zip::CompressionMethod::Stored,
            &[("sonic.md", b"rom")],
        );
        let selection = probe_archive(&only, &["md", "nes"], &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(selection.member_name(), "sonic.md");
    }

    #[test]
    fn resolve_for_hash_stages_a_matching_member_and_skips_plain_files() {
        let dir = tempfile::tempdir().unwrap();
        write_zip(
            &dir.path().join("docs.zip"),
            zip::CompressionMethod::Stored,
            &[("readme.txt", b"x")],
        );
        write_zip(
            &dir.path().join("game.zip"),
            zip::CompressionMethod::Stored,
            &[("readme.txt", b"x"), ("game.iso", b"iso")],
        );
        std::fs::write(dir.path().join("broken.zip"), b"not an archive").unwrap();
        let plain = dir.path().join("game.iso");
        std::fs::write(&plain, b"iso").unwrap();

        // No matching member, and an archive that cannot be listed: the
        // archive bytes themselves are the input.
        assert!(
            resolve_for_hash(&dir.path().join("docs.zip"), &["iso"])
                .unwrap()
                .is_none()
        );
        assert!(
            resolve_for_hash(&dir.path().join("broken.zip"), &["iso"])
                .unwrap()
                .is_none()
        );

        let resolved = resolve_for_hash(&dir.path().join("game.zip"), &["iso"])
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.path().file_name(),
            Some(std::ffi::OsStr::new("game.iso"))
        );
        assert_ne!(resolved.path(), dir.path().join("game.zip"));

        let passthrough = resolve_for_hash(&plain, &["iso"]).unwrap();
        assert!(passthrough.is_none());
    }

    #[test]
    fn rar_continuation_volumes_are_recognized_by_name() {
        for (name, expected) in [
            ("set.part2.rar", true),
            ("set.part10.rar", true),
            ("set.PART02.rar", true),
            ("Game.Part2.rar", true),
            ("set.part1.rar", false),
            ("set.part01.rar", false),
            ("set.rar", false),
            ("part2.rar", false),
            ("set.part2.zip", false),
        ] {
            assert_eq!(is_rar_continuation(Path::new(name)), expected, "{name}");
        }
    }

    #[test]
    fn zip_codec_round_trip_extraction() {
        for method in [
            zip::CompressionMethod::Stored,
            zip::CompressionMethod::Deflated,
            zip::CompressionMethod::Bzip2,
            zip::CompressionMethod::Zstd,
            zip::CompressionMethod::Xz,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let zip = dir.path().join("g.zip");
            write_zip(&zip, method, &[("game.iso", b"codec-payload")]);
            let resolved = resolve_input(&zip, &["iso"]).unwrap();
            assert_eq!(
                std::fs::read(resolved.path()).unwrap(),
                b"codec-payload",
                "{method:?}"
            );
        }
    }

    #[test]
    fn tar_round_trip_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let tar = dir.path().join("g.tar");
        write_tar(&tar, false, &[("game.chd", b"chd-data")]);
        let resolved = resolve_input(&tar, &["chd"]).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"chd-data");
    }

    /// A member whose decoder produces more bytes than the size its archive
    /// declares must fail staging and leave nothing larger than the declared
    /// size behind.
    #[test]
    fn zip_member_decoding_past_its_declared_size_fails_to_stage() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("g.zip");
        let payload = b"deflate-payload-longer-than-declared-bytes";
        write_zip(
            &zip,
            zip::CompressionMethod::Deflated,
            &[("game.iso", payload)],
        );

        // Shrink the declared uncompressed size in the local file header and
        // in the central directory entry, keeping the CRC.
        let declared = u32::try_from(payload.len() / 2).unwrap();
        let mut raw = std::fs::read(&zip).unwrap();
        let mut patched = 0;
        let mut scan = 0;
        while scan + 4 <= raw.len() {
            match u32::from_le_bytes(raw[scan..scan + 4].try_into().unwrap()) {
                0x0403_4b50 if patched & 1 == 0 => {
                    raw[scan + 22..scan + 26].copy_from_slice(&declared.to_le_bytes());
                    patched |= 1;
                }
                0x0201_4b50 if patched & 2 == 0 => {
                    raw[scan + 24..scan + 28].copy_from_slice(&declared.to_le_bytes());
                    patched |= 2;
                }
                _ => {}
            }
            scan += 1;
        }
        assert_eq!(patched, 3);
        std::fs::write(&zip, raw).unwrap();

        // The listing now reports the patched declared size.
        assert_eq!(list_members(&zip).unwrap()[0].size, declared as u64);

        let staging = tempfile::tempdir().unwrap();
        let err = extract_one(
            &zip,
            ArchiveKind::Zip,
            "game.iso",
            0,
            staging.path(),
            declared as u64,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(&declared.to_string()), "{err}");
        assert!(
            std::fs::read_dir(staging.path()).unwrap().next().is_none(),
            "a leftover file stayed in the staging dir"
        );
    }

    /// Two tar members whose raw names differ only in a non-UTF-8 byte share
    /// one lossy display name; the selection index, not the name, decides
    /// which entry's bytes are extracted.
    #[cfg(unix)]
    #[test]
    fn tar_selects_by_index_when_lossy_names_collide() {
        let dir = tempfile::tempdir().unwrap();
        let tar = dir.path().join("collide.tar");
        write_tar_raw_names(
            &tar,
            &[
                (b"z.iso", b"unselected"),
                (b".DS_Store", b"junk"),
                (b"game\xff.iso", b"first"),
                (b"game\xfe.iso", b"second"),
            ],
        );
        let members = list_members(&tar).unwrap();
        assert_eq!(members.len(), 3);
        assert_eq!(members[0].name, "z.iso");
        assert_eq!(members[1].name, members[2].name);
        let selection = ArchiveSelection {
            members,
            member_index: 2,
            candidates: 3,
        };
        let resolved = resolve_input_with_selection(&tar, &["iso"], Some(selection)).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"second");
    }

    #[test]
    fn tar_rejects_member_name_changes_after_probe() {
        for gz in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(if gz { "g.tar.gz" } else { "g.tar" });
            write_tar(&path, gz, &[("game.iso", b"original")]);
            let selection = probe_archive(&path, &["iso"], &CancelToken::new())
                .unwrap()
                .unwrap();
            write_tar(&path, gz, &[("other.iso", b"replacement")]);
            let err = resolve_input_with_selection(&path, &["iso"], Some(selection))
                .unwrap_err()
                .to_string();
            assert!(err.contains("expected game.iso, found other.iso"), "{err}");
        }
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
        write_zip(
            &zip,
            zip::CompressionMethod::Stored,
            &[("../escape.iso", b"nope"), ("ok.iso", b"good")],
        );
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
    fn rar_rejects_member_name_mismatch_at_selected_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.rar");
        std::fs::write(&path, RAR_FIXTURE).unwrap();
        let mut selection = probe_archive(&path, &["iso"], &CancelToken::new())
            .unwrap()
            .unwrap();
        selection.members[selection.member_index].name = "other.iso".into();
        let err = resolve_input_with_selection(&path, &["iso"], Some(selection))
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected other.iso, found game.iso"), "{err}");
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
        write_zip(
            &zip,
            zip::CompressionMethod::Stored,
            &[("readme.txt", b"x")],
        );
        assert!(resolve_input(&zip, &["iso"]).is_err());
    }

    #[test]
    fn multi_match_uses_first_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("multi.zip");
        write_zip(
            &zip,
            zip::CompressionMethod::Stored,
            &[("b.iso", b"second"), ("a.iso", b"first")],
        );
        let members = list_members(&zip).unwrap();
        assert_eq!(members[0].name, "b.iso");
        assert_eq!(members[1].name, "a.iso");
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
            zip::CompressionMethod::Stored,
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
    fn gdi_pulls_referenced_track_files() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("disc.zip");
        let gdi = b"2\n1 0 4 2352 track01.bin 0\n2 4500 0 2352 track02.raw 0\n";
        write_zip(
            &zip,
            zip::CompressionMethod::Stored,
            &[
                ("game.gdi", gdi.as_slice()),
                ("track01.bin", b"first-track"),
                ("track02.raw", b"second-track"),
                ("unrelated.bin", b"other"),
            ],
        );
        let resolved = resolve_input(&zip, &["gdi"]).unwrap();
        assert_eq!(resolved.path().file_name().unwrap(), "game.gdi");
        let dir = resolved.path().parent().unwrap();
        assert_eq!(
            std::fs::read(dir.join("track01.bin")).unwrap(),
            b"first-track"
        );
        assert_eq!(
            std::fs::read(dir.join("track02.raw")).unwrap(),
            b"second-track"
        );
        // Only referenced tracks are pulled, not every file in the archive.
        assert!(!dir.join("unrelated.bin").exists());
    }

    #[test]
    fn cue_rejects_ambiguous_referenced_basenames() {
        let cue = b"FILE \"Track 01.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";
        for names in [
            ["Disc 1/Track 01.bin", "Disc 2/Track 01.bin"],
            ["Disc 1/Track 01.bin", "Disc 2/TRACK 01.BIN"],
        ] {
            for extension in ["zip", "tar"] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join(format!("disc.{extension}"));
                let entries: &[(&str, &[u8])] = &[
                    ("disc.cue", cue),
                    (names[0], b"first"),
                    (names[1], b"second"),
                ];
                if extension == "zip" {
                    write_zip(&path, zip::CompressionMethod::Stored, entries);
                } else {
                    write_tar(&path, false, entries);
                }
                let err = resolve_input(&path, &["cue"]).unwrap_err().to_string();
                assert!(
                    err.contains("referenced archive basename track 01.bin appears multiple times"),
                    "{err}"
                );
            }
        }
    }

    #[test]
    fn cue_resolves_track_references_within_its_own_disc_folder() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("multi.zip");
        let cue = b"FILE \"Track 01.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";
        write_zip(
            &zip,
            zip::CompressionMethod::Stored,
            &[
                ("Disc 1/game.cue", cue.as_slice()),
                ("Disc 1/Track 01.bin", b"first-disc"),
                ("Disc 2/game.cue", cue.as_slice()),
                ("Disc 2/Track 01.bin", b"second-disc"),
            ],
        );
        let resolved = resolve_input(&zip, &["cue"]).unwrap();
        // The first sorted cue is Disc 1's, and its track resolves inside
        // Disc 1 even though Disc 2 repeats the basename.
        assert_eq!(resolved.output_basis().file_name().unwrap(), "game.cue");
        assert_eq!(
            std::fs::read(resolved.path().parent().unwrap().join("Track 01.bin")).unwrap(),
            b"first-disc"
        );
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

    #[test]
    fn probed_selection_stages_without_changing_selected_output_basis() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("bundle.zip");
        write_zip(
            &zip,
            zip::CompressionMethod::Stored,
            &[("b.iso", b"second"), ("a.iso", b"first")],
        );
        let selection = probe_archive(&zip, &["iso"], &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(
            selection.output_basis(&zip).unwrap(),
            dir.path().join("a.iso")
        );
        let resolved = resolve_input_with_selection(&zip, &["iso"], Some(selection)).unwrap();
        assert_eq!(std::fs::read(resolved.path()).unwrap(), b"first");
        assert_eq!(resolved.output_basis(), dir.path().join("a.iso"));
    }

    #[test]
    fn tar_cue_extracts_only_referenced_regular_track() {
        let dir = tempfile::tempdir().unwrap();
        let tar = dir.path().join("disc.tar");
        let cue = b"FILE \"tracks/one.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";
        write_tar(
            &tar,
            false,
            &[
                ("disc.cue", cue.as_slice()),
                ("tracks/one.bin", b"track"),
                ("other.bin", b"unreferenced"),
            ],
        );
        let resolved = resolve_input(&tar, &["cue"]).unwrap();
        let staged = resolved.path().parent().unwrap();
        assert_eq!(std::fs::read(staged.join("one.bin")).unwrap(), b"track");
        assert!(!staged.join("other.bin").exists());
    }

    #[test]
    fn cue_sidecar_preflight_counts_only_referenced_regular_entries() {
        let members = vec![
            ArchiveMember {
                name: "disc.cue".into(),
                size: 32,
            },
            ArchiveMember {
                name: "tracks/one.bin".into(),
                size: 100,
            },
            ArchiveMember {
                name: "two.bin".into(),
                size: 200,
            },
            ArchiveMember {
                name: "unrelated.bin".into(),
                size: u64::MAX,
            },
        ];
        let wanted = HashSet::from(["one.bin".to_string(), "two.bin".to_string()]);
        let tracks =
            cue_sidecar_members(Path::new("a.rar"), &members, "disc.cue", &wanted).unwrap();
        assert_eq!(tracks, [1, 2]);
        let required = tracks
            .iter()
            .try_fold(members[0].size, |sum, &index| {
                sum.checked_add(members[index].size)
            })
            .unwrap();
        assert_eq!(required, 332);
        assert!(
            space_shortfall(
                required + DEFAULT_SPACE_HEADROOM - 1,
                required,
                DEFAULT_SPACE_HEADROOM
            )
            .is_some()
        );
    }
    /// A tar carrying a pax global header (what `git archive` writes) beside
    /// one ROM holds one entry: the metadata header is no file the archive's
    /// removal would destroy.
    #[test]
    fn a_pax_global_header_is_not_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rom.tar");
        let mut builder = tar::Builder::new(File::create(&path).unwrap());
        let global = b"52 comment=abcdef0123456789abcdef0123456789abcdef01\n";
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::XGlobalHeader);
        header.set_size(global.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "pax_global_header", &global[..])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "Game.gba", &b"rom!"[..])
            .unwrap();
        builder.into_inner().unwrap();
        assert_eq!(entry_count(&path).unwrap(), 1);
    }
}
