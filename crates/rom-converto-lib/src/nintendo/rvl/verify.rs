//! Wii disc verification.
//!
//! Fast mode (default) checks the RVZ container's stored SHA-1 hashes,
//! including the partition-table hash. `--full` decrypts every partition
//! cluster and recomputes the H0/H1/H2 hash hierarchy, comparing it to the
//! on-disc hash regions to detect tampering or bit rot in the actual data.
//!
//! Scrubbed images (WBFS, scrubbed ISOs) zero-fill unused sectors, and
//! decrypting that filler can never reproduce a valid hash tree. Worse, the
//! H1/H2 levels couple every sector to its group siblings, so one scrubbed
//! sector drags the recomputed regions of intact neighbors off too. A sector
//! therefore only counts as corrupt when its own H0 table (which covers just
//! its payload) differs AND its ciphertext is not one repeated filler byte;
//! everything else is scrubbing fallout, the same call Dolphin's verifier
//! makes.

use crate::nintendo::disc_input::open_disc_input;
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_GROUP_TOTAL_SIZE, WII_SECTOR_SIZE, WII_SECTOR_SIZE_U64,
};
use crate::nintendo::rvl::disc::read_partition_table;
use crate::nintendo::rvl::partition::{
    HASH_REGION_BYTES, PartitionInfo, hash_region, read_and_decrypt_cluster, read_partition_info,
    recompute_hash_regions_into,
};
use crate::nintendo::rvz::verify::{RvzStructuralVerify, verify_rvz_structure};
use crate::util::ProgressReporter;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SAMPLE_CAP: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct RvlVerifyOptions {
    pub full: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RvlVerifyResult {
    pub game_id: String,
    /// Present only for `.rvz` input.
    pub rvz_structure: Option<RvzStructuralVerify>,
    /// Per-partition hash-tree results, `--full` only.
    pub partitions: Vec<RvlPartitionVerify>,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RvlPartitionVerify {
    pub offset: u64,
    pub partition_type: u32,
    pub kind: String,
    pub clusters_checked: u64,
    pub mismatched_clusters: u64,
    /// Clusters whose only failing sectors hold uniform filler
    /// ciphertext (scrubbing); they do not fail the verify.
    #[serde(default)]
    pub scrubbed_clusters: u64,
    /// First few mismatching cluster indices, capped for the report.
    pub sample_bad_clusters: Vec<u64>,
    pub ok: bool,
    pub note: Option<String>,
}

fn partition_kind_name(t: u32) -> &'static str {
    match t {
        0 => "data",
        1 => "update",
        2 => "channel",
        _ => "unknown",
    }
}

pub fn verify_rvl(
    path: &Path,
    options: &RvlVerifyOptions,
    progress: &dyn ProgressReporter,
) -> Result<RvlVerifyResult> {
    let rvz_structure = verify_rvz_structure(path).ok();

    let mut reader =
        open_disc_input(path).with_context(|| format!("rvl verify: open {}", path.display()))?;
    let game_id = read_game_id(&mut reader)?;

    let mut partitions = Vec::new();
    if options.full {
        let entries = read_partition_table(&mut reader)
            .map_err(|e| anyhow!("rvl verify: partition table: {e}"))?;

        // Resolve every partition's metadata first so the progress bar can be
        // sized to the total cluster count across all partitions.
        let mut infos: Vec<(u64, u32, Option<PartitionInfo>)> = Vec::with_capacity(entries.len());
        for e in &entries {
            let info = read_partition_info(&mut reader, e.offset, e.group, e.partition_type).ok();
            infos.push((e.offset, e.partition_type, info));
        }
        let total_clusters: u64 = infos
            .iter()
            .filter_map(|(_, _, i)| i.as_ref())
            .map(|i| i.cluster_count())
            .sum();
        progress.start(total_clusters, "Verifying Wii hash tree");

        let partition_count = infos.len();
        for (i, (offset, partition_type, info)) in infos.into_iter().enumerate() {
            let kind = partition_kind_name(partition_type).to_string();
            progress.set_phase(&format!(
                "Verifying {kind} partition ({}/{partition_count})",
                i + 1
            ));
            match info {
                Some(info) => partitions.push(verify_partition(&mut reader, &info, kind, progress)),
                None => partitions.push(RvlPartitionVerify {
                    offset,
                    partition_type,
                    kind,
                    clusters_checked: 0,
                    mismatched_clusters: 0,
                    scrubbed_clusters: 0,
                    sample_bad_clusters: Vec::new(),
                    ok: false,
                    note: Some("could not read partition info (bad ticket or common key)".into()),
                }),
            }
        }
        progress.finish();
    }

    let ok =
        rvz_structure.as_ref().map(|s| s.ok()).unwrap_or(true) && partitions.iter().all(|p| p.ok);

    Ok(RvlVerifyResult {
        game_id,
        rvz_structure,
        partitions,
        ok,
    })
}

fn verify_partition<R: Read + Seek>(
    reader: &mut R,
    info: &PartitionInfo,
    kind: String,
    progress: &dyn ProgressReporter,
) -> RvlPartitionVerify {
    let cluster_count = info.cluster_count();
    let data_start = info.data_start();
    let mut scratch = vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP];
    let mut checked = 0u64;
    let mut mismatched = 0u64;
    let mut scrubbed = 0u64;
    let mut sample = Vec::new();
    let mut note = None;

    for cluster_idx in 0..cluster_count {
        let cluster_enc_start = cluster_idx * WII_GROUP_TOTAL_SIZE;
        if reader
            .seek(SeekFrom::Start(data_start + cluster_enc_start))
            .is_err()
        {
            note = Some(format!(
                "seek failed at cluster {cluster_idx} (disc truncated)"
            ));
            break;
        }
        let cluster = match read_and_decrypt_cluster(reader, &info.title_key) {
            Ok(c) => c,
            Err(e) => {
                note = Some(format!("cluster {cluster_idx} read/decrypt failed: {e}"));
                break;
            }
        };
        recompute_hash_regions_into(&cluster.payloads, &mut scratch);

        // Only sectors fully within data_size carry real data; the tail of the
        // last cluster is junk padding that legitimately diverges.
        let remaining = info.data_size.saturating_sub(cluster_enc_start);
        let real_sectors = ((remaining.min(WII_GROUP_TOTAL_SIZE) / WII_SECTOR_SIZE_U64) as usize)
            .min(WII_BLOCKS_PER_GROUP);

        let bad_sectors: Vec<usize> = (0..real_sectors)
            .filter(|&s| cluster.on_disc_hash_regions[s] != scratch[s])
            .collect();
        if !bad_sectors.is_empty() {
            // A sector whose own H0 table matches carries an intact
            // payload; its region only diverges because the recomputed
            // H1/H2 cover scrubbed sibling sectors in the group.
            let suspects: Vec<usize> = bad_sectors
                .into_iter()
                .filter(|&s| {
                    cluster.on_disc_hash_regions[s][..hash_region::H0_LEN]
                        != scratch[s][..hash_region::H0_LEN]
                })
                .collect();
            match cluster_corruption_kind(reader, data_start + cluster_enc_start, &suspects) {
                Ok(true) => {
                    mismatched += 1;
                    if sample.len() < SAMPLE_CAP {
                        sample.push(cluster_idx);
                    }
                }
                Ok(false) => scrubbed += 1,
                Err(e) => {
                    note = Some(format!("cluster {cluster_idx} reread failed: {e}"));
                    break;
                }
            }
        }
        checked += 1;
        progress.inc(1);
    }

    let ok = mismatched == 0 && note.is_none();
    RvlPartitionVerify {
        offset: info.partition_offset,
        partition_type: info.partition_type,
        kind,
        clusters_checked: checked,
        mismatched_clusters: mismatched,
        scrubbed_clusters: scrubbed,
        sample_bad_clusters: sample,
        ok,
        note,
    }
}

/// Returns `Ok(true)` when at least one suspect sector holds
/// non-uniform ciphertext (real corruption); all-uniform suspects
/// mean the cluster was scrubbed.
fn cluster_corruption_kind<R: Read + Seek>(
    reader: &mut R,
    cluster_start: u64,
    bad_sectors: &[usize],
) -> std::io::Result<bool> {
    let mut raw = vec![0u8; WII_SECTOR_SIZE];
    for &s in bad_sectors {
        reader.seek(SeekFrom::Start(
            cluster_start + s as u64 * WII_SECTOR_SIZE_U64,
        ))?;
        reader.read_exact(&mut raw)?;
        let first = raw[0];
        if raw.iter().any(|&b| b != first) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_game_id<R: Read + Seek>(reader: &mut R) -> Result<String> {
    reader.seek(SeekFrom::Start(0))?;
    let mut id = [0u8; 6];
    reader.read_exact(&mut id)?;
    let end = id.iter().position(|b| *b == 0).unwrap_or(id.len());
    Ok(id[..end].iter().map(|&b| b as char).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;
    use crate::nintendo::rvz::{RvzCompressOptions, compress_disc};
    use crate::util::{NoProgress, ProgressReporter};
    use std::sync::Mutex;

    #[derive(Default)]
    struct PhaseRecorder {
        phases: Mutex<Vec<String>>,
    }

    impl ProgressReporter for PhaseRecorder {
        fn start(&self, _: u64, _: &str) {}
        fn inc(&self, _: u64) {}
        fn finish(&self) {}
        fn set_phase(&self, label: &str) {
            self.phases.lock().unwrap().push(label.to_string());
        }
    }

    #[test]
    fn full_verify_labels_per_partition_phases() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        std::fs::write(&iso, make_fake_wii_iso_with_partition(2)).unwrap();

        let recorder = PhaseRecorder::default();
        verify_rvl(&iso, &RvlVerifyOptions { full: true }, &recorder).unwrap();

        let phases = recorder.phases.lock().unwrap();
        assert!(!phases.is_empty(), "full verify should emit phase labels");
        assert!(
            phases[0].starts_with("Verifying ") && phases[0].contains(" (1/"),
            "first phase label was {:?}",
            phases[0]
        );
    }

    #[test]
    fn full_verify_on_raw_iso_finds_consistent_hash_tree() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        std::fs::write(&iso, make_fake_wii_iso_with_partition(2)).unwrap();

        let res = verify_rvl(&iso, &RvlVerifyOptions { full: true }, &NoProgress).unwrap();
        assert!(
            !res.partitions.is_empty(),
            "should find at least one partition"
        );
        for p in &res.partitions {
            assert_eq!(
                p.mismatched_clusters, 0,
                "partition {} should verify cleanly; note={:?}",
                p.kind, p.note
            );
            assert!(p.clusters_checked > 0);
            assert!(p.ok);
        }
        assert!(res.ok);
    }

    #[tokio::test]
    async fn fast_verify_on_rvz_checks_structural_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        std::fs::write(&iso, make_fake_wii_iso_with_partition(2)).unwrap();
        compress_disc(&iso, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();

        let fast = verify_rvl(&rvz, &RvlVerifyOptions { full: false }, &NoProgress).unwrap();
        let structural = fast.rvz_structure.expect("rvz input has structural hashes");
        assert!(structural.file_head_hash_ok);
        assert!(structural.disc_hash_ok);
        // A Wii RVZ carries a partition table, so part_hash is checked.
        assert_eq!(structural.part_hash_ok, Some(true));
        assert!(
            fast.partitions.is_empty(),
            "fast mode does not walk partitions"
        );
        assert!(fast.ok);
    }

    #[tokio::test]
    async fn full_verify_on_rvz_walks_partition_hash_tree() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        std::fs::write(&iso, make_fake_wii_iso_with_partition(2)).unwrap();
        compress_disc(&iso, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();

        let res = verify_rvl(&rvz, &RvlVerifyOptions { full: true }, &NoProgress).unwrap();
        assert!(!res.partitions.is_empty());
        for p in &res.partitions {
            assert_eq!(p.mismatched_clusters, 0, "note={:?}", p.note);
        }
        assert!(res.ok);
    }
}
