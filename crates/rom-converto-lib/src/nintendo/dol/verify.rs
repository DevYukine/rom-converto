//! GameCube disc verification.
//!
//! Fast mode (default) checks the RVZ container's stored SHA-1 hashes; it is a
//! no-op for plain ISO/GCM input, which carries none. `--full` additionally
//! validates the FST geometry and computes a whole-disc SHA-1. GameCube discs
//! have no built-in integrity hashes, so that digest is informational (useful
//! for matching against external DAT/Redump databases), never a pass/fail.

use crate::nintendo::disc_input::open_disc_input;
use crate::nintendo::dol::models::boot_bin::GcBootBin;
use crate::nintendo::rvz::verify::{RvzStructuralVerify, verify_rvz_structure};
use crate::util::ProgressReporter;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct DolVerifyOptions {
    pub full: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DolVerifyResult {
    pub game_id: String,
    /// Present only for `.rvz` input.
    pub rvz_structure: Option<RvzStructuralVerify>,
    /// Present only with `--full`.
    pub structural: Option<DolStructuralReport>,
    /// Whole-disc SHA-1 (hex), informational, `--full` only.
    pub disc_sha1: Option<String>,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DolStructuralReport {
    pub fst_offset: u32,
    pub fst_size: u32,
    pub fst_within_bounds: bool,
    pub notes: Vec<String>,
}

pub fn verify_dol(
    path: &Path,
    options: &DolVerifyOptions,
    progress: &dyn ProgressReporter,
) -> Result<DolVerifyResult> {
    let rvz_structure = verify_rvz_structure(path).ok();

    let mut reader =
        open_disc_input(path).with_context(|| format!("dol verify: open {}", path.display()))?;
    let boot = GcBootBin::read(&mut reader).context("dol verify: parse boot.bin")?;
    let game_id = boot.game_id.clone();

    let mut structural = None;
    let mut disc_sha1 = None;

    if options.full {
        let iso_size = reader.iso_size().context("dol verify: iso size")?;
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
        let digest = sha1_stream(&mut reader, iso_size, progress)?;
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
        structural,
        disc_sha1,
        ok,
    })
}

fn sha1_stream<R: Read>(
    reader: &mut R,
    total: u64,
    progress: &dyn ProgressReporter,
) -> Result<[u8; 20]> {
    progress.start(total, "Verifying GameCube disc");
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
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
    use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
    use crate::nintendo::rvz::{RvzCompressOptions, compress_disc};
    use crate::util::NoProgress;

    #[tokio::test]
    async fn rvz_fast_verify_passes_structural_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        std::fs::write(&iso, make_fake_gamecube_iso(4 * 1024 * 1024 + 0x123)).unwrap();
        compress_disc(&iso, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();

        let fast = verify_dol(&rvz, &DolVerifyOptions { full: false }, &NoProgress).unwrap();
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
        compress_disc(&iso, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();

        let full = verify_dol(&rvz, &DolVerifyOptions { full: true }, &NoProgress).unwrap();
        assert!(full.structural.is_some());
        assert_eq!(full.disc_sha1.as_ref().map(|s| s.len()), Some(40));
    }

    #[test]
    fn plain_iso_fast_verify_has_no_structural_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, make_fake_gamecube_iso(2 * 1024 * 1024)).unwrap();
        let fast = verify_dol(&iso, &DolVerifyOptions { full: false }, &NoProgress).unwrap();
        assert!(fast.rvz_structure.is_none());
        assert!(fast.ok);
    }
}
