//! Super NSP / super XCI merge: fold a base plus its update and DLC
//! containers into a single PFS0 (or gamecard secure partition). NCAs
//! are copied byte-verbatim; only the meta NCA is decrypted, to read the
//! CNMT and pick which content to keep. The merged container's outer
//! signatures no longer validate, so CFW installers reject it.

use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::nintendo::nx::container::{
    ContainerKind, ContainerListing, detect_container, list_container,
};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::meta::{merge_inline_tickets, read_meta_cnmt};
use crate::nintendo::nx::models::cnmt::Cnmt;
use crate::nintendo::nx::models::hfs0::{
    self as hfs0_mod, DEFAULT_HASHED_REGION, Hfs0FileSpec, Hfs0LayoutHints, hash_first_chunk,
};
use crate::nintendo::nx::models::xci::{MEDIA_UNIT, XCI_PREFIX_SIZE, build_xci_prefix};
use crate::nintendo::nx::util::{Pfs0Source, copy_range, write_pfs0_from_sources};
use crate::util::pread::file_read_exact_at;
use crate::util::{
    AtomicProgress, CancelToken, ProgressReporter, await_with_progress_cancel, publish_temp,
    scratch_output_path,
};

/// Emitted once per merge. The exact wording is part of the feature
/// contract, do not paraphrase.
pub const SIGNATURE_WARNING: &str = "Merged containers fail signature verification; CFW installers reject them unless signature checks are disabled (not advised). Intended for emulators.";

/// Output container format for a merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NxMergeFormat {
    Nsp,
    Xci,
}

/// Merges `inputs` (a base container plus its updates and DLC) into one
/// super NSP or XCI at `output`, copying NCAs byte-verbatim.
///
/// # Errors
/// Returns [`NxError::NoInputs`] for an empty `inputs`,
/// [`NxError::OutputIsInput`] when `output` is one of them, and an error
/// if an input is a compressed container, yields no CNMT content,
/// references an NCA it does not contain, or (for [`NxMergeFormat::Xci`])
/// is an NSP rather than an XCI.
pub fn merge_containers(
    inputs: &[PathBuf],
    output: &Path,
    format: NxMergeFormat,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    validate_merge_paths(inputs, output)?;
    let sel = select_content(inputs, keys)?;
    write_selection(&sel, output, format, progress, cancel)?;
    progress.warn(SIGNATURE_WARNING);
    Ok(())
}

fn write_selection(
    sel: &Selection,
    output: &Path,
    format: NxMergeFormat,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    match format {
        NxMergeFormat::Nsp => {
            let sources: Vec<Pfs0Source> = sel
                .nca_order
                .iter()
                .chain(&sel.tik_cert_order)
                .map(|r| sel.source(r))
                .collect();
            write_pfs0_from_sources(output, &sources, progress, cancel)
        }
        NxMergeFormat::Xci => write_super_xci(sel, output, progress, cancel),
    }
}

fn validate_merge_paths(inputs: &[PathBuf], output: &Path) -> NxResult<()> {
    if inputs.is_empty() {
        return Err(NxError::NoInputs);
    }
    if inputs.iter().any(|input| is_same_file(input, output)) {
        return Err(NxError::OutputIsInput(output.to_path_buf()));
    }
    Ok(())
}

/// Canonicalization fails for an output that does not exist yet, which is
/// the common case, so that falls back to a literal comparison.
fn is_same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Merges `inputs` into `output` off the async runtime, reporting progress
/// and honoring `cancel` through both the content selection and the write,
/// which goes to a scratch file published only on success.
///
/// # Errors
/// Same as [`merge_containers`], plus [`NxError::Cancelled`] when `cancel`
/// fires.
pub async fn merge_containers_async_cancellable(
    inputs: Vec<PathBuf>,
    output: PathBuf,
    format: NxMergeFormat,
    keys: KeySet,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> NxResult<()> {
    validate_merge_paths(&inputs, &output)?;
    let sel = tokio::task::spawn_blocking(move || select_content(&inputs, &keys)).await??;
    if cancel.is_cancelled() {
        return Err(NxError::Cancelled);
    }
    // A super XCI carries only the NCAs; tickets and certs are dropped.
    let total: u64 = match format {
        NxMergeFormat::Nsp => sel
            .nca_order
            .iter()
            .chain(&sel.tik_cert_order)
            .map(|r| r.size)
            .sum(),
        NxMergeFormat::Xci => sel.nca_order.iter().map(|r| r.size).sum(),
    };
    let bytes_done = Arc::new(AtomicU64::new(0));
    progress.start(total, "Merging Switch containers");
    let proxy = AtomicProgress {
        counter: bytes_done.clone(),
    };

    let write_path = scratch_output_path(&output)?;
    let write_owned = write_path.to_path_buf();
    let cancel_bg = cancel.clone();
    let handle = tokio::task::spawn_blocking(move || -> NxResult<()> {
        write_selection(&sel, &write_owned, format, &proxy, &cancel_bg)
    });

    let cleanup = {
        let write_path = write_path.to_path_buf();
        move || -> NxError {
            let _ = std::fs::remove_file(&write_path);
            NxError::Cancelled
        }
    };
    if let Err(err) =
        await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await
    {
        let _ = tokio::fs::remove_file(&write_path).await;
        return Err(err);
    }
    publish_temp(write_path, &output, true)?;
    progress.warn(SIGNATURE_WARNING);
    Ok(())
}

fn write_super_xci(
    sel: &Selection,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    for st in &sel.states {
        if !st.kind.is_xci() {
            return Err(NxError::XciMergeRequiresXciInputs(st.path.clone()));
        }
    }

    let secure_sources: Vec<Pfs0Source> = sel.nca_order.iter().map(|r| sel.source(r)).collect();

    let mut secure_specs = Vec::with_capacity(secure_sources.len());
    for s in &secure_sources {
        let take = (DEFAULT_HASHED_REGION as u64).min(s.size) as usize;
        let mut head = vec![0u8; take];
        file_read_exact_at(&s.file, &mut head, s.abs_offset)?;
        secure_specs.push(Hfs0FileSpec {
            name: s.name.clone(),
            size: s.size,
            sha256: hash_first_chunk(&head, DEFAULT_HASHED_REGION),
            hashed_region_size: DEFAULT_HASHED_REGION,
        });
    }
    // The root entry hashes the partition's first 0x200 bytes, so the
    // secure header must fill at least a whole media unit and leave the
    // NCA data behind it 0x200-aligned.
    let natural_len = hfs0_mod::build_header(&secure_specs, &Hfs0LayoutHints::default())?
        .bytes
        .len();
    let secure_header = hfs0_mod::build_header(
        &secure_specs,
        &Hfs0LayoutHints {
            target_total_header_size: Some(round_up_media_unit(natural_len)),
            first_file_data_offset: 0,
        },
    )?;

    // Empty update/normal partitions: bare 0x200-byte HFS0 headers.
    let stub = hfs0_mod::build_header(
        &[],
        &Hfs0LayoutHints {
            target_total_header_size: Some(MEDIA_UNIT as usize),
            first_file_data_offset: 0,
        },
    )?;

    let nca_bytes: u64 = secure_specs.iter().map(|s| s.size).sum();
    let secure_unpadded = secure_header.bytes.len() as u64 + nca_bytes;
    let secure_total = secure_unpadded.div_ceil(MEDIA_UNIT) * MEDIA_UNIT;

    let root_specs = vec![
        stub_root_spec("update", &stub.bytes),
        stub_root_spec("normal", &stub.bytes),
        Hfs0FileSpec {
            name: "secure".into(),
            size: secure_total,
            sha256: hash_first_chunk(&secure_header.bytes, DEFAULT_HASHED_REGION),
            hashed_region_size: DEFAULT_HASHED_REGION,
        },
    ];
    // Pad the root header to a media unit so the data section (and thus
    // every partition) starts 0x200-aligned.
    let root_header = hfs0_mod::build_header(
        &root_specs,
        &Hfs0LayoutHints {
            target_total_header_size: Some(MEDIA_UNIT as usize),
            first_file_data_offset: 0,
        },
    )?;

    let secure_offset =
        XCI_PREFIX_SIZE as u64 + root_header.bytes.len() as u64 + stub.bytes.len() as u64 * 2;

    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)?;
    out.write_all(&vec![0u8; XCI_PREFIX_SIZE])?; // placeholder prefix
    out.write_all(&root_header.bytes)?;
    out.write_all(&stub.bytes)?; // update
    out.write_all(&stub.bytes)?; // normal
    out.write_all(&secure_header.bytes)?;
    for s in &secure_sources {
        copy_range(&s.file, s.abs_offset, s.size, &mut out, progress, cancel)?;
    }
    let pad = (secure_total - secure_unpadded) as usize;
    if pad > 0 {
        out.write_all(&vec![0u8; pad])?;
    }

    let total_size = out.stream_position()?;
    let prefix = build_xci_prefix(secure_offset, total_size, &root_header.bytes);
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&prefix)?;
    out.flush()?;
    Ok(())
}

/// Rounds an HFS0 header length up to a whole media unit, never below one.
pub(super) fn round_up_media_unit(len: usize) -> usize {
    let unit = MEDIA_UNIT as usize;
    len.max(1).div_ceil(unit) * unit
}

fn stub_root_spec(name: &str, header_bytes: &[u8]) -> Hfs0FileSpec {
    Hfs0FileSpec {
        name: name.into(),
        size: header_bytes.len() as u64,
        sha256: hash_first_chunk(header_bytes, DEFAULT_HASHED_REGION),
        hashed_region_size: DEFAULT_HASHED_REGION,
    }
}

struct InputState {
    path: PathBuf,
    kind: ContainerKind,
    file: Arc<File>,
    listing: ContainerListing,
    keys: KeySet,
}

struct SourceRef {
    src_index: usize,
    abs_offset: u64,
    size: u64,
    name: String,
}

struct Selection {
    states: Vec<InputState>,
    nca_order: Vec<SourceRef>,
    tik_cert_order: Vec<SourceRef>,
}

impl Selection {
    fn source(&self, r: &SourceRef) -> Pfs0Source {
        Pfs0Source {
            file: self.states[r.src_index].file.clone(),
            abs_offset: r.abs_offset,
            size: r.size,
            name: r.name.clone(),
        }
    }
}

struct Kept {
    src_index: usize,
    cnmt: Cnmt,
    cnmt_name: String,
    cnmt_offset: u64,
    cnmt_size: u64,
}

fn select_content(inputs: &[PathBuf], keys: &KeySet) -> NxResult<Selection> {
    let mut states = Vec::with_capacity(inputs.len());
    for path in inputs {
        let kind = detect_container(path)?;
        if kind.is_compressed() {
            return Err(NxError::CompressedInputUnsupported(path.clone()));
        }
        let listing = list_container(path)?;
        let mut keys_local = keys.clone();
        merge_inline_tickets(path, &listing, &mut keys_local);
        let file = Arc::new(File::open(path)?);
        states.push(InputState {
            path: path.clone(),
            kind,
            file,
            listing,
            keys: keys_local,
        });
    }

    // Keep the highest-version CNMT per (title_id, content_type).
    let mut best: BTreeMap<(u64, u8), Kept> = BTreeMap::new();
    for (i, st) in states.iter().enumerate() {
        let mut found = 0usize;
        for entry in &st.listing.entries {
            if !entry.name.to_ascii_lowercase().ends_with(".cnmt.nca") {
                continue;
            }
            let cnmt = read_meta_cnmt(st.file.clone(), entry.abs_offset, entry.size, &st.keys)?;
            found += 1;
            let key = (cnmt.title_id, cnmt.content_type);
            let version = cnmt.version;
            let cand = Kept {
                src_index: i,
                cnmt,
                cnmt_name: entry.name.clone(),
                cnmt_offset: entry.abs_offset,
                cnmt_size: entry.size,
            };
            match best.get(&key) {
                Some(existing) if existing.cnmt.version >= version => {}
                _ => {
                    best.insert(key, cand);
                }
            }
        }
        if found == 0 {
            return Err(NxError::NoContentInInput(st.path.clone()));
        }
    }

    let mut nca_order = Vec::new();
    let mut seen = HashSet::new();
    for kept in best.values() {
        if seen.insert(nca_dedup_key(&kept.cnmt_name)) {
            nca_order.push(SourceRef {
                src_index: kept.src_index,
                abs_offset: kept.cnmt_offset,
                size: kept.cnmt_size,
                name: kept.cnmt_name.clone(),
            });
        }
        for content in &kept.cnmt.contents {
            let want = format!("{}.nca", hex::encode(content.content_id));
            let Some(entry) = states[kept.src_index]
                .listing
                .entries
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case(&want))
            else {
                return Err(NxError::MissingReferencedNca {
                    input: states[kept.src_index].path.clone(),
                    nca_id: hex::encode(content.content_id),
                });
            };
            if seen.insert(nca_dedup_key(&entry.name)) {
                nca_order.push(SourceRef {
                    src_index: kept.src_index,
                    abs_offset: entry.abs_offset,
                    size: entry.size,
                    name: entry.name.clone(),
                });
            }
        }
    }

    let mut tik_cert_order = Vec::new();
    let mut seen_tc = HashSet::new();
    for ext in [".tik", ".cert"] {
        for (i, st) in states.iter().enumerate() {
            if st.kind.is_xci() {
                continue;
            }
            for entry in &st.listing.entries {
                let lower = entry.name.to_ascii_lowercase();
                if lower.ends_with(ext) && seen_tc.insert(lower.clone()) {
                    tik_cert_order.push(SourceRef {
                        src_index: i,
                        abs_offset: entry.abs_offset,
                        size: entry.size,
                        name: entry.name.clone(),
                    });
                }
            }
        }
    }

    Ok(Selection {
        states,
        nca_order,
        tik_cert_order,
    })
}

/// Dedup key for an NCA filename: the lowercased file name with only a
/// trailing `.nca` stripped, so `<id>.cnmt` stays part of the key and a
/// meta NCA never collides with the content NCA of the same id.
fn nca_dedup_key(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".nca").unwrap_or(&lower).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::models::cnmt::{
        CNMT_TYPE_ADD_ON_CONTENT, CNMT_TYPE_APPLICATION, CNMT_TYPE_PATCH,
    };
    use crate::nintendo::nx::models::pfs0::Pfs0;
    use crate::nintendo::nx::test_fixtures::{build_meta_nca, build_test_nsp, synthetic_keyset};
    use crate::util::NoProgress;
    use std::io::{Cursor, Write};
    use tempfile::{Builder, NamedTempFile};

    fn write_nsp(files: &[(String, Vec<u8>)]) -> NamedTempFile {
        let blob = build_test_nsp(files);
        let mut f = Builder::new().suffix(".nsp").tempfile().unwrap();
        std::io::Write::write_all(&mut f, &blob).unwrap();
        f.flush().unwrap();
        f
    }

    fn nca_id(byte: u8) -> [u8; 16] {
        [byte; 16]
    }

    fn prog(id: [u8; 16], size: usize) -> (String, Vec<u8>) {
        (format!("{}.nca", hex::encode(id)), vec![0xAB; size])
    }

    fn meta_file(
        meta_id: [u8; 16],
        title_id: u64,
        version: u32,
        ty: u8,
        contents: &[[u8; 16]],
    ) -> (String, Vec<u8>) {
        (
            format!("{}.cnmt.nca", hex::encode(meta_id)),
            build_meta_nca(title_id, version, ty, contents),
        )
    }

    fn output_names(path: &Path) -> Vec<String> {
        let blob = std::fs::read(path).unwrap();
        let pfs0 = Pfs0::read(&mut Cursor::new(&blob)).unwrap();
        let mut names: Vec<String> = pfs0.files.iter().map(|f| f.name.clone()).collect();
        names.sort();
        names
    }

    #[test]
    fn merges_base_and_update_into_super_nsp() {
        let base = write_nsp(&[
            meta_file(
                nca_id(0x01),
                0x0100_AAAA_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[nca_id(0x11)],
            ),
            prog(nca_id(0x11), 0x800),
        ]);
        let update = write_nsp(&[
            meta_file(
                nca_id(0x02),
                0x0100_AAAA_0000_0800,
                0x10000,
                CNMT_TYPE_PATCH,
                &[nca_id(0x22)],
            ),
            prog(nca_id(0x22), 0x800),
        ]);

        let out = NamedTempFile::new().unwrap();
        merge_containers(
            &[base.path().to_path_buf(), update.path().to_path_buf()],
            out.path(),
            NxMergeFormat::Nsp,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();

        let mut want = vec![
            format!("{}.cnmt.nca", hex::encode(nca_id(0x01))),
            format!("{}.nca", hex::encode(nca_id(0x11))),
            format!("{}.cnmt.nca", hex::encode(nca_id(0x02))),
            format!("{}.nca", hex::encode(nca_id(0x22))),
        ];
        want.sort();
        assert_eq!(output_names(out.path()), want);
    }

    #[test]
    fn dedups_shared_nca_id() {
        let shared = nca_id(0x33);
        let base = write_nsp(&[
            meta_file(
                nca_id(0x01),
                0x0100_BBBB_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[shared],
            ),
            prog(shared, 0x800),
        ]);
        let update = write_nsp(&[
            meta_file(
                nca_id(0x02),
                0x0100_BBBB_0000_0800,
                0x10000,
                CNMT_TYPE_PATCH,
                &[shared, nca_id(0x44)],
            ),
            prog(shared, 0x800),
            prog(nca_id(0x44), 0x800),
        ]);

        let out = NamedTempFile::new().unwrap();
        merge_containers(
            &[base.path().to_path_buf(), update.path().to_path_buf()],
            out.path(),
            NxMergeFormat::Nsp,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();

        let names = output_names(out.path());
        let shared_name = format!("{}.nca", hex::encode(shared));
        assert_eq!(names.iter().filter(|n| **n == shared_name).count(), 1);
    }

    #[test]
    fn keeps_highest_cnmt_version() {
        let tid = 0x0100_CCCC_0000_0000;
        let v1 = write_nsp(&[
            meta_file(nca_id(0x01), tid, 1, CNMT_TYPE_APPLICATION, &[nca_id(0x11)]),
            prog(nca_id(0x11), 0x800),
        ]);
        let v2 = write_nsp(&[
            meta_file(nca_id(0x02), tid, 2, CNMT_TYPE_APPLICATION, &[nca_id(0x22)]),
            prog(nca_id(0x22), 0x800),
        ]);

        let out = NamedTempFile::new().unwrap();
        merge_containers(
            &[v1.path().to_path_buf(), v2.path().to_path_buf()],
            out.path(),
            NxMergeFormat::Nsp,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();

        let names = output_names(out.path());
        assert!(names.contains(&format!("{}.nca", hex::encode(nca_id(0x22)))));
        assert!(!names.contains(&format!("{}.nca", hex::encode(nca_id(0x11)))));
    }

    #[test]
    fn rejects_compressed_input() {
        let blob = build_test_nsp(&[("x.nca".into(), vec![0u8; 0x40])]);
        let mut f = Builder::new().suffix(".nsz").tempfile().unwrap();
        std::io::Write::write_all(&mut f, &blob).unwrap();
        f.flush().unwrap();

        let out = NamedTempFile::new().unwrap();
        let err = merge_containers(
            &[f.path().to_path_buf()],
            out.path(),
            NxMergeFormat::Nsp,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, NxError::CompressedInputUnsupported(_)));
    }

    #[test]
    fn errors_on_missing_referenced_nca() {
        let base = write_nsp(&[meta_file(
            nca_id(0x01),
            0x0100_FFFF_0000_0000,
            0,
            CNMT_TYPE_APPLICATION,
            &[nca_id(0x11)],
        )]); // no prog(nca_id(0x11), ..) entry: CNMT references a missing NCA.

        let out = NamedTempFile::new().unwrap();
        let err = merge_containers(
            &[base.path().to_path_buf()],
            out.path(),
            NxMergeFormat::Nsp,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, NxError::MissingReferencedNca { .. }));
    }

    #[test]
    fn xci_format_rejects_nsp_input() {
        let base = write_nsp(&[
            meta_file(
                nca_id(0x01),
                0x0100_DDDD_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[nca_id(0x11)],
            ),
            prog(nca_id(0x11), 0x800),
        ]);
        let out = NamedTempFile::new().unwrap();
        let err = merge_containers(
            &[base.path().to_path_buf()],
            out.path(),
            NxMergeFormat::Xci,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, NxError::XciMergeRequiresXciInputs(_)));
    }

    fn write_xci(files: &[(String, Vec<u8>)]) -> NamedTempFile {
        let blob = crate::nintendo::nx::test_fixtures::build_test_xci(files);
        let mut f = Builder::new().suffix(".xci").tempfile().unwrap();
        std::io::Write::write_all(&mut f, &blob).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn super_xci_secure_partition_starts_on_a_media_unit() {
        use crate::nintendo::nx::models::hfs0::Hfs0;
        use sha2::{Digest, Sha256};

        let base = write_xci(&[
            meta_file(
                nca_id(0x01),
                0x0100_1111_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[nca_id(0x11)],
            ),
            prog(nca_id(0x11), 0x800),
        ]);
        let dlc = write_xci(&[
            meta_file(
                nca_id(0x02),
                0x0100_1111_0000_1000,
                0,
                CNMT_TYPE_ADD_ON_CONTENT,
                &[nca_id(0x22)],
            ),
            prog(nca_id(0x22), 0x800),
        ]);

        let out = Builder::new().suffix(".xci").tempfile().unwrap();
        merge_containers(
            &[base.path().to_path_buf(), dlc.path().to_path_buf()],
            out.path(),
            NxMergeFormat::Xci,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();

        let blob = std::fs::read(out.path()).unwrap();
        let mut cur = Cursor::new(&blob);
        cur.seek(SeekFrom::Start(XCI_PREFIX_SIZE as u64)).unwrap();
        let root = Hfs0::read(&mut cur).unwrap();
        let secure = root.files.iter().find(|f| f.name == "secure").unwrap();
        let secure_offset = root.data_section_offset + secure.data_offset;

        assert_eq!(secure_offset % MEDIA_UNIT, 0);
        let mut cur = Cursor::new(&blob);
        cur.seek(SeekFrom::Start(secure_offset)).unwrap();
        let partition = Hfs0::read(&mut cur).unwrap();
        assert_eq!(partition.files.len(), 4);
        assert_eq!(partition.data_section_offset % MEDIA_UNIT, 0);

        let start = secure_offset as usize;
        let end = start + secure.hashed_region_size as usize;
        let want: [u8; 32] = Sha256::digest(&blob[start..end]).into();
        assert_eq!(secure.sha256, want);
    }

    #[test]
    fn rejects_empty_inputs() {
        let out = NamedTempFile::new().unwrap();
        let err = merge_containers(
            &[],
            out.path(),
            NxMergeFormat::Nsp,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, NxError::NoInputs));
    }

    #[test]
    fn rejects_output_that_is_an_input() {
        let base = write_nsp(&[
            meta_file(
                nca_id(0x01),
                0x0100_9999_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[nca_id(0x11)],
            ),
            prog(nca_id(0x11), 0x800),
        ]);
        let err = merge_containers(
            &[base.path().to_path_buf()],
            base.path(),
            NxMergeFormat::Nsp,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, NxError::OutputIsInput(_)));
    }

    #[test]
    fn super_xci_relists_via_list_container() {
        use crate::nintendo::nx::container::{ContainerKind, list_container};

        let base = write_xci(&[
            meta_file(
                nca_id(0x01),
                0x0100_EEEE_0000_0000,
                0,
                CNMT_TYPE_APPLICATION,
                &[nca_id(0x11)],
            ),
            prog(nca_id(0x11), 0x800),
        ]);
        let dlc = write_xci(&[
            meta_file(
                nca_id(0x02),
                0x0100_EEEE_0000_1000,
                0,
                CNMT_TYPE_ADD_ON_CONTENT,
                &[nca_id(0x22)],
            ),
            prog(nca_id(0x22), 0x800),
        ]);

        let out = Builder::new().suffix(".xci").tempfile().unwrap();
        merge_containers(
            &[base.path().to_path_buf(), dlc.path().to_path_buf()],
            out.path(),
            NxMergeFormat::Xci,
            &synthetic_keyset(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap();

        let listing = list_container(out.path()).unwrap();
        assert_eq!(listing.kind, ContainerKind::Xci);
        let mut secure: Vec<String> = listing
            .entries
            .iter()
            .filter(|e| e.partition == Some("secure"))
            .map(|e| e.name.clone())
            .collect();
        secure.sort();
        let mut want = vec![
            format!("{}.cnmt.nca", hex::encode(nca_id(0x01))),
            format!("{}.nca", hex::encode(nca_id(0x11))),
            format!("{}.cnmt.nca", hex::encode(nca_id(0x02))),
            format!("{}.nca", hex::encode(nca_id(0x22))),
        ];
        want.sort();
        assert_eq!(secure, want);
    }
}
