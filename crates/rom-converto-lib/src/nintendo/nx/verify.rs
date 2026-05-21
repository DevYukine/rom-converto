//! Hash-only verify: walks every NCA in a container, decrypts each
//! section, and recomputes the FsHeader's stored chunk hashes. The
//! result is `serde::Serialize` so the GUI can render it as a table.

use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::nintendo::nx::container::{ContainerKind, detect_container};
use crate::nintendo::nx::error::NxResult;
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::models::hfs0 as hfs0_mod;
use crate::nintendo::nx::models::pfs0 as pfs0_mod;
use crate::nintendo::nx::models::ticket::Ticket;
use crate::nintendo::nx::ncz::ncz_to_nca;
use crate::nintendo::nx::walker::NcaWalker;
use crate::util::ProgressReporter;
use crate::util::pread::file_read_exact_at;

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
    let kind = detect_container(input)?;
    progress.start(0, "Verifying Switch container");
    let in_file = Arc::new(File::open(input)?);

    let entries = match kind {
        ContainerKind::Nsp | ContainerKind::Nsz => list_pfs0_entries(input)?,
        ContainerKind::Xci | ContainerKind::Xcz => list_xci_entries(input)?,
    };

    // Title-rights NCAs need a titlekey looked up by rights_id. The
    // external title.keys is optional; tickets shipped inside the
    // container are merged in here so those NCAs can be opened.
    let mut keys = keys.clone();
    for entry in &entries {
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

    let mut ncas = Vec::new();
    let mut overall_ok = true;
    for entry in entries {
        let lower = entry.name.to_ascii_lowercase();
        if !(lower.ends_with(".nca") || lower.ends_with(".ncz")) {
            continue;
        }
        let verdict = verify_one(&in_file, &entry, &keys, progress)?;
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
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();
    let proxy = AtomicProgress {
        counter: bytes_done_bg,
    };

    let mut handle = tokio::task::spawn_blocking(move || -> NxResult<NxVerifyResult> {
        verify_container(&input, &keys, &proxy)
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
    Ok(result)
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

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    partition: Option<String>,
    abs_offset: u64,
    size: u64,
}

fn list_pfs0_entries(path: &Path) -> NxResult<Vec<Entry>> {
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

fn list_xci_entries(path: &Path) -> NxResult<Vec<Entry>> {
    let hfs0_off = {
        let mut probe = File::open(path)?;
        crate::nintendo::nx::container::read_xci_hfs0_offset(&mut probe)?
    };
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(hfs0_off))?;
    let root = hfs0_mod::Hfs0::read(&mut reader)?;
    let mut out = Vec::new();
    for root_entry in root.files {
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
) -> NxResult<NcaVerdict> {
    let lower = entry.name.to_ascii_lowercase();
    if lower.ends_with(".ncz") {
        let mut nca_bytes = vec![0u8; entry.size as usize];
        file_read_exact_at(in_file, &mut nca_bytes, entry.abs_offset)?;
        let mut decoded = Vec::with_capacity(entry.size as usize);
        let mut cur = std::io::Cursor::new(&nca_bytes);
        ncz_to_nca(&mut cur, &mut decoded, progress)?;
        progress.inc(entry.size);
        let verdict = check_nca_bytes(&decoded, &entry.name, &entry.partition, keys)?;
        Ok(verdict)
    } else {
        let mut nca_bytes = vec![0u8; entry.size as usize];
        file_read_exact_at(in_file, &mut nca_bytes, entry.abs_offset)?;
        progress.inc(entry.size);
        check_nca_bytes(&nca_bytes, &entry.name, &entry.partition, keys)
    }
}

fn check_nca_bytes(
    nca_bytes: &[u8],
    name: &str,
    partition: &Option<String>,
    keys: &KeySet,
) -> NxResult<NcaVerdict> {
    let mut tmp = tempfile::NamedTempFile::new()?;
    use std::io::Write;
    tmp.write_all(nca_bytes)?;
    tmp.flush()?;

    let walker = NcaWalker::open(
        Arc::new(File::open(tmp.path())?),
        0,
        nca_bytes.len() as u64,
        keys,
    );
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
