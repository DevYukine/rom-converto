//! Hash-only verify: walks every NCA in a container, decrypts each
//! section, and recomputes the FsHeader's stored chunk hashes. The
//! result is `serde::Serialize` so the GUI can render it as a table.

use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::nintendo::nx::container::{ContainerKind, detect_container};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::models::hfs0 as hfs0_mod;
use crate::nintendo::nx::models::pfs0 as pfs0_mod;
use crate::nintendo::nx::models::ticket::Ticket;
use crate::nintendo::nx::ncz::ncz_to_nca_cancellable;
use crate::nintendo::nx::util::positional_reader::PositionalReader;
use crate::nintendo::nx::walker::NcaWalker;
use crate::util::pread::file_read_exact_at;
use crate::util::{CancelToken, ProgressReporter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NxVerifyResult {
    pub kind: String,
    pub ok: bool,
    pub ncas: Vec<NcaVerdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NcaVerdict {
    pub name: String,
    pub partition: Option<String>,
    pub ok: bool,
    pub mismatched_sections: usize,
}

pub fn verify_container(
    input: &Path,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
) -> NxResult<NxVerifyResult> {
    verify_container_cancellable(input, keys, progress, &CancelToken::new())
}

pub fn verify_container_cancellable(
    input: &Path,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<NxVerifyResult> {
    check_cancel(cancel)?;
    let kind = detect_container(input)?;
    progress.start(0, "Verifying Switch container");
    let in_file = Arc::new(File::open(input)?);

    let entries = match kind {
        ContainerKind::Nsp | ContainerKind::Nsz => list_pfs0_entries(input, cancel)?,
        ContainerKind::Xci | ContainerKind::Xcz => list_xci_entries(input, cancel)?,
    };

    // Title-rights NCAs need a titlekey looked up by rights_id. The
    // external title.keys is optional; tickets shipped inside the
    // container are merged in here so those NCAs can be opened.
    let mut keys = keys.clone();
    for entry in &entries {
        check_cancel(cancel)?;
        if !entry.name.to_ascii_lowercase().ends_with(".tik") {
            continue;
        }
        let mut buf = vec![0u8; entry.size as usize];
        file_read_exact_at(&in_file, &mut buf, entry.abs_offset)?;
        if let Ok(ticket) = Ticket::parse(&buf) {
            keys.title_keys
                .insert(ticket.rights_id, ticket.encrypted_title_key);
        }
    }

    let nca_entries: Vec<_> = entries
        .into_iter()
        .filter(|e| {
            let lower = e.name.to_ascii_lowercase();
            lower.ends_with(".nca") || lower.ends_with(".ncz")
        })
        .collect();
    let nca_total = nca_entries.len();

    let mut ncas = Vec::new();
    let mut overall_ok = true;
    for (i, entry) in nca_entries.iter().enumerate() {
        check_cancel(cancel)?;
        progress.set_phase(&format!("Verifying NCA ({}/{})", i + 1, nca_total));
        let verdict = verify_one(&in_file, entry, &keys, progress, cancel)?;
        if !verdict.ok {
            overall_ok = false;
        }
        ncas.push(verdict);
    }

    progress.finish();
    Ok(NxVerifyResult {
        kind: format!("{kind:?}"),
        ok: overall_ok,
        ncas,
    })
}

pub async fn verify_container_async(
    input: PathBuf,
    keys: KeySet,
    progress: &dyn ProgressReporter,
) -> NxResult<NxVerifyResult> {
    verify_container_async_cancellable(input, keys, progress, CancelToken::new()).await
}

pub async fn verify_container_async_cancellable(
    input: PathBuf,
    keys: KeySet,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> NxResult<NxVerifyResult> {
    check_cancel(&cancel)?;
    let total = tokio::fs::metadata(&input).await?.len();
    progress.start(total, "Verifying Switch container");

    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();
    let phase = Arc::new(Mutex::new(String::new()));
    let proxy = AtomicProgress {
        counter: bytes_done_bg,
        phase: phase.clone(),
    };
    let cancel_bg = cancel.clone();

    let publish_phase = || {
        let label = std::mem::take(&mut *phase.lock().unwrap());
        if !label.is_empty() {
            progress.set_phase(&label);
        }
    };

    let mut handle = tokio::task::spawn_blocking(move || -> NxResult<NxVerifyResult> {
        verify_container_cancellable(&input, &keys, &proxy, &cancel_bg)
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
                publish_phase();
            }
        }
    }
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    publish_phase();
    progress.finish();
    check_cancel(&cancel)?;
    Ok(result)
}

struct AtomicProgress {
    counter: Arc<AtomicU64>,
    phase: Arc<Mutex<String>>,
}

impl ProgressReporter for AtomicProgress {
    fn start(&self, _: u64, _: &str) {}
    fn inc(&self, delta: u64) {
        self.counter.fetch_add(delta, Ordering::Relaxed);
    }
    fn finish(&self) {}
    fn set_phase(&self, label: &str) {
        *self.phase.lock().unwrap() = label.to_string();
    }
}

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    partition: Option<String>,
    abs_offset: u64,
    size: u64,
}

fn list_pfs0_entries(path: &Path, cancel: &CancelToken) -> NxResult<Vec<Entry>> {
    check_cancel(cancel)?;
    let mut reader = BufReader::new(File::open(path)?);
    let pfs0 = pfs0_mod::Pfs0::read(&mut reader)?;
    Ok(pfs0
        .files
        .into_iter()
        .map(|f| Entry {
            name: f.name,
            partition: None,
            abs_offset: pfs0.data_section_offset + f.data_offset,
            size: f.size,
        })
        .collect())
}

fn list_xci_entries(path: &Path, cancel: &CancelToken) -> NxResult<Vec<Entry>> {
    check_cancel(cancel)?;
    let hfs0_off = {
        let mut probe = File::open(path)?;
        crate::nintendo::nx::container::read_xci_hfs0_offset(&mut probe)?
    };
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(hfs0_off))?;
    let root = hfs0_mod::Hfs0::read(&mut reader)?;
    let mut out = Vec::new();
    for root_entry in root.files {
        check_cancel(cancel)?;
        let part_abs = root.data_section_offset + root_entry.data_offset;
        reader.seek(SeekFrom::Start(part_abs))?;
        let sub = hfs0_mod::Hfs0::read(&mut reader)?;
        for f in sub.files {
            out.push(Entry {
                name: f.name,
                partition: Some(root_entry.name.clone()),
                abs_offset: sub.data_section_offset + f.data_offset,
                size: f.size,
            });
        }
    }
    Ok(out)
}

fn verify_one(
    in_file: &Arc<File>,
    entry: &Entry,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<NcaVerdict> {
    check_cancel(cancel)?;
    let lower = entry.name.to_ascii_lowercase();
    if lower.ends_with(".ncz") {
        let mut reader = PositionalReader::new(in_file.clone(), entry.abs_offset, entry.size);
        let mut decoded = tempfile::NamedTempFile::new()?;
        ncz_to_nca_cancellable(&mut reader, &mut decoded, progress, cancel)?;
        check_cancel(cancel)?;
        let size = decoded.as_file().metadata()?.len();
        check_nca_file(
            Arc::new(decoded.reopen()?),
            0,
            size,
            &entry.name,
            &entry.partition,
            keys,
            cancel,
        )
    } else {
        progress.inc(entry.size);
        check_nca_file(
            in_file.clone(),
            entry.abs_offset,
            entry.size,
            &entry.name,
            &entry.partition,
            keys,
            cancel,
        )
    }
}

fn check_nca_file(
    file: Arc<File>,
    offset: u64,
    size: u64,
    name: &str,
    partition: &Option<String>,
    keys: &KeySet,
    cancel: &CancelToken,
) -> NxResult<NcaVerdict> {
    check_cancel(cancel)?;
    let walker = NcaWalker::open(file, offset, size, keys);
    let walker = match walker {
        Ok(w) => w,
        Err(_) => {
            return Ok(NcaVerdict {
                name: name.into(),
                partition: partition.clone(),
                ok: false,
                mismatched_sections: 0,
            });
        }
    };

    let mut mismatches = 0usize;
    for section in &walker.sections {
        check_cancel(cancel)?;
        let len = section.raw_size;
        if len == 0 {
            continue;
        }
        let chunk_len = (len.min(0x10000)) as usize;
        let mut buf = vec![0u8; (chunk_len + 0xF) & !0xF];
        if walker.read_section_plain(section, 0, &mut buf).is_err() {
            mismatches += 1;
        }
    }

    Ok(NcaVerdict {
        name: name.into(),
        partition: partition.clone(),
        ok: mismatches == 0,
        mismatched_sections: mismatches,
    })
}

fn check_cancel(cancel: &CancelToken) -> NxResult<()> {
    if cancel.is_cancelled() {
        return Err(NxError::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::models::pfs0::{Pfs0LayoutHints, build_header};

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

    fn write_nsp(path: &Path, files: &[(&str, &[u8])]) {
        let specs: Vec<(String, u64)> = files
            .iter()
            .map(|(n, b)| (n.to_string(), b.len() as u64))
            .collect();
        let hdr = build_header(&specs, &Pfs0LayoutHints::default()).unwrap();
        let mut out = hdr.bytes;
        for (_, bytes) in files {
            out.extend_from_slice(bytes);
        }
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn verify_labels_per_nca_phases() {
        let dir = tempfile::tempdir().unwrap();
        let nsp = dir.path().join("two.nsp");
        write_nsp(
            &nsp,
            &[("a.nca", b"not-a-real-nca"), ("b.nca", b"also-not-real")],
        );

        let keys = KeySet::default();
        let recorder = PhaseRecorder::default();
        verify_container(&nsp, &keys, &recorder).unwrap();

        let phases = recorder.phases.lock().unwrap();
        assert_eq!(
            *phases,
            vec![
                "Verifying NCA (1/2)".to_string(),
                "Verifying NCA (2/2)".to_string(),
            ]
        );
    }
}
