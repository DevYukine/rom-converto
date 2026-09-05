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

/// Cancellation/failure cleanup: removes exactly the header file and
/// part-file directory this run wrote, never the wider title tree.
/// Parent directories are removed too, but only non-recursively and
/// only once this run's own files are gone, so a directory that still
/// holds unrelated content is left alone.
fn cleanup_run(header_path: &Path, data_dir: &Path) {
    let _ = std::fs::remove_file(header_path);
    let _ = std::fs::remove_dir_all(data_dir);
    if let Some(content_dir) = header_path.parent() {
        let _ = std::fs::remove_dir(content_dir);
        if let Some(title_dir) = content_dir.parent() {
            let _ = std::fs::remove_dir(title_dir);
        }
    }
}

/// Convert the Xbox 360 XDVDFS ISO `input` into a GoD container under
/// `output_dir`. `title` overrides the header's display name and
/// description; without it the executable's own title name is used.
pub async fn convert_to_god(
    input: &Path,
    output_dir: &Path,
    title: Option<&str>,
    progress: &dyn ProgressReporter,
) -> GodResult<GodSummary> {
    convert_to_god_cancellable(input, output_dir, title, progress, CancelToken::new()).await
}

/// Like [`convert_to_god`] but observes `cancel` at subpart boundaries;
/// on cancel the partially written title tree is removed.
pub async fn convert_to_god_cancellable(
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
    progress.start(scan.data_size, "Converting to Xbox 360 Games on Demand");
    let (header_path, data_dir) = run_paths(output_dir, &scan);

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

    match await_with_progress_cancel(progress, &bytes_done, handle, &cancel, || {
        GodError::Cancelled
    })
    .await
    {
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
    std::fs::write(&header_path, &header)?;

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

        let summary = convert_to_god(&iso, &out, Some("Test Title"), &NoProgress)
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
        let result = convert_to_god_cancellable(&iso, &out, None, &NoProgress, cancel).await;
        assert!(matches!(result, Err(GodError::Cancelled)));
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
        let result = convert_to_god_cancellable(&iso, &out, None, &NoProgress, cancel).await;
        assert!(matches!(result, Err(GodError::Cancelled)));
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

        convert_to_god(&iso, &out, None, &NoProgress).await.unwrap();

        assert!(!data_dir.join("Data0001").exists());
        assert!(data_dir.join("Data0000").exists());
    }
}
