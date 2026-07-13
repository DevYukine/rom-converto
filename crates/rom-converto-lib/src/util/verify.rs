//! Existing-output integrity checks for `--on-conflict overwrite-invalid`:
//! decides whether a file already at the output path is a valid conversion
//! result, so a corrupt or partial output gets rewritten and a valid one
//! is kept.

use super::{CancelToken, ProgressReporter};
use anyhow::Result;
use std::path::Path;

/// Which integrity check to run against an existing output under
/// `--on-conflict overwrite-invalid`. `None` marks an output format with no
/// integrity check, where the policy falls back to existence-based skip.
/// `Nx` carries the keyset because the NX verify decrypts every NCA section;
/// when keys are missing the existing output is kept rather than rewritten.
pub enum OutputVerify {
    Chd,
    Cso,
    Rvz,
    Nx(Box<crate::nintendo::nx::KeySet>),
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    Valid,
    Invalid,
}

/// Run the format's read-only integrity check on an existing output. Any
/// verification failure, including an output that cannot be read or decoded,
/// is reported as `Invalid` so the caller rewrites it.
pub async fn verify_existing_output(
    progress: &dyn ProgressReporter,
    path: &Path,
    target: OutputVerify,
) -> VerifyOutcome {
    verify_existing_output_cancellable(progress, path, target, CancelToken::new())
        .await
        .unwrap_or(VerifyOutcome::Invalid)
}

pub async fn verify_existing_output_cancellable(
    progress: &dyn ProgressReporter,
    path: &Path,
    target: OutputVerify,
    cancel: CancelToken,
) -> Result<VerifyOutcome> {
    use crate::chd::verify_chd_cancellable;
    use crate::cso::verify_cso_cancellable;
    use crate::nintendo::nx::verify_container_async_cancellable;
    use crate::nintendo::rvz::verify::verify_rvz_structure_cancellable;
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let ok = match target {
        OutputVerify::Chd => {
            verify_chd_cancellable(progress, path.to_path_buf(), None, false, cancel.clone())
                .await
                .is_ok()
        }
        OutputVerify::Cso => {
            verify_cso_cancellable(progress, path.to_path_buf(), true, cancel.clone())
                .await
                .is_ok()
        }
        OutputVerify::Rvz => verify_rvz_structure_cancellable(path, &cancel)
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
                match verify_container_async_cancellable(
                    path.to_path_buf(),
                    *keys,
                    progress,
                    cancel.clone(),
                )
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
        anyhow::bail!("cancelled");
    }
    Ok(if ok {
        VerifyOutcome::Valid
    } else {
        VerifyOutcome::Invalid
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;
    use tempfile::tempdir;

    #[tokio::test]
    async fn verify_existing_output_none_keeps_unverifiable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.iso");
        std::fs::write(&path, b"x").unwrap();
        let outcome = verify_existing_output(&NoProgress, &path, OutputVerify::None).await;
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
        let outcome =
            verify_existing_output(&NoProgress, &path, OutputVerify::Nx(Box::default())).await;
        assert_eq!(outcome, VerifyOutcome::Valid);
    }

    #[tokio::test]
    async fn rvz_verify_non_rvz_file_is_invalid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.rvz");
        std::fs::write(&path, b"this is not an rvz container at all").unwrap();
        let outcome = verify_existing_output(&NoProgress, &path, OutputVerify::Rvz).await;
        assert_eq!(outcome, VerifyOutcome::Invalid);
    }
}
