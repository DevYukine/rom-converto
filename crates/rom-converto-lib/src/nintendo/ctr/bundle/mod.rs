//! Azahar bundle ROMs (`.bcia`/`.bcci`/`.bcxi`): plain uncompressed tar
//! archives of CTR ROM members, per Azahar PR #2369. Bundling requires
//! decrypted members because Azahar plays decrypted ROMs only.

pub mod error;
pub(crate) mod tar;

pub use error::{CtrBundleError, CtrBundleResult};

use std::collections::HashSet;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::nintendo::ctr::z3ds::models::{Z3DS_HEADER_SIZE, Z3dsHeader};
use crate::nintendo::ctr::z3ds::{
    ENCRYPTION_PROBE_SIZE, Z3dsReader, check_not_encrypted, error::Z3dsError, error::Z3dsResult,
};
use crate::util::{
    AtomicProgress, CancelToken, Cancelled, ProgressReporter, await_with_progress_cancel,
    is_safe_dirent_name, publish_set, run_scratch_write, scratch_output_file,
};
use binrw::BinRead;

/// File extensions of Azahar bundle containers.
pub const BUNDLE_EXTS: &[&str] = &["bcia", "bcci", "bcxi"];

/// File extensions accepted as bundle members.
pub const MEMBER_EXTS: &[&str] = &["cia", "zcia", "3ds", "cci", "zcci", "cxi", "zcxi"];

/// Maximum number of members a bundle may hold; readers stop listing
/// after this many and report whether more entries follow.
pub const MAX_MEMBERS: usize = 50;

/// Which bundle container a member set produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub enum BundleKind {
    Cia,
    Cci,
    Cxi,
}

impl BundleKind {
    /// The bundle file extension for this kind.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Cia => "bcia",
            Self::Cci => "bcci",
            Self::Cxi => "bcxi",
        }
    }

    /// Parses a bundle file extension, case-insensitively.
    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "bcia" => Some(Self::Cia),
            "bcci" => Some(Self::Cci),
            "bcxi" => Some(Self::Cxi),
            _ => None,
        }
    }

    /// Whether `ext` (case-insensitive) is a bootable main-member extension
    /// for this kind. A CIA bundle has no main.
    pub fn is_main_ext(self, ext: &str) -> bool {
        let lower = ext.to_ascii_lowercase();
        match self {
            Self::Cci => matches!(lower.as_str(), "3ds" | "cci" | "zcci"),
            Self::Cxi => matches!(lower.as_str(), "cxi" | "zcxi"),
            Self::Cia => false,
        }
    }
}

/// One member of a bundle, in archive order. `offset` is the absolute file
/// offset of the member's data; `regular` is false for non-file entries
/// (directories, PAX/GNU metadata), which the upstream parser still lists
/// and Azahar still matches by suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleEntry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub regular: bool,
}

/// The members of a bundle in archive order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleListing {
    pub entries: Vec<BundleEntry>,
    /// True when the listing stopped at the member cap and more entries follow.
    pub truncated: bool,
}

/// Lists the members of the bundle at `path`, in archive order.
///
/// # Errors
/// Returns [`CtrBundleError::NotABundle`] for a file that is not a tar
/// archive and [`CtrBundleError::InvalidTar`] for a malformed one.
pub fn list_bundle(path: &Path) -> CtrBundleResult<BundleListing> {
    let mut file = std::fs::File::open(path)?;
    tar::read_entries(&mut file, path)
}

/// Derives the bundle output path for `basis`: the same path with the
/// extension replaced by `kind`'s bundle extension.
pub fn derive_bundle_path(basis: &Path, kind: BundleKind) -> PathBuf {
    basis.with_extension(kind.ext())
}

/// Derives the default unbundle output directory for `input`: its stem plus
/// `_unbundled`, beside the input. Archive wrappers are stripped the way every
/// default output is, so `game.bcci.zip` yields `game_unbundled`.
pub fn derive_unbundle_dir(input: &Path) -> PathBuf {
    let mut name =
        crate::util::derive_output_stem(input.file_name().unwrap_or_default()).to_os_string();
    name.push("_unbundled");
    input.with_file_name(name)
}

/// The planned members of a bundle: `members[0]` is the main ROM (when the
/// kind has one) and the naming basis for the output; the rest are CIAs in
/// input order. Built by [`plan_bundle`], which validates member
/// extensions, name lengths, uniqueness, and the member cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundlePlan {
    pub(crate) kind: BundleKind,
    pub(crate) members: Vec<PathBuf>,
}

/// Plans a bundle from `inputs`: at most one main ROM (3ds/cci/zcci or
/// cxi/zcxi) plus CIAs, [`MAX_MEMBERS`] members in total, with names unique
/// by [`name_key`] and within the 99-byte name limit.
///
/// # Errors
/// Returns [`CtrBundleError::NoInputs`], [`CtrBundleError::TooManyMembers`],
/// [`CtrBundleError::UnsupportedMember`], [`CtrBundleError::MultipleMains`],
/// [`CtrBundleError::NameTooLong`], or [`CtrBundleError::DuplicateName`].
pub fn plan_bundle(inputs: &[PathBuf]) -> CtrBundleResult<BundlePlan> {
    if inputs.is_empty() {
        return Err(CtrBundleError::NoInputs);
    }
    let mut main: Option<(usize, BundleKind)> = None;
    for (idx, input) in inputs.iter().enumerate() {
        let ext =
            member_ext(input).ok_or_else(|| CtrBundleError::UnsupportedMember(input.clone()))?;
        if !MEMBER_EXTS.contains(&ext.as_str()) {
            return Err(CtrBundleError::UnsupportedMember(input.clone()));
        }
        let kind = [BundleKind::Cci, BundleKind::Cxi]
            .into_iter()
            .find(|kind| kind.is_main_ext(&ext));
        if let Some(kind) = kind {
            if let Some((main_idx, _)) = main {
                return Err(CtrBundleError::MultipleMains(
                    inputs[main_idx].clone(),
                    input.clone(),
                ));
            }
            main = Some((idx, kind));
        }
    }
    let kind = main.map_or(BundleKind::Cia, |(_, kind)| kind);
    let main_idx = main.map(|(idx, _)| idx);
    let mut members = Vec::with_capacity(inputs.len());
    if let Some(main_idx) = main_idx {
        members.push(inputs[main_idx].clone());
    }
    for (idx, input) in inputs.iter().enumerate() {
        if Some(idx) != main_idx {
            members.push(input.clone());
        }
    }
    let mut names = HashSet::new();
    for member in &members {
        let name = member_tar_name(member);
        if name.len() > MAX_NAME_LEN {
            return Err(CtrBundleError::NameTooLong(name));
        }
        // Same key as unbundling, so a bundle always unpacks again.
        if !names.insert(name_key(&name)) {
            return Err(CtrBundleError::DuplicateName(name));
        }
    }
    if inputs.len() > MAX_MEMBERS {
        return Err(CtrBundleError::TooManyMembers(inputs.len()));
    }
    Ok(BundlePlan { kind, members })
}

/// Key under which two member names count as the same file: Unicode
/// normalization plus a per-character upper-then-lower case fold, which also
/// merges `ſ` with `s` and the two sigma forms the way case-insensitive
/// volumes do. Both planning directions share it, so a bundle written on one
/// host does not unpack with duplicate names on the same host.
fn name_key(name: &str) -> String {
    name.nfc()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .collect()
}

impl BundlePlan {
    /// Checks that `output` ends in this plan's bundle extension, because
    /// readers pick the bundle kind from the extension.
    ///
    /// # Errors
    /// Returns [`CtrBundleError::OutputExtension`] on a mismatch.
    pub fn check_output_extension(&self, output: &Path) -> CtrBundleResult<()> {
        let ext = output
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if ext.as_deref() == Some(self.kind.ext()) {
            Ok(())
        } else {
            Err(CtrBundleError::OutputExtension {
                path: output.to_path_buf(),
                expected: self.kind.ext(),
            })
        }
    }
}

// 99 so the 100-byte tar name field always ends in NUL; a C reader that
// copies the field without its own bound would otherwise run past it.
const MAX_NAME_LEN: usize = 99;

/// Tar member name for an input: its basename with the extension lowercased,
/// because Azahar matches members by lowercase suffix.
fn member_tar_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.rfind('.') {
        Some(dot) => format!("{}{}", &name[..dot], name[dot..].to_ascii_lowercase()),
        None => name,
    }
}

/// Lowercased extension of `path`, if it has one.
fn member_ext(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Bundles the planned members into a tar archive at `output`, the main
/// ROM first. The output must end in the bundle extension the plan's kind
/// produces, because readers pick the bundle kind from the extension.
/// Refuses encrypted members outright; runs its write against a scratch
/// sibling of `output` published only on success.
///
/// # Errors
/// Returns [`CtrBundleError::OutputExtension`] when `output` does not end
/// in the plan's bundle extension, [`CtrBundleError::OutputIsInput`],
/// [`CtrBundleError::Encrypted`], and [`CtrBundleError::Cancelled`] when
/// `cancel` fires.
pub async fn bundle_async(
    plan: BundlePlan,
    output: PathBuf,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> CtrBundleResult<()> {
    plan.check_output_extension(&output)?;
    let members = plan.members.clone();
    let (warnings, total) = tokio::task::spawn_blocking({
        let output = output.clone();
        let cancel = cancel.clone();
        move || {
            for member in &members {
                if crate::util::path::is_same_file(member, &output) {
                    return Err(CtrBundleError::OutputIsInput(output));
                }
            }
            probe_members(&members, &cancel)
        }
    })
    .await??;
    for warning in &warnings {
        progress.warn(warning);
    }
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    if plan.kind == BundleKind::Cxi {
        progress.warn(
            "the Azahar bundle draft boots only CCI/3DS mains; a .bcxi follows the PR text and may not load until upstream lands CXI support",
        );
    }

    progress.start(total, "Bundling 3DS ROMs");
    run_scratch_write(
        &output,
        true,
        progress,
        &cancel,
        move |file, bytes_done, cancel| {
            let proxy = AtomicProgress {
                counter: bytes_done,
            };
            write_bundle(file, &plan, &proxy, &cancel)
        },
    )
    .await?;
    Ok(())
}

/// Probes every member for encryption, mapping the Z3DS check onto bundle
/// errors: an encrypted member is a hard error (upstream plays decrypted
/// ROMs only) and an undeterminable state only warns. Z3DS members are
/// decoded through their seekable reader before the check. Returns the
/// warnings and the total member size.
fn probe_members(members: &[PathBuf], cancel: &CancelToken) -> CtrBundleResult<(Vec<String>, u64)> {
    let mut warnings = Vec::new();
    let mut total = 0u64;
    let mut probe = vec![0u8; ENCRYPTION_PROBE_SIZE];
    for path in members {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        total += std::fs::metadata(path)?.len();
        let Some(ext) = member_ext(path) else {
            continue;
        };
        let checked = match ext.as_str() {
            "cia" | "3ds" | "cci" | "cxi" => {
                let mut file = std::fs::File::open(path)?;
                let read = tar::read_block(&mut file, &mut probe)?;
                check_not_encrypted(&probe[..read], &ext)
            }
            "zcia" | "zcci" | "zcxi" => probe_z3ds(&mut probe, path, &ext[1..])?,
            _ => continue,
        };
        match checked {
            Ok(()) => {}
            Err(Z3dsError::InputNotDecrypted) => {
                return Err(CtrBundleError::Encrypted(path.clone()));
            }
            Err(_) => warnings.push(format!(
                "could not determine whether {} is decrypted; Azahar refuses encrypted bundle members",
                path.display()
            )),
        }
    }
    Ok((warnings, total))
}

/// Decodes a Z3DS member's leading bytes through its seekable reader and
/// checks them against `ext`, the member's extension without the leading
/// `z`. I/O errors other than corrupt or truncated content are hard
/// errors; a member whose container cannot be parsed or decompressed is
/// undeterminable.
fn probe_z3ds(probe: &mut [u8], path: &Path, ext: &str) -> CtrBundleResult<Z3dsResult<()>> {
    let mut file = std::fs::File::open(path)?;
    let mut header_buf = [0u8; Z3DS_HEADER_SIZE as usize];
    if tar::read_block(&mut file, &mut header_buf)? < header_buf.len() {
        return Ok(Err(Z3dsError::EncryptionStateUnknown));
    }
    let decoded = (|| -> Result<Z3dsResult<()>, std::io::Error> {
        let header = Z3dsHeader::read(&mut Cursor::new(&header_buf)).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "unparsable Z3DS header")
        })?;
        let payload_offset = u64::from(header.header_size) + u64::from(header.metadata_size);
        let mut reader = Z3dsReader::open(file, payload_offset, header.compressed_size).map_err(
            |err| match err {
                Z3dsError::IoError(io) => io,
                other => std::io::Error::new(std::io::ErrorKind::InvalidData, other.to_string()),
            },
        )?;
        if reader.len() != header.uncompressed_size {
            return Ok(Err(Z3dsError::EncryptionStateUnknown));
        }
        let take = reader.len().min(probe.len() as u64) as usize;
        reader.read_exact(&mut probe[..take])?;
        Ok(check_not_encrypted(&probe[..take], ext))
    })();
    match decoded {
        Ok(checked) => Ok(checked),
        Err(err) => Ok(Err(probe_io_error(err)?)),
    }
}

/// Sorts a probe failure: corrupt or truncated content is an undeterminable
/// state, real I/O trouble stays a hard error.
fn probe_io_error(err: std::io::Error) -> Result<Z3dsError, std::io::Error> {
    match err.kind() {
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => {
            Ok(Z3dsError::EncryptionStateUnknown)
        }
        _ => Err(err),
    }
}

/// Byte size of the copy chunks the bundle writer and extractor use.
const COPY_CHUNK: usize = 4 * 1024 * 1024;

fn write_bundle(
    mut out: std::fs::File,
    plan: &BundlePlan,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> CtrBundleResult<()> {
    let mut buf = vec![0u8; COPY_CHUNK];
    for member in &plan.members {
        let name = member_tar_name(member);
        let mut file = std::fs::File::open(member)?;
        let meta = file.metadata()?;
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        tar::write_header(&mut out, &name, size, mtime)?;
        copy_stream(&mut file, &mut out, size, &mut buf, progress, cancel)?;
        tar::pad_to_block(&mut out, size)?;
    }
    tar::write_end(&mut out)?;
    out.flush()?;
    Ok(())
}

/// Copies `size` bytes in chunks through `buf`, checking `cancel` per chunk
/// and advancing `progress` by each chunk written.
fn copy_stream(
    file: &mut std::fs::File,
    out: &mut std::fs::File,
    size: u64,
    buf: &mut [u8],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> CtrBundleResult<()> {
    let mut remaining = size;
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let take = (buf.len() as u64).min(remaining) as usize;
        file.read_exact(&mut buf[..take])?;
        out.write_all(&buf[..take])?;
        remaining -= take as u64;
        progress.inc(take as u64);
    }
    Ok(())
}

/// Extracts every regular-file member of the bundle at `input` into
/// `output_dir`. `user_input` is the path the caller named; it differs from
/// `input` when the bundle was unpacked from an archive, and a member that
/// would replace either file is refused. Members are written to scratch files and published
/// together after the final cancel check, all or none: a failure restores
/// every overwritten file and leaves the output directory as it was.
///
/// # Errors
/// Returns [`CtrBundleError::NotABundle`] for a file that is not a tar
/// archive, [`CtrBundleError::InvalidTar`] for a malformed one or one
/// whose member sizes overflow 64 bits, [`CtrBundleError::BundleTooLarge`]
/// for more members than the listing cap, [`CtrBundleError::NoMembers`]
/// when no member is a regular file, [`CtrBundleError::UnsafeName`] for a
/// member name that could escape `output_dir`,
/// [`CtrBundleError::DuplicateName`] for post-reduction duplicates
/// (case-insensitively), [`CtrBundleError::OutputIsInput`] when a planned
/// output path is `input` itself, an [`CtrBundleError::Io`] when a planned
/// output path exists and is not a plain file, and
/// [`CtrBundleError::Cancelled`] when `cancel` fires.
pub async fn unbundle_async(
    input: PathBuf,
    user_input: PathBuf,
    output_dir: PathBuf,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> CtrBundleResult<Vec<PathBuf>> {
    // Planning lists the bundle and probes output paths, so it belongs in
    // the blocking worker with extraction.
    let plan = tokio::task::spawn_blocking({
        let input = input.clone();
        let output_dir = output_dir.clone();
        move || plan_unbundle(&input, &user_input, &output_dir)
    })
    .await??;
    for warning in &plan.warnings {
        progress.warn(warning);
    }
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    progress.start(plan.total, "Unbundling 3DS ROM bundle");
    let bytes_done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let proxy = AtomicProgress {
        counter: bytes_done.clone(),
    };
    let cancel_bg = cancel.clone();
    let handle = tokio::task::spawn_blocking(move || {
        extract_members(&input, &output_dir, plan.members, &proxy, &cancel_bg)
    });
    // Fresh token on purpose: the worker publishes every member after its
    // own final cancel check, so a cancel racing the publish must not turn
    // a completed run into Cancelled.
    await_with_progress_cancel(progress, &bytes_done, handle, &CancelToken::new()).await
}

/// The planned extraction of one bundle: output paths per regular member,
/// their total size, and warnings for unrecognized member names.
struct UnbundlePlan {
    members: Vec<(BundleEntry, PathBuf)>,
    total: u64,
    warnings: Vec<String>,
}

/// Plans the extraction inside the blocking worker. Duplicate,
/// existing-target, and input-overlap checks run here so the file-system
/// work stays off the async runtime.
fn plan_unbundle(
    input: &Path,
    user_input: &Path,
    output_dir: &Path,
) -> CtrBundleResult<UnbundlePlan> {
    let listing = list_bundle(input)?;
    if listing.truncated {
        return Err(CtrBundleError::BundleTooLarge(input.to_path_buf()));
    }
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    let mut members = Vec::with_capacity(listing.entries.len());
    for entry in listing.entries {
        if !entry.regular {
            continue;
        }
        let base = member_base_name(&entry.name)?;
        if !seen.insert(name_key(&base)) {
            return Err(CtrBundleError::DuplicateName(base));
        }
        if !is_member_name(&base) {
            warnings.push(format!(
                "extracting {}, which is not a recognized 3DS ROM or CIA",
                entry.name
            ));
        }
        let out_path = output_dir.join(&base);
        if std::fs::symlink_metadata(&out_path).is_ok_and(|meta| !meta.is_file()) {
            return Err(CtrBundleError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a file", out_path.display()),
            )));
        }
        if crate::util::path::is_same_file(&out_path, input)
            || crate::util::path::is_same_file(&out_path, user_input)
        {
            return Err(CtrBundleError::OutputIsInput(out_path));
        }
        members.push((entry, out_path));
    }
    if members.is_empty() {
        return Err(CtrBundleError::NoMembers(input.to_path_buf()));
    }
    let total = members
        .iter()
        .map(|(entry, _)| entry.size)
        .try_fold(0u64, u64::checked_add)
        .ok_or_else(|| CtrBundleError::InvalidTar {
            path: input.to_path_buf(),
            reason: "member sizes overflow 64 bits".into(),
        })?;
    Ok(UnbundlePlan {
        members,
        total,
        warnings,
    })
}

fn extract_members(
    input: &Path,
    output_dir: &Path,
    plans: Vec<(BundleEntry, PathBuf)>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> CtrBundleResult<Vec<PathBuf>> {
    // Directories this run creates, leaf first. The walk stops at an empty
    // parent, which a relative path ends in.
    let mut created: Vec<&Path> = Vec::new();
    let mut probe = output_dir;
    while !probe.exists() {
        created.push(probe);
        match probe.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => probe = parent,
            _ => break,
        }
    }
    std::fs::create_dir_all(output_dir)?;
    let result = extract_to_scratch(input, &plans, progress, cancel).and_then(|staged| {
        let paths: Vec<PathBuf> = staged.iter().map(|(_, path)| path.clone()).collect();
        publish_set(
            staged
                .into_iter()
                .zip(&paths)
                .map(|((temp, _), path)| (temp, path.as_path()))
                .collect(),
            true,
        )?;
        Ok(paths)
    });
    if result.is_err() {
        // Nothing was published into the new chain: publish_set is all or
        // none and the scratch files are gone. Plain remove_dir leaves
        // anything another process put there.
        for dir in created {
            if std::fs::remove_dir(dir).is_err() {
                break;
            }
        }
    }
    result
}

/// Copies every member to a scratch sibling of its final path without
/// publishing any of them. A failure or cancel here drops every returned
/// [`tempfile::TempPath`], which removes its scratch file, so pre-existing
/// files and directories are untouched.
fn extract_to_scratch(
    input: &Path,
    plans: &[(BundleEntry, PathBuf)],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> CtrBundleResult<Vec<(tempfile::TempPath, PathBuf)>> {
    let mut file = std::fs::File::open(input)?;
    let mut buf = vec![0u8; COPY_CHUNK];
    let mut staged = Vec::with_capacity(plans.len());
    for (entry, out_path) in plans {
        let (mut out, temp) = scratch_output_file(out_path)?.into_parts();
        file.seek(SeekFrom::Start(entry.offset))?;
        copy_stream(&mut file, &mut out, entry.size, &mut buf, progress, cancel)?;
        out.flush()?;
        staged.push((temp, out_path.clone()));
    }
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    Ok(staged)
}

/// Reduces a tar member name to a plain file name that passes
/// [`is_safe_dirent_name`]: not empty or a dot segment, no NUL, and on
/// Windows no drive prefix, NTFS stream, reserved device name, or trailing
/// dot or space.
fn member_base_name(name: &str) -> CtrBundleResult<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if !is_safe_dirent_name(base) {
        return Err(CtrBundleError::UnsafeName(name.to_string()));
    }
    Ok(base.to_string())
}

fn is_member_name(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| MEMBER_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::test_fixtures::synth_cia_with_content;
    use crate::util::NoProgress;
    use sha2::{Digest, Sha256};

    fn path(p: &str) -> PathBuf {
        PathBuf::from(p)
    }

    /// Writes two members with distinct sizes; the CIA comes first so the
    /// main-first reordering is observable.
    fn write_members(dir: &Path) -> Vec<PathBuf> {
        let cia = dir.join("update.CIA");
        std::fs::write(&cia, b"update-payload!").unwrap();
        let cci = dir.join("Game.CCI");
        std::fs::write(&cci, b"main-payload").unwrap();
        vec![cia, cci]
    }

    #[test]
    fn plan_selects_cci_main_and_orders_it_first() {
        let plan = plan_bundle(&[path("dlc.cia"), path("game.cci"), path("update.cia")]).unwrap();
        assert_eq!(plan.kind, BundleKind::Cci);
        assert_eq!(
            plan.members,
            [path("game.cci"), path("dlc.cia"), path("update.cia")]
        );
    }

    #[test]
    fn plan_selects_cxi_main() {
        let plan = plan_bundle(&[path("game.zcxi"), path("dlc.cia")]).unwrap();
        assert_eq!(plan.kind, BundleKind::Cxi);
        assert_eq!(plan.members, [path("game.zcxi"), path("dlc.cia")]);
    }

    #[test]
    fn plan_without_main_is_cia() {
        let plan = plan_bundle(&[path("a.cia"), path("b.zcia")]).unwrap();
        assert_eq!(plan.kind, BundleKind::Cia);
        assert_eq!(plan.members, [path("a.cia"), path("b.zcia")]);
    }

    #[test]
    fn plan_rejects_two_mains() {
        let err = plan_bundle(&[path("a.cci"), path("b.3ds")]).unwrap_err();
        assert!(
            matches!(err, CtrBundleError::MultipleMains(a, b) if a == path("a.cci") && b == path("b.3ds"))
        );
    }

    #[test]
    fn plan_rejects_unsupported_member() {
        let err = plan_bundle(&[path("game.cci"), path("readme.txt")]).unwrap_err();
        assert!(matches!(err, CtrBundleError::UnsupportedMember(p) if p == path("readme.txt")));
    }

    #[test]
    fn plan_rejects_empty_inputs() {
        assert!(matches!(plan_bundle(&[]), Err(CtrBundleError::NoInputs)));
    }

    #[test]
    fn plan_lowercases_member_extension_tar_names() {
        assert_eq!(member_tar_name(Path::new("Game.CCI")), "Game.cci");
        assert_eq!(member_tar_name(Path::new("roms/dlc.Cia")), "dlc.cia");
        assert_eq!(member_tar_name(Path::new("noext")), "noext");
    }

    #[test]
    fn plan_rejects_name_too_long() {
        let long = format!("{}.cci", "x".repeat(100));
        let err = plan_bundle(&[PathBuf::from(&long)]).unwrap_err();
        assert!(matches!(err, CtrBundleError::NameTooLong(n) if n == long));
        // 99 bytes fit and keep the tar name field NUL-terminated.
        let fits = format!("{}.cci", "x".repeat(95));
        assert!(plan_bundle(&[PathBuf::from(&fits)]).is_ok());
        let over = format!("{}.cci", "x".repeat(96));
        assert!(matches!(
            plan_bundle(&[PathBuf::from(&over)]),
            Err(CtrBundleError::NameTooLong(_))
        ));
    }

    #[test]
    fn plan_rejects_too_many_members() {
        let inputs: Vec<PathBuf> = (0..MAX_MEMBERS + 1)
            .map(|i| path(&format!("m{i}.cia")))
            .collect();
        let err = plan_bundle(&inputs).unwrap_err();
        assert!(matches!(err, CtrBundleError::TooManyMembers(n) if n == MAX_MEMBERS + 1));
    }

    #[test]
    fn plan_rejects_duplicate_names_after_lowercasing() {
        let err = plan_bundle(&[path("a.CIA"), path("a.cia")]).unwrap_err();
        assert!(matches!(err, CtrBundleError::DuplicateName(n) if n == "a.cia"));
    }

    #[test]
    fn plan_rejects_names_equal_under_normalization_and_sigma_folding() {
        for (a, b) in [
            ("a/caf\u{e9}.cia", "b/cafe\u{301}.cia"),
            (
                "a/\u{39f}\u{394}\u{39f}\u{3a3}.cia",
                "b/\u{3bf}\u{3b4}\u{3bf}\u{3c3}.cia",
            ),
            (
                "a/\u{3bf}\u{3b4}\u{3bf}\u{3c2}.cia",
                "b/\u{3bf}\u{3b4}\u{3bf}\u{3c3}.cia",
            ),
        ] {
            let err = plan_bundle(&[path(a), path(b)]).unwrap_err();
            assert!(
                matches!(err, CtrBundleError::DuplicateName(_)),
                "{a} vs {b}"
            );
        }
    }

    #[test]
    fn name_key_folds_long_s_and_sigma_forms() {
        assert_eq!(name_key("\u{17f}.cia"), name_key("s.cia"));
        assert_eq!(
            name_key("\u{3bf}\u{3b4}\u{3bf}\u{3c2}.cia"),
            name_key("\u{39f}\u{394}\u{39f}\u{3a3}.cia")
        );
    }

    #[test]
    fn unbundle_dir_strips_archive_and_bundle_extensions() {
        for (input, expected) in [
            ("/roms/game.bcci", "/roms/game_unbundled"),
            ("/roms/game.bcci.zip", "/roms/game_unbundled"),
            ("/roms/game.tar.gz", "/roms/game_unbundled"),
        ] {
            assert_eq!(
                derive_unbundle_dir(Path::new(input)),
                PathBuf::from(expected)
            );
        }
    }

    #[test]
    fn name_key_is_shared_by_planning_and_unbundling() {
        assert_eq!(name_key("caf\u{e9}.CIA"), name_key("cafe\u{301}.cia"));
        assert_ne!(name_key("a.cia"), name_key("b.cia"));
    }

    #[test]
    fn output_extension_must_match_the_plan_kind() {
        let plan = plan_bundle(&[path("game.cci")]).unwrap();
        assert!(plan.check_output_extension(Path::new("out.BCCI")).is_ok());
        for out in ["out.zip", "out.bcia", "out"] {
            assert!(matches!(
                plan.check_output_extension(Path::new(out)),
                Err(CtrBundleError::OutputExtension { .. })
            ));
        }
    }

    #[tokio::test]
    async fn bundle_output_is_listable_by_tar_crate_with_main_first() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(
            plan_bundle(&inputs).unwrap(),
            out.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let mut archive = ::tar::Archive::new(std::fs::File::open(&out).unwrap());
        let listed: Vec<(String, u64)> = archive
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.path().unwrap().to_string_lossy().into_owned(),
                    entry.header().size().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            listed,
            vec![("Game.cci".to_string(), 12), ("update.cia".to_string(), 15),]
        );
    }

    #[tokio::test]
    async fn encrypted_cia_member_is_rejected() {
        let content = vec![0x55u8; 0x400];
        let hash: [u8; 32] = Sha256::digest(&content).into();
        let (_fixture, fixture_cia) = synth_cia_with_content(
            0x0004000000030000,
            vec![(0, 0, content.clone(), hash)],
            content,
            true,
        );
        let tmp = tempfile::tempdir().unwrap();
        let member = tmp.path().join("enc.cia");
        std::fs::copy(&fixture_cia, &member).unwrap();
        let out = tmp.path().join("out.bcia");
        let err = bundle_async(
            plan_bundle(&[member]).unwrap(),
            out,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CtrBundleError::Encrypted(_)));
    }

    #[tokio::test]
    async fn encrypted_zcia_member_is_rejected_through_the_z3ds_reader() {
        use crate::nintendo::ctr::z3ds::compress_rom;
        let content = vec![0x55u8; 0x400];
        let hash: [u8; 32] = Sha256::digest(&content).into();
        let (_fixture, fixture_cia) = synth_cia_with_content(
            0x0004000000030000,
            vec![(0, 0, content.clone(), hash)],
            content,
            true,
        );
        let tmp = tempfile::tempdir().unwrap();
        let zcia = tmp.path().join("enc.zcia");
        compress_rom(
            &fixture_cia,
            &zcia,
            None,
            true,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let out = tmp.path().join("out.bcia");
        let err = bundle_async(
            plan_bundle(&[zcia]).unwrap(),
            out,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CtrBundleError::Encrypted(_)));
    }

    struct WarnCollector(std::sync::Mutex<Vec<String>>);

    impl ProgressReporter for WarnCollector {
        fn start(&self, _total: u64, _msg: &str) {}
        fn inc(&self, _delta: u64) {}
        fn finish(&self) {}
        fn warn(&self, message: &str) {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(message.to_owned());
        }
    }

    #[tokio::test]
    async fn undeterminable_zcia_member_warns_instead_of_failing() {
        let tmp = tempfile::tempdir().unwrap();
        let zcia = tmp.path().join("junk.zcia");
        std::fs::write(&zcia, vec![0u8; 64]).unwrap();
        let out = tmp.path().join("out.bcia");
        let reporter = WarnCollector(std::sync::Mutex::new(Vec::new()));
        bundle_async(
            plan_bundle(&[zcia]).unwrap(),
            out,
            &reporter,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let warnings = reporter.0.into_inner().unwrap_or_else(|e| e.into_inner());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("junk.zcia"));
    }

    #[tokio::test]
    async fn bundle_rejects_an_output_with_the_wrong_extension() {
        let plan = plan_bundle(&[path("game.cci")]).unwrap();
        for out in ["out.zip", "out.bcia", "out"] {
            let err = bundle_async(
                plan.clone(),
                PathBuf::from(out),
                &NoProgress,
                CancelToken::new(),
            )
            .await
            .unwrap_err();
            assert!(
                matches!(err, CtrBundleError::OutputExtension { .. }),
                "{out}"
            );
        }
    }

    #[tokio::test]
    async fn bundle_rejects_an_output_that_is_an_input() {
        if cfg!(windows) {
            // Creating the symlink needs privileges Windows test runs lack.
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let member = tmp.path().join("game.cia");
        std::fs::write(&member, b"payload").unwrap();
        let out = tmp.path().join("alias.bcia");
        std::os::unix::fs::symlink(&member, &out).unwrap();
        let err = bundle_async(
            plan_bundle(&[member]).unwrap(),
            out,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CtrBundleError::OutputIsInput(_)));
    }

    // Drives write_bundle directly: bundle_async only relays the writer's
    // progress on a 100 ms poll (run_scratch_write), so a reporter-triggered
    // cancel cannot be timed against a small fixture through the async
    // entry point.
    #[tokio::test]
    async fn bundle_cancel_mid_write_stops_the_writer() {
        let tmp = tempfile::tempdir().unwrap();
        let member = tmp.path().join("game.cia");
        std::fs::write(&member, vec![0u8; 5 * 1024 * 1024]).unwrap();
        let out = tmp.path().join("out.bcia");
        let cancel = CancelToken::new();
        let reporter = CancelOnFirstInc {
            cancel: cancel.clone(),
            fired: std::sync::atomic::AtomicBool::new(false),
        };
        let plan = plan_bundle(&[member]).unwrap();
        let err = write_bundle(
            std::fs::File::create(&out).unwrap(),
            &plan,
            &reporter,
            &cancel,
        )
        .unwrap_err();
        assert!(Cancelled::in_chain(&anyhow::Error::from(err)));
    }

    #[tokio::test]
    async fn bundle_output_is_deterministic_for_identical_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let member = tmp.path().join("game.cia");
        std::fs::write(&member, b"payload").unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&member)
            .unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_234_567)),
        )
        .unwrap();
        drop(file);

        let first = tmp.path().join("a.bcia");
        let second = tmp.path().join("b.bcia");
        let plan = plan_bundle(&[member]).unwrap();
        bundle_async(plan.clone(), first.clone(), &NoProgress, CancelToken::new())
            .await
            .unwrap();
        bundle_async(plan, second.clone(), &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let a = std::fs::read(&first).unwrap();
        let b = std::fs::read(&second).unwrap();
        assert_eq!(a, b);
        // The header carries the member's mtime, not the time of the run.
        let mtime = std::str::from_utf8(&a[136..147])
            .unwrap()
            .trim_end_matches('\0');
        assert_eq!(u64::from_str_radix(mtime, 8).unwrap(), 1_234_567);
    }

    #[tokio::test]
    async fn unbundle_round_trips_members() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(
            plan_bundle(&inputs).unwrap(),
            out.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let dir = tmp.path().join("members");
        let written = unbundle_async(
            out.clone(),
            out,
            dir.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(written, [dir.join("Game.cci"), dir.join("update.cia")]);
        assert_eq!(
            std::fs::read(dir.join("Game.cci")).unwrap(),
            b"main-payload"
        );
        assert_eq!(
            std::fs::read(dir.join("update.cia")).unwrap(),
            b"update-payload!"
        );
    }

    #[test]
    fn member_base_name_reduces_to_a_safe_basename() {
        assert_eq!(
            member_base_name("roms/dir/Game.CCI").unwrap(),
            "Game.CCI".to_string()
        );
        for name in ["..", ".", "", "a/..", "dir/.", "a\0.cia"] {
            assert!(
                matches!(member_base_name(name), Err(CtrBundleError::UnsafeName(_))),
                "{name:?}"
            );
        }
        // Colons name a drive prefix or an NTFS stream on Windows only.
        if cfg!(windows) {
            assert!(matches!(
                member_base_name("C:evil.cia"),
                Err(CtrBundleError::UnsafeName(_))
            ));
            assert!(matches!(
                member_base_name("a.cia:stream"),
                Err(CtrBundleError::UnsafeName(_))
            ));
        } else {
            assert_eq!(
                member_base_name("Title: Sub.cci").unwrap(),
                "Title: Sub.cci".to_string()
            );
        }
    }

    #[test]
    fn member_base_name_rejects_windows_unsafe_names_on_windows() {
        if !cfg!(windows) {
            return;
        }
        for name in ["CON.cia", "x.cia.", "x.cia "] {
            assert!(
                matches!(member_base_name(name), Err(CtrBundleError::UnsafeName(_))),
                "{name:?}"
            );
        }
    }

    #[tokio::test]
    async fn unbundle_round_trips_a_coloned_member_name() {
        if cfg!(windows) {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bcia");
        let mut buf = Vec::new();
        tar::write_header(&mut buf, "Title: Sub.cia", 1, 0).unwrap();
        buf.extend_from_slice(b"x");
        tar::pad_to_block(&mut buf, 1).unwrap();
        tar::write_end(&mut buf).unwrap();
        std::fs::write(&out, buf).unwrap();

        let dir = tmp.path().join("members");
        let written = unbundle_async(
            out.clone(),
            out,
            dir.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(written, [dir.join("Title: Sub.cia")]);
    }

    #[tokio::test]
    async fn unbundle_skips_non_regular_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let cci = tmp.path().join("game.cci");
        std::fs::write(&cci, b"main-payload").unwrap();
        let out = tmp.path().join("out.bcci");
        let mut builder = ::tar::Builder::new(std::fs::File::create(&out).unwrap());
        // A hand-built directory header, not `append_dir`: real directory
        // metadata can stamp a nonzero on-disk "size" that would desync
        // the reader, which trusts the size field for the next offset.
        let mut dir_header = ::tar::Header::new_ustar();
        dir_header.set_path("folder").unwrap();
        dir_header.set_size(0);
        dir_header.set_entry_type(::tar::EntryType::Directory);
        dir_header.set_mode(0o755);
        dir_header.set_cksum();
        builder.append(&dir_header, std::io::empty()).unwrap();
        builder.append_path_with_name(&cci, "game.cci").unwrap();
        builder.finish().unwrap();

        let dir = tmp.path().join("members");
        let written = unbundle_async(
            out.clone(),
            out,
            dir.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(written, [dir.join("game.cci")]);
        assert_eq!(
            std::fs::read(dir.join("game.cci")).unwrap(),
            b"main-payload"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    struct CancelOnFirstInc {
        cancel: CancelToken,
        fired: std::sync::atomic::AtomicBool,
    }

    impl ProgressReporter for CancelOnFirstInc {
        fn start(&self, _total: u64, _msg: &str) {}
        fn inc(&self, _delta: u64) {
            if !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.cancel.cancel();
            }
        }
        fn finish(&self) {}
    }

    // Drives extract_members directly: unbundle_async only relays the worker's
    // progress on a 100ms poll, so a reporter-triggered cancel cannot be timed
    // against a small fixture through the async entry point.
    #[tokio::test]
    async fn unbundle_cancel_mid_extraction_preserves_foreign_and_stale_files() {
        let tmp = tempfile::tempdir().unwrap();
        let cci = tmp.path().join("game.cci");
        std::fs::write(&cci, b"fresh-main").unwrap();
        let cia = tmp.path().join("update.cia");
        std::fs::write(&cia, b"update-payload!").unwrap();
        let out = tmp.path().join("out.bcci");
        bundle_async(
            plan_bundle(&[cci, cia]).unwrap(),
            out.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let dir = tmp.path().join("members");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep.txt"), b"keep me").unwrap();
        std::fs::write(dir.join("game.cci"), b"stale").unwrap();

        let cancel = CancelToken::new();
        let reporter = CancelOnFirstInc {
            cancel: cancel.clone(),
            fired: std::sync::atomic::AtomicBool::new(false),
        };
        let plans = list_bundle(&out)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| {
                let path = dir.join(&entry.name);
                (entry, path)
            })
            .collect();
        let err = extract_members(&out, &dir, plans, &reporter, &cancel).unwrap_err();
        assert!(Cancelled::in_chain(&anyhow::Error::from(err)));
        assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"keep me");
        assert_eq!(std::fs::read(dir.join("game.cci")).unwrap(), b"stale");
        assert!(!dir.join("update.cia").exists());
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .all(|e| e.path().extension().and_then(|s| s.to_str()) != Some("tmp"))
        );
    }

    #[tokio::test]
    async fn unbundle_cancelled_before_extraction_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cia = tmp.path().join("update.cia");
        std::fs::write(&cia, b"update-payload!").unwrap();
        let out = tmp.path().join("out.bcia");
        bundle_async(
            plan_bundle(&[cia]).unwrap(),
            out.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let dir = tmp.path().join("members");
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = unbundle_async(out.clone(), out, dir.clone(), &NoProgress, cancel)
            .await
            .unwrap_err();
        assert!(Cancelled::in_chain(&anyhow::Error::from(err)));
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn unbundle_rejects_a_last_member_whose_size_exceeds_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bcci");
        let mut file = std::fs::File::create(&out).unwrap();
        for i in 0..MAX_MEMBERS - 1 {
            tar::write_header(&mut file, &format!("m{i}.cia"), 0, 0).unwrap();
        }
        tar::write_header(&mut file, "big.cia", 5000, 0).unwrap();
        drop(file);

        let dir = tmp.path().join("fresh");
        let err = unbundle_async(
            out.clone(),
            out,
            dir.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CtrBundleError::InvalidTar { .. }));
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn unbundle_rejects_bundles_over_the_member_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bcci");
        let mut file = std::fs::File::create(&out).unwrap();
        for i in 0..MAX_MEMBERS + 1 {
            tar::write_header(&mut file, &format!("m{i}.cia"), 0, 0).unwrap();
        }
        tar::write_end(&mut file).unwrap();
        drop(file);

        let dir = tmp.path().join("members");
        let err = unbundle_async(
            out.clone(),
            out,
            dir.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CtrBundleError::BundleTooLarge(_)));
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn unbundle_rejects_a_bundle_without_regular_members() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bcci");
        let mut builder = ::tar::Builder::new(std::fs::File::create(&out).unwrap());
        let mut dir_header = ::tar::Header::new_ustar();
        dir_header.set_path("folder").unwrap();
        dir_header.set_size(0);
        dir_header.set_entry_type(::tar::EntryType::Directory);
        dir_header.set_mode(0o755);
        dir_header.set_cksum();
        builder.append(&dir_header, std::io::empty()).unwrap();
        builder.finish().unwrap();

        let dir = tmp.path().join("members");
        let err = unbundle_async(
            out.clone(),
            out,
            dir.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CtrBundleError::NoMembers(_)));
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn unbundle_rejects_duplicates_after_basename_reduction() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bcia");
        let mut buf = Vec::new();
        for name in ["a/x.cia", "b/x.cia"] {
            tar::write_header(&mut buf, name, 1, 0).unwrap();
            buf.extend_from_slice(b"x");
            tar::pad_to_block(&mut buf, 1).unwrap();
        }
        tar::write_end(&mut buf).unwrap();
        std::fs::write(&out, buf).unwrap();

        let dir = tmp.path().join("members");
        let err = unbundle_async(out.clone(), out, dir, &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, CtrBundleError::DuplicateName(n) if n == "x.cia"));
    }

    #[tokio::test]
    async fn unbundle_rejects_normalization_equivalent_names() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bcia");
        let mut buf = Vec::new();
        for name in ["caf\u{e9}.cia", "cafe\u{301}.cia"] {
            tar::write_header(&mut buf, name, 1, 0).unwrap();
            buf.extend_from_slice(b"x");
            tar::pad_to_block(&mut buf, 1).unwrap();
        }
        tar::write_end(&mut buf).unwrap();
        std::fs::write(&out, buf).unwrap();

        let dir = tmp.path().join("members");
        let err = unbundle_async(out.clone(), out, dir, &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, CtrBundleError::DuplicateName(_)));
    }

    #[tokio::test]
    async fn unbundle_failure_removes_the_created_directory_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(
            plan_bundle(&inputs).unwrap(),
            out.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let dir = tmp.path().join("new").join("members");
        let cancel = CancelToken::new();
        let reporter = CancelOnFirstInc {
            cancel: cancel.clone(),
            fired: std::sync::atomic::AtomicBool::new(false),
        };
        let plans = list_bundle(&out)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| {
                let path = dir.join(&entry.name);
                (entry, path)
            })
            .collect();
        let err = extract_members(&out, &dir, plans, &reporter, &cancel).unwrap_err();
        assert!(Cancelled::in_chain(&anyhow::Error::from(err)));
        assert!(!tmp.path().join("new").exists());
    }

    // A relative output path ends its parent walk at an empty parent; the
    // new leaf must still be removed. The directory name is unique per
    // process and lives under the test's working directory.
    #[tokio::test]
    async fn unbundle_failure_removes_a_relative_output_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(
            plan_bundle(&inputs).unwrap(),
            out.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let dir = PathBuf::from(format!("rc_unbundle_relative_{}", std::process::id()));
        let cancel = CancelToken::new();
        let reporter = CancelOnFirstInc {
            cancel: cancel.clone(),
            fired: std::sync::atomic::AtomicBool::new(false),
        };
        let plans = list_bundle(&out)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| {
                let path = dir.join(&entry.name);
                (entry, path)
            })
            .collect();
        let result = extract_members(&out, &dir, plans, &reporter, &cancel);
        let left_behind = dir.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.is_err());
        assert!(!left_behind);
    }

    #[tokio::test]
    async fn unbundle_refuses_to_replace_the_users_archive_when_reading_a_staged_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = tmp.path().join("staged.bcia");
        let mut buf = Vec::new();
        tar::write_header(&mut buf, "pack.zip", 1, 0).unwrap();
        buf.extend_from_slice(b"x");
        tar::pad_to_block(&mut buf, 1).unwrap();
        tar::write_end(&mut buf).unwrap();
        std::fs::write(&staged, buf).unwrap();
        // The user's archive sits where a member would be written.
        let user_input = tmp.path().join("pack.zip");
        std::fs::write(&user_input, b"archive").unwrap();

        let err = unbundle_async(
            staged,
            user_input.clone(),
            tmp.path().to_path_buf(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CtrBundleError::OutputIsInput(_)));
        assert_eq!(std::fs::read(&user_input).unwrap(), b"archive");
    }

    #[tokio::test]
    async fn unbundle_overwrites_pre_existing_same_named_file() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(
            plan_bundle(&inputs).unwrap(),
            out.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let dir = tmp.path().join("members");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Game.cci"), b"stale-content").unwrap();

        let written = unbundle_async(
            out.clone(),
            out,
            dir.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(written, [dir.join("Game.cci"), dir.join("update.cia")]);
        assert_eq!(
            std::fs::read(dir.join("Game.cci")).unwrap(),
            b"main-payload"
        );
    }

    #[tokio::test]
    async fn unbundle_fails_before_publishing_when_target_is_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(
            plan_bundle(&inputs).unwrap(),
            out.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let dir = tmp.path().join("members");
        std::fs::create_dir_all(dir.join("Game.cci")).unwrap();

        let err = unbundle_async(
            out.clone(),
            out,
            dir.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CtrBundleError::Io(_)));
        assert!(!dir.join("update.cia").exists());
    }

    // Publishing must be all or none: the first member overwrites an
    // existing file, the second target is a directory (which the caller's
    // pre-publish checks reject, but the publish itself must survive), so
    // the publish fails and the overwrite has to be rolled back.
    #[tokio::test]
    async fn unbundle_publish_failure_restores_overwritten_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(
            plan_bundle(&inputs).unwrap(),
            out.clone(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let dir = tmp.path().join("members");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Game.cci"), b"stale").unwrap();
        std::fs::create_dir_all(dir.join("update.cia")).unwrap();

        let plans = list_bundle(&out)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| (entry.clone(), dir.join(&entry.name)))
            .collect();
        let err = extract_members(&out, &dir, plans, &NoProgress, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, CtrBundleError::Io(_)));
        assert_eq!(std::fs::read(dir.join("Game.cci")).unwrap(), b"stale");
        assert!(dir.join("update.cia").is_dir());
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .all(|e| e.path().extension().and_then(|s| s.to_str()) != Some("tmp"))
        );
    }
}
