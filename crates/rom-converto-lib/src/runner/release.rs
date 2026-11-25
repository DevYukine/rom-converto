//! `delete_source`: once a conversion wrote its output, delete its input
//! with every file it consists of (a sheet and its tracks, split parts, a
//! folder input, an archive whose every entry was used, per-game key
//! files), then the folders that leaves empty. Files go through organize's
//! [`remove_sources`], so both share its guards: nothing outside the input
//! root, nothing another scanned unit still claims, never an output.

use super::models::{RunData, RunRequest, RunResponse, RunStatus, WupTitleInputOption};
use super::ops::{first_wup_input, totals_for};
use super::organize::{
    FileIdentity, entry_location, real_layout, remove_sources, restore_source, sync_outputs,
    unit_source_files,
};
use crate::dat::units::{DatUnit, collect_units};
use crate::disc::cue::CueParser;
use crate::nintendo::wup::disc::sector_stream::split_part_number;
use crate::util::fs::{has_ext, is_os_junk_file};
use crate::util::{CancelToken, Cancelled, FileStatus, ProgressReporter, is_archive_path};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Canonical names of the operations that accept `delete_source`: each
/// writes the copy the user keeps in place of its input.
pub(crate) const DELETE_SOURCE_OPS: &[&str] = &[
    "chd.compress",
    "chd.migrate",
    "cso.compress",
    "cso.decompress",
    "dol.compress",
    "dol.decompress",
    "rvl.compress",
    "rvl.decompress",
    "rvz.compress",
    "rvz.decompress",
    "ctr.compress",
    "ctr.decompress",
    "nx.compress",
    "nx.decompress",
    "wup.compress",
    "wup.to_wux",
    "wup.to_wud",
    "xenon.compress",
    "xbox.convert",
    "ps3.decrypt",
];

/// The operations of [`DELETE_SOURCE_OPS`] whose output `verify_after`
/// checks. A real run reads the verdict off its response; a dry run has no
/// output to check, so it keeps the sources of every other operation the
/// way the real run will.
const CHECKED_OPS: &[&str] = &[
    "chd.compress",
    "chd.migrate",
    "cso.compress",
    "dol.compress",
    "rvl.compress",
    "rvz.compress",
    "nx.compress",
];

const NO_CHECK: &str = "no output check for this format";

/// The row of a walked input that an earlier child deleted with its set.
pub(crate) const RELEASED_PART: &str = "deleted with the source set it belongs to";
/// [`RELEASED_PART`] in a dry run, which deletes nothing.
pub(crate) const PLANNED_PART: &str = "would be deleted with the source set it belongs to";

/// Outputs the runs of this process claimed, from where each resolved its
/// output until its write landed, and for good once a `delete_source` run
/// deleted its source: another source written onto one would replace the
/// only copy of a source already gone. Keyed by the output's lowercased own location, the way the batch dedupe
/// keys outputs, so a case variant on a case-insensitive volume is the same
/// output. On a case-sensitive volume a false match keeps the source of a
/// `delete_source` run, and fails a run that keeps its own. It spans one
/// CLI run, or one GUI session, where each queued file is a run of its own.
static CLAIMS: Mutex<BTreeMap<String, Claim>> = Mutex::new(BTreeMap::new());

struct Claim {
    /// Tells this claim from a later one on the same output.
    token: u64,
    /// The source's exact own location: names that differ only in case
    /// are different sources on a case-sensitive volume.
    source: PathBuf,
    /// The output as the claiming run spelled it.
    output: PathBuf,
    /// Its source is deleted, so the output is the only copy left.
    committed: bool,
}

fn output_key(output: &Path) -> String {
    real_layout(output).to_string_lossy().to_lowercase()
}

/// Why `claim` holds `output` against any writer: a run is writing it, or
/// its source was deleted and the output still exists.
fn held(claim: &Claim, output: &Path) -> Option<String> {
    if !claim.committed {
        return Some(format!(
            "{} is being written from {}",
            output.display(),
            claim.source.display()
        ));
    }
    claim.output.exists().then(|| {
        format!(
            "{} is the only copy of {}, which was already deleted",
            output.display(),
            claim.source.display()
        )
    })
}

/// Why no run may write `output`, and the error saying so: for a
/// `delete_source` run, that its source is kept.
fn refusal(claims: &BTreeMap<String, Claim>, output: &Path, delete_source: bool) -> Result<()> {
    match claims
        .get(&output_key(output))
        .and_then(|claim| held(claim, output))
    {
        Some(reason) if delete_source => bail!("source kept: {reason}"),
        Some(reason) => bail!("{reason}"),
        None => Ok(()),
    }
}

/// Checks that a run may write `output`, claiming nothing: the read a dry
/// run makes, refused the way [`claim_output`] refuses the real run.
///
/// # Errors
/// Errors when another run is writing the output, or it is the only copy
/// of a source a `delete_source` run deleted.
pub(crate) fn check_output(output: &Path, delete_source: bool) -> Result<()> {
    let claims = CLAIMS.lock().unwrap_or_else(PoisonError::into_inner);
    if claims.is_empty() {
        return Ok(());
    }
    refusal(&claims, output, delete_source)
}

/// Checks that a run may write into the folder `dir`, which it claims no
/// path in: neither the folder nor any output in it may be held.
///
/// # Errors
/// Errors on the first held output, see [`check_output`].
pub(crate) fn check_output_dir(dir: &Path) -> Result<()> {
    let claims = CLAIMS.lock().unwrap_or_else(PoisonError::into_inner);
    let key = output_key(dir);
    let inside = format!("{key}{}", std::path::MAIN_SEPARATOR);
    let reason = claims
        .range(key.clone()..)
        .take_while(|(claimed, _)| claimed.starts_with(&key))
        .filter(|(claimed, _)| **claimed == key || claimed.starts_with(&inside))
        .find_map(|(_, claim)| held(claim, &claim.output));
    match reason {
        Some(reason) => bail!("{reason}"),
        None => Ok(()),
    }
}

/// Claims `output` for `source` once a run resolved it, until the write
/// landed, or for good once [`OutputClaim::commit`] marks the source of a
/// `delete_source` run deleted. A pending claim refuses every other one, the
/// same source's too: a second job for one file would write over the
/// first. So does a committed claim while its output exists: its source is
/// gone, so a file now at that path is a new one. A retry after a failure
/// finds its own claim already dropped.
///
/// # Errors
/// Errors when another run holds the output, see [`check_output`].
pub(crate) fn claim_output(
    source: &Path,
    output: &Path,
    delete_source: bool,
) -> Result<OutputClaim> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let key = output_key(output);
    let source = entry_location(source);
    let token = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut claims = CLAIMS.lock().unwrap_or_else(PoisonError::into_inner);
    refusal(&claims, output, delete_source)?;
    claims.insert(
        key.clone(),
        Claim {
            token,
            source,
            output: output.to_path_buf(),
            committed: false,
        },
    );
    Ok(OutputClaim { key, token })
}

/// A pending claim a run holds on its output while it writes. Dropped
/// uncommitted, once the write landed or failed, or by a `delete_source`
/// run that kept its source, it frees the output again. Both act only on
/// the entry this claim made.
#[derive(Debug)]
pub(crate) struct OutputClaim {
    key: String,
    token: u64,
}

impl OutputClaim {
    /// Keeps the claim for the rest of the process: the source is gone.
    pub(crate) fn commit(self) {
        if let Some(claim) = CLAIMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&self.key)
            .filter(|claim| claim.token == self.token)
        {
            claim.committed = true;
        }
    }
}

impl Drop for OutputClaim {
    fn drop(&mut self) {
        let mut claims = CLAIMS.lock().unwrap_or_else(PoisonError::into_inner);
        if claims
            .get(&self.key)
            .is_some_and(|claim| claim.token == self.token && !claim.committed)
        {
            claims.remove(&self.key);
        }
    }
}

/// True for a split WUD continuation that is gone: the run of its first
/// part deleted it with the set.
pub(crate) fn is_released_part(input: &Path) -> bool {
    has_ext(input, "wud")
        && split_part_number(input).is_some_and(|number| number >= 2)
        && !input.exists()
}

/// Removed paths, failures, and notes on kept sources, the way organize's
/// [`remove_sources`] reports them.
type Removal = (Vec<PathBuf>, Vec<(PathBuf, std::io::Error)>, Vec<String>);

/// What a `delete_source` run may delete and already deleted. A recursive
/// run shares one scope across its children, so a bin two sheets share goes
/// with the last of them and emptied folders are pruned once, after the
/// last child.
pub(crate) struct ReleaseScope {
    /// The canonical walk root of a recursive run, `None` for a single run,
    /// where each input guards its own folder.
    walk_root: Option<PathBuf>,
    /// Pruning never reaches this folder: the walk root, the folder the GUI
    /// queued a single run from, or else a single input's grandparent, so
    /// only the input's own folder can go.
    stop: PathBuf,
    state: tokio::sync::Mutex<ReleaseState>,
}

#[derive(Default)]
struct ReleaseState {
    /// The sheets and split sets each folder holds, listed once per scope.
    listings: HashMap<PathBuf, Listing>,
    /// Sheets a recursive run's walk collected, parsed into `walked` on
    /// first use.
    unparsed: Vec<PathBuf>,
    /// The walked sheets. One anywhere in the walk can reach a file
    /// through a linked folder, which no folder above the file shows.
    walked: Vec<Holder>,
    /// Own locations of every file deleted, or in a dry run planned for
    /// deletion.
    released: HashSet<PathBuf>,
    /// Canonical folders files were deleted from.
    touched: BTreeSet<PathBuf>,
    /// Own locations of the outputs released so far: no later child's
    /// source, and the folders holding them stay.
    outputs: HashSet<PathBuf>,
}

impl ReleaseScope {
    /// The scope of a recursive run over `root` that walked `inputs`.
    ///
    /// # Errors
    /// Errors when `root` cannot be resolved.
    pub(crate) fn batch<'a>(
        root: &Path,
        inputs: impl Iterator<Item = &'a Path>,
    ) -> std::io::Result<Self> {
        let canonical = std::fs::canonicalize(root)?;
        let state = ReleaseState {
            unparsed: inputs
                .filter(|input| has_ext(input, "cue") || has_ext(input, "gdi"))
                .map(Path::to_path_buf)
                .collect(),
            ..ReleaseState::default()
        };
        Ok(Self {
            state: tokio::sync::Mutex::new(state),
            walk_root: Some(canonical.clone()),
            stop: canonical,
        })
    }

    /// The scope of a single run of `input`. A `source_root` holding the
    /// input stops pruning in its place.
    fn single(input: &Path, source_root: Option<&Path>) -> Self {
        let folder = folder_of(input);
        let stop = source_root
            .and_then(|root| std::fs::canonicalize(root).ok())
            .filter(|root| folder.starts_with(root))
            .unwrap_or_else(|| folder.parent().unwrap_or(&folder).to_path_buf());
        Self {
            state: tokio::sync::Mutex::default(),
            walk_root: None,
            stop,
        }
    }

    /// True when a walked input went with a set released earlier in the
    /// run: a split part, or a track its sheet covered.
    pub(crate) async fn is_released(&self, path: &Path) -> bool {
        self.state
            .lock()
            .await
            .released
            .contains(&entry_location(path))
    }

    /// Prunes the folders the run emptied and warns about each one kept
    /// for another file. A dry run emptied none.
    ///
    /// # Errors
    /// Errors when the blocking prune task fails to run.
    pub(crate) async fn finish(&self, progress: &dyn ProgressReporter) -> Result<()> {
        let (touched, outputs) = {
            let mut state = self.state.lock().await;
            (
                std::mem::take(&mut state.touched),
                std::mem::take(&mut state.outputs),
            )
        };
        if touched.is_empty() {
            return Ok(());
        }
        let stop = self.stop.clone();
        let notes = tokio::task::spawn_blocking(move || {
            let mut keep: Vec<PathBuf> = outputs.into_iter().collect();
            keep.extend(std::env::current_dir().and_then(std::fs::canonicalize).ok());
            if let Some(home) = dirs::home_dir().and_then(|home| std::fs::canonicalize(home).ok()) {
                keep.extend(home_folders(&touched, &home));
            }
            prune_emptied(&touched, &stop, &keep)
        })
        .await?;
        for note in &notes {
            progress.warn(note);
        }
        Ok(())
    }
}

/// A folder's sheets and split sets, or why it could not be listed.
type Listing = std::result::Result<Vec<Holder>, (std::io::ErrorKind, String)>;

/// A sheet or split set that reads files besides its own.
struct Holder {
    /// The own location of its sheet or first part.
    location: PathBuf,
    identity: Option<FileIdentity>,
    files: HashSet<FileIdentity>,
}

impl Holder {
    /// `unit` as a reader of other files: a sheet, or a split set's first
    /// part.
    fn of(unit: &DatUnit) -> Option<Self> {
        let files = unit_source_files(unit);
        if matches!(unit, DatUnit::File(_)) && files.len() < 2 {
            return None;
        }
        let path = unit.display_path();
        Some(Self {
            location: entry_location(path),
            identity: FileIdentity::probe(path).ok(),
            files: files
                .iter()
                .filter_map(|file| FileIdentity::probe(file).ok())
                .collect(),
        })
    }

    /// Its identity when it still reads `file` and is not `releasing`.
    fn reader(
        &self,
        file: &FileIdentity,
        releasing: Option<&FileIdentity>,
        released: &HashSet<PathBuf>,
    ) -> Option<&FileIdentity> {
        self.identity.as_ref().filter(|identity| {
            Some(*identity) != releasing
                && !released.contains(&self.location)
                && self.files.contains(file)
        })
    }
}

/// The sheets and split sets directly in `folder`.
async fn holders(folder: &Path, cancel: &CancelToken) -> std::io::Result<Vec<Holder>> {
    let units = collect_units(folder, Some(1), cancel).await?;
    Ok(units.iter().filter_map(Holder::of).collect())
}

/// The files `sheet` names, the way the unit walk groups them; `None` for
/// a sheet that cannot be read, which the walk skips too.
async fn sheet_files(sheet: &Path) -> Option<Vec<PathBuf>> {
    if has_ext(sheet, "gdi") {
        return crate::sega::katana::parse_gdi_index(sheet)
            .ok()
            .map(|index| index.track_paths(sheet));
    }
    let parsed = CueParser::new(sheet).parse().await.ok()?;
    let folder = sheet.parent().unwrap_or_else(|| Path::new("."));
    Some(
        parsed
            .files
            .iter()
            .map(|file| folder.join(&file.filename))
            .collect(),
    )
}

/// How many units read each of `files` now: the releasing `unit`, plus
/// every sheet or split set not yet gone that reads it, matched by
/// identity. A sheet names files in folders below its own, so the readers
/// sit in the folders above where the file really is or where the run
/// spelled it, or, through a linked folder, anywhere: a recursive run also
/// counts every sheet its walk collected. Also returns the files that stay
/// whatever their count, each with its note: a track the sheet reaches
/// through a linked folder, which a sheet beside the link reads unseen,
/// and a file under a folder that cannot be listed.
async fn live_claims(
    unit: &DatUnit,
    files: &[PathBuf],
    root: &Path,
    state: &mut ReleaseState,
    cancel: &CancelToken,
) -> (HashMap<FileIdentity, usize>, Vec<(PathBuf, String)>) {
    let ReleaseState {
        listings,
        unparsed,
        walked,
        released,
        ..
    } = state;
    for sheet in std::mem::take(unparsed) {
        if let Some(bins) = sheet_files(&sheet).await {
            walked.extend(Holder::of(&DatUnit::CueSet { cue: sheet, bins }));
        }
    }
    let releasing = FileIdentity::probe(unit.display_path()).ok();
    let releasing = releasing.as_ref();
    let linked = linked_tracks(unit);
    let mut claims = HashMap::new();
    let mut held = Vec::new();
    'files: for file in files {
        if linked.contains(file) {
            let note = format!(
                "kept {}: it is reached through a linked folder",
                file.display()
            );
            held.push((file.clone(), note));
            continue;
        }
        let Ok(identity) = FileIdentity::probe(file) else {
            continue;
        };
        let mut folders: Vec<PathBuf> = entry_location(file)
            .ancestors()
            .skip(1)
            .map(Path::to_path_buf)
            .collect();
        if let Ok(spelled) = std::path::absolute(file) {
            for folder in lexical(&spelled).ancestors().skip(1) {
                if let Ok(real) = std::fs::canonicalize(folder)
                    && !folders.contains(&real)
                {
                    folders.push(real);
                }
            }
        }
        let mut readers: HashSet<&FileIdentity> = walked
            .iter()
            .filter_map(|holder| holder.reader(&identity, releasing, released))
            .collect();
        for folder in &folders {
            if !listings.contains_key(folder) {
                let listing = holders(folder, cancel)
                    .await
                    .map_err(|err| (err.kind(), err.to_string()));
                listings.insert(folder.clone(), listing);
            }
        }
        for folder in &folders {
            match &listings[folder] {
                Ok(holders) => readers.extend(
                    holders
                        .iter()
                        .filter_map(|holder| holder.reader(&identity, releasing, released)),
                ),
                // A folder above the input may let the user pass but not
                // list it, as a shared home parent does.
                Err((std::io::ErrorKind::PermissionDenied, err)) if !folder.starts_with(root) => {
                    log::debug!("Counting no sheets in {}: {err}", folder.display());
                }
                Err((_, err)) => {
                    let note = format!(
                        "kept {}: could not list {}: {err}",
                        file.display(),
                        folder.display()
                    );
                    held.push((file.clone(), note));
                    continue 'files;
                }
            }
        }
        claims.insert(identity, 1 + readers.len());
    }
    (claims, held)
}

/// The tracks a sheet names through a linked folder: the folder each
/// really sits in is not the one its name leads to.
fn linked_tracks(unit: &DatUnit) -> Vec<PathBuf> {
    let DatUnit::CueSet { cue, bins } = unit else {
        return Vec::new();
    };
    let folder = cue.parent().unwrap_or_else(|| Path::new("."));
    let real = folder_of(cue);
    bins.iter()
        .filter(|bin| {
            let named = real.join(bin.strip_prefix(folder).unwrap_or(bin));
            lexical(&named) != entry_location(bin)
        })
        .cloned()
        .collect()
}

/// `path` with `.` and `..` folded by name, as if no folder were a link.
fn lexical(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }
    folded
}

/// Deletes `unit` through organize's guards, its claims counted now. A file
/// [`live_claims`] holds stays: a held track alone, any other held file
/// with the whole set.
async fn release_unit(
    unit: DatUnit,
    own: &[PathBuf],
    outputs: &[PathBuf],
    root: &Path,
    state: &mut ReleaseState,
    dry_run: bool,
    cancel: &CancelToken,
) -> Removal {
    let (mut claims, held) = live_claims(&unit, own, root, state, cancel).await;
    let (held, notes): (Vec<PathBuf>, Vec<String>) = held.into_iter().unzip();
    let unit = match unit {
        DatUnit::CueSet { cue, bins } if !held.contains(&cue) => DatUnit::CueSet {
            cue,
            bins: bins.into_iter().filter(|bin| !held.contains(bin)).collect(),
        },
        unit if held.is_empty() => unit,
        _ => return (Vec::new(), Vec::new(), notes),
    };
    let mut removal = remove_sources(&unit, own, outputs, root, &mut claims, dry_run).await;
    removal.2.extend(notes);
    removal
}

/// Deletes the sources of the conversion `response` reports once its one
/// record wrote an output. With `verify_after` the output must have passed
/// its check, else the record fails with its source kept; a dry run keeps
/// the source the same way when the format has no check. A dry run reports
/// what it would delete and deletes nothing. A failed removal fails the
/// record; a source the guards keep is warned about. A single run prunes
/// the folders it emptied here, a recursive run after its last child.
///
/// # Errors
/// Errors when cancelled or a blocking task fails.
pub(crate) async fn release_after(
    req: &RunRequest,
    operation: &str,
    input_exts: &'static [&'static str],
    response: &mut RunResponse,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let [record] = response.records.as_slice() else {
        return Ok(());
    };
    if cancel.is_cancelled() || record.status != FileStatus::Ok || record.output_path.is_empty() {
        return Ok(());
    }
    let input = record.input_path.clone();
    let outputs = [PathBuf::from(&record.output_path)];
    if req.options.verify_after == Some(true) {
        let reason = if req.dry_run {
            (!CHECKED_OPS.contains(&operation)).then(|| NO_CHECK.to_string())
        } else {
            let report = match &response.data {
                Some(RunData::Comparison(data)) => data.comparison.verify.as_ref(),
                _ => None,
            };
            match report {
                Some(report) if report.ok => None,
                Some(report) => Some(report.message.clone()),
                None => Some(NO_CHECK.to_string()),
            }
        };
        if let Some(reason) = reason {
            progress.warn(&format!("kept source {input}: {reason}"));
            fail(response, format!("source kept: {reason}"));
            return Ok(());
        }
    }
    let scope = match &req.ctx.release {
        Some(scope) => scope.clone(),
        None => {
            let Some(primary) = req.input.clone().or_else(|| first_wup_input(req)) else {
                return Ok(());
            };
            Arc::new(ReleaseScope::single(
                &primary,
                req.options.source_root.as_deref(),
            ))
        }
    };
    let (removed, failures, kept) =
        release_sources(req, operation, input_exts, &outputs, &scope, cancel).await?;
    // Once any source file is gone the output is the only copy of it, even
    // when the rest of the set stayed or failed to go. A failed removal
    // whose source no longer sits at its path went past the rename aside:
    // a folder only partly deleted, or a file its restore could not put back.
    let destructive = !removed.is_empty()
        || failures
            .iter()
            .any(|(path, _)| std::fs::symlink_metadata(path).is_err());
    if !req.dry_run
        && destructive
        && let Some(claim) = req
            .ctx
            .output_claim
            .as_ref()
            .and_then(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).take())
    {
        claim.commit();
    }
    for path in &removed {
        if req.dry_run {
            log::info!("Would delete {}", path.display());
        } else {
            log::info!("Deleted source: {}", path.display());
        }
    }
    for note in &kept {
        progress.warn(note);
    }
    if !failures.is_empty() {
        for (path, err) in &failures {
            progress.warn(&format!(
                "could not delete source {}: {err}",
                path.display()
            ));
        }
        let detail = failures
            .iter()
            .map(|(path, err)| format!("source not removed: {}: {err}", path.display()))
            .collect::<Vec<_>>()
            .join("; ");
        fail(response, detail);
    }
    if req.ctx.release.is_none() {
        scope.finish(progress).await?;
    }
    Ok(())
}

/// Fails the one record: its output stays, but its sources did not go as
/// asked.
fn fail(response: &mut RunResponse, detail: String) {
    if let Some(record) = response.records.first_mut() {
        record.status = FileStatus::Failed;
        record.error = Some(detail.clone());
        response.totals = Some(totals_for(record));
    }
    response.ok = false;
    response.status = RunStatus::Failed.as_i32();
    response.code = RunStatus::Failed.code().to_string();
    response.message = detail;
}

/// How a source is deleted.
enum Source {
    /// A folder input, deleted whole.
    Folder,
    /// A file unit and every file it occupies.
    Files(DatUnit, Vec<PathBuf>),
    /// A source that stays, and why.
    Kept(String),
}

/// Deletes every source `req` converted (`options.inputs` of a Wii U title
/// set, else its input), then its per-game key files, but those only once
/// the images went with no failure and nothing kept. Every source is
/// sorted, and its keys found, before any goes: an archive names its key
/// after its member. Records what went in `scope`.
async fn release_sources(
    req: &RunRequest,
    operation: &str,
    input_exts: &'static [&'static str],
    outputs: &[PathBuf],
    scope: &ReleaseScope,
    cancel: &CancelToken,
) -> Result<Removal> {
    let sources: Vec<PathBuf> = match req
        .options
        .inputs
        .as_ref()
        .filter(|_| operation == "wup.compress")
    {
        Some(inputs) => inputs
            .iter()
            .map(|input| match input {
                WupTitleInputOption::Path(path) | WupTitleInputOption::Object { path, .. } => {
                    path.clone()
                }
            })
            .collect(),
        None => req.input.iter().cloned().collect(),
    };
    let mut sorted = Vec::with_capacity(sources.len());
    for source in &sources {
        sorted.push((source, classify(source, input_exts, cancel).await?));
    }
    let files: Vec<&Path> = sorted
        .iter()
        .filter(|(_, how)| matches!(how, Source::Files(..)))
        .map(|(source, _)| source.as_path())
        .collect();
    let (keys, key_notes) = key_files(req, operation, &files, input_exts, cancel).await?;
    let mut state = scope.state.lock().await;
    let state = &mut *state;
    let mut removal = Removal::default();
    let mut touched = Vec::new();
    for (source, how) in sorted {
        let (removed, failures, kept) = match how {
            Source::Kept(note) => (Vec::new(), Vec::new(), vec![note]),
            Source::Folder => release_folder(source, outputs, req.dry_run).await,
            Source::Files(unit, own) => {
                // A sheet naming an earlier child's output must not take it
                // after that child's own sources already went.
                if let Some(output) = own
                    .iter()
                    .find(|path| state.outputs.contains(&entry_location(path)))
                {
                    let note = format!(
                        "kept {}: {} is an output of this run",
                        source.display(),
                        output.display()
                    );
                    (Vec::new(), Vec::new(), vec![note])
                } else {
                    let root = scope.walk_root.clone().unwrap_or_else(|| folder_of(source));
                    let released =
                        release_unit(unit, &own, outputs, &root, state, req.dry_run, cancel).await;
                    touched.extend(released.0.iter().map(|path| folder_of(path)));
                    released
                }
            }
        };
        removal.0.extend(removed);
        removal.1.extend(failures);
        removal.2.extend(kept);
    }
    if removal.1.is_empty() && removal.2.is_empty() {
        removal.2.extend(key_notes);
        for key in keys {
            let root = scope.walk_root.clone().unwrap_or_else(|| folder_of(&key));
            let (removed, failures, kept) = release_unit(
                DatUnit::File(key.clone()),
                std::slice::from_ref(&key),
                outputs,
                &root,
                state,
                req.dry_run,
                cancel,
            )
            .await;
            touched.extend(removed.iter().map(|path| folder_of(path)));
            removal.0.extend(removed);
            removal.1.extend(failures);
            removal.2.extend(kept);
        }
    }
    state
        .released
        .extend(removal.0.iter().map(|path| entry_location(path)));
    state
        .outputs
        .extend(outputs.iter().map(|path| entry_location(path)));
    if !req.dry_run && removal.1.is_empty() {
        state.touched.extend(touched);
    }
    Ok(removal)
}

/// Sorts `source` into how it is deleted: a folder whole, an archive only
/// when the conversion used every entry (a split rar with its volumes), a
/// sheet with its tracks, a split image with its parts, anything else as
/// itself.
async fn classify(
    source: &Path,
    input_exts: &'static [&'static str],
    cancel: &CancelToken,
) -> Result<Source> {
    if std::fs::symlink_metadata(source).is_ok_and(|meta| meta.is_dir()) {
        return Ok(Source::Folder);
    }
    // Deleting through a link to a folder would empty a tree the input
    // never named.
    if source.is_dir() {
        return Ok(Source::Kept(format!(
            "kept {}: it is a link to a folder",
            source.display()
        )));
    }
    if is_archive_path(source) {
        // A later volume is read through the first. In the old `.rNN`
        // naming the archive header, not the name, tells which files the
        // reader opens, so that set is not known whole.
        if crate::util::archive::is_rar_continuation(source) {
            return Ok(Source::Kept(format!(
                "kept {}: it is a later volume of a split set",
                source.display()
            )));
        }
        if crate::util::archive::has_old_rar_volumes(source) {
            return Ok(Source::Kept(format!(
                "kept {}: its split volumes use the old .rNN naming",
                source.display()
            )));
        }
        let path = source.to_path_buf();
        let cancel = cancel.clone();
        let used = tokio::task::spawn_blocking(move || {
            crate::util::archive::uses_every_entry(&path, input_exts, &cancel)
        })
        .await?;
        return Ok(match used {
            Ok(true) => Source::Files(
                DatUnit::File(source.to_path_buf()),
                crate::util::archive::rar_volumes(source),
            ),
            Ok(false) => Source::Kept(format!(
                "kept {}: it holds entries this conversion did not use",
                source.display()
            )),
            Err(err) if Cancelled::in_chain(&err) => return Err(err),
            Err(err) => Source::Kept(format!(
                "kept {}: could not inspect it: {err}",
                source.display()
            )),
        });
    }
    if has_ext(source, "cue") || has_ext(source, "gdi") {
        return Ok(match whole_tracks(source).await {
            Ok(bins) => {
                let unit = DatUnit::CueSet {
                    cue: source.to_path_buf(),
                    bins,
                };
                let own = unit_source_files(&unit);
                Source::Files(unit, own)
            }
            Err(note) => Source::Kept(note),
        });
    }
    if has_ext(source, "wbfs") {
        let mut parts = vec![source.to_path_buf()];
        parts.extend(
            (1..crate::nintendo::disc::wbfs::reader::MAX_SPLIT_PARTS).map_while(|idx| {
                crate::nintendo::disc::wbfs::reader::sibling_path(source, idx)
                    .filter(|part| part.is_file())
            }),
        );
        return Ok(Source::Files(DatUnit::File(source.to_path_buf()), parts));
    }
    let unit = DatUnit::File(source.to_path_buf());
    let own = unit_source_files(&unit);
    Ok(Source::Files(unit, own))
}

/// The track files of `sheet` when its conversion read each one whole,
/// else the note that keeps the whole set. A FILE no TRACK lands in is
/// never read, and a size that is no whole number of its tracks' sectors
/// is cut short, so a crafted sheet cannot name a file for deletion that
/// the output does not hold.
async fn whole_tracks(sheet: &Path) -> std::result::Result<Vec<PathBuf>, String> {
    let unreadable = |err: &dyn std::fmt::Display| {
        format!("kept {}: could not read its tracks: {err}", sheet.display())
    };
    let folder = sheet.parent().unwrap_or_else(|| Path::new("."));
    let files: Vec<(PathBuf, Vec<u64>)> = if has_ext(sheet, "gdi") {
        crate::sega::katana::parse_gdi_index(sheet)
            .map_err(|err| unreadable(&err))?
            .tracks
            .iter()
            .map(|track| {
                (
                    folder.join(&track.filename),
                    vec![u64::from(track.sector_size)],
                )
            })
            .collect()
    } else {
        let parsed = CueParser::new(sheet)
            .parse()
            .await
            .map_err(|err| unreadable(&err))?;
        parsed
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let widths = parsed
                    .tracks
                    .iter()
                    .filter(|track| track.file_index == index)
                    .map(|track| u64::from(track.track_type.block_size()))
                    .collect();
                (folder.join(&file.filename), widths)
            })
            .collect()
    };
    for (path, widths) in &files {
        let len = std::fs::metadata(path).map(|meta| meta.len()).ok();
        if widths.is_empty()
            || !len.is_some_and(|len| widths.iter().all(|&width| width > 0 && len % width == 0))
        {
            return Err(format!(
                "kept {}: {} is not read as whole tracks",
                sheet.display(),
                path.display()
            ));
        }
    }
    Ok(files.into_iter().map(|(path, _)| path).collect())
}

/// Deletes a folder input whole, never one holding an output of this run,
/// the current folder, or the home folder, and never a filesystem root.
async fn release_folder(dir: &Path, outputs: &[PathBuf], dry_run: bool) -> Removal {
    let mut removal = Removal::default();
    let resolved = real_layout(dir);
    let holds = |inner: Option<PathBuf>| inner.is_some_and(|inner| inner.starts_with(&resolved));
    let guard = if resolved.parent().is_none() {
        Some("it is a filesystem root")
    } else if holds(dirs::home_dir().and_then(|home| std::fs::canonicalize(home).ok())) {
        Some("it is or holds the home folder")
    } else if holds(std::env::current_dir().and_then(std::fs::canonicalize).ok()) {
        Some("it is or holds the current folder")
    } else {
        None
    };
    if let Some(reason) = guard {
        removal
            .2
            .push(format!("kept folder {}: {reason}", dir.display()));
        return removal;
    }
    if outputs
        .iter()
        .any(|output| real_layout(output).starts_with(&resolved))
    {
        removal.2.push(format!(
            "kept {}: it holds an output of this run",
            dir.display()
        ));
        return removal;
    }
    if dry_run {
        removal.0.push(dir.to_path_buf());
        return removal;
    }
    match remove_dir_source(dir, outputs).await {
        Ok(true) => removal.0.push(dir.to_path_buf()),
        Ok(false) => removal.2.push(format!(
            "kept {}: it holds an output of this run",
            dir.display()
        )),
        Err(err) => removal.1.push((dir.to_path_buf(), err)),
    }
    removal
}

/// Deletes the folder `dir` the way organize deletes a file: renamed aside
/// first, so an output reached through it under another spelling changes
/// identity and keeps it (`Ok(false)`, restored). A folder that fails to
/// delete after the rename keeps its temporary name, which the error names:
/// a half-deleted folder must not look intact.
async fn remove_dir_source(dir: &Path, outputs: &[PathBuf]) -> std::io::Result<bool> {
    let identities = outputs
        .iter()
        .map(|output| FileIdentity::probe(output))
        .collect::<std::io::Result<Vec<_>>>()?;
    let synced = outputs.to_vec();
    tokio::task::spawn_blocking(move || sync_outputs(&synced))
        .await
        .map_err(std::io::Error::other)??;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = dir.parent().unwrap_or_else(|| Path::new("."));
    let temporary = loop {
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = parent.join(format!(".rom-converto-delete-{}-{id}", std::process::id()));
        match std::fs::symlink_metadata(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break path,
            Err(err) => return Err(err),
            Ok(_) => {}
        }
    };
    tokio::fs::rename(dir, &temporary).await?;
    if outputs
        .iter()
        .zip(&identities)
        .any(|(output, identity)| FileIdentity::probe(output).as_ref().ok() != Some(identity))
    {
        restore_source(dir, &temporary)?;
        return Ok(false);
    }
    let removing = temporary.clone();
    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&removing))
        .await
        .map_err(std::io::Error::other)?
        .map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("{} was only partly deleted: {err}", temporary.display()),
            )
        })?;
    Ok(true)
}

/// The per-game key files the conversion read beside its images: a PS3
/// disc's `<iso>.dkey` (named after the member for an archive), and for a
/// Wii U disc compressed into a WUA, whose content is decrypted, the key
/// its own lookup reads: `<disc>.key`, else the `game.key` beside a
/// `game.wud` or `game_part1.wud`, unless another disc in the folder still
/// falls back to it. A re-encode to WUX or WUD keeps every key, since its
/// output still needs one, and with a key passed by option the conversion
/// read that one, so none goes. Returns the keys and notes on kept ones.
async fn key_files(
    req: &RunRequest,
    operation: &str,
    files: &[&Path],
    input_exts: &'static [&'static str],
    cancel: &CancelToken,
) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let mut keys = Vec::new();
    let mut notes = Vec::new();
    let option_key = req.options.key.is_some()
        || req.options.inputs.iter().flatten().any(|input| {
            matches!(input, WupTitleInputOption::Object { key, key_path, .. }
                if key.is_some() || key_path.is_some())
        });
    if option_key {
        return Ok((keys, notes));
    }
    match operation {
        "ps3.decrypt" => {
            for source in files {
                let basis = if is_archive_path(source) {
                    let path = source.to_path_buf();
                    let cancel = cancel.clone();
                    let probed = tokio::task::spawn_blocking(move || {
                        crate::util::probe_archive(&path, input_exts, &cancel)
                    })
                    .await?;
                    let pick = match probed {
                        Err(err) if Cancelled::in_chain(&err) => return Err(err),
                        probed => probed.ok().flatten(),
                    };
                    match pick.map(|pick| pick.output_basis(source)) {
                        Some(Ok(basis)) => basis,
                        _ => continue,
                    }
                } else {
                    source.to_path_buf()
                };
                keys.push(basis.with_extension("dkey"));
            }
        }
        "wup.compress" => {
            let sources: HashSet<PathBuf> = files.iter().map(|path| entry_location(path)).collect();
            for disc in files.iter().filter(|disc| !is_archive_path(disc)) {
                let own = disc.with_extension("key");
                let key = if own.is_file() {
                    own
                } else if disc.file_name().is_some_and(|name| {
                    name.eq_ignore_ascii_case("game.wud")
                        || name.eq_ignore_ascii_case("game_part1.wud")
                }) {
                    disc.with_file_name("game.key")
                } else {
                    continue;
                };
                if key
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("game.key"))
                    && let Some(other) = disc_using_game_key(disc, &sources)
                {
                    notes.push(format!(
                        "kept {}: {} still falls back to it",
                        key.display(),
                        other.display()
                    ));
                    continue;
                }
                keys.push(key);
            }
        }
        _ => {}
    }
    let mut seen = HashSet::new();
    keys.retain(|key| key.is_file() && seen.insert(entry_location(key)));
    Ok((keys, notes))
}

/// A first-part disc image beside `disc`, other than this run's `sources`,
/// with no `<stem>.key` of its own: it still reads `game.key`.
fn disc_using_game_key(disc: &Path, sources: &HashSet<PathBuf>) -> Option<PathBuf> {
    let folder = disc
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            (has_ext(path, "wud") || has_ext(path, "wux"))
                && matches!(split_part_number(path), None | Some(1))
                && path.is_file()
                && !sources.contains(&entry_location(path))
                && !path.with_extension("key").is_file()
        })
}

/// The canonical folder `path` sits in.
fn folder_of(path: &Path) -> PathBuf {
    let location = entry_location(path);
    location
        .parent()
        .map_or_else(|| location.clone(), Path::to_path_buf)
}

/// The folders directly in `home` that hold a touched folder. A single run
/// stops pruning only above the input's own folder, so without this an
/// emptied Downloads or Desktop would go with the last file in it.
fn home_folders(touched: &BTreeSet<PathBuf>, home: &Path) -> Vec<PathBuf> {
    touched
        .iter()
        .filter_map(|dir| dir.ancestors().find(|folder| folder.parent() == Some(home)))
        .map(Path::to_path_buf)
        .collect()
}

/// Removes each touched folder, then its parents, while it holds nothing
/// but OS junk files, which go with it. Never reaches `stop`, a folder
/// holding (or being) one of `keep`, or a folder holding a subfolder.
/// Returns a note per folder kept for another entry, naming the first; a
/// folder kept for `keep` stays quietly. All paths are canonical.
fn prune_emptied(touched: &BTreeSet<PathBuf>, stop: &Path, keep: &[PathBuf]) -> Vec<String> {
    let candidates: BTreeSet<&Path> = touched
        .iter()
        .flat_map(|dir| {
            dir.ancestors()
                .take_while(|folder| *folder != stop && folder.starts_with(stop))
        })
        .collect();
    let mut candidates: Vec<&Path> = candidates.into_iter().collect();
    // Deepest first: a folder is judged after every folder inside it.
    candidates.sort_by_key(|folder| std::cmp::Reverse(folder.components().count()));
    let mut kept: Vec<&Path> = Vec::new();
    let mut notes = Vec::new();
    for folder in candidates {
        if kept
            .iter()
            .copied()
            .chain(keep.iter().map(PathBuf::as_path))
            .any(|inner| inner.starts_with(folder))
        {
            kept.push(folder);
            continue;
        }
        match prune_folder(folder) {
            Ok(None) => {}
            // Another run pruning the same folder got there first.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Ok(Some(leftover)) => {
                notes.push(format!(
                    "kept folder {}: it still holds {}",
                    folder.display(),
                    leftover.to_string_lossy()
                ));
                kept.push(folder);
            }
            Err(err) => {
                notes.push(format!(
                    "could not remove folder {}: {err}",
                    folder.display()
                ));
                kept.push(folder);
            }
        }
    }
    notes
}

/// Deletes `dir` with its OS junk files when that is all it holds; else
/// returns the name of its first other entry.
fn prune_folder(dir: &Path) -> std::io::Result<Option<OsString>> {
    let mut junk = Vec::new();
    let mut leftover: Option<OsString> = None;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_file() && name.to_str().is_some_and(is_os_junk_file) {
            junk.push(entry.path());
        } else if leftover.as_ref().is_none_or(|first| name < *first) {
            leftover = Some(name);
        }
    }
    if leftover.is_some() {
        return Ok(leftover);
    }
    for file in junk {
        std::fs::remove_file(file)?;
    }
    std::fs::remove_dir(dir)?;
    log::info!("Deleted folder: {}", dir.display());
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RecordingProgress;
    use crate::runner::models::{ProgressEvent, RunContext, RunOptions};

    const EXTS: &[&str] = &["cue", "gdi", "iso", "wbfs", "wud"];

    fn request(operation: &str, input: &Path, dry_run: bool) -> RunRequest {
        RunRequest {
            schema: None,
            operation: operation.to_string(),
            input: Some(input.to_path_buf()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions::default(),
            ctx: RunContext::default(),
            dry_run,
        }
    }

    fn write(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
        path.to_path_buf()
    }

    /// A CD cue set: a 300-sector MODE2/2352 bin and the sheet naming it.
    fn cue_set(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
        std::fs::create_dir_all(dir).unwrap();
        let bin = dir.join(format!("{name} (Track 1).bin"));
        std::fs::write(&bin, vec![0u8; 2352 * 300]).unwrap();
        let cue = dir.join(format!("{name}.cue"));
        std::fs::write(
            &cue,
            format!(
                "FILE \"{name} (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n"
            ),
        )
        .unwrap();
        (cue, bin)
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, data).unwrap();
        }
        zip.finish().unwrap();
    }

    /// `path` spelled relative to the current directory.
    fn relative_to_cwd(path: &Path) -> PathBuf {
        let normal = |path: &Path| -> Vec<OsString> {
            path.components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(name) => Some(name.to_os_string()),
                    _ => None,
                })
                .collect()
        };
        let mut relative: PathBuf = normal(&std::env::current_dir().unwrap())
            .iter()
            .map(|_| "..")
            .collect();
        relative.extend(normal(path));
        relative
    }

    fn single(input: &Path) -> ReleaseScope {
        ReleaseScope::single(input, None)
    }

    fn batch(root: &Path, walked: &[&Path]) -> ReleaseScope {
        ReleaseScope::batch(root, walked.iter().copied()).unwrap()
    }

    fn refused(source: &Path, output: &Path) -> String {
        claim_output(source, output, true)
            .err()
            .unwrap()
            .to_string()
    }

    async fn release(req: &RunRequest, scope: &ReleaseScope, output: &Path) -> Removal {
        release_sources(
            req,
            &req.operation,
            EXTS,
            &[output.to_path_buf()],
            scope,
            &CancelToken::new(),
        )
        .await
        .unwrap()
    }

    async fn own_files(source: &Path) -> Vec<PathBuf> {
        match classify(source, EXTS, &CancelToken::new()).await.unwrap() {
            Source::Files(_, own) => own,
            Source::Folder => panic!("{} is a folder", source.display()),
            Source::Kept(note) => panic!("{note}"),
        }
    }

    async fn kept_note(source: &Path) -> String {
        match classify(source, EXTS, &CancelToken::new()).await.unwrap() {
            Source::Kept(note) => note,
            _ => panic!("{} is not kept", source.display()),
        }
    }

    fn warnings(progress: &RecordingProgress) -> Vec<String> {
        progress
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Warn { message } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn each_source_shape_yields_the_files_it_consists_of() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let (cue, bin) = cue_set(&root.join("cd"), "Game");
        assert_eq!(own_files(&cue).await, [cue.clone(), bin]);

        let tracks = [root.join("dc/track01.bin"), root.join("dc/track02.raw")];
        std::fs::create_dir_all(root.join("dc")).unwrap();
        for track in &tracks {
            std::fs::write(track, vec![0u8; 2352]).unwrap();
        }
        let gdi = root.join("dc/game.gdi");
        std::fs::write(
            &gdi,
            "2\n1 0 4 2352 track01.bin 0\n2 450 0 2352 track02.raw 0\n",
        )
        .unwrap();
        let mut expected = vec![gdi.clone()];
        expected.extend(tracks);
        assert_eq!(own_files(&gdi).await, expected);

        let wbfs = write(&root.join("wii/game.wbfs"));
        let parts = [
            write(&root.join("wii/game.wbf1")),
            write(&root.join("wii/game.wbf2")),
        ];
        // Past a gap the reader stops, so a stray part is no part of the set.
        write(&root.join("wii/game.wbf4"));
        let mut expected = vec![wbfs.clone()];
        expected.extend(parts);
        assert_eq!(own_files(&wbfs).await, expected);

        let first = write(&root.join("wiiu/game_part1.wud"));
        let second = write(&root.join("wiiu/game_part2.wud"));
        assert_eq!(own_files(&first).await, [first.clone(), second]);

        let iso = write(&root.join("ps2/game.iso"));
        assert_eq!(own_files(&iso).await, std::slice::from_ref(&iso));

        let whole = root.join("whole.zip");
        write_zip(
            &whole,
            &[("game.iso", b"iso"), ("__MACOSX/._game.iso", b"junk")],
        );
        assert_eq!(own_files(&whole).await, std::slice::from_ref(&whole));
        let extra = root.join("extra.zip");
        write_zip(&extra, &[("game.iso", b"iso"), ("readme.txt", b"hi")]);
        assert!(kept_note(&extra).await.contains("did not use"));
        let later = write(&root.join("rar/set.part2.rar"));
        assert!(kept_note(&later).await.contains("later volume"));
        let old = write(&root.join("rar/old.rar"));
        write(&root.join("rar/old.r00"));
        assert!(kept_note(&old).await.contains("old .rNN naming"));

        assert!(matches!(
            classify(&root.join("ps2"), EXTS, &CancelToken::new())
                .await
                .unwrap(),
            Source::Folder
        ));
        #[cfg(unix)]
        {
            let link = root.join("link");
            std::os::unix::fs::symlink(root.join("ps2"), &link).unwrap();
            assert!(kept_note(&link).await.contains("link to a folder"));
        }
    }

    #[tokio::test]
    async fn a_shared_bin_stays_until_its_last_sheet_goes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        let (a, bin) = cue_set(&root.join("Pair"), "A");
        let b = root.join("Pair/B.cue");
        std::fs::copy(&a, &b).unwrap();
        let output = write(&dir.path().join("out/A.chd"));
        let scope = batch(&root, &[&a, &b]);

        let (removed, failures, kept) =
            release(&request("chd.compress", &a, false), &scope, &output).await;
        assert_eq!(removed, std::slice::from_ref(&a));
        assert!(failures.is_empty());
        assert!(
            kept.iter()
                .any(|note| note.contains("another scanned unit references it")),
            "{kept:?}"
        );
        assert!(bin.exists());

        let (removed, failures, kept) =
            release(&request("chd.compress", &b, false), &scope, &output).await;
        assert_eq!(removed, [bin, b]);
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );
        scope.finish(&RecordingProgress::default()).await.unwrap();
        assert!(!root.join("Pair").exists());
        assert!(root.is_dir());
    }

    /// The count lists the folders above where a track really sits, so a
    /// sheet beside the link that reaches it would go unseen.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_track_reached_through_a_linked_folder_stays() {
        let dir = tempfile::tempdir().unwrap();
        let output = write(&dir.path().join("out/Game.chd"));
        // The link leads out of the input folder, or to a folder inside it.
        for (game, target) in [("Out", "shared"), ("In", "In/real")] {
            let track = dir.path().join(target).join("Game (Track 1).bin");
            std::fs::create_dir_all(track.parent().unwrap()).unwrap();
            std::fs::write(&track, vec![0u8; 2352]).unwrap();
            let game = dir.path().join(game);
            std::fs::create_dir_all(&game).unwrap();
            std::os::unix::fs::symlink(dir.path().join(target), game.join("tracks")).unwrap();
            let cue = game.join("Game.cue");
            std::fs::write(
                &cue,
                "FILE \"tracks/Game (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
            )
            .unwrap();
            let (removed, failures, kept) = release(
                &request("chd.compress", &cue, false),
                &single(&cue),
                &output,
            )
            .await;
            assert_eq!(removed, [cue]);
            assert!(failures.is_empty());
            assert_eq!(
                kept,
                [format!(
                    "kept {}: it is reached through a linked folder",
                    game.join("tracks/Game (Track 1).bin").display()
                )]
            );
            assert!(track.exists());
        }
    }

    #[tokio::test]
    async fn a_dkey_goes_after_its_image_and_only_for_a_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let output = write(&dir.path().join("out/Game.iso"));

        let iso = write(&dir.path().join("ps3/Game.iso"));
        let dkey = write(&dir.path().join("ps3/Game.dkey"));
        let (removed, failures, kept) =
            release(&request("ps3.decrypt", &iso, false), &single(&iso), &output).await;
        assert_eq!(removed, [iso, dkey]);
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );

        // A sheet still reads the image, so it stays and so does its key.
        let iso = write(&dir.path().join("sheet/Game.iso"));
        let dkey = write(&dir.path().join("sheet/Game.dkey"));
        std::fs::write(
            dir.path().join("sheet/Game.cue"),
            "FILE \"Game.iso\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let (removed, _, kept) =
            release(&request("ps3.decrypt", &iso, false), &single(&iso), &output).await;
        assert!(removed.is_empty());
        assert!(
            kept[0].contains("another scanned unit references it"),
            "{kept:?}"
        );
        assert!(iso.exists() && dkey.exists());

        // With a key passed by option the conversion read that one: the
        // sibling stays, and so does the option's.
        let iso = write(&dir.path().join("option/Game.iso"));
        let dkey = write(&dir.path().join("option/Game.dkey"));
        let option = write(&dir.path().join("option/keys/Other.dkey"));
        let mut req = request("ps3.decrypt", &iso, false);
        req.options.key = Some(option.clone());
        let (removed, ..) = release(&req, &single(&iso), &output).await;
        assert_eq!(removed, [iso]);
        assert!(dkey.exists() && option.exists());

        // An archive names the key after its member, found before the
        // archive itself goes.
        let zip = dir.path().join("zip/Game (Disc).zip");
        std::fs::create_dir_all(zip.parent().unwrap()).unwrap();
        write_zip(&zip, &[("Game.iso", b"iso")]);
        let dkey = write(&dir.path().join("zip/Game.dkey"));
        let (removed, failures, kept) =
            release(&request("ps3.decrypt", &zip, false), &single(&zip), &output).await;
        assert_eq!(removed, [zip.clone(), dkey.clone()]);
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );
        assert!(!zip.exists() && !dkey.exists());

        // A WUX keeps its content encrypted and still needs the key.
        let wud = write(&dir.path().join("wiiu/Game.wud"));
        let key = write(&dir.path().join("wiiu/Game.key"));
        let (removed, ..) =
            release(&request("wup.to_wux", &wud, false), &single(&wud), &output).await;
        assert_eq!(removed, [wud]);
        assert!(key.exists());
    }

    #[tokio::test]
    async fn a_wii_u_disc_takes_only_the_key_its_lookup_reads() {
        let dir = tempfile::tempdir().unwrap();
        let output = write(&dir.path().join("out/Game.wua"));
        let compress = |disc: &Path| request("wup.compress", disc, false);

        // Its own key, and game.key stays for the others.
        let folder = dir.path().join("own");
        let disc = write(&folder.join("Disc.wud"));
        let own = write(&folder.join("Disc.key"));
        let game_key = write(&folder.join("game.key"));
        let (removed, ..) = release(&compress(&disc), &single(&disc), &output).await;
        assert_eq!(removed, [disc, own]);
        assert!(game_key.exists());

        // Another disc falls back to game.key, which only a game.wud or
        // game_part1.wud reads as its own.
        let folder = dir.path().join("shared");
        let disc = write(&folder.join("Disc.wud"));
        let game = write(&folder.join("game.wud"));
        let game_key = write(&folder.join("game.key"));
        let (removed, _, kept) = release(&compress(&game), &single(&game), &output).await;
        assert_eq!(removed, [game]);
        assert!(
            kept.iter()
                .any(|note| note.contains("Disc.wud still falls back to it")),
            "{kept:?}"
        );
        let (removed, ..) = release(&compress(&disc), &single(&disc), &output).await;
        assert_eq!(removed, [disc]);
        assert!(game_key.exists());

        let folder = dir.path().join("split");
        let first = write(&folder.join("game_part1.wud"));
        let second = write(&folder.join("game_part2.wud"));
        let game_key = write(&folder.join("game.key"));
        let (removed, failures, kept) = release(&compress(&first), &single(&first), &output).await;
        assert_eq!(removed, [second, first, game_key]);
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );

        // A key passed with the input is the one the conversion read.
        let folder = dir.path().join("option");
        let disc = write(&folder.join("Disc.wud"));
        let own = write(&folder.join("Disc.key"));
        let mut req = request("wup.compress", &disc, false);
        req.options.inputs = Some(vec![WupTitleInputOption::Object {
            path: disc.clone(),
            format: None,
            key: Some(own.clone()),
            key_path: None,
        }]);
        let (removed, ..) = release(&req, &single(&disc), &output).await;
        assert_eq!(removed, [disc]);
        assert!(own.exists());
    }

    #[tokio::test]
    async fn a_folder_input_goes_whole_unless_it_holds_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let title = dir.path().join("Title");
        write(&title.join("code/app.rpx"));
        let inside = write(&title.join("Title.xiso"));
        let (removed, failures, kept) = release(
            &request("xbox.convert", &title, false),
            &single(&title),
            &inside,
        )
        .await;
        assert!(removed.is_empty() && failures.is_empty());
        assert!(kept[0].contains("holds an output"), "{kept:?}");
        assert!(inside.exists());

        let output = write(&dir.path().join("Title.xiso"));
        std::fs::remove_file(&inside).unwrap();
        let (removed, failures, kept) = release(
            &request("xbox.convert", &title, false),
            &single(&title),
            &output,
        )
        .await;
        assert_eq!(removed, std::slice::from_ref(&title));
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );
        assert!(!title.exists());
        let names: Vec<OsString> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from("Title.xiso")]);

        #[cfg(unix)]
        {
            let target = write(&dir.path().join("Target/app.rpx"));
            let link = dir.path().join("Link");
            std::os::unix::fs::symlink(dir.path().join("Target"), &link).unwrap();
            let (removed, _, kept) = release(
                &request("xbox.convert", &link, false),
                &single(&link),
                &output,
            )
            .await;
            assert!(removed.is_empty());
            assert!(kept[0].contains("link to a folder"), "{kept:?}");
            assert!(target.exists());
        }
    }

    #[test]
    fn prune_removes_folders_left_with_only_os_junk() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let disc = root.join("lib/Game/Disc 1");
        write(&disc.join(".DS_Store"));
        write(&root.join("lib/Game/Thumbs.db"));
        let notes = prune_emptied(&BTreeSet::from([disc]), &root.join("lib"), &[]);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!root.join("lib/Game").exists());
        assert!(root.join("lib").is_dir());
    }

    #[test]
    fn prune_keeps_a_folder_with_another_file_and_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let game = root.join("lib/Game");
        write(&game.join("readme.txt"));
        write(&game.join("zz.txt"));
        write(&game.join(".DS_Store"));
        let notes = prune_emptied(&BTreeSet::from([game.clone()]), &root.join("lib"), &[]);
        assert_eq!(
            notes,
            [format!(
                "kept folder {}: it still holds readme.txt",
                game.display()
            )]
        );
        assert!(game.join(".DS_Store").exists());

        // A subfolder stops the prune too, whatever its name.
        let parent = root.join("other/Game");
        std::fs::create_dir_all(parent.join(".Trashes")).unwrap();
        let notes = prune_emptied(&BTreeSet::from([parent.clone()]), &root.join("other"), &[]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(parent.join(".Trashes").is_dir());
    }

    #[test]
    fn prune_spares_the_stop_and_kept_folders_quietly() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        // The output's own folder, and every folder holding it.
        let output = write(&root.join("lib/Game/out/Game.chd"));
        let notes = prune_emptied(
            &BTreeSet::from([root.join("lib/Game/out"), root.join("lib/Game")]),
            &root.join("lib"),
            std::slice::from_ref(&output),
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert!(output.exists());

        // A kept folder (the current directory) and its parents stay even
        // when empty.
        let cwd = root.join("lib/Shell/cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let notes = prune_emptied(
            &BTreeSet::from([cwd.clone()]),
            &root.join("lib"),
            std::slice::from_ref(&cwd),
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert!(cwd.is_dir());

        // The stop itself never goes.
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(prune_emptied(&BTreeSet::from([empty.clone()]), &empty, &[]).is_empty());
        assert!(empty.is_dir());
    }

    #[test]
    fn prune_spares_the_home_folder_and_the_folders_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap().join("home");
        let downloads = home.join("Downloads");
        let game = home.join("Games/PS1/Game A");
        write(&downloads.join(".DS_Store"));
        std::fs::create_dir_all(&game).unwrap();
        let touched = BTreeSet::from([downloads.clone(), game.clone()]);
        let keep = home_folders(&touched, &home);
        assert_eq!(keep, [downloads.clone(), home.join("Games")]);

        let notes = prune_emptied(&touched, &dir.path().canonicalize().unwrap(), &keep);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(downloads.is_dir());
        assert!(!home.join("Games/PS1").exists());
        assert!(home.join("Games").is_dir());
    }

    #[tokio::test]
    async fn a_relative_input_releases_and_prunes_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (cue, _) = cue_set(&dir.path().join("lib/Game"), "Game");
        write(&dir.path().join("lib/Game/.DS_Store"));
        let output = write(&dir.path().join("out/Game.chd"));
        let input = relative_to_cwd(&cue);
        assert!(input.is_relative() && input.is_file());

        let scope = single(&input);
        let (removed, failures, kept) =
            release(&request("chd.compress", &input, false), &scope, &output).await;
        assert_eq!(removed.len(), 2, "{removed:?}");
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );
        let progress = RecordingProgress::default();
        scope.finish(&progress).await.unwrap();
        assert!(warnings(&progress).is_empty());
        assert!(!dir.path().join("lib/Game").exists());
        assert!(dir.path().join("lib").is_dir());
    }

    #[tokio::test]
    async fn a_dry_run_reports_what_would_go_and_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        let (cue, bin) = cue_set(&root.join("Game"), "Game");
        let first = write(&root.join("WiiU/game_part1.wud"));
        let second = write(&root.join("WiiU/game_part2.wud"));
        let output = dir.path().join("out/Game.chd");
        let scope = batch(&root, &[&cue, &first, &second]);

        let (removed, failures, kept) =
            release(&request("chd.compress", &cue, true), &scope, &output).await;
        assert_eq!(removed, [bin.clone(), cue.clone()]);
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );
        let (removed, ..) = release(&request("wup.to_wux", &first, true), &scope, &output).await;
        assert_eq!(removed, [second.clone(), first.clone()]);
        assert!(scope.is_released(&second).await);

        scope.finish(&RecordingProgress::default()).await.unwrap();
        for path in [&cue, &bin, &first, &second] {
            assert!(path.exists(), "{}", path.display());
        }
    }

    #[tokio::test]
    async fn a_sheet_set_goes_only_when_its_conversion_read_every_file_whole() {
        let dir = tempfile::tempdir().unwrap();
        let (cue, _) = cue_set(&dir.path().join("unread"), "Game");
        let victim = write(&dir.path().join("unread/saves.bin"));
        std::fs::write(
            &cue,
            "FILE \"Game (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\nFILE \"saves.bin\" BINARY\n",
        )
        .unwrap();
        let note = kept_note(&cue).await;
        assert_eq!(
            note,
            format!(
                "kept {}: {} is not read as whole tracks",
                cue.display(),
                victim.display()
            )
        );

        let (cue, bin) = cue_set(&dir.path().join("short"), "Game");
        std::fs::write(&bin, vec![0u8; 2352 * 300 + 17]).unwrap();
        assert!(
            kept_note(&cue)
                .await
                .contains("is not read as whole tracks")
        );

        let gdi = dir.path().join("dc/game.gdi");
        std::fs::create_dir_all(gdi.parent().unwrap()).unwrap();
        std::fs::write(dir.path().join("dc/track01.bin"), vec![0u8; 2048 * 3]).unwrap();
        std::fs::write(&gdi, "1\n1 0 4 2352 track01.bin 0\n").unwrap();
        assert!(
            kept_note(&gdi)
                .await
                .contains("is not read as whole tracks")
        );
        std::fs::write(&gdi, "1\n1 0 4 2048 track01.bin 0\n").unwrap();
        assert_eq!(own_files(&gdi).await.len(), 2);
    }

    #[tokio::test]
    async fn an_output_of_an_earlier_child_is_no_later_source() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        let (a, _) = cue_set(&root, "A");
        let first = root.join("A.chd");
        std::fs::write(&first, vec![0u8; 2352]).unwrap();
        let b = root.join("B.cue");
        std::fs::write(
            &b,
            "FILE \"A.chd\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let scope = batch(&root, &[&a, &b]);
        release(&request("chd.compress", &a, false), &scope, &first).await;
        let second = write(&root.join("B.chd"));
        let (removed, _, kept) =
            release(&request("chd.compress", &b, false), &scope, &second).await;
        assert!(removed.is_empty());
        assert!(kept[0].contains("is an output of this run"), "{kept:?}");
        assert!(first.exists() && b.exists());
    }

    #[tokio::test]
    async fn a_sheet_in_the_tracks_folder_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("Game");
        let (inner, track) = cue_set(&game.join("tracks"), "T");
        let outer = game.join("A.cue");
        std::fs::write(
            &outer,
            "FILE \"tracks/T (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let output = write(&dir.path().join("out/A.chd"));
        let (removed, _, kept) = release(
            &request("chd.compress", &outer, false),
            &single(&outer),
            &output,
        )
        .await;
        assert_eq!(removed, [outer]);
        assert!(
            kept[0].contains("another scanned unit references it"),
            "{kept:?}"
        );
        assert!(track.exists() && inner.exists());
    }

    #[tokio::test]
    async fn a_walked_track_goes_with_the_sheet_that_covers_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        std::fs::create_dir_all(&root).unwrap();
        let cue = root.join("Game.cue");
        let iso = root.join("Game.iso");
        std::fs::write(&iso, vec![0u8; 2352 * 4]).unwrap();
        std::fs::write(
            &cue,
            "FILE \"Game.iso\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let output = write(&dir.path().join("out/Game.chd"));

        let scope = batch(&root, &[&cue, &iso]);
        assert!(!scope.is_released(&cue).await);
        let (removed, ..) = release(&request("chd.compress", &cue, true), &scope, &output).await;
        assert_eq!(removed, [iso.clone(), cue.clone()]);
        assert!(scope.is_released(&iso).await);

        // When the track's own child runs first, or its sheet failed, the
        // sheet still reads it; the sheet takes it once it goes.
        let scope = batch(&root, &[&cue, &iso]);
        assert!(!scope.is_released(&iso).await);
        let (removed, _, kept) =
            release(&request("chd.compress", &iso, true), &scope, &output).await;
        assert!(removed.is_empty());
        assert!(
            kept[0].contains("another scanned unit references it"),
            "{kept:?}"
        );
        let (removed, ..) = release(&request("chd.compress", &cue, true), &scope, &output).await;
        assert_eq!(removed, [iso, cue]);
    }

    #[tokio::test]
    async fn a_sheet_above_the_input_folder_keeps_the_track_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        let game = lib.join("Game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(
            lib.join("All.cue"),
            "FILE \"Game/Game (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let output = write(&dir.path().join("out/Game.chd"));
        // A single run of the sheet, and a recursive run over its folder.
        for recursive in [false, true] {
            let (cue, bin) = cue_set(&game, "Game");
            let scope = if recursive {
                batch(&game, &[&cue])
            } else {
                single(&cue)
            };
            let (removed, failures, kept) =
                release(&request("chd.compress", &cue, false), &scope, &output).await;
            assert_eq!(removed, [cue]);
            assert!(failures.is_empty(), "{failures:?}");
            assert!(
                kept[0].contains("another scanned unit references it"),
                "{kept:?}"
            );
            assert!(bin.exists());
        }
    }

    #[tokio::test]
    async fn a_folder_holding_the_current_or_home_folder_or_a_root_stays() {
        let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let mut guarded = vec![
            (cwd.clone(), "holds the current folder"),
            (
                cwd.parent().unwrap().to_path_buf(),
                "holds the current folder",
            ),
            (
                cwd.ancestors().last().unwrap().to_path_buf(),
                "is a filesystem root",
            ),
        ];
        guarded.extend(dirs::home_dir().map(|home| (home, "holds the home folder")));
        for (dir, reason) in guarded {
            let (removed, _, kept) = release_folder(&dir, &[], true).await;
            assert!(removed.is_empty(), "{}", dir.display());
            assert!(
                kept[0].starts_with(&format!("kept folder {}", dir.display()))
                    && kept[0].contains(reason),
                "{kept:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_source_root_stops_the_prune_of_a_single_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("dropped");
        let output = write(&dir.path().join("out/Game.chd"));
        let flat = write(&root.join("Flat.iso"));
        let nested = write(&root.join("Game/Disc 1/Game.iso"));
        for input in [&flat, &nested] {
            let scope = ReleaseScope::single(input, Some(&root));
            release(&request("cso.compress", input, false), &scope, &output).await;
            scope.finish(&RecordingProgress::default()).await.unwrap();
        }
        assert!(!root.join("Game").exists());
        assert!(root.is_dir());
    }

    #[test]
    fn an_output_claim_holds_while_written_and_once_its_source_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let first = write(&dir.path().join("A.iso"));
        let second = write(&dir.path().join("B.iso"));
        let output = dir.path().join("out/Same.cso");
        let variant = dir.path().join("out/SAME.cso");

        let pending = claim_output(&first, &output, true).unwrap();
        let writing = format!(
            "{} is being written from {}",
            variant.display(),
            entry_location(&first).display()
        );
        assert_eq!(
            refused(&second, &variant),
            format!("source kept: {writing}")
        );
        assert_eq!(
            check_output(&variant, false).unwrap_err().to_string(),
            writing
        );
        // Its own source waits too: a second job for one file would write
        // over the first.
        assert_eq!(
            refused(&first, &output),
            format!(
                "source kept: {} is being written from {}",
                output.display(),
                entry_location(&first).display()
            )
        );
        assert!(check_output(&output, false).is_err());
        drop(pending);

        write(&output);
        claim_output(&second, &output, true).unwrap().commit();
        let only = format!(
            "{} is the only copy of {}, which was already deleted",
            output.display(),
            entry_location(&second).display()
        );
        assert_eq!(refused(&first, &output), format!("source kept: {only}"));
        assert_eq!(check_output(&output, false).unwrap_err().to_string(), only);
        assert_eq!(refused(&second, &output), format!("source kept: {only}"));

        std::fs::remove_file(&output).unwrap();
        drop(claim_output(&first, &output, true).unwrap());
        check_output(&output, false).unwrap();
    }

    /// A committed claim's source is gone, so a file now at its path is a
    /// new one and holds no right to the output.
    #[test]
    fn a_committed_claim_holds_against_a_new_file_at_its_source_path() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("A.iso");
        let other = dir.path().join("B.iso");
        let output = write(&dir.path().join("out/Same.cso"));
        claim_output(&source, &output, true).unwrap().commit();
        let only = format!(
            "{} is the only copy of {}, which was already deleted",
            output.display(),
            entry_location(&source).display()
        );
        assert_eq!(refused(&source, &output), format!("source kept: {only}"));
        assert_eq!(
            claim_output(&source, &output, false)
                .err()
                .unwrap()
                .to_string(),
            only
        );

        // With the output gone the claim holds nothing, and the next
        // claimant gets a pending entry of its own.
        std::fs::remove_file(&output).unwrap();
        let again = claim_output(&source, &output, true).unwrap();
        assert!(
            refused(&other, &output).contains("is being written from"),
            "{}",
            refused(&other, &output)
        );
        drop(again);
        drop(claim_output(&other, &output, true).unwrap());
    }

    #[test]
    fn a_write_that_keeps_its_source_holds_the_output_too() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("A.iso");
        let second = dir.path().join("B.iso");
        let output = dir.path().join("out/Same.cso");
        let writing = claim_output(&first, &output, false).unwrap();
        let reason = format!(
            "{} is being written from {}",
            output.display(),
            entry_location(&first).display()
        );
        assert_eq!(refused(&second, &output), format!("source kept: {reason}"));
        assert_eq!(
            claim_output(&second, &output, false)
                .err()
                .unwrap()
                .to_string(),
            reason
        );
        drop(writing);
        drop(claim_output(&second, &output, true).unwrap());
        check_output(&output, false).unwrap();
    }

    #[test]
    fn sources_named_apart_by_case_alone_cannot_share_an_output() {
        let dir = tempfile::tempdir().unwrap();
        let upper = dir.path().join("Game.cue");
        let lower = dir.path().join("game.cue");
        let output = write(&dir.path().join("out/Game.chd"));
        claim_output(&upper, &output, true).unwrap().commit();
        let only = format!(
            "{} is the only copy of {}, which was already deleted",
            output.display(),
            entry_location(&upper).display()
        );
        assert_eq!(refused(&lower, &output), format!("source kept: {only}"));
        assert_eq!(check_output(&output, false).unwrap_err().to_string(), only);
    }

    /// The single record of a run that wrote `output` from `input`.
    fn written(input: &Path, output: &Path) -> RunResponse {
        use crate::util::report::{ReportRecord, ReportRecordInput};
        RunResponse::ok("", None).with_record(ReportRecord::new(ReportRecordInput {
            input_path: input.display().to_string(),
            output_path: output.display().to_string(),
            operation: "compress".to_string(),
            status: FileStatus::Ok,
            input_bytes: 0,
            output_bytes: 0,
            elapsed_ms: 0,
            error: None,
        }))
    }

    /// Releases `input` the way a run that claimed `output` does, and
    /// returns its response once the claim left with the run.
    async fn release_claimed(operation: &str, input: &Path, output: &Path) -> RunResponse {
        let mut req = request(operation, input, false);
        let claim = claim_output(input, output, true).unwrap();
        req.ctx.output_claim = Some(Arc::new(Mutex::new(Some(claim))));
        let mut response = written(input, output);
        let progress = RecordingProgress::default();
        release_after(
            &req,
            operation,
            EXTS,
            &mut response,
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        response
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_claim_commits_once_any_source_went_and_only_then() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        let dir = tempfile::tempdir().unwrap();

        // A file whose folder refuses the rename never left its place.
        let locked = dir.path().join("Locked");
        let iso = write(&locked.join("Game.iso"));
        let output = write(&dir.path().join("out/Game.cso"));
        mode(&locked, 0o555);
        let response = release_claimed("cso.compress", &iso, &output).await;
        mode(&locked, 0o755);
        assert_eq!(
            response.records[0].status,
            FileStatus::Failed,
            "{response:?}"
        );
        assert!(iso.exists());
        check_output(&output, false).unwrap();

        // A folder input renamed aside and then only partly deleted.
        let title = dir.path().join("Title");
        write(&title.join("default.xbe"));
        write(&title.join("media/locked/intro.xmv"));
        mode(&title.join("media/locked"), 0o555);
        let output = write(&dir.path().join("out/Title.xiso"));
        let response = release_claimed("xbox.convert", &title, &output).await;
        let aside = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_string_lossy().contains(".rom-converto-delete-"))
            .expect("the renamed folder");
        mode(&aside.join("media/locked"), 0o755);
        assert_eq!(
            response.records[0].status,
            FileStatus::Failed,
            "{response:?}"
        );
        assert!(
            response.message.contains("was only partly deleted"),
            "{response:?}"
        );
        assert!(!title.exists());
        assert!(
            check_output(&output, false)
                .unwrap_err()
                .to_string()
                .contains("is the only copy of"),
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_folder_that_cannot_be_listed_keeps_the_file_under_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("Game");
        let sub = game.join("sub");
        let bin = sub.join("Game (Track 1).bin");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(&bin, vec![0u8; 2352]).unwrap();
        let cue = game.join("Game.cue");
        std::fs::write(
            &cue,
            "FILE \"sub/Game (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let output = write(&dir.path().join("out/Game.chd"));
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o311)).unwrap();
        let (removed, failures, kept) = release(
            &request("chd.compress", &cue, false),
            &single(&cue),
            &output,
        )
        .await;
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(removed, [cue]);
        assert!(failures.is_empty(), "{failures:?}");
        let listed = format!(
            "kept {}: could not list {}: ",
            bin.display(),
            folder_of(&bin).display()
        );
        assert!(kept.len() == 1 && kept[0].starts_with(&listed), "{kept:?}");
        assert!(bin.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_folder_above_the_input_that_cannot_be_listed_holds_no_sheets() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("Locked");
        let (cue, bin) = cue_set(&locked.join("Game"), "Game");
        let output = write(&dir.path().join("out/Game.chd"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o311)).unwrap();
        let (removed, failures, kept) = release(
            &request("chd.compress", &cue, false),
            &single(&cue),
            &output,
        )
        .await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(removed, [bin, cue]);
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );
    }

    #[tokio::test]
    async fn a_dry_run_plans_a_shared_bin_with_its_last_sheet() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        let (a, bin) = cue_set(&root.join("Pair"), "A");
        let b = root.join("Pair/B.cue");
        std::fs::copy(&a, &b).unwrap();
        let output = dir.path().join("out/A.chd");
        let scope = batch(&root, &[&a, &b]);
        let (removed, _, kept) = release(&request("chd.compress", &a, true), &scope, &output).await;
        assert_eq!(removed, std::slice::from_ref(&a));
        assert!(
            kept[0].contains("another scanned unit references it"),
            "{kept:?}"
        );
        let (removed, failures, kept) =
            release(&request("chd.compress", &b, true), &scope, &output).await;
        assert_eq!(removed, [bin.clone(), b.clone()]);
        assert!(
            failures.is_empty() && kept.is_empty(),
            "{failures:?} {kept:?}"
        );
        assert!(a.exists() && b.exists() && bin.exists());
    }

    /// A sheet elsewhere in the walk that reads the track through a link.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_walked_sheet_reading_a_track_through_a_link_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        let track = lib.join("A/track.bin");
        std::fs::create_dir_all(lib.join("A")).unwrap();
        std::fs::create_dir_all(lib.join("B")).unwrap();
        std::fs::write(&track, vec![0u8; 2352]).unwrap();
        let sheet = |name: &str| {
            format!("FILE \"{name}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n")
        };
        let a = lib.join("A/A.cue");
        std::fs::write(&a, sheet("track.bin")).unwrap();
        let b = lib.join("B/B.cue");
        std::fs::write(&b, sheet("linkA/track.bin")).unwrap();
        std::os::unix::fs::symlink(lib.join("A"), lib.join("B/linkA")).unwrap();
        let output = write(&dir.path().join("out/A.chd"));

        let scope = batch(&lib, &[&a, &b]);
        let (removed, _, kept) = release(&request("chd.compress", &a, true), &scope, &output).await;
        assert_eq!(removed, std::slice::from_ref(&a));
        assert!(
            kept[0].contains("another scanned unit references it"),
            "{kept:?}"
        );

        let scope = batch(&lib, &[&a, &b]);
        let (removed, ..) = release(&request("chd.compress", &b, false), &scope, &output).await;
        assert_eq!(removed, std::slice::from_ref(&b));
        let (removed, ..) = release(&request("chd.compress", &a, false), &scope, &output).await;
        assert_eq!(removed, [track.clone(), a]);
        assert!(!track.exists());
    }

    /// The input reached through a linked folder: a sheet above the folder
    /// it was spelled in still reads its track.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sheet_above_the_folder_the_input_was_spelled_in_keeps_the_track() {
        let dir = tempfile::tempdir().unwrap();
        cue_set(&dir.path().join("real/Game"), "Game");
        let lib = dir.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real/Game"), lib.join("Game")).unwrap();
        std::fs::write(
            lib.join("All.cue"),
            "FILE \"Game/Game (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let cue = lib.join("Game/Game.cue");
        let output = write(&dir.path().join("out/Game.chd"));
        let (removed, _, kept) =
            release(&request("chd.compress", &cue, true), &single(&cue), &output).await;
        assert_eq!(removed, [cue]);
        assert!(
            kept[0].contains("another scanned unit references it"),
            "{kept:?}"
        );
    }
}
