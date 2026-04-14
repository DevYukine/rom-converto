//! Wii and GameCube RVZ disc image compression.
//!
//! RVZ is Dolphin's zstd-based evolution of Wiimm's WIA format. It stores a
//! disc image as a chunk table of independently-compressed blocks. Both
//! GameCube and Wii discs use the same on-disc format; the console-specific
//! pieces (disc detection, encryption, common keys) live in
//! [`crate::nintendo::dol`] and [`crate::nintendo::rvl`].
//!
//! Spec reference:
//! <https://github.com/dolphin-emu/dolphin/blob/master/docs/WiaAndRvz.md>
//!
//! # Async/sync boundary
//!
//! [`compress_disc`] and [`decompress_disc`] are `async`. They hand the
//! whole blocking pipeline to [`tokio::task::spawn_blocking`] and poll a
//! shared `Arc<AtomicU64>` for progress. Rules:
//!
//! * All `std::fs` calls live inside `spawn_blocking` or worker
//!   threads spawned from it. No sync I/O ever runs on the async
//!   runtime.
//! * `tokio::fs` is avoided in the hot path. On Windows it wraps
//!   `std::fs` + `spawn_blocking` with no speed gain; on Linux it
//!   lacks positional reads, which the worker pools need.
//!   `tokio::fs::metadata` is used once at the entry point for
//!   progress reporting, nothing more.
//! * Worker pools use `std::thread::spawn` with `std::sync::mpsc`,
//!   not Tokio primitives. The pool is owned by the outer
//!   `spawn_blocking` closure and never touches the runtime.

pub mod constants;
pub mod error;
pub mod format;
pub mod packing;
pub mod regions;
pub mod worker_pool;

pub mod compress;
pub mod decompress;

pub use compress::{RvzCompressOptions, compress_disc};
pub use decompress::decompress_disc;
pub use error::{RvzError, RvzResult};

use std::path::{Path, PathBuf};

/// Derives the output path for RVZ compression by mapping the input extension
/// to `.rvz`.
pub fn derive_rvz_path(input: &Path) -> PathBuf {
    input.with_extension("rvz")
}

/// Derives the output path for RVZ decompression, defaulting to `.iso`
/// regardless of whether the source disc was GameCube or Wii.
pub fn derive_disc_path(input: &Path) -> PathBuf {
    input.with_extension("iso")
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
    use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso;
    use crate::util::NoProgress;

    #[tokio::test]
    async fn gamecube_round_trip_small() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let restored = dir.path().join("game.round.iso");

        // 5 MiB gets several 2 MiB chunks plus a short tail.
        let original = make_fake_gamecube_iso(5 * 1024 * 1024 + 123);
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(&iso, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();

        decompress_disc(&rvz, &restored, &NoProgress).await.unwrap();

        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(original, result, "GC round trip must be byte-identical");
    }

    #[tokio::test]
    async fn gamecube_compress_produces_smaller_file_for_compressible_input() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");

        // All zeros compresses heavily.
        let mut original = make_fake_gamecube_iso(4 * 1024 * 1024);
        for b in original.iter_mut().skip(0x80) {
            *b = 0;
        }
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(&iso, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();

        let rvz_size = tokio::fs::metadata(&rvz).await.unwrap().len();
        assert!(
            rvz_size < original.len() as u64,
            "compressed {} >= original {}",
            rvz_size,
            original.len()
        );
    }

    #[tokio::test]
    async fn wii_partition_round_trips_at_small_chunk_size() {
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("wii.round.iso");

        let original = make_fake_wii_iso_with_partition(2);
        tokio::fs::write(&iso, &original).await.unwrap();

        // 128 KiB chunks → 16 chunks per Wii cluster → exercises the
        // sub-cluster path: per-chunk exception lists with chunk-local
        // offsets, deferred exception application during decompress.
        let opts = RvzCompressOptions {
            chunk_size: 128 * 1024,
            ..RvzCompressOptions::default()
        };
        compress_disc(&iso, &rvz, opts, &NoProgress).await.unwrap();
        decompress_disc(&rvz, &restored, &NoProgress).await.unwrap();

        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(
            original, result,
            "Wii sub-cluster round-trip must be byte-identical"
        );
    }

    #[tokio::test]
    async fn wii_partial_last_cluster_round_trips() {
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partial_partition;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz_2m = dir.path().join("wii_2mib.rvz");
        let rvz_128k = dir.path().join("wii_128k.rvz");
        let restored_2m = dir.path().join("wii_2mib.round.iso");
        let restored_128k = dir.path().join("wii_128k.round.iso");

        // 2 full clusters + partial last cluster with 13 sectors of
        // real data (51 padding sectors fall into the raw region that
        // follows). Exercises both the partial-cluster encoder path
        // (zero-padded payload recompute, chunk-local exception
        // filtering) and the partial-chunk decoder path at both the
        // 2 MiB and 128 KiB chunk sizes.
        let original = make_fake_wii_iso_with_partial_partition(2, 13);
        tokio::fs::write(&iso, &original).await.unwrap();

        let opts_2m = RvzCompressOptions {
            chunk_size: 2 * 1024 * 1024,
            ..RvzCompressOptions::default()
        };
        compress_disc(&iso, &rvz_2m, opts_2m, &NoProgress)
            .await
            .unwrap();
        decompress_disc(&rvz_2m, &restored_2m, &NoProgress)
            .await
            .unwrap();
        assert_eq!(
            original,
            tokio::fs::read(&restored_2m).await.unwrap(),
            "partial-cluster round-trip at 2 MiB chunks must be byte-identical"
        );

        let opts_128k = RvzCompressOptions {
            chunk_size: 128 * 1024,
            ..RvzCompressOptions::default()
        };
        compress_disc(&iso, &rvz_128k, opts_128k, &NoProgress)
            .await
            .unwrap();
        decompress_disc(&rvz_128k, &restored_128k, &NoProgress)
            .await
            .unwrap();
        assert_eq!(
            original,
            tokio::fs::read(&restored_128k).await.unwrap(),
            "partial-cluster round-trip at 128 KiB chunks must be byte-identical"
        );
    }

    #[tokio::test]
    async fn wii_with_real_partition_round_trips() {
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("wii.round.iso");

        // 2 clusters = 4 MiB of partition data, plenty to exercise the
        // partition pipeline (encrypt, hash, decrypt, exception list).
        let original = make_fake_wii_iso_with_partition(2);
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(&iso, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();

        decompress_disc(&rvz, &restored, &NoProgress).await.unwrap();
        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(
            original.len(),
            result.len(),
            "Wii partition round-trip should preserve file size"
        );
        assert_eq!(
            original, result,
            "Wii partition round-trip must be byte-identical"
        );
    }

    #[tokio::test]
    async fn wii_round_trip_small() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("wii.round.iso");

        let original = make_fake_wii_iso(3 * 1024 * 1024 + 17);
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(&iso, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();

        decompress_disc(&rvz, &restored, &NoProgress).await.unwrap();
        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(original, result, "Wii round trip must be byte-identical");
    }

    #[tokio::test]
    async fn gamecube_with_zero_stretch_uses_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let restored = dir.path().join("game.round.iso");

        // 4 MiB total: 0x80 header, then ~1 MiB of pattern, then ~3 MiB of
        // zeros, then a tail of pattern. With the default 128 KiB chunk size
        // the zero stretch covers many full chunks.
        let mut original = make_fake_gamecube_iso(4 * 1024 * 1024);
        for byte in original.iter_mut().skip(1024 * 1024).take(3 * 1024 * 1024) {
            *byte = 0;
        }
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(&iso, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();
        decompress_disc(&rvz, &restored, &NoProgress).await.unwrap();

        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(original, result, "round-trip with sentinels must be exact");

        // Sanity-check that the encoder shrunk the output. With 3 MiB of
        // zeros this should be well under half the original size.
        let rvz_size = tokio::fs::metadata(&rvz).await.unwrap().len();
        assert!(
            rvz_size < (original.len() / 2) as u64,
            "expected sentinel-shrunk output, got {rvz_size} for {} byte input",
            original.len()
        );
    }

    #[tokio::test]
    async fn small_chunk_size_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let restored = dir.path().join("game.round.iso");

        let original = make_fake_gamecube_iso(512 * 1024 + 11);
        tokio::fs::write(&iso, &original).await.unwrap();

        let opts = RvzCompressOptions {
            chunk_size: 32 * 1024,
            ..RvzCompressOptions::default()
        };
        compress_disc(&iso, &rvz, opts, &NoProgress).await.unwrap();
        decompress_disc(&rvz, &restored, &NoProgress).await.unwrap();

        assert_eq!(original, tokio::fs::read(&restored).await.unwrap());
    }

    #[tokio::test]
    async fn invalid_chunk_size_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        tokio::fs::write(&iso, make_fake_gamecube_iso(64 * 1024))
            .await
            .unwrap();

        // Not a power of two.
        let opts = RvzCompressOptions {
            chunk_size: 100 * 1024,
            ..RvzCompressOptions::default()
        };
        let err = compress_disc(&iso, &rvz, opts, &NoProgress)
            .await
            .unwrap_err();
        assert!(matches!(err, RvzError::InvalidChunkSize(_, _, _)));

        // Below MIN_CHUNK_SIZE.
        let opts = RvzCompressOptions {
            chunk_size: 16 * 1024,
            ..RvzCompressOptions::default()
        };
        let err = compress_disc(&iso, &rvz, opts, &NoProgress)
            .await
            .unwrap_err();
        assert!(matches!(err, RvzError::InvalidChunkSize(_, _, _)));

        // Above MAX_CHUNK_SIZE.
        let opts = RvzCompressOptions {
            chunk_size: 4 * 1024 * 1024,
            ..RvzCompressOptions::default()
        };
        let err = compress_disc(&iso, &rvz, opts, &NoProgress)
            .await
            .unwrap_err();
        assert!(matches!(err, RvzError::InvalidChunkSize(_, _, _)));
    }

    #[tokio::test]
    async fn decompress_rejects_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.rvz");
        tokio::fs::write(&bad, vec![0u8; 200]).await.unwrap();
        let out = dir.path().join("out.iso");
        let err = decompress_disc(&bad, &out, &NoProgress).await.unwrap_err();
        assert!(matches!(err, RvzError::InvalidMagic(_)));
    }

    /// Bidirectional byte-identical round-trip against Dolphin's own
    /// `DolphinTool.exe`. Gated behind env vars so CI stays
    /// deterministic:
    ///
    /// * `ROM_CONVERTO_DOLPHIN_PARITY_ISO`: optional GameCube ISO/GCM.
    /// * `ROM_CONVERTO_DOLPHIN_PARITY_WII_ISO`: optional Wii ISO.
    /// * `ROM_CONVERTO_DOLPHIN_TOOL`: path to `DolphinTool[.exe]`.
    ///
    /// For each ISO that's set, the test runs four steps:
    /// 1. Compress the input ISO with rom-converto (L5, 128 KiB).
    /// 2. Decompress that `.rvz` with Dolphin's `convert -f iso` and
    ///    assert byte equality (SHA-1) against the input.
    /// 3. Compress the input ISO with Dolphin (`convert -f rvz -l 5 -b 131072`).
    /// 4. Decompress Dolphin's `.rvz` with rom-converto and assert
    ///    byte equality (SHA-1) against the input.
    ///
    /// Skipped with a printed note when the tool is unset or no ISO
    /// env vars are set.
    #[tokio::test]
    async fn dolphin_parity_cross_tool_round_trip() {
        let dolphin_tool = match std::env::var("ROM_CONVERTO_DOLPHIN_TOOL") {
            Ok(p) => p,
            Err(_) => {
                eprintln!(
                    "skipping dolphin_parity_cross_tool_round_trip: ROM_CONVERTO_DOLPHIN_TOOL not set"
                );
                return;
            }
        };
        assert!(
            PathBuf::from(&dolphin_tool).is_file(),
            "ROM_CONVERTO_DOLPHIN_TOOL does not point at a file: {}",
            dolphin_tool
        );

        let gc_iso = std::env::var("ROM_CONVERTO_DOLPHIN_PARITY_ISO").ok();
        let wii_iso = std::env::var("ROM_CONVERTO_DOLPHIN_PARITY_WII_ISO").ok();
        if gc_iso.is_none() && wii_iso.is_none() {
            eprintln!(
                "skipping dolphin_parity_cross_tool_round_trip: no ISO env var set (ROM_CONVERTO_DOLPHIN_PARITY_ISO or ROM_CONVERTO_DOLPHIN_PARITY_WII_ISO)"
            );
            return;
        }

        if let Some(p) = gc_iso {
            run_cross_tool_parity(&PathBuf::from(&p), &dolphin_tool, "GameCube").await;
        }
        if let Some(p) = wii_iso {
            run_cross_tool_parity(&PathBuf::from(&p), &dolphin_tool, "Wii").await;
        }
    }

    /// Run both directions of rom-converto ↔ Dolphin parity on a single
    /// input ISO. Shared helper so the same four steps cover GameCube
    /// and Wii when their respective env vars are set.
    async fn run_cross_tool_parity(iso_path: &Path, dolphin_tool: &str, label: &str) {
        use sha1::{Digest, Sha1};

        assert!(
            iso_path.is_file(),
            "{label} ISO does not point at a file: {}",
            iso_path.display()
        );

        eprintln!("[{label}] SHA-1 of input ISO...");
        let input_sha1 = sha1_file(iso_path).await;

        let dir = tempfile::tempdir().unwrap();
        let ours_rvz = dir.path().join("ours.rvz");
        let ours_from_dolphin_iso = dir.path().join("ours.from_dolphin.iso");
        let dolphin_rvz = dir.path().join("dolphin.rvz");
        let dolphin_from_ours_iso = dir.path().join("dolphin.from_ours.iso");

        // Step 1: compress with rom-converto.
        eprintln!("[{label}] step 1: rom-converto compress");
        let opts = RvzCompressOptions {
            chunk_size: 131072,
            compression_level: 5,
            ..RvzCompressOptions::default()
        };
        compress_disc(iso_path, &ours_rvz, opts, &NoProgress)
            .await
            .expect("our compress failed");

        // Step 2: Dolphin decompresses our RVZ; result must hash-match.
        eprintln!("[{label}] step 2: DolphinTool decode ours.rvz");
        run_dolphin_with_timeout(
            dolphin_tool,
            &[
                "convert",
                "-i",
                ours_rvz.to_str().unwrap(),
                "-o",
                ours_from_dolphin_iso.to_str().unwrap(),
                "-f",
                "iso",
            ],
            600,
            &format!("{label} Dolphin decode of ours"),
        );
        let dolphin_decoded_sha1 = sha1_file(&ours_from_dolphin_iso).await;
        assert_eq!(
            input_sha1, dolphin_decoded_sha1,
            "[{label}] Dolphin's decode of our RVZ does not match the original ISO"
        );

        // Step 3: compress with Dolphin.
        eprintln!("[{label}] step 3: DolphinTool encode reference.iso");
        run_dolphin_with_timeout(
            dolphin_tool,
            &[
                "convert",
                "-i",
                iso_path.to_str().unwrap(),
                "-o",
                dolphin_rvz.to_str().unwrap(),
                "-f",
                "rvz",
                "-b",
                "131072",
                "-c",
                "zstd",
                "-l",
                "5",
            ],
            900,
            &format!("{label} Dolphin compress"),
        );

        // Step 4: our decoder on Dolphin's RVZ must hash-match.
        eprintln!("[{label}] step 4: rom-converto decode dolphin.rvz");
        decompress_disc(&dolphin_rvz, &dolphin_from_ours_iso, &NoProgress)
            .await
            .expect("our decompress failed on Dolphin's RVZ");
        let ours_decoded_sha1 = sha1_file(&dolphin_from_ours_iso).await;
        assert_eq!(
            input_sha1, ours_decoded_sha1,
            "[{label}] our decode of Dolphin's RVZ does not match the original ISO"
        );

        async fn sha1_file(path: &Path) -> [u8; 20] {
            let mut file = tokio::fs::File::open(path).await.expect("open");
            let mut hasher = Sha1::new();
            let mut buf = vec![0u8; 1 << 20];
            use tokio::io::AsyncReadExt;
            loop {
                let n = file.read(&mut buf).await.expect("read");
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            hasher.finalize().into()
        }
    }

    /// Run `DolphinTool` with a hard timeout. If the process doesn't
    /// finish within `timeout_secs`, kill it and panic. This prevents
    /// failed runs from hanging on a modal error dialog on Windows
    /// (`DolphinTool.exe` pops "Unable to open disc image" etc. on
    /// failure and blocks on user acknowledgment without a timeout).
    fn run_dolphin_with_timeout(tool: &str, args: &[&str], timeout_secs: u64, label: &str) {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let mut child = Command::new(tool)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("[{label}] failed to spawn DolphinTool: {e}"));

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(
                        status.success(),
                        "[{label}] DolphinTool exited with failure: {status}"
                    );
                    return;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("[{label}] DolphinTool exceeded {timeout_secs}s timeout; killed");
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => panic!("[{label}] DolphinTool wait failed: {e}"),
            }
        }
    }

    #[test]
    fn wii_exception_format_is_spec_compliant() {
        // Verify that pack_partition_chunk emits bytes matching Dolphin's
        // wia_except_list_t layout:
        //   [u16 BE count][count × (u16 BE offset, 20-byte hash)][0x1F0000 payloads]
        use crate::nintendo::rvl::constants::{
            WII_BLOCKS_PER_GROUP, WII_GROUP_PAYLOAD_SIZE, WII_SECTOR_PAYLOAD_SIZE,
        };
        use crate::nintendo::rvl::partition::{HashException, pack_partition_chunk};

        let payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = (0..WII_BLOCKS_PER_GROUP)
            .map(|_| [0u8; WII_SECTOR_PAYLOAD_SIZE])
            .collect();
        let exceptions = vec![
            HashException {
                offset: 0x0042,
                hash: [0x11u8; 20],
            },
            HashException {
                offset: 0xFFE0,
                hash: [0x22u8; 20],
            },
        ];

        let packed = pack_partition_chunk(&exceptions, &payloads).unwrap();

        // u16 count in big-endian
        assert_eq!(&packed[0..2], &[0x00, 0x02]);
        // First entry: offset 0x0042 + 20 bytes of 0x11
        assert_eq!(&packed[2..4], &[0x00, 0x42]);
        assert_eq!(&packed[4..24], &[0x11u8; 20]);
        // Second entry: offset 0xFFE0 + 20 bytes of 0x22
        assert_eq!(&packed[24..26], &[0xFF, 0xE0]);
        assert_eq!(&packed[26..46], &[0x22u8; 20]);
        // Payloads follow immediately, no padding.
        assert_eq!(packed.len(), 2 + 2 * 22 + WII_GROUP_PAYLOAD_SIZE as usize);
        for b in &packed[46..] {
            assert_eq!(*b, 0);
        }
    }
}
