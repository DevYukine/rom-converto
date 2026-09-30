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

/// Result of running the integrity check. `Unverified` carries the reason
/// the target was never actually checked (no integrity check for this
/// output format, missing NX keys, or a check that could not run), so the
/// caller can tell "kept because it verified" from "kept because nobody
/// could check", and a `verify_after` row can name the cause.
#[derive(Clone, Debug, PartialEq)]
pub enum VerifyOutcome {
    Valid,
    Invalid,
    Unverified(String),
}

/// True when any error in `err`'s chain means "nobody checked this output":
/// cancellation, unusable key material (a missing or unreadable keyset is
/// this machine's problem, not the container's), worker-pool infrastructure
/// trouble, and io errors that say "we could not read right now"
/// (`NotFound`, `PermissionDenied`, `Interrupted`, `OutOfMemory`,
/// `TimedOut`) rather than "the data is wrong". Everything else (parse,
/// decode, truncation, and hash mismatches) is the container itself being
/// broken and reads as a failed verification.
pub(crate) fn unverifiable(err: &(dyn std::error::Error + 'static)) -> bool {
    use crate::cso::error::CsoError;
    use crate::disc::chd::error::ChdError;
    use crate::nintendo::disc::rvz::error::RvzError;
    use crate::nintendo::nx::NxError;

    let mut err = err;
    loop {
        // The cancellation itself, whatever wraps it.
        if err.is::<Cancelled>()
            || matches!(err.downcast_ref::<NxError>(), Some(NxError::Cancelled(_)))
            || matches!(err.downcast_ref::<ChdError>(), Some(ChdError::Cancelled(_)))
            || matches!(err.downcast_ref::<CsoError>(), Some(CsoError::Cancelled(_)))
            || matches!(err.downcast_ref::<RvzError>(), Some(RvzError::Cancelled(_)))
        {
            return true;
        }
        // io errors that say "we could not read right now": the
        // environment, not the data. The container enums' io variants are
        // matched directly (the chain walk would also reach them); the binrw
        // parse variants need it, because `binrw::Error` has no `source()`:
        // its `Io` case is a plain read failure while every other case is
        // the data being wrong, and a field-level failure surfaces as
        // `Error::Backtrace`, which `root_cause()` unwraps.
        let io = err
            .downcast_ref::<std::io::Error>()
            .or_else(|| match err.downcast_ref::<NxError>() {
                Some(NxError::IoError(io)) => Some(io),
                Some(NxError::BinRwError(e)) => binrw_io(e),
                _ => None,
            })
            .or_else(|| match err.downcast_ref::<ChdError>() {
                Some(ChdError::IoError(io)) => Some(io),
                Some(ChdError::BinRWError(e)) => binrw_io(e),
                _ => None,
            })
            .or_else(|| match err.downcast_ref::<CsoError>() {
                Some(CsoError::IoError(io)) => Some(io),
                Some(CsoError::BinRWError(e)) => binrw_io(e),
                _ => None,
            })
            .or_else(|| match err.downcast_ref::<RvzError>() {
                Some(RvzError::IoError(io)) => Some(io),
                Some(RvzError::BinRWError(e)) => binrw_io(e),
                _ => None,
            });
        if let Some(io) = io
            && (io.get_ref().is_some_and(|inner| inner.is::<Cancelled>()) || env_io_error(io))
        {
            return true;
        }
        // Unusable key material is this machine's problem, not the
        // container's.
        if let Some(nx) = err.downcast_ref::<NxError>()
            && matches!(
                nx,
                NxError::KeyfileMissing(_)
                    | NxError::KeyfileParse { .. }
                    | NxError::MissingKey { .. }
                    | NxError::InvalidKeyHex { .. }
            )
        {
            return true;
        }
        // Infrastructure, not data: a panicked/dropped worker or a closed
        // worker pool means the check never ran to a verdict.
        if err.is::<tokio::task::JoinError>()
            || err.is::<crate::util::worker_pool::PoolChannelClosed>()
            || matches!(
                err.downcast_ref::<NxError>(),
                Some(NxError::JoinError(_) | NxError::WorkerPoolClosed(_))
            )
            || matches!(
                err.downcast_ref::<ChdError>(),
                Some(
                    ChdError::JoinError(_)
                        | ChdError::WorkerPoolClosed(_)
                        | ChdError::WorkerPoolPanic
                )
            )
            || matches!(
                err.downcast_ref::<CsoError>(),
                Some(
                    CsoError::JoinError(_)
                        | CsoError::WorkerPoolClosed(_)
                        | CsoError::WorkerPoolPanic
                )
            )
            || matches!(
                err.downcast_ref::<RvzError>(),
                Some(RvzError::JoinError(_) | RvzError::WorkerPoolClosed(_))
            )
        {
            return true;
        }
        err = match err.source() {
            Some(source) => source,
            None => return false,
        };
    }
}

/// The io error behind a binrw parse failure: the failure's own `Io` case,
/// or the cause binrw wrapped in an `Error::Backtrace` when a struct field
/// failed (`Backtrace` implements no `source()`), so the plain chain walk
/// never reaches it and [`Error::root_cause`] does.
fn binrw_io(err: &binrw::Error) -> Option<&std::io::Error> {
    match err.root_cause() {
        binrw::Error::Io(io) => Some(io),
        _ => None,
    }
}

/// The io error kinds that say "could not read right now": the
/// environment, not the data. `Other` and `EIO` are deliberately not here:
/// from these decoders they surface data-side breakage (a stream that went
/// bad mid-read), not an environment that refused to serve the file.
fn env_io_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::OutOfMemory
            | std::io::ErrorKind::TimedOut
    )
}

/// Classifies an existing output's integrity check for
/// `--on-conflict overwrite-invalid`: a passed check is `Valid` (keep the
/// output), a failed one `Invalid` (rewrite it), and an error the fail-closed
/// [`unverifiable`] filter lets through (cancellation, unusable keys,
/// worker-pool trouble, environment io) becomes `Unverified` (keep it, naming
/// the cause, since the check never produced a verdict). Cancellation is
/// re-raised as an error after the match. Compare
/// [`crate::runner::ops`]'s comparison verify, which maps the same
/// classes onto a report instead of a keep/rewrite decision.
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
    let outcome = match target {
        OutputVerify::Chd => {
            match verify_chd(progress, path.to_path_buf(), None, false, cancel.clone()).await {
                Ok(()) => VerifyOutcome::Valid,
                Err(e) if unverifiable(&e) => {
                    log::debug!(
                        "overwrite-invalid: chd verify could not run for {}, keeping existing output: {e}",
                        path.display()
                    );
                    VerifyOutcome::Unverified(e.to_string())
                }
                Err(e) => {
                    log::debug!(
                        "overwrite-invalid: chd verify failed for {}, rewriting output: {e}",
                        path.display()
                    );
                    VerifyOutcome::Invalid
                }
            }
        }
        OutputVerify::Cso => {
            match verify_cso(progress, path.to_path_buf(), true, cancel.clone()).await {
                Ok(()) => VerifyOutcome::Valid,
                Err(e) if unverifiable(&e) => {
                    log::debug!(
                        "overwrite-invalid: cso verify could not run for {}, keeping existing output: {e}",
                        path.display()
                    );
                    VerifyOutcome::Unverified(e.to_string())
                }
                Err(e) => {
                    log::debug!(
                        "overwrite-invalid: cso verify failed for {}, rewriting output: {e}",
                        path.display()
                    );
                    VerifyOutcome::Invalid
                }
            }
        }
        OutputVerify::Rvz => {
            let moved = path.to_path_buf();
            let cancel = cancel.clone();
            match tokio::task::spawn_blocking(move || verify_rvz_structure(&moved, &cancel)).await {
                Ok(Ok(structure)) if structure.ok() => VerifyOutcome::Valid,
                Ok(Ok(_)) => VerifyOutcome::Invalid,
                Ok(Err(e)) if unverifiable(&e) => {
                    log::debug!(
                        "overwrite-invalid: rvz verify could not run for {}, keeping existing output: {e}",
                        path.display()
                    );
                    VerifyOutcome::Unverified(e.to_string())
                }
                Ok(Err(e)) => {
                    log::debug!(
                        "overwrite-invalid: rvz verify failed for {}, rewriting output: {e}",
                        path.display()
                    );
                    VerifyOutcome::Invalid
                }
                // The blocking task never produced a verdict: infrastructure,
                // not data, like the Chd/Cso worker pools.
                Err(e) => {
                    log::debug!(
                        "overwrite-invalid: rvz verify task failed for {}, keeping existing output: {e}",
                        path.display()
                    );
                    VerifyOutcome::Unverified(e.to_string())
                }
            }
        }
        OutputVerify::Nx(keys) => {
            if keys.header_key.is_none() {
                log::debug!(
                    "overwrite-invalid: nx keys unavailable for {}, keeping existing output",
                    path.display()
                );
                VerifyOutcome::Unverified("keyset has no header key".to_string())
            } else {
                match verify_container_async(path.to_path_buf(), *keys, progress, cancel.clone())
                    .await
                {
                    Ok(result) if result.ok => VerifyOutcome::Valid,
                    Ok(_) => VerifyOutcome::Invalid,
                    Err(e) if unverifiable(&e) => {
                        log::debug!(
                            "overwrite-invalid: nx verify could not run for {}, keeping existing output: {e}",
                            path.display()
                        );
                        VerifyOutcome::Unverified(e.to_string())
                    }
                    Err(e) => {
                        log::debug!(
                            "overwrite-invalid: nx verify failed for {}, rewriting output: {e}",
                            path.display()
                        );
                        VerifyOutcome::Invalid
                    }
                }
            }
        }
        OutputVerify::None => {
            log::debug!(
                "overwrite-invalid: no integrity check for {}, keeping existing output",
                path.display()
            );
            VerifyOutcome::Unverified("no integrity check for this output format".to_string())
        }
    };
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    Ok(outcome)
}

/// Format label for the verify cache. `None` means the target is not cached:
/// an output with no integrity check, or an NX container with no usable keyset
/// (its verify is skipped and its output kept, so caching it would be wrong).
fn verify_label(target: &OutputVerify) -> Option<&'static str> {
    match target {
        OutputVerify::Chd => Some("chd"),
        OutputVerify::Cso => Some("cso"),
        // "rvz2" keys verdicts produced by the current RVZ checks; entries
        // under other labels are never read.
        OutputVerify::Rvz => Some("rvz2"),
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
        assert_eq!(verify_label(&OutputVerify::Rvz), Some("rvz2"));
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
        assert!(matches!(outcome, VerifyOutcome::Unverified(_)));
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
        assert!(matches!(outcome, VerifyOutcome::Unverified(_)));
    }

    /// The unverifiable classes: cancellation, unusable keys, worker-pool
    /// infrastructure trouble, and environment io errors keep the existing
    /// output; container corruption and plain data io errors do not.
    #[tokio::test]
    async fn unverifiable_splits_environment_from_container_errors() {
        use crate::cso::error::CsoError;
        use crate::nintendo::nx::NxError;
        use crate::util::worker_pool::PoolChannelClosed;
        use std::io::ErrorKind;

        fn is_unverifiable(err: &NxError) -> bool {
            unverifiable(err)
        }

        let joined = tokio::task::spawn(async {});
        joined.abort();
        let join_err = joined.await.expect_err("aborted task yields a JoinError");
        assert!(is_unverifiable(&NxError::JoinError(join_err)));
        assert!(is_unverifiable(&NxError::WorkerPoolClosed(
            PoolChannelClosed
        )));
        // A binrw parse failure whose cause is a plain io read failure is
        // environment, not data; any other binrw failure is the container.
        assert!(is_unverifiable(&NxError::BinRwError(binrw::Error::Io(
            std::io::Error::from(ErrorKind::NotFound)
        ))));
        assert!(!is_unverifiable(&NxError::BinRwError(
            binrw::Error::NoVariantMatch { pos: 0 }
        )));
        // binrw wraps a failing struct field in an `Error::Backtrace`,
        // which implements no `source()`: only `root_cause()` reaches the
        // wrapped io error.
        use binrw::error::ContextExt;
        assert!(is_unverifiable(&NxError::BinRwError(
            binrw::Error::Io(std::io::Error::from(ErrorKind::NotFound))
                .with_context(binrw::error::BacktraceFrame::Message("field".into()),),
        )));
        // The CSO classifications mirror the NX ones: io-through-binrw and
        // the worker-pool closure are environment/infrastructure; a parse
        // that matched no variant and a data io error are the container.
        assert!(unverifiable(&CsoError::BinRWError(binrw::Error::Io(
            std::io::Error::from(ErrorKind::NotFound)
        ))));
        assert!(!unverifiable(&CsoError::BinRWError(
            binrw::Error::NoVariantMatch { pos: 0 }
        )));
        assert!(!unverifiable(&CsoError::from(std::io::Error::from(
            ErrorKind::InvalidData
        ))));
        assert!(unverifiable(&CsoError::WorkerPoolClosed(PoolChannelClosed)));
        assert!(unverifiable(&CsoError::from(Cancelled)));
        // The RVZ worker-pool closure is infrastructure, not data.
        assert!(unverifiable(
            &crate::nintendo::disc::rvz::error::RvzError::WorkerPoolClosed(PoolChannelClosed)
        ));
        // Key/environment trouble and cancellation are never the container's
        // fault.
        assert!(is_unverifiable(&NxError::KeyfileMissing(vec![
            "prod.keys".into()
        ])));
        assert!(is_unverifiable(&NxError::from(Cancelled)));
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::PermissionDenied,
            ErrorKind::Interrupted,
            ErrorKind::OutOfMemory,
            ErrorKind::TimedOut,
        ] {
            assert!(
                is_unverifiable(&NxError::from(std::io::Error::from(kind))),
                "{kind:?} is environment, not container"
            );
        }
        // Container trouble and plain data io errors rewrite the output.
        assert!(!is_unverifiable(&NxError::from(std::io::Error::from(
            ErrorKind::InvalidData
        ))));
        assert!(!is_unverifiable(&NxError::InvalidNcaHeader));
        assert!(!is_unverifiable(&NxError::IncompleteSection));
    }

    /// A decode failure from the container verify is the container being
    /// broken: a truncated NSZ must be rewritten, not kept unverified.
    #[tokio::test]
    async fn nx_verify_decode_failure_is_invalid() {
        use crate::nintendo::nx::KeySet;
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.nsz");
        // PFS0 magic, then a header cut off mid-read: parse fails.
        std::fs::write(&path, b"PFS0\x01\x00\x00").unwrap();
        let keys = KeySet {
            header_key: Some([0u8; 32]),
            ..Default::default()
        };
        let outcome = verify_existing_output(
            &NoProgress,
            &path,
            OutputVerify::Nx(Box::new(keys)),
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, VerifyOutcome::Invalid);
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
