//! Xbox 360 ISO -> Games on Demand (GoD) conversion. The game partition
//! is copied out of the XDVDFS image into SHA-1 hash-chained `DataNNNN`
//! part files, fronted by a `LIVE` header, laid out the way the console
//! expects to find an installed title:
//!
//! ```text
//! <output dir>/<title id>/00007000/<media id>        (header)
//! <output dir>/<title id>/00007000/<media id>.data/  (part files)
//! ```

mod error;
mod header;
mod layout;
mod parts;
mod xex;

pub use error::{GodError, GodResult};

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::task;

use crate::util::{CancelToken, ProgressReporter, await_with_progress_cancel};

/// Content-type directory every Games on Demand title installs under.
const CONTENT_DIR: &str = "00007000";

/// Outcome of a GoD conversion.
#[derive(Debug, Clone, Copy)]
pub struct GodSummary {
    pub title_id: u32,
    pub media_id: u32,
    pub part_count: u64,
    pub total_bytes: u64,
}

/// The per-title tree a Games on Demand install lives under. Other
/// content can already exist here (other content types, other discs),
/// so cleanup must never touch the tree wholesale.
fn title_dir(output_dir: &Path, title_id: u32) -> PathBuf {
    output_dir.join(format!("{title_id:08X}"))
}

/// The two paths a run writes: the header file and its part-file
/// directory.
fn run_paths(output_dir: &Path, scan: &layout::GodScan) -> (PathBuf, PathBuf) {
    let content_dir = title_dir(output_dir, scan.execution.title_id).join(CONTENT_DIR);
    let media_id = format!("{:08X}", scan.execution.media_id);
    let header_path = content_dir.join(&media_id);
    let data_dir = content_dir.join(format!("{media_id}.data"));
    (header_path, data_dir)
}

/// Refuses a run whose content or title directory is a symlink: either
/// would redirect every write outside the chosen output tree. A
/// pre-existing part directory, link or not, is removed and recreated
/// before use, and the header is published by rename, so no other
/// component matters. A link swapped in after this check, during the
/// run, is a residual race this check does not close. Missing paths
/// are fine; only an existing symlink is rejected.
fn reject_symlinks(header_path: &Path) -> GodResult<()> {
    let content_dir = header_path.parent();
    for dir in content_dir
        .into_iter()
        .chain(content_dir.and_then(Path::parent))
    {
        if let Ok(meta) = std::fs::symlink_metadata(dir)
            && meta.file_type().is_symlink()
        {
            return Err(GodError::SymlinkInOutput(dir.to_path_buf()));
        }
    }
    Ok(())
}

/// Cancellation/failure cleanup: removes exactly the header file and
/// part-file directory this run wrote, never the wider title tree.
/// Parent directories are removed too, but only non-recursively and
/// only once this run's own files are gone, so a directory that still
/// holds unrelated content is left alone.
fn cleanup_run(header_path: &Path, data_dir: &Path) {
    // A link planted during the run must not redirect the cleanup.
    if reject_symlinks(header_path).is_err() {
        return;
    }
    let _ = std::fs::remove_file(header_path);
    let _ = std::fs::remove_dir_all(data_dir);
    if let Some(content_dir) = header_path.parent() {
        let _ = std::fs::remove_dir(content_dir);
        if let Some(title_dir) = content_dir.parent() {
            let _ = std::fs::remove_dir(title_dir);
        }
    }
}

/// Convert the Xbox 360 disc image at `input` into a Games on Demand
/// title tree under `output_dir`; on cancel the partially written tree
/// is removed.
///
/// # Errors
/// Returns [`GodError::SymlinkInOutput`] if the title or content directory
/// is a symlink, and [`GodError::Cancelled`] on cancellation. Also errors
/// on invalid or truncated images, I/O failures, or worker failures.
pub async fn convert_to_god(
    input: &Path,
    output_dir: &Path,
    title: Option<&str>,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> GodResult<GodSummary> {
    let scan = {
        let input = input.to_path_buf();
        task::spawn_blocking(move || -> GodResult<layout::GodScan> {
            layout::scan(&mut std::fs::File::open(input)?)
        })
        .await??
    };
    let (header_path, data_dir) = run_paths(output_dir, &scan);
    // Nothing has been written yet, so a refusal here must not trigger
    // cleanup.
    reject_symlinks(&header_path)?;
    progress.start(scan.data_size, "Converting to Xbox 360 Games on Demand");

    let input_owned = input.to_path_buf();
    let output_owned = output_dir.to_path_buf();
    let title_owned = title.map(str::to_string);
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = task::spawn_blocking(move || -> GodResult<GodSummary> {
        convert_blocking(
            &input_owned,
            &output_owned,
            title_owned.as_deref(),
            &scan,
            &bytes_done_bg,
            &cancel_bg,
        )
    });

    match await_with_progress_cancel(progress, &bytes_done, handle, &cancel).await {
        Ok(summary) => Ok(summary),
        Err(err) => {
            cleanup_run(&header_path, &data_dir);
            Err(err)
        }
    }
}

fn convert_blocking(
    input: &Path,
    output_dir: &Path,
    title: Option<&str>,
    scan: &layout::GodScan,
    bytes_done: &AtomicU64,
    cancel: &CancelToken,
) -> GodResult<GodSummary> {
    let (header_path, data_dir) = run_paths(output_dir, scan);
    // A stale .data dir from a prior run must not leak old DataNNNN
    // files into this one.
    if data_dir.exists() {
        std::fs::remove_dir_all(&data_dir)?;
    }
    std::fs::create_dir_all(&data_dir)?;

    let mut reader = std::fs::File::open(input)?;
    let parts = parts::write_parts(&mut reader, scan, &data_dir, bytes_done, cancel)?;
    let header = header::build_header(scan, &parts, title.or(scan.title_name.as_deref()));
    crate::util::atomic_write(&header_path, true, |file| file.write_all(&header))?;

    Ok(GodSummary {
        title_id: scan.execution.title_id,
        media_id: scan.execution.media_id,
        part_count: scan.part_count,
        total_bytes: parts.total_size + header.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::xdvdfs::{SECTOR_SIZE, VOLUME_DESCRIPTOR_SECTOR};
    use crate::microsoft::xenon::test_fixtures::{descriptor, dirent};
    use crate::util::NoProgress;
    use sha1::{Digest, Sha1};

    const TITLE_ID: u32 = 0x4541_08A7;
    const MEDIA_ID: u32 = 0x1122_3344;

    /// Writes a trimmed-base (base 0) X360 image holding `DEFAULT.XEX`
    /// at the root, small enough that the whole conversion runs in a
    /// single part.
    fn write_source_iso(path: &Path) {
        let root_sector = 40u32;
        let xex_sector = 50u32;
        let xex = xex::synthetic_xex(&xex::ExecutionId {
            media_id: MEDIA_ID,
            title_id: TITLE_ID,
            platform: 2,
            executable_type: 1,
            disc_number: 1,
            disc_count: 1,
        });

        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        let entry = dirent(0, 0, xex_sector, xex.len() as u32, 0, b"DEFAULT.XEX");
        root[..entry.len()].copy_from_slice(&entry);

        let mut image = vec![0u8; ((xex_sector as u64 + 2) * SECTOR_SIZE) as usize];
        let put = |image: &mut Vec<u8>, sector: u32, bytes: &[u8]| {
            let at = (sector as u64 * SECTOR_SIZE) as usize;
            image[at..at + bytes.len()].copy_from_slice(bytes);
        };
        put(
            &mut image,
            VOLUME_DESCRIPTOR_SECTOR,
            &descriptor(root_sector, SECTOR_SIZE as u32),
        );
        put(&mut image, root_sector, &root);
        put(&mut image, xex_sector, &xex);
        std::fs::write(path, &image).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn converts_an_iso_into_a_hash_chained_container() {
        let work = tempfile::tempdir().unwrap();
        let iso = work.path().join("game.iso");
        write_source_iso(&iso);
        let out = work.path().join("out");

        let summary = convert_to_god(
            &iso,
            &out,
            Some("Test Title"),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(summary.title_id, TITLE_ID);
        assert_eq!(summary.media_id, MEDIA_ID);
        assert_eq!(summary.part_count, 1);

        let content = out.join(format!("{TITLE_ID:08X}")).join(CONTENT_DIR);
        let header = std::fs::read(content.join(format!("{MEDIA_ID:08X}"))).unwrap();
        assert_eq!(header.len(), header::HEADER_SIZE);

        let part = std::fs::read(
            content
                .join(format!("{MEDIA_ID:08X}.data"))
                .join("Data0000"),
        )
        .unwrap();
        assert_eq!(
            &header[0x037D..0x0391],
            Sha1::digest(&part[..0x1000]).as_slice()
        );
        assert_eq!(summary.total_bytes, part.len() as u64 + header.len() as u64);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_run_leaves_no_title_directory() {
        let work = tempfile::tempdir().unwrap();
        let iso = work.path().join("game.iso");
        write_source_iso(&iso);
        let out = work.path().join("out");

        let cancel = CancelToken::new();
        cancel.cancel();
        let result = convert_to_god(&iso, &out, None, &NoProgress, cancel).await;
        assert!(matches!(result, Err(GodError::Cancelled(_))));
        assert!(!title_dir(&out, TITLE_ID).exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_run_does_not_touch_pre_existing_sibling_content() {
        let work = tempfile::tempdir().unwrap();
        let iso = work.path().join("game.iso");
        write_source_iso(&iso);
        let out = work.path().join("out");

        // A different content type already installed under the same
        // title id, unrelated to this GoD conversion.
        let sibling_dir = title_dir(&out, TITLE_ID).join("00004000");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        let sibling_file = sibling_dir.join("marker");
        std::fs::write(&sibling_file, b"keep me").unwrap();

        let cancel = CancelToken::new();
        cancel.cancel();
        let result = convert_to_god(&iso, &out, None, &NoProgress, cancel).await;
        assert!(matches!(result, Err(GodError::Cancelled(_))));
        assert!(sibling_file.exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_part_file_is_removed_before_reconverting() {
        let work = tempfile::tempdir().unwrap();
        let iso = work.path().join("game.iso");
        write_source_iso(&iso);
        let out = work.path().join("out");

        let data_dir = title_dir(&out, TITLE_ID)
            .join(CONTENT_DIR)
            .join(format!("{MEDIA_ID:08X}.data"));
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("Data0001"), b"stale").unwrap();

        convert_to_god(&iso, &out, None, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        assert!(!data_dir.join("Data0001").exists());
        assert!(data_dir.join("Data0000").exists());
    }

    /// Plants an external tree holding content under the run's ids and
    /// returns the `keep` marker file inside it. `below` is the path a
    /// write-through of the run's own outputs would land at relative to
    /// `real`, so the marker only survives a refused run where the
    /// refusal actually guards it.
    #[cfg(unix)]
    fn plant_external_tree(real: &Path, below: &Path) -> std::path::PathBuf {
        let keep = real
            .join(below)
            .join(format!("{MEDIA_ID:08X}.data"))
            .join("keep");
        std::fs::create_dir_all(keep.parent().unwrap()).unwrap();
        std::fs::write(&keep, b"keep").unwrap();
        keep
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_symlinked_title_directory_is_refused() {
        let work = tempfile::tempdir().unwrap();
        let iso = work.path().join("game.iso");
        write_source_iso(&iso);
        let out = work.path().join("out");
        std::fs::create_dir_all(&out).unwrap();

        let outside = tempfile::tempdir().unwrap();
        let keep = plant_external_tree(outside.path(), Path::new(CONTENT_DIR));
        std::os::unix::fs::symlink(outside.path(), title_dir(&out, TITLE_ID)).unwrap();

        let result = convert_to_god(&iso, &out, None, &NoProgress, CancelToken::new()).await;
        assert!(matches!(result, Err(GodError::SymlinkInOutput(_))));
        assert!(keep.exists());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_symlinked_content_directory_is_refused() {
        let work = tempfile::tempdir().unwrap();
        let iso = work.path().join("game.iso");
        write_source_iso(&iso);
        let out = work.path().join("out");

        let outside = tempfile::tempdir().unwrap();
        let keep = plant_external_tree(outside.path(), Path::new(""));
        std::fs::create_dir_all(title_dir(&out, TITLE_ID)).unwrap();
        std::os::unix::fs::symlink(outside.path(), title_dir(&out, TITLE_ID).join(CONTENT_DIR))
            .unwrap();

        let result = convert_to_god(&iso, &out, None, &NoProgress, CancelToken::new()).await;
        assert!(matches!(result, Err(GodError::SymlinkInOutput(_))));
        assert!(keep.exists());
    }

    /// Links planted at the part directory and the header path are not
    /// refused: both are replaced in place and their targets stay
    /// untouched.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_conversion_replaces_links_at_the_data_and_header_paths() {
        let work = tempfile::tempdir().unwrap();
        let iso = work.path().join("game.iso");
        write_source_iso(&iso);
        let out = work.path().join("out");

        let outside = tempfile::tempdir().unwrap();
        let keep = outside.path().join("keep");
        std::fs::write(&keep, b"keep").unwrap();
        let outside_header = outside.path().join("header.bin");
        std::fs::write(&outside_header, b"outside").unwrap();

        let content_dir = title_dir(&out, TITLE_ID).join(CONTENT_DIR);
        std::fs::create_dir_all(&content_dir).unwrap();
        let data_dir = content_dir.join(format!("{MEDIA_ID:08X}.data"));
        let header_path = content_dir.join(format!("{MEDIA_ID:08X}"));
        std::os::unix::fs::symlink(outside.path(), &data_dir).unwrap();
        std::os::unix::fs::symlink(&outside_header, &header_path).unwrap();

        convert_to_god(&iso, &out, None, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        let data_meta = std::fs::symlink_metadata(&data_dir).unwrap();
        assert!(!data_meta.file_type().is_symlink());
        assert!(data_dir.join("Data0000").exists());
        let header_meta = std::fs::symlink_metadata(&header_path).unwrap();
        assert!(!header_meta.file_type().is_symlink());
        assert_eq!(
            std::fs::read(&header_path).unwrap().len(),
            header::HEADER_SIZE
        );
        assert_eq!(std::fs::read(&keep).unwrap(), b"keep");
        assert_eq!(std::fs::read(&outside_header).unwrap(), b"outside");
    }
}
