use std::path::{Path, PathBuf};

mod compress;
mod decompress;
pub mod error;
pub mod models;
mod seekable;

pub use compress::compress_rom;
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
        for i in 0x200..size {
            data[i] = (i % 251) as u8;
        }
        data
    }

    // Produces a fake 3DSX file of the given size filled with a compressible
    // pattern. 3DSX has no encryption check so any content is valid.
    fn make_fake_3dsx(size: usize) -> Vec<u8> {
        let mut data = vec![0u8; size];
        data[0..4].copy_from_slice(&underlying_magic::THREEDSX);
        for i in 4..size {
            data[i] = (i % 251) as u8;
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

        compress_rom(&input, &compressed, &NoProgress)
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

        compress_rom(&input, &compressed, &NoProgress)
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

    #[tokio::test]
    async fn compress_produces_smaller_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.cxi");
        let compressed = dir.path().join("game.zcxi");

        let original = make_fake_decrypted_cxi(256 * 1024);
        tokio::fs::write(&input, &original).await.unwrap();

        compress_rom(&input, &compressed, &NoProgress)
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

        let result = compress_rom(&input, &output, &NoProgress).await;
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

        let result = compress_rom(&input, &output, &NoProgress).await;
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
        compress_rom(&input, &output, &NoProgress).await.unwrap();

        let header_bytes = tokio::fs::read(&output).await.unwrap();
        assert_eq!(&header_bytes[0..4], Z3DS_MAGIC);
    }
}
