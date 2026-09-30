//! GameCube disc verification.
//!
//! Fast mode (default) checks the RVZ container's stored SHA-1 hashes; it is a
//! no-op for plain ISO/GCM input, which carries none. `--full` additionally
//! validates the FST geometry and computes a whole-disc SHA-1. GameCube discs
//! have no built-in integrity hashes, so that digest is informational (useful
//! for matching against external DAT/Redump databases), never a pass/fail.

use crate::nintendo::disc::input::open_disc_input;
use crate::nintendo::disc::rvz::verify::RvzStructuralVerify;
use crate::nintendo::dol::models::boot_bin::GcBootBin;
use crate::util::{CancelToken, Cancelled, ProgressReporter};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Options for [`verify_dol`].
#[derive(Debug, Clone, Default)]
pub struct DolVerifyOptions {
    pub full: bool,
}

/// Result of verifying a GameCube disc image.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct DolVerifyResult {
    pub game_id: String,
    /// Present only for `.rvz` input.
    pub rvz_structure: Option<RvzStructuralVerify>,
    /// Why the RVZ container could not be structurally checked (a broken
    /// table, for example): `None` for non-RVZ input, healthy containers,
    /// and containers whose stored hashes alone fail (those fail through
    /// `rvz_structure` instead).
    pub rvz_note: Option<String>,
    /// Present only with `--full`.
    pub structural: Option<DolStructuralReport>,
    /// Whole-disc SHA-1 (hex), informational, `--full` only.
    pub disc_sha1: Option<String>,
    pub ok: bool,
}

/// FST geometry checked by the `--full` pass, plus any notes about what
/// was found.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct DolStructuralReport {
    pub fst_offset: u32,
    pub fst_size: u32,
    pub fst_within_bounds: bool,
    pub notes: Vec<String>,
}

/// Verify the GameCube disc image at `path`: game id and, for `.rvz`
/// input, its structure, plus FST geometry and the whole-disc SHA-1
/// when `options.full` is set.
pub fn verify_dol(
    path: &Path,
    options: &DolVerifyOptions,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<DolVerifyResult> {
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let (rvz_structure, rvz_note) =
        crate::nintendo::disc::rvz::verify::verify_rvz_structure_reported(path, cancel)?;
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }

    // A structurally broken container usually cannot be opened as a disc
    // at all (a tail-cut RVZ loses its closing group table), so the
    // verify fails on the note alone instead of aborting the run.
    if let Some(note) = rvz_note {
        return Ok(DolVerifyResult {
            game_id: String::new(),
            rvz_structure: None,
            rvz_note: Some(note),
            structural: None,
            disc_sha1: None,
            ok: false,
        });
    }

    // A container whose stored hashes fail fails the same checks when
    // opened as a disc, so report the per-hash structure instead of
    // erroring the whole verify on the open.
    if rvz_structure.as_ref().is_some_and(|s| !s.ok()) {
        return Ok(DolVerifyResult {
            game_id: String::new(),
            rvz_structure,
            rvz_note: None,
            structural: None,
            disc_sha1: None,
            ok: false,
        });
    }

    let mut reader =
        open_disc_input(path).with_context(|| format!("dol verify: open {}", path.display()))?;
    let boot = GcBootBin::read(&mut reader).context("dol verify: parse boot.bin")?;
    let game_id = boot.game_id.clone();

    let mut structural = None;
    let mut disc_sha1 = None;

    if options.full {
        let iso_size = reader.logical_size();
        let fst_end = boot.fst_offset as u64 + boot.fst_size as u64;
        let fst_within_bounds = boot.fst_size > 0 && boot.fst_offset > 0 && fst_end <= iso_size;
        let mut notes = vec![
            "GameCube discs carry no built-in integrity hashes; the whole-disc SHA-1 is informational."
                .to_string(),
        ];
        if !fst_within_bounds {
            notes.push("FST geometry is missing or out of bounds.".to_string());
        }
        structural = Some(DolStructuralReport {
            fst_offset: boot.fst_offset,
            fst_size: boot.fst_size,
            fst_within_bounds,
            notes,
        });

        reader
            .seek(SeekFrom::Start(0))
            .context("dol verify: rewind for digest")?;
        let digest = sha1_stream(&mut reader, iso_size, progress, cancel)?;
        disc_sha1 = Some(hex::encode(digest));
    }

    let ok = rvz_structure.as_ref().map(|s| s.ok()).unwrap_or(true)
        && structural
            .as_ref()
            .map(|s| s.fst_within_bounds)
            .unwrap_or(true);

    Ok(DolVerifyResult {
        game_id,
        rvz_structure,
        rvz_note: None,
        structural,
        disc_sha1,
        ok,
    })
}

fn sha1_stream<R: Read>(
    reader: &mut R,
    total: u64,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<[u8; 20]> {
    progress.start(total, "Verifying GameCube disc");
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        progress.inc(n as u64);
    }
    progress.finish();
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::disc::rvz::{RvzCompressOptions, compress_disc};
    use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
    use crate::util::NoProgress;

    #[tokio::test]
    async fn rvz_fast_verify_passes_structural_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        std::fs::write(&iso, make_fake_gamecube_iso(4 * 1024 * 1024 + 0x123)).unwrap();
        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let fast = verify_dol(
            &rvz,
            &DolVerifyOptions { full: false },
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        let structural = fast.rvz_structure.expect("rvz input has structural hashes");
        assert!(structural.file_head_hash_ok);
        assert!(structural.disc_hash_ok);
        assert!(fast.ok);
        assert!(fast.disc_sha1.is_none());
    }

    #[tokio::test]
    async fn full_verify_emits_disc_digest() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        std::fs::write(&iso, make_fake_gamecube_iso(4 * 1024 * 1024)).unwrap();
        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let full = verify_dol(
            &rvz,
            &DolVerifyOptions { full: true },
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        assert!(full.structural.is_some());
        assert_eq!(full.disc_sha1.as_ref().map(|s| s.len()), Some(40));
    }

    #[test]
    fn plain_iso_fast_verify_has_no_structural_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, make_fake_gamecube_iso(2 * 1024 * 1024)).unwrap();
        let fast = verify_dol(
            &iso,
            &DolVerifyOptions { full: false },
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        assert!(fast.rvz_structure.is_none());
        assert!(fast.ok);
    }

    /// An environment failure (here: the input cannot be opened at all)
    /// means nobody checked the container: it must surface as an error
    /// for the fail-closed Unverified classification, not as a failed
    /// verify result that reads the container as broken.
    #[test]
    fn missing_input_fails_as_error_not_broken_container() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.rvz");
        let result = verify_dol(
            &missing,
            &DolVerifyOptions { full: false },
            &NoProgress,
            &CancelToken::new(),
        );
        assert!(result.is_err());
    }

    /// A container whose stored disc hash fails must produce the failed
    /// per-hash report instead of erroring the whole verify: opening it
    /// as a disc fails the same hash.
    #[tokio::test]
    async fn failed_disc_hash_reports_instead_of_open_error() {
        use crate::nintendo::disc::rvz::format::WIA_FILE_HEAD_SIZE;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        std::fs::write(&iso, make_fake_gamecube_iso(2 * 1024 * 1024)).unwrap();
        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        // Flip a byte inside the disc struct's embedded boot head: the
        // disc hash no longer matches while the file head stays valid.
        let mut bytes = std::fs::read(&rvz).unwrap();
        bytes[WIA_FILE_HEAD_SIZE + 0x21] ^= 0xFF;
        std::fs::write(&rvz, &bytes).unwrap();

        let result = verify_dol(
            &rvz,
            &DolVerifyOptions { full: false },
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("a failed stored hash must report, not error");
        assert!(!result.ok);
        assert!(result.rvz_note.is_none());
        let structure = result
            .rvz_structure
            .expect("rvz input has structural hashes");
        assert!(structure.file_head_hash_ok);
        assert!(!structure.disc_hash_ok);
    }

    /// A structurally truncated RVZ fails the fast verify with
    /// `ok == false` and the reason in `rvz_note`.
    #[test]
    fn truncated_rvz_fails_fast_verify_with_note() {
        use crate::nintendo::disc::rvz::format::RvzGroup;
        use crate::nintendo::disc::rvz::verify::test_support::build_rvz;

        let dir = tempfile::tempdir().unwrap();
        // A middle group claims 8 bytes at 16 KiB, far past the tables,
        // and the last entry is a zero sentinel, so only a bound over
        // every group catches the truncation. The note path returns
        // before any disc input is opened, so the container head bytes
        // need not describe a real disc.
        let file = build_rvz(
            [0u8; 128],
            &[
                RvzGroup::new_compressed(0, 8, 0),
                RvzGroup::new_compressed(0x4000 / 4, 8, 0),
                RvzGroup {
                    data_off4: 0,
                    data_size: 0,
                    rvz_packed_size: 0,
                },
            ],
            &[],
        );
        let rvz = dir.path().join("game.rvz");
        std::fs::write(&rvz, file).unwrap();

        let result = verify_dol(
            &rvz,
            &DolVerifyOptions { full: false },
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();
        assert!(!result.ok);
        assert!(result.rvz_structure.is_none());
        assert!(
            result
                .rvz_note
                .as_deref()
                .is_some_and(|note| note.contains("truncated"))
        );
    }
}
