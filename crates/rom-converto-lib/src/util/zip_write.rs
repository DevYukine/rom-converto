//! Deflate zip writing for cartridge ROMs: one streamed member, staged on a
//! sibling temp file and published only once the archive is complete.

use crate::util::{CancelToken, Cancelled, ProgressReporter, publish_temp, scratch_output_path};
use std::io::{Read, Write};
use std::path::Path;

/// Source size from which the archive must flag zip64 (`large_file`), the
/// format's 32-bit entry-size limit.
const LARGE_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Read/write chunk size for the streaming copy, mirroring the other
/// streaming helpers in this crate.
const CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// Writes `source` as the single member `member_name` of a new deflate zip
/// at `output`, streamed in fixed-size chunks with per-chunk progress and
/// cancellation. The archive is written to a sibling temp file and renamed
/// into place only after the write completes, so a cancelled or failed run
/// never leaves a partial file behind.
///
/// # Errors
/// Returns an error when the source cannot be read or the archive cannot be
/// written; cancellation surfaces as [`crate::util::Cancelled`].
pub fn write_zip(
    source: &Path,
    member_name: &str,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<()> {
    let mut input = std::fs::File::open(source)?;
    let total = input.metadata()?.len();

    let temp = scratch_output_path(output)?;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&temp)?);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(9))
        .large_file(total >= LARGE_FILE_BYTES);
    zip.start_file(member_name, options)?;

    progress.start(total, "zip");
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        zip.write_all(&buf[..n])?;
        progress.inc(n as u64);
    }
    drop(zip.finish()?);
    progress.finish();

    publish_temp(temp, output, true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;
    use std::io::Read;
    use tempfile::tempdir;

    #[test]
    fn round_trips_single_member() {
        let dir = tempdir().expect("temp dir");
        let source = dir.path().join("cart.gba");
        std::fs::write(&source, b"cartridge bytes").expect("write source");
        let output = dir.path().join("cart.zip");

        write_zip(
            &source,
            "cart.gba",
            &output,
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("write_zip succeeds");

        let mut archive =
            zip::ZipArchive::new(std::fs::File::open(&output).expect("open zip")).expect("archive");
        assert_eq!(archive.len(), 1);
        let mut member = archive.by_name("cart.gba").expect("member");
        let mut bytes = Vec::new();
        member.read_to_end(&mut bytes).expect("read member");
        assert_eq!(bytes, b"cartridge bytes");
    }

    #[test]
    fn pre_cancelled_token_writes_nothing() {
        let dir = tempdir().expect("temp dir");
        let source = dir.path().join("cart.gba");
        std::fs::write(&source, b"cartridge bytes").expect("write source");
        let output = dir.path().join("cart.zip");
        let cancel = CancelToken::new();
        cancel.cancel();

        let err = write_zip(&source, "cart.gba", &output, &NoProgress, &cancel)
            .expect_err("pre-cancelled write fails");
        assert!(Cancelled::in_chain(&err));
        assert!(!output.exists(), "no output file is published");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(leftovers, vec![source.file_name().expect("file name")]);
    }
}
