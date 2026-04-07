use std::path::{Path, PathBuf};

pub mod error;
pub mod models;
mod seekable;
mod compress;
mod decompress;

pub use compress::compress_rom;
pub use decompress::decompress_rom;

/// Derives the output path for compression by prefixing the extension with "z".
///
/// Examples: `game.cia` → `game.zcia`, `game.cci` → `game.zcci`,
///           `game.3ds` → `game.zcci`, `game.3dsx` → `game.z3dsx`,
///           `game.cxi` → `game.zcxi`.
pub fn derive_compressed_path(input: &Path) -> PathBuf {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let new_ext = match ext.as_str() {
        "cia" => "zcia",
        "cci" | "3ds" => "zcci",
        "cxi" => "zcxi",
        "3dsx" => "z3dsx",
        _ => "z3ds",
    };

    input.with_extension(new_ext)
}

/// Derives the output path for decompression by removing the "z" prefix from
/// the extension.
///
/// Examples: `game.zcia` → `game.cia`, `game.zcci` → `game.cci`,
///           `game.zcxi` → `game.cxi`, `game.z3dsx` → `game.3dsx`.
pub fn derive_decompressed_path(input: &Path) -> PathBuf {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let new_ext = match ext.as_str() {
        "zcia" => "cia",
        "zcci" => "cci",
        "zcxi" => "cxi",
        "z3dsx" => "3dsx",
        _ => "3ds",
    };

    input.with_extension(new_ext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // --- derive_compressed_path ---

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

    // --- derive_decompressed_path ---

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

    // --- Full compress → decompress round-trips ---

    // Produces a fake decrypted CXI of the given size.
    // Places the NCCH magic + NoCrypto flag at the correct offsets so the
    // encryption check passes, then fills the rest with a compressible pattern.
    fn make_fake_decrypted_cxi(size: usize) -> Vec<u8> {
        let size = size.max(0x200);
        let mut data = vec![0u8; size];
        data[0x100..0x104].copy_from_slice(b"NCCH");
        data[0x18F] = 0x04; // NoCrypto flag
        for i in 0x200..size {
            data[i] = (i % 251) as u8;
        }
        data
    }

    // Produces a fake 3DSX file of the given size filled with a compressible
    // pattern. 3DSX has no encryption check so any content is valid.
    fn make_fake_3dsx(size: usize) -> Vec<u8> {
        let mut data = vec![0u8; size];
        data[0..4].copy_from_slice(b"3DSX");
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

        compress_rom(&input, &compressed).await.unwrap();
        decompress_rom(&compressed, &decompressed).await.unwrap();

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

        compress_rom(&input, &compressed).await.unwrap();
        decompress_rom(&compressed, &decompressed).await.unwrap();

        let result = tokio::fs::read(&decompressed).await.unwrap();
        assert_eq!(original, result, "decompressed 3DSX does not match original");
    }

    #[tokio::test]
    async fn compress_produces_smaller_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.cxi");
        let compressed = dir.path().join("game.zcxi");

        let original = make_fake_decrypted_cxi(256 * 1024);
        tokio::fs::write(&input, &original).await.unwrap();

        compress_rom(&input, &compressed).await.unwrap();

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

        // NCCH magic present but NoCrypto flag NOT set → encrypted
        let mut data = vec![0u8; 0x200];
        data[0x100..0x104].copy_from_slice(b"NCCH");
        tokio::fs::write(&input, &data).await.unwrap();

        let result = compress_rom(&input, &output).await;
        assert!(
            matches!(result, Err(crate::nintendo::ctr::z3ds::error::Z3dsError::InputNotDecrypted)),
            "expected InputNotDecrypted, got {result:?}"
        );
    }

    #[tokio::test]
    async fn compress_unsupported_extension_fails() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.nds");
        let output = dir.path().join("game.znds");

        tokio::fs::write(&input, b"dummy").await.unwrap();

        let result = compress_rom(&input, &output).await;
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

        tokio::fs::write(&input, &make_fake_3dsx(16 * 1024)).await.unwrap();
        compress_rom(&input, &output).await.unwrap();

        let header_bytes = tokio::fs::read(&output).await.unwrap();
        assert_eq!(&header_bytes[0..4], b"Z3DS");
    }
}
