//! Plain extraction of the segments an `EBOOT.PBP` carries.
//!
//! This unpacks the container only. `DATA.PSAR` comes out as stored, so it
//! stays encrypted for `NPUMDIMG` images; [`crate::sony::psp::npumd::to_iso`]
//! decrypts those into an ISO.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::sony::psp::pbp::{Pbp, SEGMENT_NAMES};
use crate::util::{BYTES_PER_MB, ProgressReporter};

const CHUNK_BYTES: usize = 1024 * 1024;

/// Writes every segment the PBP at `input` carries into `out_dir` under its
/// standard name (`PARAM.SFO`, `ICON0.PNG`, ..., `DATA.PSAR`), skipping
/// absent segments, and returns the paths written.
///
/// # Errors
/// Returns an error if `input` is not a valid PBP, or if any read, write,
/// or directory creation fails.
pub fn extract_segments(
    progress: &dyn ProgressReporter,
    input: &Path,
    out_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let mut file =
        File::open(input).with_context(|| format!("pbp extract: open {}", input.display()))?;
    let pbp =
        Pbp::read(&mut file).with_context(|| format!("pbp extract: parse {}", input.display()))?;
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("pbp extract: create {}", out_dir.display()))?;

    let total: u64 = pbp.segments.iter().map(|s| s.size).sum();
    progress.start(
        total,
        &format!(
            "Extracting PBP segments (~{:.2} MB)",
            total as f64 / BYTES_PER_MB
        ),
    );

    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut written = Vec::new();
    for (index, segment) in pbp.segments.iter().enumerate() {
        if segment.size == 0 {
            continue;
        }
        let path = out_dir.join(SEGMENT_NAMES[index]);
        let mut out = BufWriter::new(
            File::create(&path)
                .with_context(|| format!("pbp extract: create {}", path.display()))?,
        );
        file.seek(SeekFrom::Start(segment.offset))?;
        let mut remaining = segment.size;
        while remaining > 0 {
            let n = remaining.min(CHUNK_BYTES as u64) as usize;
            file.read_exact(&mut buf[..n])?;
            out.write_all(&buf[..n])?;
            remaining -= n as u64;
            progress.inc(n as u64);
        }
        out.flush()?;
        written.push(path);
    }
    progress.finish();
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sony::psp::pbp::test_fixtures::build_pbp;
    use crate::util::NoProgress;

    #[test]
    fn writes_only_the_present_segments() {
        let psar = vec![0xABu8; CHUNK_BYTES + 7];
        let bytes = build_pbp(
            1,
            &[b"sfo bytes", b"icon", &[], &[], &[], &[], b"psp", &psar],
        );

        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("EBOOT.PBP");
        std::fs::write(&input, &bytes).expect("write eboot");
        let out = dir.path().join("out");

        let written = extract_segments(&NoProgress, &input, &out).expect("extract");
        let names: Vec<String> = written
            .iter()
            .map(|p| p.file_name().expect("name").to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["PARAM.SFO", "ICON0.PNG", "DATA.PSP", "DATA.PSAR"]
        );

        assert_eq!(
            std::fs::read(out.join("PARAM.SFO")).expect("sfo"),
            b"sfo bytes"
        );
        assert_eq!(std::fs::read(out.join("ICON0.PNG")).expect("icon"), b"icon");
        assert_eq!(std::fs::read(out.join("DATA.PSAR")).expect("psar"), psar);
        assert!(!out.join("PIC1.PNG").exists());
    }

    #[test]
    fn rejects_a_non_pbp_input() {
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("not.pbp");
        std::fs::write(&input, vec![0u8; 128]).expect("write");
        assert!(extract_segments(&NoProgress, &input, &dir.path().join("out")).is_err());
    }
}
