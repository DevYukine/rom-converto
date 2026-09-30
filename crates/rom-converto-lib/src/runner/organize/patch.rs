//! The organize patch pass: index patch files and expand plans into patched
//! variants of their units.

use super::plan::{Decision, UnitPlan};
use crate::patch::Patch;
use crate::util::{CancelToken, Cancelled, ProgressReporter, TemplateTokens};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

/// Every patch file found under the run's `patch` paths, keyed for lookup
/// against each unit's source ROM.
#[derive(Default)]
pub(super) struct PatchIndex {
    /// Patch paths by the source CRC32 of the ROM they apply to. Several
    /// patches may share one source CRC.
    by_crc: HashMap<u32, Vec<PathBuf>>,
}

impl PatchIndex {
    /// Checks every `--patch` path exists before any scanning or staging:
    /// a missing path is an invalid argument naming it, and any other
    /// lookup failure (a permission error, say) is reported as itself,
    /// never as "not found".
    pub(super) fn validate_paths(paths: &[PathBuf]) -> Result<()> {
        for path in paths {
            match std::fs::symlink_metadata(path) {
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Err(crate::runner::invalid_arg(format!(
                        "patch path not found: {}",
                        path.display()
                    )));
                }
                Err(err) => {
                    return Err(crate::runner::invalid_arg(format!(
                        "patch path {}: {err}",
                        path.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Walks `paths` (files or directories) collecting patch files. A patch
    /// that cannot be read, or that encodes no source CRC and so can never
    /// match, warns through `progress` and is skipped.
    pub(super) async fn build(
        paths: &[PathBuf],
        progress: &dyn ProgressReporter,
        cancel: &CancelToken,
    ) -> Result<Self> {
        let mut files = Vec::new();
        for path in paths {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            if path.is_dir() {
                files.extend(crate::util::fs::collect_files_with_exts(
                    path,
                    crate::patch::PATCH_EXTENSIONS,
                    None,
                    cancel,
                )?);
            } else {
                files.push(path.clone());
            }
        }

        let mut by_crc: HashMap<u32, Vec<PathBuf>> = HashMap::new();
        for path in files {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            // BPS/UPS footers hash the whole patch file, so open off the
            // blocking pool and treat cancellation as an error rather than
            // an unreadable patch.
            let open_path = path.clone();
            let open_cancel = cancel.clone();
            let opened =
                tokio::task::spawn_blocking(move || Patch::open(&open_path, &open_cancel)).await;
            let patch = match opened {
                Ok(Ok(patch)) => patch,
                Ok(Err(err)) if Cancelled::in_chain(&err) => return Err(err),
                Ok(Err(err)) => {
                    progress.warn(&format!(
                        "skipping unreadable patch {}: {err:#}",
                        path.display()
                    ));
                    continue;
                }
                Err(join) => return Err(join.into()),
            };
            match patch.source_crc() {
                Some(crc) => by_crc.entry(crc).or_default().push(path),
                None => progress.warn(&format!(
                    "patch {} encodes no source CRC; it will never match a ROM",
                    path.display()
                )),
            }
        }
        Ok(Self { by_crc })
    }

    /// True when no patch files are indexed; callers skip CRC computation
    /// and plan expansion entirely.
    pub(super) fn is_empty(&self) -> bool {
        self.by_crc.is_empty()
    }

    /// Whether some patch pairs to a ROM with source CRC32 `crc`.
    pub(super) fn contains(&self, crc: u32) -> bool {
        self.by_crc.contains_key(&crc)
    }
}

/// Expands `plans` with patched variants of matched units. With
/// `patch_only`, matched units keep only their patched variant and unmatched
/// units are skipped.
pub(super) fn expand_patched(
    plans: &mut Vec<UnitPlan>,
    index: &PatchIndex,
    crc_of: &dyn Fn(usize) -> Option<u32>,
    patch_only: bool,
) {
    // Appended after the loop: the clones' unit indices refer to `units`,
    // which stay aligned with the plans already in `plans`.
    let mut patched = Vec::new();
    for plan in plans.iter_mut() {
        if plan.decision != Decision::Keep {
            continue;
        }
        let paths = crc_of(plan.index).and_then(|crc| index.by_crc.get(&crc));
        if let Some(paths) = paths {
            patched.extend(paths.iter().map(|path| patched_variant(plan, path.clone())));
        }
        if patch_only {
            plan.decision = Decision::Skip("patch-only".to_string());
        }
    }
    plans.extend(patched);
}

/// A copy of `plan` rebased onto `patch_path`: the patch file's stem names
/// the output, the DAT game ref is dropped (the patched ROM is a distinct
/// artifact), and the unit index is unchanged. Trim padding is dropped: a
/// patch may change the ROM's size, so the padded form no longer applies.
fn patched_variant(plan: &UnitPlan, patch_path: PathBuf) -> UnitPlan {
    let basename = patch_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("output")
        .to_string();
    UnitPlan {
        index: plan.index,
        source: plan.source.clone(),
        source_ext: plan.source_ext.clone(),
        action: plan.action.clone(),
        tokens: TemplateTokens {
            basename,
            ..plan.tokens.clone()
        },
        input_subdir: plan.input_subdir.clone(),
        game: None,
        header: plan.header.clone(),
        strip: plan.strip,
        trim: None,
        pad: 0,
        pad_fill: 0,
        patch: Some(patch_path),
        staging_error: None,
        output_error: None,
        match_error: None,
        member: plan.member.as_ref().map(|member| super::plan::MemberFacts {
            basis: member.basis.clone(),
            digests: None,
            crc: member.crc,
        }),
        desired: plan.desired.clone(),
        decision: Decision::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::super::plan::Action;
    use super::super::trim;
    use super::*;
    use crate::runner::RecordingProgress;
    use crate::util::CancelToken;
    use std::path::Path;

    fn plan(index: usize) -> UnitPlan {
        UnitPlan {
            index,
            source: PathBuf::from("Star Fox.sfc"),
            source_ext: "sfc".to_string(),
            action: Action::Copy,
            tokens: TemplateTokens::new(None, Path::new("Star Fox.sfc"), "sfc"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0,
            member: None,
            patch: None,
            desired: None,
            decision: Decision::Keep,
            staging_error: None,
            output_error: None,
            match_error: None,
        }
    }

    /// A patch directory with the given files plus the index built over it.
    async fn index_with(files: &[(&str, &[u8])]) -> (tempfile::TempDir, PatchIndex) {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, bytes) in files {
            std::fs::write(dir.path().join(name), bytes).expect("write patch file");
        }
        let index = PatchIndex::build(
            &[dir.path().to_path_buf()],
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .expect("build index");
        (dir, index)
    }

    #[test]
    fn a_missing_patch_path_is_an_invalid_argument() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope");
        let present = dir.path().join("here.ips");
        std::fs::write(&present, b"PATCH").expect("write");
        // An existing path passes; the missing one is named.
        PatchIndex::validate_paths(std::slice::from_ref(&present)).expect("existing path passes");
        let err = PatchIndex::validate_paths(&[present, missing.clone()])
            .expect_err("a missing patch path must fail");
        assert_eq!(
            err.to_string(),
            format!("patch path not found: {}", missing.display())
        );
    }

    #[tokio::test]
    async fn build_indexes_by_source_crc() {
        let (_dir, index) = index_with(&[
            ("deadbeef-a.ips", b"" as &[u8]),
            ("deadbeef-b.ips", b"PATCH"),
            ("cafe1234.ips", b""),
            ("nocrc.ips", b""),
        ])
        .await;
        assert_eq!(index.by_crc.get(&0xdead_beef).map(Vec::len), Some(2));
        assert_eq!(index.by_crc.get(&0xcafe_1234).map(Vec::len), Some(1));
        assert!(!index.by_crc.contains_key(&0x0000_0001));
        assert!(!index.is_empty());

        assert!(
            PatchIndex::build(&[], &RecordingProgress::default(), &CancelToken::new())
                .await
                .expect("build empty index")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn expand_pushes_one_variant_per_patch() {
        let (_dir, index) =
            index_with(&[("deadbeef-a.ips", b"" as &[u8]), ("deadbeef-b.ips", b"")]).await;
        let mut base = plan(0);
        // Trim padding rides on the base plan; a patch may change the ROM's
        // size, so variants must drop it.
        base.trim = Some(trim::TrimInfo { padded_size: 4096 });
        base.pad = 8;
        let mut plans = vec![base];
        expand_patched(
            &mut plans,
            &index,
            &|index: usize| (index == 0).then_some(0xdead_beef),
            false,
        );

        assert_eq!(plans.len(), 3);
        // The original stays first and untouched; variants are appended.
        assert_eq!(plans[0].decision, Decision::Keep);
        assert_eq!(plans[0].patch, None);
        assert_eq!(plans[0].tokens.basename, "Star Fox");
        assert_eq!(plans[1].decision, Decision::Keep);
        assert!(plans[0].trim.is_some() && plans[0].pad > 0);
        for variant in &plans[1..] {
            assert_eq!(variant.index, 0);
            assert!(variant.patch.is_some());
            assert!(variant.game.is_none());
            assert_eq!(variant.action, Action::Copy);
            assert!(variant.trim.is_none());
            assert!(variant.pad == 0);
        }
        let mut basenames: Vec<&str> = plans[1..]
            .iter()
            .map(|variant| variant.tokens.basename.as_str())
            .collect();
        basenames.sort_unstable();
        assert_eq!(basenames, ["deadbeef-a", "deadbeef-b"]);
    }

    #[tokio::test]
    async fn patch_only_skips_the_original() {
        let (_dir, index) =
            index_with(&[("deadbeef-a.ips", b"" as &[u8]), ("deadbeef-b.ips", b"")]).await;
        let mut plans = vec![plan(0)];
        expand_patched(&mut plans, &index, &|_| Some(0xdead_beef), true);

        assert_eq!(plans.len(), 3);
        assert_eq!(plans[0].decision, Decision::Skip("patch-only".to_string()));
        assert!(plans[1..].iter().all(|p| p.decision == Decision::Keep));
    }

    #[tokio::test]
    async fn unmatched_units_are_skipped_under_patch_only() {
        let (_dir, index) = index_with(&[("deadbeef-a.ips", b"" as &[u8])]).await;
        // A CRC with no patch, and no CRC at all: both unmatched.
        let mut plans = vec![plan(0), plan(1)];
        expand_patched(
            &mut plans,
            &index,
            &|index: usize| (index == 0).then_some(0x1234_5678),
            true,
        );

        assert_eq!(plans.len(), 2);
        assert!(
            plans
                .iter()
                .all(|p| p.decision == Decision::Skip("patch-only".to_string()))
        );
    }

    #[tokio::test]
    async fn no_crc_leaves_plans_untouched() {
        let (_dir, index) = index_with(&[("deadbeef-a.ips", b"" as &[u8])]).await;
        let mut plans = vec![plan(0)];
        expand_patched(&mut plans, &index, &|_| None, false);

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].decision, Decision::Keep);
        assert_eq!(plans[0].patch, None);
    }
}
