//! PS3 ISO decryption: turn an encrypted disc image into a decrypted
//! ISO using the per-disc data key.
//!
//! The disc alternates plain and encrypted sector regions (see
//! [`region`]). Plain regions, including the sector 0 region table, are
//! copied byte-for-byte; encrypted regions are AES-128-CBC decrypted a
//! sector at a time. The output covers the sector span the region table
//! describes; trailing padding past it is not copied.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use log::info;

use crate::cd::IO_BUFFER_SIZE;
use crate::util::{
    BYTES_PER_MB, CancelToken, ProgressReporter, await_with_progress_cancel, scratch_output_path,
};

pub mod error;
pub mod info;

pub(crate) mod crypto;
pub(crate) mod embedded_keys;
pub(crate) mod fs;
pub(crate) mod key;
pub(crate) mod region;
pub(crate) mod sfb;
pub(crate) mod worker;

pub use error::{Ps3Error, Ps3Result};
pub use info::{Ps3Info, read_ps3_info};
pub use key::{Ps3Key, resolve_ps3_key};

use crypto::decrypt_sector;
pub use region::{Region, SECTOR_SIZE, parse_region_table};
use worker::{Ps3DecryptWork, Ps3DecryptedOut, make_ps3_decrypt_workers};

/// Sectors per chunk handed to a worker. Bounds per-item memory while
/// keeping the pool fed.
const CHUNK_SECTORS: u32 = 512;

/// Distinct byte values at or above which a sector counts as ciphertext.
/// A random 2048-byte sector hits all 256 values with overwhelming
/// probability; filesystem plaintext stays far below.
const CIPHERTEXT_DISTINCT_BYTES: usize = 250;

/// Default output path for a PS3 decrypt. Input and output are both
/// `.iso`, so the default can't reuse the input's name.
pub fn derive_decrypted_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    input.with_file_name(format!("{stem}.decrypted.iso"))
}

fn looks_like_ciphertext(sector: &[u8; SECTOR_SIZE]) -> bool {
    let mut seen = [false; 256];
    for &b in sector.iter() {
        seen[b as usize] = true;
    }
    seen.iter().filter(|s| **s).count() >= CIPHERTEXT_DISTINCT_BYTES
}

/// Sectors the pre-flight probe inspects.
const PROBE_SAMPLES: usize = 8;

/// LBAs to probe: the first sector of each encrypted region in order,
/// then, while short of [`PROBE_SAMPLES`], evenly spaced sectors inside
/// the first encrypted region.
fn probe_sample_lbas(regions: &[Region]) -> Vec<u32> {
    let mut lbas: Vec<u32> = regions
        .iter()
        .filter(|r| !r.plain)
        .map(|r| r.start)
        .take(PROBE_SAMPLES)
        .collect();
    if let Some(first) = regions.iter().find(|r| !r.plain) {
        let span = (first.last - first.start + 1) as u64;
        for i in 1..PROBE_SAMPLES as u64 {
            if lbas.len() == PROBE_SAMPLES {
                break;
            }
            let lba = (first.start as u64 + span * i / PROBE_SAMPLES as u64).min(first.last as u64)
                as u32;
            if !lbas.contains(&lba) {
                lbas.push(lba);
            }
        }
    }
    lbas
}

/// Pre-flight check over [`PROBE_SAMPLES`] spread encrypted sectors: two
/// or more raw samples reading as plaintext (or the one sample there is,
/// when only one was taken) means the image was decrypted before, and a
/// key that leaves every sample looking like noise is the wrong key.
///
/// The test is byte diversity only, so both verdicts can still be wrong
/// in principle. A correct key looks like [`Ps3Error::KeyMismatch`] when
/// every sampled sector decrypts to compressed or re-encrypted payload
/// (PSARC, EDAT, SELF), which stays high-diversity after decryption; an
/// already-decrypted disc looks encrypted the same way, and is then
/// reported as a key mismatch rather than [`Ps3Error::AlreadyDecrypted`].
/// Both need every sample to miss ordinary filesystem plaintext at once.
/// Spreading the samples across distinct encrypted regions, and across
/// the first region when there are few, makes that unlikely: acceptance
/// needs only one low-diversity sample, and a whole disc of nothing but
/// packed payload at eight separated offsets is not a real layout.
/// Requiring at least two low-diversity raw samples before declaring the
/// disc already decrypted keeps a single anomalous sector from flipping
/// the whole verdict.
fn probe_key_against_samples(input: &Path, regions: &[Region], key: &Ps3Key) -> Ps3Result<()> {
    let lbas = probe_sample_lbas(regions);
    if lbas.is_empty() {
        return Err(Ps3Error::AlreadyDecrypted);
    }
    let mut file = std::fs::File::open(input)?;
    let mut samples = Vec::with_capacity(lbas.len());
    let mut low_diversity_raw = 0;
    for lba in lbas {
        file.seek(SeekFrom::Start(lba as u64 * SECTOR_SIZE as u64))?;
        let mut sector = [0u8; SECTOR_SIZE];
        file.read_exact(&mut sector)?;
        if !looks_like_ciphertext(&sector) {
            low_diversity_raw += 1;
        }
        samples.push((lba, sector));
    }
    // A single anomalous raw sector isn't enough to call the whole disc
    // decrypted, unless it's the only sample taken.
    if low_diversity_raw >= 2 || (samples.len() == 1 && low_diversity_raw == 1) {
        return Err(Ps3Error::AlreadyDecrypted);
    }
    for (lba, mut sector) in samples {
        decrypt_sector(&key.0, lba, &mut sector);
        if !looks_like_ciphertext(&sector) {
            return Ok(());
        }
    }
    Err(Ps3Error::KeyMismatch)
}

#[derive(Debug, Clone, Copy)]
struct Chunk {
    start_lba: u32,
    sectors: u32,
    plain: bool,
}

/// Split the regions into worker chunks of at most [`CHUNK_SECTORS`]
/// sectors, never straddling a region boundary.
///
/// The `r.last - lba + 1` and `lba += sectors` arithmetic cannot
/// overflow: [`parse_region_table`] rejects a region ending at
/// `u32::MAX`.
fn build_chunks(regions: &[Region]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for r in regions {
        let mut lba = r.start;
        while lba <= r.last {
            let remaining = r.last - lba + 1;
            let sectors = remaining.min(CHUNK_SECTORS);
            chunks.push(Chunk {
                start_lba: lba,
                sectors,
                plain: r.plain,
            });
            lba += sectors;
        }
    }
    chunks
}

/// Decrypt a PS3 ISO into a plain ISO.
pub async fn decrypt_ps3_iso(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    key: Ps3Key,
    force: bool,
    skip_probe: bool,
) -> Ps3Result<()> {
    decrypt_ps3_iso_cancellable(
        progress,
        input_path,
        output_path,
        key,
        force,
        skip_probe,
        CancelToken::new(),
    )
    .await
}

/// Decrypt a PS3 ISO, observing `cancel` at every chunk boundary. On
/// cancel the partial output is removed and a pre-existing overwrite
/// target is left untouched (the writer targets a sibling temp file that
/// is renamed into place only on success).
pub async fn decrypt_ps3_iso_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    key: Ps3Key,
    force: bool,
    // Escape hatch for probe false positives on discs whose sampled sectors are all packed payload.
    skip_probe: bool,
    cancel: CancelToken,
) -> Ps3Result<()> {
    let preexisting = tokio::fs::metadata(&output_path).await.is_ok();
    if preexisting && !force {
        return Err(Ps3Error::OutputAlreadyExists);
    }

    let input_size = tokio::fs::metadata(&input_path).await?.len();

    let peek_path = input_path.clone();
    let (regions, total_sectors) =
        tokio::task::spawn_blocking(move || -> Ps3Result<(Vec<Region>, u32)> {
            let mut file = std::fs::File::open(&peek_path)?;
            let mut sector0 = [0u8; SECTOR_SIZE];
            file.read_exact(&mut sector0)?;
            parse_region_table(&sector0)
        })
        .await??;

    // Trailing padding past the last table-covered sector is ignored.
    let expected_size = total_sectors as u64 * SECTOR_SIZE as u64;
    if input_size < expected_size {
        return Err(Ps3Error::InvalidRegionTable(format!(
            "input size {input_size} is smaller than the region table total {expected_size} (total_sectors={total_sectors})"
        )));
    }

    if !skip_probe {
        let probe_path = input_path.clone();
        let probe_regions = regions.clone();
        tokio::task::spawn_blocking(move || {
            probe_key_against_samples(&probe_path, &probe_regions, &key)
        })
        .await??;
    }

    let total_mb = expected_size as f64 / BYTES_PER_MB;
    progress.start(
        expected_size,
        &format!("Decrypting PS3 ISO ({total_sectors} sectors, ~{total_mb:.2} MB)"),
    );

    let chunks = build_chunks(&regions);
    let write_path = scratch_output_path(&output_path)?;
    let input_owned = input_path.clone();
    let write_owned = write_path.to_path_buf();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> Ps3Result<()> {
        use crate::util::worker_pool::{Pool, drive, parallelism};

        let in_file = std::fs::File::open(&input_owned)?;
        let mut reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, in_file);
        let out_file = std::fs::File::create(&write_owned)?;
        let mut writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, out_file);

        let workers = make_ps3_decrypt_workers(parallelism(), key.0);
        let pool: Pool<Ps3DecryptWork, Ps3DecryptedOut, Ps3Error> = Pool::spawn(workers);

        let total = chunks.len() as u64;
        let result = drive(
            &pool,
            total,
            parallelism() * 2,
            |seq| -> Ps3Result<Ps3DecryptWork> {
                if cancel_bg.is_cancelled() {
                    return Err(Ps3Error::Cancelled);
                }
                let chunk = chunks[seq as usize];
                let mut bytes = vec![0u8; chunk.sectors as usize * SECTOR_SIZE];
                reader.read_exact(&mut bytes)?;
                Ok(Ps3DecryptWork {
                    start_lba: chunk.start_lba,
                    plain: chunk.plain,
                    bytes,
                })
            },
            |_seq, out: Ps3DecryptedOut| -> Ps3Result<()> {
                let len = out.bytes.len() as u64;
                writer.write_all(&out.bytes)?;
                bytes_done_bg.fetch_add(len, Ordering::Relaxed);
                Ok(())
            },
        );
        pool.shutdown();
        result?;

        writer.flush()?;
        Ok(())
    });

    let cleanup = {
        let write_path = write_path.to_path_buf();
        move || -> Ps3Error {
            let _ = std::fs::remove_file(&write_path);
            Ps3Error::Cancelled
        }
    };
    if let Err(err) =
        await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await
    {
        let _ = tokio::fs::remove_file(&write_path).await;
        return Err(err);
    }

    crate::util::publish_temp(write_path, &output_path, force)?;

    info!(
        "Decrypted: {:.2} MB PS3 ISO from {}",
        total_mb,
        input_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;
    use crypto::{decrypt_sector, encrypt_sector};

    #[test]
    fn region_table_parses_plain_enc_plain() {
        let mut sector0 = vec![0u8; SECTOR_SIZE];
        sector0[0..4].copy_from_slice(&2u32.to_be_bytes());
        for (i, v) in [1u32, 4, 4].iter().enumerate() {
            let off = 0x0C + i * 4;
            sector0[off..off + 4].copy_from_slice(&v.to_be_bytes());
        }

        let (regions, total) = parse_region_table(&sector0).unwrap();
        assert_eq!(
            regions,
            vec![
                Region {
                    start: 0,
                    last: 1,
                    plain: true
                },
                Region {
                    start: 2,
                    last: 3,
                    plain: false
                },
                Region {
                    start: 4,
                    last: 4,
                    plain: true
                },
            ]
        );
        assert_eq!(total, 5);
    }

    /// Sector 0 for `N` plain regions with the given raw table entries.
    fn region_table_sector(n: u32, entries: &[u32]) -> Vec<u8> {
        let mut sector0 = vec![0u8; SECTOR_SIZE];
        sector0[0..4].copy_from_slice(&n.to_be_bytes());
        for (i, v) in entries.iter().enumerate() {
            let off = 0x0C + i * 4;
            sector0[off..off + 4].copy_from_slice(&v.to_be_bytes());
        }
        sector0
    }

    /// A 5-sector image: plain 0..=1, encrypted 2..=3, plain 4. Returns
    /// the image plus the plaintext of the two encrypted sectors.
    fn synthetic_image(key: &Ps3Key) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut image = region_table_sector(2, &[1, 4, 4]);
        image.resize(5 * SECTOR_SIZE, 0);
        for (i, b) in image[SECTOR_SIZE..2 * SECTOR_SIZE].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        for (i, b) in image[4 * SECTOR_SIZE..5 * SECTOR_SIZE]
            .iter_mut()
            .enumerate()
        {
            *b = (i % 97) as u8;
        }
        // Low byte diversity so the decrypted sectors read as plaintext.
        let plain2: Vec<u8> = (0..SECTOR_SIZE).map(|i| (i % 16) as u8).collect();
        let plain3: Vec<u8> = (0..SECTOR_SIZE).map(|i| (i % 7) as u8).collect();
        image[2 * SECTOR_SIZE..3 * SECTOR_SIZE].copy_from_slice(&plain2);
        image[3 * SECTOR_SIZE..4 * SECTOR_SIZE].copy_from_slice(&plain3);
        encrypt_sector(&key.0, 2, &mut image[2 * SECTOR_SIZE..3 * SECTOR_SIZE]);
        encrypt_sector(&key.0, 3, &mut image[3 * SECTOR_SIZE..4 * SECTOR_SIZE]);
        (image, plain2, plain3)
    }

    /// A high-diversity sector, standing in for compressed or already
    /// encrypted payload that stays ciphertext-like after decryption.
    fn noisy_sector(seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        (0..SECTOR_SIZE)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    /// An 18-sector image: plain 0, encrypted 1..=16, plain 17. `fill`
    /// supplies each encrypted sector's plaintext by LBA; the sectors are
    /// left as plaintext unless `key` is given.
    fn spread_image(key: Option<&Ps3Key>, fill: impl Fn(u32) -> Vec<u8>) -> Vec<u8> {
        let mut image = region_table_sector(2, &[0, 17, 17]);
        image.resize(18 * SECTOR_SIZE, 0);
        for lba in 1..=16u32 {
            let off = lba as usize * SECTOR_SIZE;
            image[off..off + SECTOR_SIZE].copy_from_slice(&fill(lba));
            if let Some(key) = key {
                encrypt_sector(&key.0, lba, &mut image[off..off + SECTOR_SIZE]);
            }
        }
        image
    }

    #[test]
    fn probe_samples_spread_across_the_first_encrypted_region() {
        let (regions, _) = parse_region_table(&region_table_sector(2, &[0, 17, 17])).unwrap();
        assert_eq!(probe_sample_lbas(&regions), vec![1, 3, 5, 7, 9, 11, 13, 15]);
    }

    #[test]
    fn probe_samples_dedupe_tiny_encrypted_regions() {
        let one_sector = [
            Region {
                start: 0,
                last: 4,
                plain: true,
            },
            Region {
                start: 5,
                last: 5,
                plain: false,
            },
        ];
        assert_eq!(probe_sample_lbas(&one_sector), vec![5]);

        let two_sector = [
            Region {
                start: 0,
                last: 9,
                plain: true,
            },
            Region {
                start: 10,
                last: 11,
                plain: false,
            },
        ];
        assert_eq!(probe_sample_lbas(&two_sector), vec![10, 11]);
    }

    #[test]
    fn probe_samples_cap_at_eight_across_many_encrypted_regions() {
        let regions: Vec<Region> = (0..10u32)
            .map(|i| Region {
                start: i,
                last: i,
                plain: false,
            })
            .collect();
        assert_eq!(probe_sample_lbas(&regions), vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn region_table_rejects_entry_ending_at_last_sector() {
        let sector0 = region_table_sector(1, &[u32::MAX]);
        assert!(matches!(
            parse_region_table(&sector0),
            Err(Ps3Error::InvalidRegionTable(_))
        ));
    }

    #[test]
    fn region_table_rejects_non_monotonic_entry() {
        // Plain 0..=5, then an encrypted entry that ends at sector 2.
        let sector0 = region_table_sector(2, &[5, 3, 10]);
        assert!(matches!(
            parse_region_table(&sector0),
            Err(Ps3Error::InvalidRegionTable(_))
        ));
    }

    #[test]
    fn sector_encrypt_decrypt_round_trips() {
        let key = [0x24u8; 16];
        let mut data = [0u8; SECTOR_SIZE];
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        for b in data.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        let original = data;

        encrypt_sector(&key, 7, &mut data);
        assert_ne!(data, original);
        decrypt_sector(&key, 7, &mut data);
        assert_eq!(data, original);
    }

    #[tokio::test]
    async fn full_pipeline_decrypts_encrypted_regions_only() {
        let key = Ps3Key([0x24u8; 16]);
        let (image, plain2, plain3) = synthetic_image(&key);

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        let output = dir.path().join("game.dec.iso");
        std::fs::write(&input, &image).unwrap();

        decrypt_ps3_iso(&NoProgress, input, output.clone(), key, false, false)
            .await
            .unwrap();

        let out = std::fs::read(&output).unwrap();
        assert_eq!(out.len(), image.len());
        // Plain regions copied verbatim.
        assert_eq!(out[0..2 * SECTOR_SIZE], image[0..2 * SECTOR_SIZE]);
        assert_eq!(out[4 * SECTOR_SIZE..], image[4 * SECTOR_SIZE..]);
        // Encrypted regions now hold the known plaintext.
        assert_eq!(out[2 * SECTOR_SIZE..3 * SECTOR_SIZE], plain2[..]);
        assert_eq!(out[3 * SECTOR_SIZE..4 * SECTOR_SIZE], plain3[..]);
    }

    #[test]
    fn read_ps3_info_leaves_encryption_undetermined_without_a_key() {
        let key = Ps3Key([0x24u8; 16]);
        let (image, _, _) = synthetic_image(&key);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.iso");
        std::fs::write(&path, &image).unwrap();

        // No PARAM.SFO to key the embedded database on and no sibling
        // .dkey, so there is nothing to probe with.
        let info = read_ps3_info(&path).unwrap();
        assert_eq!(info.encrypted, None);
    }

    #[tokio::test]
    async fn probe_encrypted_tells_the_encrypted_image_from_its_decrypted_output() {
        let key = Ps3Key([0x24u8; 16]);
        let (image, _, _) = synthetic_image(&key);
        let (regions, _) = parse_region_table(&image[..SECTOR_SIZE]).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        let output = dir.path().join("game.decrypted.iso");
        std::fs::write(&input, &image).unwrap();

        assert_eq!(
            info::probe_encrypted(&input, &regions, Some(&key)),
            Some(true)
        );

        decrypt_ps3_iso(&NoProgress, input, output.clone(), key, false, false)
            .await
            .unwrap();

        assert_eq!(
            info::probe_encrypted(&output, &regions, Some(&key)),
            Some(false)
        );
    }

    #[tokio::test]
    async fn trailing_padding_is_ignored() {
        let key = Ps3Key([0x24u8; 16]);
        let (mut image, plain2, _) = synthetic_image(&key);
        image.extend(std::iter::repeat_n(0xAAu8, 4096));

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        let output = dir.path().join("game.decrypted.iso");
        std::fs::write(&input, &image).unwrap();

        decrypt_ps3_iso(&NoProgress, input, output.clone(), key, false, false)
            .await
            .unwrap();

        let out = std::fs::read(&output).unwrap();
        assert_eq!(out.len(), 5 * SECTOR_SIZE);
        assert_eq!(out[2 * SECTOR_SIZE..3 * SECTOR_SIZE], plain2[..]);
    }

    #[tokio::test]
    async fn plaintext_first_encrypted_sector_reports_already_decrypted() {
        let mut image = region_table_sector(2, &[1, 4, 4]);
        image.resize(5 * SECTOR_SIZE, 0);

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, &image).unwrap();

        let err = decrypt_ps3_iso(
            &NoProgress,
            input,
            dir.path().join("game.decrypted.iso"),
            Ps3Key([0x24u8; 16]),
            false,
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Ps3Error::AlreadyDecrypted));
    }

    #[tokio::test]
    async fn image_without_encrypted_regions_reports_already_decrypted() {
        let mut image = region_table_sector(1, &[1]);
        image.resize(2 * SECTOR_SIZE, 0);

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, &image).unwrap();

        let err = decrypt_ps3_iso(
            &NoProgress,
            input,
            dir.path().join("game.decrypted.iso"),
            Ps3Key([0x24u8; 16]),
            false,
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Ps3Error::AlreadyDecrypted));
    }

    #[tokio::test]
    async fn wrong_key_reports_key_mismatch_across_every_sample() {
        let key = Ps3Key([0x24u8; 16]);
        let image = spread_image(Some(&key), |lba| {
            if lba % 2 == 1 {
                (0..SECTOR_SIZE).map(|i| (i % 16) as u8).collect()
            } else {
                noisy_sector(lba as u64)
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        let output = dir.path().join("game.decrypted.iso");
        std::fs::write(&input, &image).unwrap();

        let err = decrypt_ps3_iso(
            &NoProgress,
            input,
            output.clone(),
            Ps3Key([0x99u8; 16]),
            false,
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Ps3Error::KeyMismatch));
        assert!(!output.exists());
    }

    #[tokio::test]
    async fn correct_key_accepted_when_most_samples_look_compressed() {
        let key = Ps3Key([0x24u8; 16]);
        let plain9: Vec<u8> = (0..SECTOR_SIZE).map(|i| (i % 16) as u8).collect();
        let image = spread_image(Some(&key), |lba| {
            if lba == 9 {
                (0..SECTOR_SIZE).map(|i| (i % 16) as u8).collect()
            } else {
                noisy_sector(lba as u64)
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        let output = dir.path().join("game.decrypted.iso");
        std::fs::write(&input, &image).unwrap();

        decrypt_ps3_iso(&NoProgress, input, output.clone(), key, false, false)
            .await
            .unwrap();

        let out = std::fs::read(&output).unwrap();
        assert_eq!(out.len(), 18 * SECTOR_SIZE);
        assert_eq!(out[9 * SECTOR_SIZE..10 * SECTOR_SIZE], plain9[..]);
        assert_eq!(
            out[SECTOR_SIZE..2 * SECTOR_SIZE],
            noisy_sector(1)[..],
            "encrypted region decrypted with the accepted key"
        );
    }

    #[tokio::test]
    async fn already_decrypted_detected_past_compressed_leading_samples() {
        let image = spread_image(None, |lba| {
            if lba < 9 {
                noisy_sector(lba as u64)
            } else {
                (0..SECTOR_SIZE).map(|i| (i % 16) as u8).collect()
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.iso");
        std::fs::write(&input, &image).unwrap();

        let err = decrypt_ps3_iso(
            &NoProgress,
            input,
            dir.path().join("game.decrypted.iso"),
            Ps3Key([0x24u8; 16]),
            false,
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Ps3Error::AlreadyDecrypted));
    }

    #[test]
    fn decrypted_path_forces_iso_extension() {
        assert_eq!(
            derive_decrypted_path(Path::new("/games/Disc.iso")),
            PathBuf::from("/games/Disc.decrypted.iso")
        );
    }
}
