//! Wii U content integrity verification.
//!
//! For NUS directories and WUD/WUX discs, every raw-mode content is decrypted
//! and its SHA-1 compared against the matching TMD content hash (the TMD
//! stores SHA-1 in the first 20 bytes of the 32-byte hash field for raw
//! contents), mirroring the per-NCA hash check in
//! [`crate::nintendo::nx::verify`].
//!
//! Hashed-mode content (TMD `HASHED` flag / FST `HashInterleaved`) is reported
//! as *skipped* rather than checked: its TMD hash covers the H3 hash tree, not
//! the content bytes, so a whole-content digest would not match. `.wua`
//! and loadiine inputs hold already-decrypted files with no TMD to verify
//! against, so they get a structural readability check only.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::nintendo::wup::crypto::aes_cbc_decrypt_in_place;
use crate::nintendo::wup::disc::compress::{
    content_partitions_with_index, find_matching_title, parse_si_titles, plan_partition,
};
use crate::nintendo::wup::disc::partition::PartitionContentSource;
use crate::nintendo::wup::disc::{load_disc_key, open_disc, parse_partition_table};
use crate::nintendo::wup::error::WupError;
use crate::nintendo::wup::models::WupTmd;
use crate::nintendo::wup::nus::content_stream::{ContentBytesSource, raw_content_iv};
use crate::nintendo::wup::nus::fst_parser::{FstClusterHashMode, VirtualFs};
use crate::nintendo::wup::nus::source::NusSource;
use crate::nintendo::wup::nus::ticket_parser::TitleKey;
use crate::util::{CancelToken, ProgressReporter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WupVerifyResult {
    pub kind: String,
    pub ok: bool,
    pub titles: Vec<TitleVerdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleVerdict {
    pub title_id: u64,
    pub title_id_hex: String,
    pub ok: bool,
    /// Contents whose SHA-1 matched the TMD hash.
    pub verified_content: usize,
    /// Contents whose SHA-1 differed from the TMD hash.
    pub mismatched_content: usize,
    /// Contents that could not be hash-checked (hashed-mode, unknown mode,
    /// or no TMD as with `.wua`/loadiine).
    pub skipped_content: usize,
}

pub fn verify_wup(
    input: &Path,
    key_override: Option<&Path>,
    progress: &dyn ProgressReporter,
) -> Result<WupVerifyResult> {
    verify_wup_cancellable(input, key_override, progress, &CancelToken::new())
}

pub fn verify_wup_cancellable(
    input: &Path,
    key_override: Option<&Path>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<WupVerifyResult> {
    check_cancel(cancel)?;
    if input.is_file() {
        let ext = input
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        return match ext.as_str() {
            "wua" => structural_verdict(input, None, "wua", cancel),
            "wud" | "wux" => verify_disc(input, key_override, progress, cancel),
            other => Err(anyhow!(
                "wup verify: unsupported file type .{other}; expected .wua, .wud, or .wux"
            )),
        };
    }
    if is_loadiine_dir(input) {
        return structural_verdict(input, None, "loadiine", cancel);
    }
    verify_nus(input, progress, cancel)
}

pub async fn verify_wup_async(
    input: PathBuf,
    key_override: Option<PathBuf>,
    progress: &dyn ProgressReporter,
) -> Result<WupVerifyResult> {
    verify_wup_async_cancellable(input, key_override, progress, CancelToken::new()).await
}

pub async fn verify_wup_async_cancellable(
    input: PathBuf,
    key_override: Option<PathBuf>,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<WupVerifyResult> {
    check_cancel(&cancel)?;
    let total = tokio::fs::metadata(&input)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    progress.start(total, "Verifying Wii U content");

    let bytes_done = Arc::new(AtomicU64::new(0));
    let proxy = AtomicProgress {
        counter: bytes_done.clone(),
    };
    let cancel_bg = cancel.clone();

    let mut handle = tokio::task::spawn_blocking(move || -> Result<WupVerifyResult> {
        verify_wup_cancellable(&input, key_override.as_deref(), &proxy, &cancel_bg)
    });

    let result;
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(r) => {
                result = r??;
                break;
            }
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    }
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();
    check_cancel(&cancel)?;
    Ok(result)
}

/// Decrypt each raw-mode content and compare its SHA-1 to the TMD hash.
/// Returns `(verified, mismatched, skipped)` counts.
#[cfg(test)]
fn verify_title_contents(
    tmd: &WupTmd,
    fs: &VirtualFs,
    title_key: &TitleKey,
    source: &mut dyn ContentBytesSource,
    progress: &dyn ProgressReporter,
) -> Result<(usize, usize, usize)> {
    verify_title_contents_cancellable(tmd, fs, title_key, source, progress, &CancelToken::new())
}

fn verify_title_contents_cancellable(
    tmd: &WupTmd,
    fs: &VirtualFs,
    title_key: &TitleKey,
    source: &mut dyn ContentBytesSource,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<(usize, usize, usize)> {
    let mut verified = 0;
    let mut mismatched = 0;
    let mut skipped = 0;

    for (cluster_index, cluster) in fs.clusters.iter().enumerate() {
        check_cancel(cancel)?;
        let Some(tmd_entry) = tmd.content_by_index(cluster_index as u16) else {
            skipped += 1;
            continue;
        };
        match cluster.hash_mode {
            FstClusterHashMode::Raw | FstClusterHashMode::RawStream => {
                let mut hasher = Sha1::new();
                let mut iv = raw_content_iv(cluster_index as u16);
                let mut remaining = tmd_entry.size;
                source
                    .visit_encrypted_content(tmd_entry.content_id, &mut |encrypted| {
                        if cancel.is_cancelled() {
                            return Err(WupError::Cancelled);
                        }
                        if !encrypted.len().is_multiple_of(16) {
                            return Err(WupError::AesError(format!(
                                "raw content length {} is not a multiple of 16",
                                encrypted.len()
                            )));
                        }
                        let next_iv: [u8; 16] = encrypted[encrypted.len() - 16..]
                            .try_into()
                            .expect("chunk is at least one AES block");
                        aes_cbc_decrypt_in_place(&title_key.0, &iv, encrypted)?;
                        let take = remaining.min(encrypted.len() as u64) as usize;
                        hasher.update(&encrypted[..take]);
                        remaining -= take as u64;
                        iv = next_iv;
                        Ok(())
                    })
                    .with_context(|| format!("read/decrypt content {}", tmd_entry.content_id))?;
                check_cancel(cancel)?;
                let digest: [u8; 20] = hasher.finalize().into();
                if digest == tmd_entry.hash[..20] {
                    verified += 1;
                } else {
                    mismatched += 1;
                }
                progress.inc(tmd_entry.size);
            }
            FstClusterHashMode::HashInterleaved | FstClusterHashMode::Unknown(_) => {
                skipped += 1;
                progress.inc(tmd_entry.size);
            }
        }
    }
    Ok((verified, mismatched, skipped))
}

fn verify_nus(
    dir: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<WupVerifyResult> {
    check_cancel(cancel)?;
    let src = NusSource::open(dir).map_err(|e| anyhow!("wup verify: open NUS: {e}"))?;
    let title_id = src.tmd().title_id;
    let tmd = src.tmd().clone();
    let title_key = src.title_key();
    let fs = src
        .virtual_fs()
        .map_err(|e| anyhow!("wup verify: load FST: {e}"))?;
    let mut content_source = src.content_source();

    let (verified, mismatched, skipped) = verify_title_contents_cancellable(
        &tmd,
        &fs,
        &title_key,
        &mut content_source,
        progress,
        cancel,
    )?;
    let ok = mismatched == 0;
    Ok(WupVerifyResult {
        kind: "nus".to_string(),
        ok,
        titles: vec![TitleVerdict {
            title_id,
            title_id_hex: format!("{:016X}", title_id),
            ok,
            verified_content: verified,
            mismatched_content: mismatched,
            skipped_content: skipped,
        }],
    })
}

fn verify_disc(
    path: &Path,
    key_override: Option<&Path>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<WupVerifyResult> {
    check_cancel(cancel)?;
    let mut disc = open_disc(path).map_err(|e| anyhow!("wup verify: open disc: {e}"))?;
    let key = load_disc_key(path, key_override).map_err(|e| anyhow!("wup verify: {e}"))?;
    let table = parse_partition_table(&mut *disc, &key)
        .map_err(|e| anyhow!("wup verify: partition table: {e}"))?;
    let si = table
        .find_si()
        .cloned()
        .ok_or_else(|| anyhow!("wup verify: disc has no SI partition"))?;
    let si_titles = parse_si_titles(&mut *disc, &si, &key)
        .map_err(|e| anyhow!("wup verify: SI titles: {e}"))?;

    let content_partitions: Vec<_> = content_partitions_with_index(&table)
        .map(|(i, p)| (i, p.clone()))
        .collect();
    let mut titles = Vec::new();
    let mut overall = true;

    for (toc_index, partition) in &content_partitions {
        check_cancel(cancel)?;
        let Some(si_title) = find_matching_title(&si_titles, *toc_index) else {
            continue;
        };
        let plan = plan_partition(&mut *disc, partition, si_title)
            .map_err(|e| anyhow!("wup verify: plan {}: {e}", partition.name))?;
        let title_id = plan.title_id;
        let mut source = PartitionContentSource::new(&mut *disc, plan.locations);
        let (verified, mismatched, skipped) = verify_title_contents_cancellable(
            &plan.tmd,
            &plan.fs,
            &plan.title_key,
            &mut source,
            progress,
            cancel,
        )?;
        let ok = mismatched == 0;
        overall &= ok;
        titles.push(TitleVerdict {
            title_id,
            title_id_hex: format!("{:016X}", title_id),
            ok,
            verified_content: verified,
            mismatched_content: mismatched,
            skipped_content: skipped,
        });
    }

    if titles.is_empty() {
        return Err(anyhow!(
            "wup verify: disc has no verifiable content partitions"
        ));
    }
    Ok(WupVerifyResult {
        kind: "disc".to_string(),
        ok: overall,
        titles,
    })
}

/// `.wua` / loadiine inputs hold already-decrypted files with no TMD to hash
/// against, so verification is a structural readability check: the title
/// parses, but no content hashes are compared.
fn structural_verdict(
    path: &Path,
    key_override: Option<&Path>,
    kind_label: &str,
    cancel: &CancelToken,
) -> Result<WupVerifyResult> {
    check_cancel(cancel)?;
    let info = crate::nintendo::wup::info::read_info(path, key_override)
        .map_err(|e| anyhow!("wup verify: parse {kind_label}: {e}"))?;
    check_cancel(cancel)?;
    let mut titles: Vec<TitleVerdict> = info
        .bundled_titles
        .iter()
        .map(|b| TitleVerdict {
            title_id: b.title_id,
            title_id_hex: b.title_id_hex.clone(),
            ok: true,
            verified_content: 0,
            mismatched_content: 0,
            skipped_content: 0,
        })
        .collect();
    if titles.is_empty() {
        titles.push(TitleVerdict {
            title_id: info.title_id,
            title_id_hex: info.title_id_hex.clone(),
            ok: true,
            verified_content: 0,
            mismatched_content: 0,
            skipped_content: 0,
        });
    }
    Ok(WupVerifyResult {
        kind: format!("{kind_label} (structural; content hashes not checked)"),
        ok: true,
        titles,
    })
}

fn check_cancel(cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(WupError::Cancelled.into());
    }
    Ok(())
}

fn is_loadiine_dir(dir: &Path) -> bool {
    dir.join("code/app.xml").is_file()
        && dir.join("meta/meta.xml").is_file()
        && dir.join("code/cos.xml").is_file()
}

struct AtomicProgress {
    counter: Arc<AtomicU64>,
}

impl ProgressReporter for AtomicProgress {
    fn start(&self, _: u64, _: &str) {}
    fn inc(&self, delta: u64) {
        self.counter.fetch_add(delta, Ordering::Relaxed);
    }
    fn finish(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::models::tmd::{TmdContentEntry, TmdContentFlags};
    use crate::nintendo::wup::nus::content_stream::decrypt_raw_content;
    use crate::nintendo::wup::nus::fst_parser::{FstCluster, VirtualFile};
    use crate::util::NoProgress;

    struct MemSource {
        bytes: Vec<u8>,
        content_id: u32,
    }
    impl ContentBytesSource for MemSource {
        fn read_encrypted_content(
            &mut self,
            content_id: u32,
        ) -> crate::nintendo::wup::WupResult<Vec<u8>> {
            assert_eq!(content_id, self.content_id);
            Ok(self.bytes.clone())
        }
    }

    struct CancellingSource {
        cancel: CancelToken,
    }

    impl ContentBytesSource for CancellingSource {
        fn read_encrypted_content(&mut self, _: u32) -> crate::nintendo::wup::WupResult<Vec<u8>> {
            unreachable!()
        }

        fn visit_encrypted_content(
            &mut self,
            _: u32,
            visitor: &mut dyn FnMut(&mut [u8]) -> crate::nintendo::wup::WupResult<()>,
        ) -> crate::nintendo::wup::WupResult<()> {
            visitor(&mut [0u8; 16])?;
            self.cancel.cancel();
            visitor(&mut [0u8; 16])
        }
    }

    fn fs_with_one_cluster(hash_mode: FstClusterHashMode) -> VirtualFs {
        VirtualFs {
            offset_factor: 1,
            hash_is_disabled: false,
            clusters: vec![FstCluster {
                offset: 0,
                size: 64,
                owner_title_id: 0,
                group_id: 0,
                hash_mode,
            }],
            files: vec![VirtualFile {
                path: "code/app.xml".to_string(),
                cluster_index: 0,
                file_offset: 0,
                file_size: 64,
                is_shared: false,
            }],
        }
    }

    fn tmd_with_one_content(hash: [u8; 32]) -> WupTmd {
        WupTmd {
            signature_type: 0,
            tmd_version: 1,
            title_id: 0x0005_0000_1010_1000,
            title_type: 0,
            group_id: 0,
            access_rights: 0,
            title_version: 0,
            boot_index: 0,
            content_info_hash: [0u8; 32],
            contents: vec![TmdContentEntry {
                content_id: 7,
                index: 0,
                flags: TmdContentFlags::ENCRYPTED,
                size: 64,
                hash,
            }],
        }
    }

    #[test]
    fn raw_content_matching_tmd_hash_verifies() {
        let key = TitleKey([0x42u8; 16]);
        let encrypted = vec![0u8; 64];
        let decrypted = decrypt_raw_content(encrypted.clone(), &key, 0).unwrap();
        let mut hash = [0u8; 32];
        let digest: [u8; 20] = Sha1::digest(&decrypted).into();
        hash[..20].copy_from_slice(&digest);

        let fs = fs_with_one_cluster(FstClusterHashMode::Raw);
        let tmd = tmd_with_one_content(hash);
        let mut src = MemSource {
            bytes: encrypted,
            content_id: 7,
        };
        let (v, m, s) = verify_title_contents(&tmd, &fs, &key, &mut src, &NoProgress).unwrap();
        assert_eq!((v, m, s), (1, 0, 0));
    }

    #[test]
    fn raw_content_wrong_tmd_hash_mismatches() {
        let key = TitleKey([0x42u8; 16]);
        let encrypted = vec![0u8; 64];
        let fs = fs_with_one_cluster(FstClusterHashMode::Raw);
        let tmd = tmd_with_one_content([0xFFu8; 32]);
        let mut src = MemSource {
            bytes: encrypted,
            content_id: 7,
        };
        let (v, m, s) = verify_title_contents(&tmd, &fs, &key, &mut src, &NoProgress).unwrap();
        assert_eq!((v, m, s), (0, 1, 0));
    }

    #[test]
    fn hashed_mode_content_is_skipped_not_failed() {
        let key = TitleKey([0x42u8; 16]);
        let fs = fs_with_one_cluster(FstClusterHashMode::HashInterleaved);
        let tmd = tmd_with_one_content([0u8; 32]);
        let mut src = MemSource {
            bytes: vec![0u8; 64],
            content_id: 7,
        };
        let (v, m, s) = verify_title_contents(&tmd, &fs, &key, &mut src, &NoProgress).unwrap();
        assert_eq!((v, m, s), (0, 0, 1));
    }

    #[test]
    fn raw_content_stops_between_streamed_chunks() {
        let cancel = CancelToken::new();
        let mut source = CancellingSource {
            cancel: cancel.clone(),
        };
        let result = verify_title_contents_cancellable(
            &tmd_with_one_content([0u8; 32]),
            &fs_with_one_cluster(FstClusterHashMode::Raw),
            &TitleKey([0u8; 16]),
            &mut source,
            &NoProgress,
            &cancel,
        );
        assert!(matches!(
            result.unwrap_err().downcast_ref::<WupError>(),
            Some(WupError::Cancelled)
        ));
    }
}
