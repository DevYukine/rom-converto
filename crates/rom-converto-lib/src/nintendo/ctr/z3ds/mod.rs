use std::path::{Path, PathBuf};

mod compress;
mod compress_parallel;
mod decompress;
mod decompress_parallel;
pub mod error;
pub mod models;
mod seekable;

pub use compress::{DEFAULT_ZSTD_LEVEL, MAX_ZSTD_LEVEL, MIN_ZSTD_LEVEL, compress_rom};
pub use decompress::decompress_rom;
pub use seekable::decode_seekable;

/// Maps a file extension using the given table, falling back to `default`.
fn map_extension(input: &Path, table: &[(&str, &str)], default: &str) -> PathBuf {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let new_ext = table
        .iter()
        .find(|(from, _)| *from == ext.as_str())
        .map(|(_, to)| *to)
        .unwrap_or(default);

    input.with_extension(new_ext)
}

const COMPRESS_MAP: &[(&str, &str)] = &[
    ("cia", "zcia"),
    ("cci", "zcci"),
    ("3ds", "zcci"),
    ("cxi", "zcxi"),
    ("3dsx", "z3dsx"),
];

const DECOMPRESS_MAP: &[(&str, &str)] = &[
    ("zcia", "cia"),
    ("zcci", "cci"),
    ("zcxi", "cxi"),
    ("z3dsx", "3dsx"),
];

/// Derives the output path for compression by prefixing the extension with "z".
pub fn derive_compressed_path(input: &Path) -> PathBuf {
    map_extension(input, COMPRESS_MAP, "z3ds")
}

/// Derives the output path for decompression by removing the "z" prefix from
/// the extension.
pub fn derive_decompressed_path(input: &Path) -> PathBuf {
    map_extension(input, DECOMPRESS_MAP, "3ds")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::constants::{
        NCCH_FLAGS_OFFSET, NCCH_FLAGS7_NOCRYPTO, NCCH_MAGIC_OFFSET,
    };
    use crate::nintendo::ctr::z3ds::models::{Z3DS_MAGIC, underlying_magic};
    use crate::util::NoProgress;
    use std::path::PathBuf;

    #[test]
    fn compress_path_cia() {
        assert_eq!(
            derive_compressed_path(Path::new("game.cia")),
            PathBuf::from("game.zcia")
        );
    }

    #[test]
    fn compress_path_cci() {
        assert_eq!(
            derive_compressed_path(Path::new("game.cci")),
            PathBuf::from("game.zcci")
        );
    }

    #[test]
    fn compress_path_3ds() {
        assert_eq!(
            derive_compressed_path(Path::new("game.3ds")),
            PathBuf::from("game.zcci")
        );
    }

    #[test]
    fn compress_path_3dsx() {
        assert_eq!(
            derive_compressed_path(Path::new("game.3dsx")),
            PathBuf::from("game.z3dsx")
        );
    }

    #[test]
    fn compress_path_cxi() {
        assert_eq!(
            derive_compressed_path(Path::new("game.cxi")),
            PathBuf::from("game.zcxi")
        );
    }

    #[test]
    fn compress_path_preserves_directory() {
        assert_eq!(
            derive_compressed_path(Path::new("/roms/game.cia")),
            PathBuf::from("/roms/game.zcia")
        );
    }

    #[test]
    fn decompress_path_zcia() {
        assert_eq!(
            derive_decompressed_path(Path::new("game.zcia")),
            PathBuf::from("game.cia")
        );
    }

    #[test]
    fn decompress_path_zcci() {
        assert_eq!(
            derive_decompressed_path(Path::new("game.zcci")),
            PathBuf::from("game.cci")
        );
    }

    #[test]
    fn decompress_path_zcxi() {
        assert_eq!(
            derive_decompressed_path(Path::new("game.zcxi")),
            PathBuf::from("game.cxi")
        );
    }

    #[test]
    fn decompress_path_z3dsx() {
        assert_eq!(
            derive_decompressed_path(Path::new("game.z3dsx")),
            PathBuf::from("game.3dsx")
        );
    }

    #[test]
    fn decompress_path_preserves_directory() {
        assert_eq!(
            derive_decompressed_path(Path::new("/roms/game.zcia")),
            PathBuf::from("/roms/game.cia")
        );
    }

    // Produces a fake decrypted CXI of the given size.
    // Places the NCCH magic + NoCrypto flag at the correct offsets so the
    // encryption check passes, then fills the rest with a compressible pattern.
    fn make_fake_decrypted_cxi(size: usize) -> Vec<u8> {
        let size = size.max(0x200);
        let mut data = vec![0u8; size];
        data[NCCH_MAGIC_OFFSET..NCCH_MAGIC_OFFSET + 4].copy_from_slice(&underlying_magic::NCCH);
        data[NCCH_FLAGS_OFFSET + 7] = NCCH_FLAGS7_NOCRYPTO;
        for (i, b) in data.iter_mut().enumerate().skip(0x200) {
            *b = (i % 251) as u8;
        }
        data
    }

    // Produces a fake 3DSX file of the given size filled with a compressible
    // pattern. 3DSX has no encryption check so any content is valid.
    fn make_fake_3dsx(size: usize) -> Vec<u8> {
        let mut data = vec![0u8; size];
        data[0..4].copy_from_slice(&underlying_magic::THREEDSX);
        for (i, b) in data.iter_mut().enumerate().skip(4) {
            *b = (i % 251) as u8;
        }
        data
    }

    #[tokio::test]
    async fn round_trip_cxi() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.cxi");
        let compressed = dir.path().join("game.zcxi");
        let decompressed = dir.path().join("game_out.cxi");

        let original = make_fake_decrypted_cxi(64 * 1024);
        tokio::fs::write(&input, &original).await.unwrap();

        compress_rom(&input, &compressed, None, &NoProgress)
            .await
            .unwrap();
        decompress_rom(&compressed, &decompressed, &NoProgress)
            .await
            .unwrap();

        let result = tokio::fs::read(&decompressed).await.unwrap();
        assert_eq!(original, result, "decompressed CXI does not match original");
    }

    #[tokio::test]
    async fn round_trip_3dsx() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("app.3dsx");
        let compressed = dir.path().join("app.z3dsx");
        let decompressed = dir.path().join("app_out.3dsx");

        let original = make_fake_3dsx(128 * 1024);
        tokio::fs::write(&input, &original).await.unwrap();

        compress_rom(&input, &compressed, None, &NoProgress)
            .await
            .unwrap();
        decompress_rom(&compressed, &decompressed, &NoProgress)
            .await
            .unwrap();

        let result = tokio::fs::read(&decompressed).await.unwrap();
        assert_eq!(
            original, result,
            "decompressed 3DSX does not match original"
        );
    }

    /// Exercises the streaming `compress_rom` path with an input large enough
    /// to span many `FRAME_SIZE_DEFAULT` (256 KB) frames. The other round-trip
    /// tests use inputs of 64 KB to 128 KB, which fit in a single frame and so
    /// do not cover frame iteration, the spawn_blocking progress counter loop,
    /// or the placeholder-header rewrite at a non-zero payload position.
    #[tokio::test]
    async fn round_trip_multi_frame_cxi() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("big.cxi");
        let compressed = dir.path().join("big.zcxi");
        let decompressed = dir.path().join("big_out.cxi");

        // 2 MB → ~8 frames at 256 KB each + a partial last frame.
        let original = make_fake_decrypted_cxi(2 * 1024 * 1024 + 7);
        tokio::fs::write(&input, &original).await.unwrap();

        compress_rom(&input, &compressed, None, &NoProgress)
            .await
            .unwrap();
        decompress_rom(&compressed, &decompressed, &NoProgress)
            .await
            .unwrap();

        let result = tokio::fs::read(&decompressed).await.unwrap();
        assert_eq!(
            original, result,
            "multi-frame CXI did not round-trip cleanly"
        );

        // Confirm the output really has multiple frames, so the test exercises
        // the seek-table path and not the single-frame degenerate case.
        let compressed_bytes = tokio::fs::read(&compressed).await.unwrap();
        // The seek table footer sits at the end of the file. num_frames is the
        // little-endian u32 at offset (len - 9).
        let num_frames = u32::from_le_bytes(
            compressed_bytes[compressed_bytes.len() - 9..compressed_bytes.len() - 5]
                .try_into()
                .unwrap(),
        );
        assert!(
            num_frames >= 8,
            "expected ≥8 frames in 2MB+ payload, got {num_frames}"
        );
    }

    #[tokio::test]
    async fn compress_produces_smaller_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.cxi");
        let compressed = dir.path().join("game.zcxi");

        let original = make_fake_decrypted_cxi(256 * 1024);
        tokio::fs::write(&input, &original).await.unwrap();

        compress_rom(&input, &compressed, None, &NoProgress)
            .await
            .unwrap();

        let compressed_size = tokio::fs::metadata(&compressed).await.unwrap().len();
        assert!(
            compressed_size < original.len() as u64,
            "compressed file ({compressed_size} bytes) is not smaller than original ({} bytes)",
            original.len()
        );
    }

    #[tokio::test]
    async fn compress_encrypted_cxi_fails() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("encrypted.cxi");
        let output = dir.path().join("encrypted.zcxi");

        // NCCH magic present but NoCrypto flag NOT set
        let mut data = vec![0u8; 0x200];
        data[NCCH_MAGIC_OFFSET..NCCH_MAGIC_OFFSET + 4].copy_from_slice(&underlying_magic::NCCH);
        tokio::fs::write(&input, &data).await.unwrap();

        let result = compress_rom(&input, &output, None, &NoProgress).await;
        assert!(
            matches!(
                result,
                Err(crate::nintendo::ctr::z3ds::error::Z3dsError::InputNotDecrypted)
            ),
            "expected InputNotDecrypted, got {result:?}"
        );
    }

    #[tokio::test]
    async fn compress_unsupported_extension_fails() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.nds");
        let output = dir.path().join("game.znds");

        tokio::fs::write(&input, b"dummy").await.unwrap();

        let result = compress_rom(&input, &output, None, &NoProgress).await;
        assert!(
            matches!(
                result,
                Err(crate::nintendo::ctr::z3ds::error::Z3dsError::UnsupportedInputFormat(_))
            ),
            "expected UnsupportedInputFormat, got {result:?}"
        );
    }

    #[tokio::test]
    async fn compressed_file_starts_with_z3ds_magic() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.3dsx");
        let output = dir.path().join("game.z3dsx");

        tokio::fs::write(&input, &make_fake_3dsx(16 * 1024))
            .await
            .unwrap();
        compress_rom(&input, &output, None, &NoProgress)
            .await
            .unwrap();

        let header_bytes = tokio::fs::read(&output).await.unwrap();
        assert_eq!(&header_bytes[0..4], Z3DS_MAGIC);
    }

    /// Every valid zstd level must produce output that decompresses
    /// back to the original bytes. Covers default (None), explicit
    /// default (Some(0)), a mid level, and the max. Uses a 1 MB
    /// input so the output spans multiple 256 KB frames and the
    /// parallel pipeline has real work to do at each level.
    #[tokio::test]
    async fn compress_all_levels_round_trip() {
        for level in [None, Some(0), Some(3), Some(9), Some(22)] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("game.cxi");
            let compressed = dir.path().join("game.zcxi");
            let decompressed = dir.path().join("game_out.cxi");

            let original = make_fake_decrypted_cxi(1024 * 1024);
            tokio::fs::write(&input, &original).await.unwrap();

            compress_rom(&input, &compressed, level, &NoProgress)
                .await
                .unwrap_or_else(|e| panic!("compress failed at level={level:?}: {e}"));
            decompress_rom(&compressed, &decompressed, &NoProgress)
                .await
                .unwrap_or_else(|e| panic!("decompress failed at level={level:?}: {e}"));

            let result = tokio::fs::read(&decompressed).await.unwrap();
            assert_eq!(original, result, "round trip mismatch at level={level:?}");
        }
    }

    /// Out-of-range levels must be rejected before the file is
    /// opened so bad CLI input fails fast without side effects.
    #[tokio::test]
    async fn compress_rejects_out_of_range_level() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.cxi");
        let output = dir.path().join("game.zcxi");

        tokio::fs::write(&input, &make_fake_decrypted_cxi(16 * 1024))
            .await
            .unwrap();

        for bad in [-1, 23, 100] {
            let result = compress_rom(&input, &output, Some(bad), &NoProgress).await;
            assert!(
                matches!(
                    result,
                    Err(
                        crate::nintendo::ctr::z3ds::error::Z3dsError::InvalidCompressionLevel { .. }
                    )
                ),
                "expected InvalidCompressionLevel for level={bad}, got {result:?}"
            );
        }
    }

    /// The compression level the caller asked for must be recorded
    /// in the `zstdlevel` metadata string so downstream tools and
    /// future versions can tell what produced the file.
    #[tokio::test]
    async fn compressed_file_records_zstd_level_in_metadata() {
        use crate::nintendo::ctr::z3ds::models::{Z3DS_HEADER_SIZE, Z3dsMetadata};

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.cxi");
        let output = dir.path().join("game.zcxi");

        tokio::fs::write(&input, &make_fake_decrypted_cxi(16 * 1024))
            .await
            .unwrap();

        compress_rom(&input, &output, Some(9), &NoProgress)
            .await
            .unwrap();

        let bytes = tokio::fs::read(&output).await.unwrap();
        let metadata_start = Z3DS_HEADER_SIZE as usize;
        // Read enough bytes after the header for a metadata block
        // with a few short items. The fake input has exactly four
        // metadata strings which fit in well under 256 bytes.
        let metadata_end = (metadata_start + 256).min(bytes.len());
        let items = Z3dsMetadata::from_bytes(&bytes[metadata_start..metadata_end]);
        let level_item = items
            .iter()
            .find(|i| i.name == "zstdlevel")
            .expect("zstdlevel missing from metadata");
        assert_eq!(level_item.data.as_slice(), b"9");
    }
}
