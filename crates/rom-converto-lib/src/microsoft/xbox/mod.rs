//! Original Xbox XISO support: build an image from a directory or an
//! existing XDVDFS image (which trims a full disc image down to the
//! game partition and re-lays it out), extract one back to a directory,
//! and summarize one.

mod create;
mod error;
mod extract;
mod info;

pub use create::input_total_bytes;
pub use error::{XboxError, XboxResult};
pub use info::{XisoInfo, read_info};

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use log::info;
use tokio::task;

use crate::util::{CancelToken, ProgressReporter, await_with_progress_cancel, scratch_output_path};

#[derive(Debug, Clone, Copy)]
pub struct XisoCreateOptions {
    /// Rewrite the XDK media-type check in every `.xbe`. On by default:
    /// xdvdfs-built images are known not to boot on some BIOSes without
    /// it, and the patch is inert on the ones that do not need it.
    pub media_patch: bool,
}

impl Default for XisoCreateOptions {
    fn default() -> Self {
        Self { media_patch: true }
    }
}

/// Build an XISO from `input`, which may be either a directory to pack or
/// an existing XDVDFS image to trim and re-lay-out.
pub async fn convert_to_xiso(
    input: &Path,
    output: &Path,
    options: XisoCreateOptions,
    progress: &dyn ProgressReporter,
) -> XboxResult<()> {
    convert_to_xiso_cancellable(input, output, options, progress, CancelToken::new()).await
}

/// Like [`convert_to_xiso`] but observes `cancel` at file and chunk
/// boundaries; on cancel the partial image is removed (the writer targets
/// a sibling temp file renamed into place only on success).
pub async fn convert_to_xiso_cancellable(
    input: &Path,
    output: &Path,
    options: XisoCreateOptions,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> XboxResult<()> {
    let total_bytes = {
        let input = input.to_path_buf();
        task::spawn_blocking(move || create::input_total_bytes(&input)).await??
    };
    progress.start(total_bytes, "Building XISO");

    let write_path = scratch_output_path(output)?;
    let input_owned = input.to_path_buf();
    let write_owned = write_path.to_path_buf();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = task::spawn_blocking(move || {
        create::create_blocking(
            &input_owned,
            &write_owned,
            options,
            bytes_done_bg,
            &cancel_bg,
        )
    });

    let cleanup = {
        let write_path = write_path.to_path_buf();
        move || -> XboxError {
            let _ = std::fs::remove_file(&write_path);
            XboxError::Cancelled
        }
    };
    if let Err(err) =
        await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await
    {
        let _ = tokio::fs::remove_file(&write_path).await;
        return Err(err);
    }
    crate::util::publish_temp(write_path, output, true)?;

    info!("Wrote XISO {} -> {}", input.display(), output.display());
    Ok(())
}

/// Extract every file in an XISO into `output_dir`, mirroring the disc's
/// directory tree.
pub async fn extract_xiso(
    input: &Path,
    output_dir: &Path,
    progress: &dyn ProgressReporter,
) -> XboxResult<()> {
    extract_xiso_cancellable(input, output_dir, progress, CancelToken::new()).await
}

/// Like [`extract_xiso`] but observes `cancel` at file and chunk
/// boundaries. Files already written stay on disk.
pub async fn extract_xiso_cancellable(
    input: &Path,
    output_dir: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> XboxResult<()> {
    let total_bytes = {
        let input = input.to_path_buf();
        task::spawn_blocking(move || read_info(&input))
            .await??
            .total_file_bytes
    };
    progress.start(total_bytes, "Extracting XISO");

    let input_owned = input.to_path_buf();
    let output_owned = output_dir.to_path_buf();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = task::spawn_blocking(move || {
        extract::extract_blocking(&input_owned, &output_owned, bytes_done_bg, &cancel_bg)
    });
    await_with_progress_cancel(progress, &bytes_done, handle, &cancel, || {
        XboxError::Cancelled
    })
    .await?;

    info!(
        "Extracted XISO {} -> {}",
        input.display(),
        output_dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::xdvdfs::{
        PartitionKind, SECTOR_SIZE, VOLUME_DESCRIPTOR_SECTOR, VOLUME_MAGIC,
    };
    use crate::util::NoProgress;
    use std::collections::BTreeMap;
    use std::io::{Seek, Write};
    use std::path::PathBuf;

    /// Reads a tree back as `relative path -> contents`, with directories
    /// recorded as an empty entry so empty ones still show up.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut out = BTreeMap::new();
        fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap())
                .collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                let key = path.strip_prefix(base).unwrap().to_path_buf();
                if path.is_dir() {
                    out.insert(key, None);
                    walk(&path, base, out);
                } else {
                    out.insert(key, Some(std::fs::read(&path).unwrap()));
                }
            }
        }
        walk(root, root, &mut out);
        out
    }

    fn build_source_tree(root: &Path) {
        std::fs::create_dir_all(root.join("media/sound")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("default.xbe"), vec![0xAAu8; 100]).unwrap();
        std::fs::write(root.join("readme.txt"), b"hello").unwrap();
        std::fs::write(root.join("zero.bin"), b"").unwrap();
        std::fs::write(root.join("media/big.bin"), vec![0x5Au8; 5000]).unwrap();
        std::fs::write(root.join("media/sound/a.wav"), vec![1u8; 3]).unwrap();
    }

    #[tokio::test]
    async fn directory_round_trips_through_an_xiso() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        build_source_tree(&source);

        let image = dir.path().join("game.iso");
        convert_to_xiso(&source, &image, XisoCreateOptions::default(), &NoProgress)
            .await
            .unwrap();

        let info = read_info(&image).unwrap();
        assert_eq!(info.kind, PartitionKind::Trimmed);
        assert_eq!(info.base, 0);
        assert_eq!(info.file_count, 5);
        assert_eq!(info.dir_count, 3);
        assert_eq!(info.total_file_bytes, 100 + 5 + 5000 + 3);

        let extracted = dir.path().join("out");
        extract_xiso(&image, &extracted, &NoProgress).await.unwrap();
        assert_eq!(snapshot(&source), snapshot(&extracted));
    }

    #[tokio::test]
    async fn image_invariants_match_the_format() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        build_source_tree(&source);
        let image = dir.path().join("game.iso");
        convert_to_xiso(&source, &image, XisoCreateOptions::default(), &NoProgress)
            .await
            .unwrap();

        let bytes = std::fs::read(&image).unwrap();
        assert_eq!(bytes.len() as u64 % 0x10000, 0);
        let tag = format!("in!xiso!{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(&bytes[31337..31337 + tag.len()], tag.as_bytes());
        assert_eq!(&bytes[0x8000..0x8006], &[1, b'C', b'D', b'0', b'0', b'1']);
        assert_eq!(
            &bytes[0x8800..0x8806],
            &[0xFF, b'C', b'D', b'0', b'0', b'1']
        );

        let descriptor = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        assert_eq!(&bytes[descriptor..descriptor + 20], VOLUME_MAGIC.as_slice());
        assert_eq!(
            &bytes[descriptor + 0x7EC..descriptor + 0x800],
            VOLUME_MAGIC.as_slice()
        );
        // The PVD's volume space size is the whole padded image.
        assert_eq!(
            u32::from_le_bytes(bytes[0x8050..0x8054].try_into().unwrap()) as u64,
            bytes.len() as u64 / SECTOR_SIZE
        );
    }

    #[tokio::test]
    async fn media_patch_hits_xbes_across_a_read_boundary_and_spares_the_rest() {
        // The pattern straddles the 1 MiB read buffer: five bytes before
        // the boundary, three after.
        const PATTERN: [u8; 8] = [0xE8, 0xCA, 0xFD, 0xFF, 0xFF, 0x85, 0xC0, 0x7D];
        let split = 1024 * 1024 - 5;
        let mut payload = vec![0u8; split + 8];
        payload[split..split + 8].copy_from_slice(&PATTERN);

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("default.xbe"), &payload).unwrap();
        std::fs::write(source.join("data.bin"), &payload).unwrap();

        for (media_patch, want) in [(true, 0xEBu8), (false, 0x7Du8)] {
            let image = dir.path().join(format!("game{media_patch}.iso"));
            convert_to_xiso(
                &source,
                &image,
                XisoCreateOptions { media_patch },
                &NoProgress,
            )
            .await
            .unwrap();
            let extracted = dir.path().join(format!("out{media_patch}"));
            extract_xiso(&image, &extracted, &NoProgress).await.unwrap();

            let xbe = std::fs::read(extracted.join("default.xbe")).unwrap();
            assert_eq!(xbe[split + 7], want, "media_patch = {media_patch}");
            let bin = std::fs::read(extracted.join("data.bin")).unwrap();
            assert_eq!(bin[split + 7], 0x7D, "non-xbe must never be patched");
        }
    }

    #[tokio::test]
    async fn names_the_format_cannot_carry_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("\u{4E2D}.bin"), b"x").unwrap();

        let err = convert_to_xiso(
            &source,
            &dir.path().join("a.iso"),
            XisoCreateOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, XboxError::NameNotCp1252 { .. }), "{err}");
    }

    /// A file's dirent offsets have to survive the sector-straddle bump,
    /// so pack a directory whose entries cannot all fit in one sector and
    /// check every one is still reachable through the on-disk BST.
    #[tokio::test]
    async fn straddling_dirtab_entries_stay_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        // 14 + 242 = 256 bytes of dirent each, so the eighth entry is
        // bumped into the second sector.
        let names: Vec<String> = (0..9).map(|i| format!("{}{i}", "N".repeat(242))).collect();
        for name in &names {
            std::fs::write(source.join(name), name.as_bytes()).unwrap();
        }

        let image = dir.path().join("game.iso");
        convert_to_xiso(&source, &image, XisoCreateOptions::default(), &NoProgress)
            .await
            .unwrap();

        assert_eq!(read_info(&image).unwrap().root_size, 2 * 2048);
        let extracted = dir.path().join("out");
        extract_xiso(&image, &extracted, &NoProgress).await.unwrap();
        assert_eq!(snapshot(&source), snapshot(&extracted));
    }

    #[tokio::test]
    async fn a_full_dump_is_trimmed_and_relaid_out() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        build_source_tree(&source);

        // Build a trimmed image first, then shift it to the XGD2 base to
        // synthesize a full disc image.
        let trimmed = dir.path().join("trimmed.iso");
        convert_to_xiso(&source, &trimmed, XisoCreateOptions::default(), &NoProgress)
            .await
            .unwrap();

        // Seek past the base rather than materializing it, so the video
        // partition costs nothing to synthesize.
        const XGD2_BASE: u64 = 0x0FD9_0000;
        let full_path = dir.path().join("full.iso");
        let mut full = std::fs::File::create(&full_path).unwrap();
        full.seek(std::io::SeekFrom::Start(XGD2_BASE)).unwrap();
        full.write_all(&std::fs::read(&trimmed).unwrap()).unwrap();
        drop(full);
        assert_eq!(read_info(&full_path).unwrap().kind, PartitionKind::Xgd2);

        let rebuilt = dir.path().join("rebuilt.iso");
        convert_to_xiso(
            &full_path,
            &rebuilt,
            XisoCreateOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap();

        let info = read_info(&rebuilt).unwrap();
        assert_eq!(info.kind, PartitionKind::Trimmed);
        assert_eq!(info.file_count, 5);
        assert_eq!(info.dir_count, 3);

        let extracted = dir.path().join("out");
        extract_xiso(&rebuilt, &extracted, &NoProgress)
            .await
            .unwrap();
        assert_eq!(snapshot(&source), snapshot(&extracted));
    }
}
