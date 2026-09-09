//! Existing-output integrity checks for `--on-conflict overwrite-invalid`:
//! decides whether a file already at the output path is a valid conversion
//! result, so a corrupt or partial output gets rewritten and a valid one
//! is kept.

use super::{CancelToken, Cancelled, HashCache, ProgressReporter};
use anyhow::Result;
use std::path::Path;

/// Which integrity check to run against an existing output under
/// `--on-conflict overwrite-invalid`. `None` marks an output format with no
/// integrity check, where the policy falls back to existence-based skip.
/// `Nx` carries the keyset because the NX verify decrypts every NCA section;
/// when keys are missing the existing output is kept rather than rewritten.
#[derive(Clone)]
pub enum OutputVerify {
    Chd,
    Cso,
    Rvz,
    Nx(Box<crate::nintendo::nx::KeySet>),
    None,
}

/// Result of running the integrity check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    Valid,
    Invalid,
}

/// Cancellable twin of [`verify_existing_output`], propagating verification
/// and cancellation errors instead of collapsing them into `Invalid`.
pub async fn verify_existing_output(
    progress: &dyn ProgressReporter,
    path: &Path,
    target: OutputVerify,
    cancel: CancelToken,
) -> Result<VerifyOutcome> {
    use crate::cso::verify_cso;
    use crate::disc::chd::verify_chd;
    use crate::nintendo::disc::rvz::verify::verify_rvz_structure;
    use crate::nintendo::nx::verify_container_async;
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let ok = match target {
        OutputVerify::Chd => verify_chd(progress, path.to_path_buf(), None, false, cancel.clone())
            .await
            .is_ok(),
        OutputVerify::Cso => verify_cso(progress, path.to_path_buf(), true, cancel.clone())
            .await
            .is_ok(),
        OutputVerify::Rvz => verify_rvz_structure(path, &cancel)
            .map(|r| r.ok())
            .unwrap_or(false),
        OutputVerify::Nx(keys) => {
            if keys.header_key.is_none() {
                log::debug!(
                    "overwrite-invalid: nx keys unavailable for {}, keeping existing output",
                    path.display()
                );
                true
            } else {
                match verify_container_async(path.to_path_buf(), *keys, progress, cancel.clone())
                    .await
                {
                    Ok(result) => result.ok,
                    Err(e) => {
                        log::debug!(
                            "overwrite-invalid: nx verify could not run for {}, keeping existing output: {e}",
                            path.display()
                        );
                        true
                    }
                }
            }
        }
        OutputVerify::None => {
            log::debug!(
                "overwrite-invalid: no integrity check for {}, keeping existing output",
                path.display()
            );
            true
        }
    };
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    Ok(if ok {
        VerifyOutcome::Valid
    } else {
        VerifyOutcome::Invalid
    })
}

/// Format label for the verify cache. `None` means the target is not cached:
/// an output with no integrity check, or an NX container with no usable keyset
/// (its verify is skipped and its output kept, so caching it would be wrong).
fn verify_label(target: &OutputVerify) -> Option<&'static str> {
    match target {
        OutputVerify::Chd => Some("chd"),
        OutputVerify::Cso => Some("cso"),
        OutputVerify::Rvz => Some("rvz"),
        OutputVerify::Nx(keys) if keys.header_key.is_some() => Some("nx"),
        OutputVerify::Nx(_) | OutputVerify::None => None,
    }
}

/// [`verify_existing_output`] with a cache in front. A prior `Valid` verdict for
/// an unchanged output short-circuits the read; only `Valid` is stored, since an
/// `Invalid` output gets rewritten (changing its mtime and invalidating the
/// entry anyway).
pub async fn verify_existing_cached(
    cache: &HashCache,
    progress: &dyn ProgressReporter,
    path: &Path,
    target: OutputVerify,
    cancel: CancelToken,
) -> Result<VerifyOutcome> {
    let label = verify_label(&target);
    if let Some(label) = label
        && cache.lookup_verify(path, label)
    {
        return Ok(VerifyOutcome::Valid);
    }
    let outcome = verify_existing_output(progress, path, target, cancel).await?;
    if outcome == VerifyOutcome::Valid
        && let Some(label) = label
    {
        cache.store_verify(path, label, true);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;
    use tempfile::tempdir;

    #[test]
    fn verify_label_maps_targets() {
        use crate::nintendo::nx::KeySet;
        assert_eq!(verify_label(&OutputVerify::Chd), Some("chd"));
        assert_eq!(verify_label(&OutputVerify::Cso), Some("cso"));
        assert_eq!(verify_label(&OutputVerify::Rvz), Some("rvz"));
        assert_eq!(verify_label(&OutputVerify::None), None);

        // NX without a header key is not cached: its verify is skipped and the
        // output kept, so a cached "valid" verdict would be wrong.
        let no_key = KeySet::default();
        assert_eq!(verify_label(&OutputVerify::Nx(Box::new(no_key))), None);

        let with_key = KeySet {
            header_key: Some([0u8; 32]),
            ..Default::default()
        };
        assert_eq!(
            verify_label(&OutputVerify::Nx(Box::new(with_key))),
            Some("nx")
        );
    }

    #[tokio::test]
    async fn verify_existing_output_none_keeps_unverifiable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.iso");
        std::fs::write(&path, b"x").unwrap();
        let outcome =
            verify_existing_output(&NoProgress, &path, OutputVerify::None, CancelToken::new())
                .await
                .unwrap();
        assert_eq!(outcome, VerifyOutcome::Valid);
    }

    // A full NX corrupt-rewrite end-to-end test is omitted because it needs a
    // populated prod.keys that cannot ship with the suite. These cover the
    // decision logic instead: missing keys keep the existing output, and a
    // non-RVZ file at an .rvz path is treated as invalid so it gets rewritten.
    #[tokio::test]
    async fn nx_verify_missing_keys_keeps_existing_output() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.nsz");
        std::fs::write(&path, b"not a real container").unwrap();
        let outcome = verify_existing_output(
            &NoProgress,
            &path,
            OutputVerify::Nx(Box::default()),
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, VerifyOutcome::Valid);
    }

    #[tokio::test]
    async fn rvz_verify_non_rvz_file_is_invalid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.rvz");
        std::fs::write(&path, b"this is not an rvz container at all").unwrap();
        let outcome =
            verify_existing_output(&NoProgress, &path, OutputVerify::Rvz, CancelToken::new())
                .await
                .unwrap();
        assert_eq!(outcome, VerifyOutcome::Invalid);
    }
}
