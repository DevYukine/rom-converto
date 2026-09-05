//! Split a super / multi-title NSP or XCI into one NSP per title group.
//! Every NCA is copied byte-verbatim; only the meta NCAs are decrypted,
//! to map each content NCA to its owning title.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::nintendo::nx::container::{detect_container, list_container};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::meta::{merge_inline_tickets, read_meta_cnmt};
use crate::nintendo::nx::models::cnmt::{CNMT_TYPE_ADD_ON_CONTENT, CNMT_TYPE_PATCH};
use crate::nintendo::nx::util::{Pfs0Source, write_pfs0_from_sources};
use crate::nintendo::nx::walker::NcaWalker;
use crate::util::{AtomicProgress, CancelToken, ProgressReporter, await_with_progress_cancel};

/// Splits a multi-title NSP/XCI into one NSP per title, copying every NCA
/// byte-verbatim, and returns the written paths.
///
/// # Errors
/// Returns an error if `input` is a compressed container or a meta NCA
/// cannot be decrypted with `keys`.
pub fn split_container(
    input: &Path,
    output_dir: &Path,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<Vec<PathBuf>> {
    let plan = plan_split(input, keys)?;
    for warning in &plan.warnings {
        progress.warn(warning);
    }
    write_split(&plan, output_dir, progress, cancel)
}

/// The per-title source ranges a split will copy, plus the naming inputs
/// for the output files.
struct SplitPlan {
    groups: BTreeMap<u64, Vec<Pfs0Source>>,
    titles: BTreeMap<u64, TitleInfo>,
    stem: String,
    /// Planning runs off the reporter (behind a byte-counter proxy under
    /// `spawn_blocking`), so its warnings are replayed by the caller.
    warnings: Vec<String>,
}

impl SplitPlan {
    fn total_bytes(&self) -> u64 {
        self.groups.values().flatten().map(|s| s.size).sum()
    }

    fn output_paths(&self, output_dir: &Path) -> Vec<PathBuf> {
        self.groups
            .keys()
            .map(|title_id| {
                let info = self.titles.get(title_id).copied().unwrap_or(TitleInfo {
                    version: 0,
                    content_type: 0,
                });
                let suffix = match info.content_type {
                    CNMT_TYPE_PATCH => " [UPD]",
                    CNMT_TYPE_ADD_ON_CONTENT => " [DLC]",
                    _ => "",
                };
                output_dir.join(format!(
                    "{} [{title_id:016X}] [v{}]{suffix}.nsp",
                    self.stem, info.version
                ))
            })
            .collect()
    }
}

fn plan_split(input: &Path, keys: &KeySet) -> NxResult<SplitPlan> {
    let kind = detect_container(input)?;
    if kind.is_compressed() {
        return Err(NxError::CompressedInputUnsupported(input.to_path_buf()));
    }
    let listing = list_container(input)?;
    let mut keys = keys.clone();
    merge_inline_tickets(input, &listing, &mut keys);
    let file = Arc::new(File::open(input)?);

    let mut nca_to_title: BTreeMap<String, u64> = BTreeMap::new();
    let mut titles: BTreeMap<u64, TitleInfo> = BTreeMap::new();
    for entry in &listing.entries {
        if !entry.name.to_ascii_lowercase().ends_with(".cnmt.nca") {
            continue;
        }
        let cnmt = read_meta_cnmt(file.clone(), entry.abs_offset, entry.size, &keys)?;
        // A super container can hold several versions of the same title;
        // the filename tag has to report the newest of them.
        let info = titles.entry(cnmt.title_id).or_insert(TitleInfo {
            version: cnmt.version,
            content_type: cnmt.content_type,
        });
        if cnmt.version >= info.version {
            info.version = cnmt.version;
            info.content_type = cnmt.content_type;
        }
        nca_to_title.insert(content_id_of(&entry.name), cnmt.title_id);
        for content in &cnmt.contents {
            nca_to_title.insert(hex::encode(content.content_id), cnmt.title_id);
        }
    }

    let mut warnings = Vec::new();
    let mut groups: BTreeMap<u64, Vec<Pfs0Source>> = BTreeMap::new();
    for entry in &listing.entries {
        let lower = entry.name.to_ascii_lowercase();
        if !lower.ends_with(".nca") {
            continue;
        }
        let id = content_id_of(&entry.name);
        let title_id = match nca_to_title.get(&id) {
            Some(&t) => t,
            None => match NcaWalker::open(file.clone(), entry.abs_offset, entry.size, &keys) {
                Ok(walker) => walker.header.title_id,
                Err(err) => {
                    warnings.push(format!(
                        "skipping {}: no CNMT references it and its header could not be read ({err})",
                        entry.name
                    ));
                    continue;
                }
            },
        };
        groups.entry(title_id).or_default().push(Pfs0Source {
            file: file.clone(),
            abs_offset: entry.abs_offset,
            size: entry.size,
            name: entry.name.clone(),
        });
    }

    // Attach tickets and certs to the group whose title id matches the
    // 16-hex-char filename prefix (the rights-id title id).
    for entry in &listing.entries {
        let lower = entry.name.to_ascii_lowercase();
        if !lower.ends_with(".tik") && !lower.ends_with(".cert") {
            continue;
        }
        let Some(title_id) = title_id_from_prefix(&entry.name) else {
            continue;
        };
        if let Some(sources) = groups.get_mut(&title_id) {
            sources.push(Pfs0Source {
                file: file.clone(),
                abs_offset: entry.abs_offset,
                size: entry.size,
                name: entry.name.clone(),
            });
        }
    }

    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());

    Ok(SplitPlan {
        groups,
        titles,
        stem,
        warnings,
    })
}

fn write_split(
    plan: &SplitPlan,
    output_dir: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<Vec<PathBuf>> {
    std::fs::create_dir_all(output_dir)?;
    let paths = plan.output_paths(output_dir);
    let mut written = Vec::with_capacity(paths.len());
    for (sources, out_path) in plan.groups.values().zip(paths) {
        if let Err(err) = write_pfs0_from_sources(&out_path, sources, progress, cancel) {
            let _ = std::fs::remove_file(&out_path);
            for path in &written {
                let _ = std::fs::remove_file(path);
            }
            return Err(err);
        }
        written.push(out_path);
    }

    Ok(written)
}

/// Splits `input` off the async runtime, reporting progress and honoring
/// `cancel` through both the planning pass and the writes.
///
/// # Errors
/// Same as [`split_container`], plus [`NxError::Cancelled`] when `cancel`
/// fires.
pub async fn split_container_async_cancellable(
    input: PathBuf,
    output_dir: PathBuf,
    keys: KeySet,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> NxResult<Vec<PathBuf>> {
    let plan = tokio::task::spawn_blocking(move || plan_split(&input, &keys)).await??;
    for warning in &plan.warnings {
        progress.warn(warning);
    }
    if cancel.is_cancelled() {
        return Err(NxError::Cancelled);
    }
    progress.start(plan.total_bytes(), "Splitting Switch container");
    let bytes_done = Arc::new(AtomicU64::new(0));
    let proxy = AtomicProgress {
        counter: bytes_done.clone(),
    };
    // write_split only cleans up what it managed to start; a cancel race
    // or a panicked worker leaves whichever per-title files exist behind.
    let outputs = plan.output_paths(&output_dir);
    let cancel_bg = cancel.clone();
    let handle = tokio::task::spawn_blocking(move || -> NxResult<Vec<PathBuf>> {
        write_split(&plan, &output_dir, &proxy, &cancel_bg)
    });
    let cleanup = || NxError::Cancelled;
    match await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await {
        Ok(written) => Ok(written),
        Err(err) => {
            for path in &outputs {
                let _ = std::fs::remove_file(path);
            }
            Err(err)
        }
    }
}

#[derive(Clone, Copy)]
struct TitleInfo {
    version: u32,
    content_type: u8,
}

/// Bare content id of an NCA filename: lowercased, minus a trailing
/// `.cnmt.nca` or `.nca`, matching the ids CNMT content records carry.
fn content_id_of(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let stem = lower
        .strip_suffix(".cnmt.nca")
        .or_else(|| lower.strip_suffix(".nca"))
        .unwrap_or(&lower);
    stem.to_string()
}

fn title_id_from_prefix(name: &str) -> Option<u64> {
    let prefix = name.get(..16)?;
    if !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(prefix, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::merge::{NxMergeFormat, merge_containers};
    use crate::nintendo::nx::models::cnmt::{
        CNMT_TYPE_ADD_ON_CONTENT, CNMT_TYPE_APPLICATION, CNMT_TYPE_PATCH,
    };
    use crate::nintendo::nx::models::pfs0::Pfs0;
    use crate::nintendo::nx::test_fixtures::{build_meta_nca, build_test_nsp, synthetic_keyset};
    use crate::util::NoProgress;
    use std::io::{Cursor, Write};
    use std::sync::Mutex;
    use tempfile::{Builder, NamedTempFile, tempdir};

    fn id(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    fn prog(id: [u8; 16], size: usize) -> (String, Vec<u8>) {
        (format!("{}.nca", hex::encode(id)), vec![0xAB; size])
    }

    fn meta_file(
        meta_id: [u8; 16],
        title_id: u64,
        version: u32,
        ty: u8,
        contents: &[[u8; 16]],
    ) -> (String, Vec<u8>) {
        (
            format!("{}.cnmt.nca", hex::encode(meta_id)),
            build_meta_nca(title_id, version, ty, contents),
        )
    }

    fn nsp_names(path: &Path) -> Vec<String> {
        let blob = std::fs::read(path).unwrap();
        let pfs0 = Pfs0::read(&mut Cursor::new(&blob)).unwrap();
        let mut names: Vec<String> = pfs0.files.iter().map(|f| f.name.clone()).collect();
        names.sort();
        names
    }

    fn write_nsp(files: &[(String, Vec<u8>)]) -> NamedTempFile {
        let blob = build_test_nsp(files);
        let mut f = Builder::new().suffix(".nsp").tempfile().unwrap();
        std::io::Write::write_all(&mut f, &blob).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn splits_two_titles() {
        let nsp = write_nsp(&[
            meta_file(
                id(0x01),
                0x0100_AAAA_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[id(0x11)],
            ),
            prog(id(0x11), 0x800),
            meta_file(
                id(0x02),
                0x0100_BBBB_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[id(0x22)],
            ),
            prog(id(0x22), 0x800),
        ]);
        let dir = tempdir().unwrap();
        let out = split_container(
            nsp.path(),
            dir.path(),
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(out.len(), 2);

        let mut sets: Vec<Vec<String>> = out.iter().map(|p| nsp_names(p)).collect();
        sets.sort();
        let mut want = vec![
            {
                let mut v = vec![
                    format!("{}.cnmt.nca", hex::encode(id(0x01))),
                    format!("{}.nca", hex::encode(id(0x11))),
                ];
                v.sort();
                v
            },
            {
                let mut v = vec![
                    format!("{}.cnmt.nca", hex::encode(id(0x02))),
                    format!("{}.nca", hex::encode(id(0x22))),
                ];
                v.sort();
                v
            },
        ];
        want.sort();
        assert_eq!(sets, want);
    }

    #[test]
    fn names_base_patch_dlc() {
        let nsp = write_nsp(&[
            meta_file(
                id(0x01),
                0x0100_AAAA_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[id(0x11)],
            ),
            prog(id(0x11), 0x800),
            meta_file(
                id(0x02),
                0x0100_AAAA_0000_0800,
                0x10000,
                CNMT_TYPE_PATCH,
                &[id(0x22)],
            ),
            prog(id(0x22), 0x800),
            meta_file(
                id(0x03),
                0x0100_AAAA_0000_1000,
                0,
                CNMT_TYPE_ADD_ON_CONTENT,
                &[id(0x33)],
            ),
            prog(id(0x33), 0x800),
        ]);
        let dir = tempdir().unwrap();
        let out = split_container(
            nsp.path(),
            dir.path(),
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(out.len(), 3);

        let names: Vec<String> = out
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n.contains("[0100AAAA00000000]")
            && !n.contains("[UPD]")
            && !n.contains("[DLC]")));
        assert!(
            names
                .iter()
                .any(|n| n.contains("[0100AAAA00000800]") && n.contains("[UPD]"))
        );
        assert!(
            names
                .iter()
                .any(|n| n.contains("[0100AAAA00001000]") && n.contains("[DLC]"))
        );
    }

    /// Captures every `warn()` call; every other method is a no-op.
    struct WarnRecorder {
        warnings: Mutex<Vec<String>>,
    }

    impl ProgressReporter for WarnRecorder {
        fn start(&self, _total: u64, _msg: &str) {}
        fn inc(&self, _delta: u64) {}
        fn finish(&self) {}
        fn warn(&self, message: &str) {
            self.warnings
                .lock()
                .expect("warn lock")
                .push(message.into());
        }
    }

    #[test]
    fn names_title_with_its_highest_cnmt_version() {
        let title = 0x0100_DDDD_0000_0800;
        let nsp = write_nsp(&[
            meta_file(id(0x01), title, 1, CNMT_TYPE_PATCH, &[id(0x11)]),
            prog(id(0x11), 0x800),
            meta_file(id(0x02), title, 0x30000, CNMT_TYPE_PATCH, &[id(0x22)]),
            prog(id(0x22), 0x800),
        ]);
        let dir = tempdir().unwrap();
        let out = split_container(
            nsp.path(),
            dir.path(),
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        let name = out[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.contains("[v196608]"), "{name}");
    }

    #[test]
    fn skips_unreferenced_nca_whose_header_cannot_be_read() {
        let nsp = write_nsp(&[
            meta_file(
                id(0x01),
                0x0100_EEEE_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[id(0x11)],
            ),
            prog(id(0x11), 0x800),
            (format!("{}.nca", hex::encode(id(0x99))), vec![0u8; 0x400]),
        ]);
        let dir = tempdir().unwrap();
        let recorder = WarnRecorder {
            warnings: Mutex::new(Vec::new()),
        };
        let out = split_container(
            nsp.path(),
            dir.path(),
            &synthetic_keyset(),
            &recorder,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(nsp_names(&out[0]), {
            let mut v = vec![
                format!("{}.cnmt.nca", hex::encode(id(0x01))),
                format!("{}.nca", hex::encode(id(0x11))),
            ];
            v.sort();
            v
        });
        let warnings = recorder.warnings.lock().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains(&hex::encode(id(0x99))) && w.contains("skipping")),
            "{warnings:?}"
        );
    }

    /// Fires `cancel` once the blocking write has already finished, the
    /// race `await_with_progress_cancel` maps to `Cancelled`.
    struct CancelOnFinish(CancelToken);

    impl ProgressReporter for CancelOnFinish {
        fn start(&self, _total: u64, _msg: &str) {}
        fn inc(&self, _delta: u64) {}
        fn finish(&self) {
            self.0.cancel();
        }
    }

    #[tokio::test]
    async fn async_split_cancelled_after_completion_leaves_no_files() {
        let nsp = write_nsp(&[
            meta_file(
                id(0x01),
                0x0100_7777_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[id(0x11)],
            ),
            prog(id(0x11), 0x800),
        ]);
        let dir = tempdir().unwrap();
        let cancel = CancelToken::new();
        let err = split_container_async_cancellable(
            nsp.path().to_path_buf(),
            dir.path().to_path_buf(),
            synthetic_keyset(),
            &CancelOnFinish(cancel.clone()),
            cancel,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, NxError::Cancelled));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn async_split_replays_plan_warnings() {
        let nsp = write_nsp(&[
            meta_file(
                id(0x01),
                0x0100_8888_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[id(0x11)],
            ),
            prog(id(0x11), 0x800),
            (format!("{}.nca", hex::encode(id(0x99))), vec![0u8; 0x400]),
        ]);
        let dir = tempdir().unwrap();
        let recorder = WarnRecorder {
            warnings: Mutex::new(Vec::new()),
        };
        let out = split_container_async_cancellable(
            nsp.path().to_path_buf(),
            dir.path().to_path_buf(),
            synthetic_keyset(),
            &recorder,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.len(), 1);
        let warnings = recorder.warnings.lock().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains(&hex::encode(id(0x99))) && w.contains("skipping")),
            "{warnings:?}"
        );
    }

    #[test]
    fn merge_then_split_round_trips() {
        let base = write_nsp(&[
            meta_file(
                id(0x01),
                0x0100_CCCC_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[id(0x11)],
            ),
            prog(id(0x11), 0x800),
        ]);
        let update = write_nsp(&[
            meta_file(
                id(0x02),
                0x0100_CCCC_0000_0800,
                0x10000,
                CNMT_TYPE_PATCH,
                &[id(0x22)],
            ),
            prog(id(0x22), 0x800),
        ]);

        let merged = NamedTempFile::new().unwrap();
        merge_containers(
            &[base.path().to_path_buf(), update.path().to_path_buf()],
            merged.path(),
            NxMergeFormat::Nsp,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();

        let dir = tempdir().unwrap();
        let out = split_container(
            merged.path(),
            dir.path(),
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(out.len(), 2);

        let mut sets: Vec<Vec<String>> = out.iter().map(|p| nsp_names(p)).collect();
        sets.sort();
        let mut base_set = vec![
            format!("{}.cnmt.nca", hex::encode(id(0x01))),
            format!("{}.nca", hex::encode(id(0x11))),
        ];
        base_set.sort();
        let mut update_set = vec![
            format!("{}.cnmt.nca", hex::encode(id(0x02))),
            format!("{}.nca", hex::encode(id(0x22))),
        ];
        update_set.sort();
        let mut want = vec![base_set, update_set];
        want.sort();
        assert_eq!(sets, want);
    }
}
