//! Azahar bundle ROMs (`.bcia`/`.bcci`/`.bcxi`): plain uncompressed tar
//! archives of CTR ROM members, per Azahar PR #2369. Bundling requires
//! decrypted members because Azahar plays decrypted ROMs only.

pub mod error;
pub(crate) mod tar;

pub use error::{CtrBundleError, CtrBundleResult};

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::nintendo::ctr::z3ds::{ENCRYPTION_PROBE_SIZE, check_not_encrypted, error::Z3dsError};
use crate::util::{
    AtomicProgress, CancelToken, Cancelled, ProgressReporter, await_with_progress_cancel,
    publish_temp, run_scratch_write, scratch_output_path,
};

/// File extensions of Azahar bundle containers.
pub const BUNDLE_EXTS: &[&str] = &["bcia", "bcci", "bcxi"];

/// File extensions accepted as bundle members.
pub const MEMBER_EXTS: &[&str] = &["cia", "zcia", "3ds", "cci", "zcci", "cxi", "zcxi"];

/// Maximum number of members a bundle may hold; microtar-based readers
/// stop listing after this many.
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
/// (directories, PAX/GNU metadata), which microtar still lists and Azahar
/// still matches by suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleEntry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub regular: bool,
}

/// Lists the members of the bundle at `path`, in archive order.
///
/// # Errors
/// Returns [`CtrBundleError::NotABundle`] for a file that is not a tar
/// archive and [`CtrBundleError::InvalidTar`] for a malformed one.
pub fn list_bundle(path: &Path) -> CtrBundleResult<Vec<BundleEntry>> {
    let mut file = std::fs::File::open(path)?;
    tar::read_entries(&mut file, path)
}

/// Derives the bundle output path for `basis`: the same path with the
/// extension replaced by `kind`'s bundle extension.
pub fn derive_bundle_path(basis: &Path, kind: BundleKind) -> PathBuf {
    basis.with_extension(kind.ext())
}

/// Derives the default unbundle output directory for `input`: its stem plus
/// `_unbundled`, beside the input.
pub fn derive_unbundle_dir(input: &Path) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    input.with_file_name(format!("{stem}_unbundled"))
}

/// The planned members of a bundle: `members[0]` is the main ROM (when the
/// kind has one) and the naming basis for the output; the rest are CIAs in
/// input order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundlePlan {
    pub kind: BundleKind,
    pub members: Vec<PathBuf>,
}

/// Plans a bundle from `inputs`: exactly one optional main ROM
/// (3ds/cci/zcci or cxi/zcxi) plus any number of CIAs, member names unique
/// after extension lowercasing and within the 100-byte tar limit.
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
        // Case-insensitive so the bundle also unpacks on macOS and Windows.
        if !names.insert(name.to_lowercase()) {
            return Err(CtrBundleError::DuplicateName(name));
        }
    }
    if inputs.len() > MAX_MEMBERS {
        return Err(CtrBundleError::TooManyMembers(inputs.len()));
    }
    Ok(BundlePlan { kind, members })
}

const MAX_NAME_LEN: usize = 100;

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

/// Bundles the member files at `inputs` into a tar archive at `output`,
/// the main ROM first. Refuses encrypted members outright; runs its write
/// against a scratch sibling of `output` published only on success.
///
/// # Errors
/// Same as [`plan_bundle`], plus [`CtrBundleError::OutputIsInput`],
/// [`CtrBundleError::Encrypted`], and [`CtrBundleError::Cancelled`] when
/// `cancel` fires.
pub async fn bundle_async(
    inputs: Vec<PathBuf>,
    output: PathBuf,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> CtrBundleResult<()> {
    let plan = plan_bundle(&inputs)?;
    if inputs
        .iter()
        .any(|input| crate::util::path::is_same_file(input, &output))
    {
        return Err(CtrBundleError::OutputIsInput(output));
    }
    let (warnings, total) = tokio::task::spawn_blocking({
        let members = plan.members.clone();
        move || probe_members(&members)
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
        move |write_path, bytes_done, cancel| {
            let proxy = AtomicProgress {
                counter: bytes_done,
            };
            write_bundle(&write_path, &plan, &proxy, &cancel)
        },
    )
    .await?;
    Ok(())
}

/// Probes every uncompressed member for encryption, mapping the Z3DS check
/// onto bundle errors: an encrypted member is a hard error (upstream plays
/// decrypted ROMs only), an undeterminable state only warns, and Z-prefixed
/// members skip the check. Returns the warnings and the total member size.
fn probe_members(members: &[PathBuf]) -> CtrBundleResult<(Vec<String>, u64)> {
    let mut warnings = Vec::new();
    let mut total = 0u64;
    for path in members {
        total += std::fs::metadata(path)?.len();
        let Some(ext) = member_ext(path) else {
            continue;
        };
        if !matches!(ext.as_str(), "cia" | "3ds" | "cci" | "cxi") {
            continue;
        }
        let mut probe = vec![0u8; ENCRYPTION_PROBE_SIZE];
        let mut file = std::fs::File::open(path)?;
        let mut read = 0usize;
        while read < probe.len() {
            match file.read(&mut probe[read..])? {
                0 => break,
                n => read += n,
            }
        }
        probe.truncate(read);
        match check_not_encrypted(&probe, &ext) {
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

fn write_bundle(
    write_path: &Path,
    plan: &BundlePlan,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> CtrBundleResult<()> {
    let mut out = std::fs::File::create(write_path)?;
    for member in &plan.members {
        let name = member_tar_name(member);
        let mut file = std::fs::File::open(member)?;
        let size = file.metadata()?.len();
        tar::write_header(&mut out, &name, size)?;
        copy_stream(&mut file, &mut out, size, progress, cancel)?;
        tar::pad_to_block(&mut out, size)?;
    }
    tar::write_end(&mut out)?;
    out.flush()?;
    Ok(())
}

/// Copies `size` bytes in 4 MiB chunks, checking `cancel` per chunk and
/// advancing `progress` by each chunk written.
fn copy_stream(
    file: &mut std::fs::File,
    out: &mut std::fs::File,
    size: u64,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> CtrBundleResult<()> {
    const CHUNK: usize = 4 * 1024 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut remaining = size;
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let take = (CHUNK as u64).min(remaining) as usize;
        file.read_exact(&mut buf[..take])?;
        out.write_all(&buf[..take])?;
        remaining -= take as u64;
        progress.inc(take as u64);
    }
    Ok(())
}

/// Extracts every regular-file member of the bundle at `input` into
/// `output_dir`. Members are written to scratch files and published
/// together after the final cancel check; a failure before publishing
/// leaves the output directory as it was (a leaf directory this run
/// created is removed when empty; ancestors `create_dir_all` made are
/// kept); a failure while publishing leaves the already published
/// members in place.
///
/// # Errors
/// Returns [`CtrBundleError::UnsafeName`] for a member name that could
/// escape `output_dir`, [`CtrBundleError::DuplicateName`] for
/// post-reduction duplicates (case-insensitively),
/// [`CtrBundleError::OutputIsInput`] when a planned output path is `input`
/// itself, an [`CtrBundleError::Io`] when a planned output path exists and
/// is not a plain file, and [`CtrBundleError::Cancelled`] when `cancel`
/// fires.
pub async fn unbundle_async(
    input: PathBuf,
    output_dir: PathBuf,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> CtrBundleResult<Vec<PathBuf>> {
    let entries = tokio::task::spawn_blocking({
        let input = input.clone();
        move || list_bundle(&input)
    })
    .await??;
    let mut seen = HashSet::new();
    let mut plans: Vec<(BundleEntry, PathBuf)> = Vec::with_capacity(entries.len());
    for entry in entries {
        if !entry.regular {
            continue;
        }
        let base = member_base_name(&entry.name)?;
        let key = base.to_lowercase();
        if !seen.insert(key) {
            return Err(CtrBundleError::DuplicateName(base));
        }
        if !is_member_name(&base) {
            progress.warn(&format!(
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
        if crate::util::path::is_same_file(&out_path, &input) {
            return Err(CtrBundleError::OutputIsInput(out_path));
        }
        plans.push((entry, out_path));
    }
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let total = plans.iter().map(|(entry, _)| entry.size).sum();
    progress.start(total, "Unbundling 3DS ROM bundle");
    let bytes_done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let proxy = AtomicProgress {
        counter: bytes_done.clone(),
    };
    let cancel_bg = cancel.clone();
    let handle = tokio::task::spawn_blocking(move || {
        extract_members(&input, &output_dir, plans, &proxy, &cancel_bg)
    });
    await_with_progress_cancel(progress, &bytes_done, handle, &CancelToken::new()).await
}

fn extract_members(
    input: &Path,
    output_dir: &Path,
    plans: Vec<(BundleEntry, PathBuf)>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> CtrBundleResult<Vec<PathBuf>> {
    let created_dir = !output_dir.exists();
    std::fs::create_dir_all(output_dir)?;
    let result = extract_to_scratch(input, &plans, progress, cancel).and_then(|staged| {
        let mut written = Vec::with_capacity(staged.len());
        for (temp, out_path) in staged {
            publish_temp(temp, &out_path, true)?;
            written.push(out_path);
        }
        Ok(written)
    });
    if result.is_err() && created_dir {
        // Only removes the directory this run created, and only when it
        // ended up empty: nothing was published into it.
        let _ = std::fs::remove_dir(output_dir);
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
    let mut staged = Vec::with_capacity(plans.len());
    for (entry, out_path) in plans {
        let temp = scratch_output_path(out_path)?;
        let mut out = std::fs::File::create(&temp)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        copy_stream(&mut file, &mut out, entry.size, progress, cancel)?;
        out.flush()?;
        staged.push((temp, out_path.clone()));
    }
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    Ok(staged)
}

/// Reduces a tar member name to a plain file name, rejecting names that
/// could escape the output directory or resolve to a Windows drive prefix
/// or NTFS alternate data stream: the reduced basename must contain no
/// colon and be exactly one normal path component.
fn member_base_name(name: &str) -> CtrBundleResult<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if base.contains(':') {
        return Err(CtrBundleError::UnsafeName(name.to_string()));
    }
    let mut components = Path::new(base).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(base.to_string()),
        _ => Err(CtrBundleError::UnsafeName(name.to_string())),
    }
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

    #[tokio::test]
    async fn bundle_output_is_listable_by_tar_crate_with_main_first() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(inputs, out.clone(), &NoProgress, CancelToken::new())
            .await
            .unwrap();
        // `bundle::tar` shadows the crate here; the extern crate needs the
        // leading path qualifier.
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
        let err = bundle_async(vec![member], out, &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, CtrBundleError::Encrypted(_)));
    }

    #[tokio::test]
    async fn unbundle_round_trips_members() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(inputs, out.clone(), &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let dir = tmp.path().join("members");
        let written = unbundle_async(out, dir.clone(), &NoProgress, CancelToken::new())
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
    fn member_base_name_rejects_dot_segments_and_drive_prefix() {
        assert_eq!(
            member_base_name("roms/dir/Game.CCI").unwrap(),
            "Game.CCI".to_string()
        );
        for name in ["..", ".", "", "a/..", "dir/.", "C:evil.cia", "a.cia:stream"] {
            assert!(
                matches!(member_base_name(name), Err(CtrBundleError::UnsafeName(_))),
                "{name:?}"
            );
        }
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
        let written = unbundle_async(out, dir.clone(), &NoProgress, CancelToken::new())
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
        bundle_async(vec![cci, cia], out.clone(), &NoProgress, CancelToken::new())
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
        bundle_async(vec![cia], out.clone(), &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let dir = tmp.path().join("members");
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = unbundle_async(out, dir.clone(), &NoProgress, cancel)
            .await
            .unwrap_err();
        assert!(Cancelled::in_chain(&anyhow::Error::from(err)));
        assert!(!dir.exists());
    }

    /// `read_entries` trusts a member's declared size to compute the next
    /// header's offset, so a truncated member only surfaces once extraction
    /// tries to read its declared byte range; reaching that worker-side
    /// failure without the listing itself validating the truncated span
    /// requires the bad member to be the archive's last (50th) one, since
    /// the `MAX_MEMBERS` cap returns the listing without checking for a
    /// following header or end record.
    #[tokio::test]
    async fn unbundle_fails_inside_worker_when_last_member_size_exceeds_data() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.bcci");
        let mut file = std::fs::File::create(&out).unwrap();
        for i in 0..MAX_MEMBERS - 1 {
            tar::write_header(&mut file, &format!("m{i}.cia"), 0).unwrap();
        }
        tar::write_header(&mut file, "big.cia", 5000).unwrap();
        drop(file);

        let dir = tmp.path().join("fresh");
        let err = unbundle_async(out, dir.clone(), &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(!matches!(err, CtrBundleError::Cancelled(_)));
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn unbundle_overwrites_pre_existing_same_named_file() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = write_members(tmp.path());
        let out = tmp.path().join("out.bcci");
        bundle_async(inputs, out.clone(), &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let dir = tmp.path().join("members");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Game.cci"), b"stale-content").unwrap();

        let written = unbundle_async(out, dir.clone(), &NoProgress, CancelToken::new())
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
        bundle_async(inputs, out.clone(), &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let dir = tmp.path().join("members");
        std::fs::create_dir_all(dir.join("Game.cci")).unwrap();

        let err = unbundle_async(out, dir.clone(), &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, CtrBundleError::Io(_)));
        assert!(!dir.join("update.cia").exists());
    }
}
