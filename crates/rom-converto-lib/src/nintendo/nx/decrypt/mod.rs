//! Encrypted NSP/XCI -> NxEmu DNSP/DXCI.
//!
//! NxEmu ships no decryption code and loads `.dnsp` / `.dxci` instead:
//! the same containers with every NCA rewritten as plaintext (see
//! [`nca`]) and the gamecard magic changed from `HEAD` to `DXCI`.
//! Decryption preserves every size, so the output mirrors the input
//! byte for byte except for the rewritten NCAs, the HFS0 entry hashes
//! that cover their headers, and the gamecard header's root-HFS0 hash.
//! Tickets, certificates, and CNMT XMLs are copied through untouched.

pub(crate) mod bucket_tree;
pub(crate) mod nca;

use std::fs::File;
use std::io::{BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::nintendo::nx::constants::{HFS0_ENTRY_SIZE, HFS0_HEADER_SIZE};
use crate::nintendo::nx::container::{
    ContainerKind, DXCI_MAGIC, XCI_HEAD_MAGIC_OFFSET, list_container, read_xci_hfs0_offset,
    xci_root_partitions,
};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::meta::merge_inline_tickets;
use crate::nintendo::nx::models::hfs0::Hfs0;
use crate::nintendo::nx::util::copy_range;
use crate::util::bytes::u64_le;
use crate::util::pread::file_read_exact_at;
use crate::util::{AtomicProgress, CancelToken, ProgressReporter, run_scratch_write};
use nca::NcaPlainPlan;

const XCI_ROOT_HEADER_SIZE_OFFSET: usize = 0x138;
const XCI_ROOT_HEADER_HASH_OFFSET: usize = 0x140;
/// Gamecard header length; the root HFS0 can never start inside it.
const XCI_HEADER_SIZE: u64 = 0x200;
const HFS0_ENTRY_HASH_OFFSET: usize = 0x20;

/// Decrypts the NSP or XCI at `input` into the DNSP/DXCI at `output`.
/// Tickets bundled in the container supply the title keys of
/// rights-protected NCAs; `keys` must hold the header key and the
/// key-area/titlekek keys for each NCA's key generation.
///
/// # Errors
/// Fails if `input` is compressed (NSZ/XCZ), if a needed key is
/// missing, if an NCA already carries the `DNCA` magic, if a section
/// uses XTS (encrypted input only) or a sparse layer, or on the
/// underlying I/O and parsing errors. Plaintext NCAs (NCA3 magic,
/// header and sections in the clear) are accepted and only get their
/// headers rewritten.
pub fn decrypt_container(
    input: &Path,
    output: &Path,
    keys: &KeySet,
    progress: &dyn ProgressReporter,
    cancel: Option<&CancelToken>,
) -> NxResult<()> {
    let plan = plan_decrypt(input, keys)?;
    for warning in &plan.warnings {
        progress.warn(warning);
    }
    let never = CancelToken::new();
    let cancel = cancel.unwrap_or(&never);
    let mut out = BufWriter::new(File::create(output)?);
    write_segments(&plan, &mut out, progress, cancel)?;
    out.flush()?;
    Ok(())
}

/// Async twin of [`decrypt_container`]: writes to a scratch sibling of
/// `output` and publishes it on success; a cancelled or failed run
/// leaves nothing behind.
pub async fn decrypt_container_async(
    input: PathBuf,
    output: PathBuf,
    keys: KeySet,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> NxResult<()> {
    let plan = tokio::task::spawn_blocking(move || plan_decrypt(&input, &keys)).await??;
    for warning in &plan.warnings {
        progress.warn(warning);
    }
    progress.start(plan.total, "Decrypting Switch container");
    run_scratch_write(
        &output,
        true,
        progress,
        &cancel,
        move |file, bytes_done, cancel| {
            let proxy = AtomicProgress {
                counter: bytes_done,
            };
            let mut out = BufWriter::new(file);
            write_segments(&plan, &mut out, &proxy, &cancel)?;
            out.flush()?;
            Ok(())
        },
    )
    .await
}

/// Everything a decrypt run writes: the rewritten byte ranges of the
/// input plus the warnings planning produced.
struct DecryptPlan {
    file: Arc<File>,
    total: u64,
    segments: Vec<Segment>,
    /// Planning runs off the reporter (behind a byte-counter proxy under
    /// `spawn_blocking`), so its warnings are replayed by the caller.
    warnings: Vec<String>,
}

fn plan_decrypt(input: &Path, keys: &KeySet) -> NxResult<DecryptPlan> {
    let listing = list_container(input)?;
    if listing.kind.is_compressed() {
        return Err(NxError::CompressedInputUnsupported(input.to_path_buf()));
    }
    let mut keys = keys.clone();
    merge_inline_tickets(input, &listing, &mut keys);

    let file = Arc::new(File::open(input)?);
    let total = file.metadata()?.len();
    let mut warnings = Vec::new();
    let segments = match listing.kind {
        ContainerKind::Nsp => nsp_segments(&file, input, &listing.entries, &keys)?,
        ContainerKind::Xci => xci_segments(&file, input, &keys, &mut warnings)?,
        ContainerKind::Nsz | ContainerKind::Xcz => unreachable!("compressed kinds rejected above"),
    };
    Ok(DecryptPlan {
        file,
        total,
        segments,
        warnings,
    })
}

/// One byte range of the output that differs from the input: either
/// replacement bytes (patched headers) or an NCA to rewrite.
struct Segment {
    abs: u64,
    len: u64,
    kind: SegmentKind,
}

enum SegmentKind {
    Bytes(Vec<u8>),
    Nca(NcaPlainPlan),
}

fn is_nca(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".nca")
}

fn is_ncz(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".ncz")
}

fn nsp_segments(
    file: &Arc<File>,
    input: &Path,
    entries: &[crate::nintendo::nx::container::ContainerEntry],
    keys: &KeySet,
) -> NxResult<Vec<Segment>> {
    // An NSZ renamed to .nsp passes the extension gate, so the entry
    // names are the only reliable compression signal.
    if entries.iter().any(|e| is_ncz(&e.name)) {
        return Err(NxError::CompressedInputUnsupported(input.to_path_buf()));
    }
    let mut segments = Vec::new();
    for entry in entries.iter().filter(|e| is_nca(&e.name)) {
        let plan = NcaPlainPlan::open(
            file.clone(),
            entry.abs_offset,
            entry.size,
            &entry.name,
            keys,
        )?;
        segments.push(Segment {
            abs: entry.abs_offset,
            len: entry.size,
            kind: SegmentKind::Nca(plan),
        });
    }
    Ok(segments)
}

/// Plans an XCI rewrite: the gamecard prefix with the `DXCI` magic and
/// refreshed root-HFS0 hash, the root and partition headers with
/// refreshed entry hashes, and every NCA in every partition. NxEmu
/// only reads the `update` partition to install firmware and skips
/// any update NCA it cannot open, while its system NCAs may need
/// newer keys or layouts than the game itself, so an update NCA that
/// cannot be planned is copied through with a warning instead of
/// failing the conversion. I/O errors stay fatal.
fn xci_segments(
    file: &Arc<File>,
    input: &Path,
    keys: &KeySet,
    warnings: &mut Vec<String>,
) -> NxResult<Vec<Segment>> {
    let hfs0_off = {
        let mut probe = File::open(input)?;
        read_xci_hfs0_offset(&mut probe)?
    };
    if hfs0_off < XCI_HEADER_SIZE {
        return Err(NxError::InvalidXci);
    }
    let mut reader = BufReader::new(File::open(input)?);
    reader.seek(SeekFrom::Start(hfs0_off))?;
    let root = Hfs0::read(&mut reader)?;
    let mut root_header = read_at(file, hfs0_off, root.data_section_offset - hfs0_off)?;

    let mut segments = Vec::new();
    for (index, (partition, part_abs, sub)) in xci_root_partitions(&mut reader, &root)?
        .into_iter()
        .enumerate()
    {
        let mut sub_header = read_at(file, part_abs, sub.data_section_offset - part_abs)?;

        for (i, entry) in sub.files.iter().enumerate() {
            if is_ncz(&entry.name) {
                return Err(NxError::CompressedInputUnsupported(input.to_path_buf()));
            }
            if !is_nca(&entry.name) {
                continue;
            }
            let abs = sub
                .data_section_offset
                .checked_add(entry.data_offset)
                .ok_or(NxError::InvalidXci)?;
            let plan = match NcaPlainPlan::open(file.clone(), abs, entry.size, &entry.name, keys) {
                Err(err)
                    if partition.name.eq_ignore_ascii_case("update")
                        && !matches!(err, NxError::IoError(_)) =>
                {
                    warnings.push(format!(
                        "update partition NCA {} left encrypted: {err}",
                        entry.name
                    ));
                    continue;
                }
                plan => plan?,
            };
            let prefix = plan.plain_prefix(file, abs, u64::from(entry.hashed_region_size))?;
            set_entry_hash(&mut sub_header, i, &prefix);
            segments.push(Segment {
                abs,
                len: entry.size,
                kind: SegmentKind::Nca(plan),
            });
        }

        // The root entry hashes the partition header; a region that
        // ran into file data would need the rewritten bytes too.
        let hashed = partition.hashed_region_size as usize;
        if hashed > sub_header.len() {
            return Err(NxError::InvalidXci);
        }
        set_entry_hash(&mut root_header, index, &sub_header[..hashed]);
        segments.push(Segment {
            abs: part_abs,
            len: sub_header.len() as u64,
            kind: SegmentKind::Bytes(sub_header),
        });
    }

    let mut prefix = read_at(file, 0, hfs0_off)?;
    prefix[XCI_HEAD_MAGIC_OFFSET as usize..XCI_HEAD_MAGIC_OFFSET as usize + 4]
        .copy_from_slice(&DXCI_MAGIC);
    let hashed = u64_le(&prefix, XCI_ROOT_HEADER_SIZE_OFFSET) as usize;
    if hashed > root_header.len() {
        return Err(NxError::InvalidXci);
    }
    let root_hash = Sha256::digest(&root_header[..hashed]);
    prefix[XCI_ROOT_HEADER_HASH_OFFSET..XCI_ROOT_HEADER_HASH_OFFSET + 32]
        .copy_from_slice(&root_hash);

    segments.push(Segment {
        abs: hfs0_off,
        len: root_header.len() as u64,
        kind: SegmentKind::Bytes(root_header),
    });
    segments.push(Segment {
        abs: 0,
        len: hfs0_off,
        kind: SegmentKind::Bytes(prefix),
    });
    Ok(segments)
}

fn read_at(file: &File, abs: u64, len: u64) -> NxResult<Vec<u8>> {
    let mut buf = vec![0u8; len as usize];
    file_read_exact_at(file, &mut buf, abs)?;
    Ok(buf)
}

/// Stores the SHA-256 of `hashed` in HFS0 entry `index` of `header`.
fn set_entry_hash(header: &mut [u8], index: usize, hashed: &[u8]) {
    let at = HFS0_HEADER_SIZE + index * HFS0_ENTRY_SIZE + HFS0_ENTRY_HASH_OFFSET;
    header[at..at + 32].copy_from_slice(&Sha256::digest(hashed));
}

/// Streams the input to `out`, copying every byte outside a segment
/// verbatim and emitting each segment in its place.
fn write_segments<W: Write>(
    plan: &DecryptPlan,
    out: &mut W,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    let file = &*plan.file;
    let total = plan.total;
    let mut order: Vec<&Segment> = plan.segments.iter().collect();
    order.sort_by_key(|s| s.abs);

    let mut pos = 0u64;
    for segment in order {
        let end = segment
            .abs
            .checked_add(segment.len)
            .filter(|end| *end <= total)
            .ok_or(NxError::OverlappingEntries)?;
        if segment.abs < pos {
            return Err(NxError::OverlappingEntries);
        }
        copy_range(file, pos, segment.abs - pos, out, progress, cancel)?;
        match &segment.kind {
            SegmentKind::Bytes(bytes) => {
                out.write_all(bytes)?;
                progress.inc(bytes.len() as u64);
            }
            SegmentKind::Nca(plan) => {
                plan.write_plain(file, segment.abs, out, progress, Some(cancel))?
            }
        }
        pos = end;
    }
    copy_range(file, pos, total - pos, out, progress, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::constants::NCA_HEADER_SIZE;
    use crate::nintendo::nx::crypto::aes_ctr::apply_ctr;
    use crate::nintendo::nx::crypto::aes_xts::decrypt_nca_header;
    use crate::nintendo::nx::models::cnmt::CNMT_CONTENT_TYPE_PROGRAM;
    use crate::nintendo::nx::models::hfs0::hash_first_chunk;
    use crate::nintendo::nx::test_fixtures::{
        TEST_BODY_KEY, TEST_HEADER_KEY, WarnRecorder, build_meta_nca,
        build_synthetic_nca_with_rights_id, build_test_nsp, build_test_xci,
        build_test_xci_partitions, synthetic_keyset,
    };
    use crate::util::NoProgress;
    use tempfile::TempDir;

    fn write_temp(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        File::create(&path).unwrap().write_all(bytes).unwrap();
        path
    }

    fn expected_plain_meta_nca(dir: &TempDir, encrypted: &[u8]) -> Vec<u8> {
        let file = Arc::new(File::open(write_temp(dir, "x.nca", encrypted)).unwrap());
        let plan = NcaPlainPlan::open(
            file.clone(),
            0,
            encrypted.len() as u64,
            "x.nca",
            &synthetic_keyset(),
        )
        .unwrap();
        let mut out = Vec::new();
        plan.write_plain(&file, 0, &mut out, &NoProgress, None)
            .unwrap();
        out
    }

    #[test]
    fn nsp_rewrites_ncas_in_place_and_keeps_other_files() {
        let nca = build_meta_nca(
            0x0100_0000_0000_1000,
            1,
            CNMT_CONTENT_TYPE_PROGRAM,
            &[[0xAB; 16]],
        );
        let tik = vec![0x5Au8; 0x2C0];
        let nsp = build_test_nsp(&[
            (
                "0000000000000000000000000000000a.cnmt.nca".into(),
                nca.clone(),
            ),
            ("title.tik".into(), tik.clone()),
        ]);
        let dir = TempDir::new().unwrap();
        let input = write_temp(&dir, "game.nsp", &nsp);
        let output = dir.path().join("game.dnsp");

        decrypt_container(&input, &output, &synthetic_keyset(), &NoProgress, None).unwrap();

        let out = std::fs::read(&output).unwrap();
        assert_eq!(out.len(), nsp.len());
        let nca_start = nsp.len() - tik.len() - nca.len();
        assert_eq!(&out[..nca_start], &nsp[..nca_start]);
        assert_eq!(
            &out[nca_start..nca_start + nca.len()],
            expected_plain_meta_nca(&dir, &nca)
        );
        assert_eq!(&out[nca_start + nca.len()..], tik.as_slice());

        let mut header = [0u8; NCA_HEADER_SIZE];
        header.copy_from_slice(&nca[..NCA_HEADER_SIZE]);
        decrypt_nca_header(&mut header, &TEST_HEADER_KEY).unwrap();
        let plain = &out[nca_start..nca_start + NCA_HEADER_SIZE];
        assert_eq!(&plain[0x200..0x204], b"DNCA");
        assert_eq!(&plain[0x204..0x280], &header[0x204..0x280]);

        // Independent oracle for the section body: decrypt it straight
        // from the input with the fixture's body key and its counter
        // (both CTR halves zero, section starting at 0x4000), rather
        // than trusting the planner with itself.
        let mut section = nca[0x4000..].to_vec();
        let mut counter = [0u8; 16];
        counter[8..].copy_from_slice(&(0x4000u64 / 16).to_be_bytes());
        apply_ctr(&TEST_BODY_KEY, &counter, &mut section).unwrap();
        assert_eq!(
            &out[nca_start + 0x4000..nca_start + nca.len()],
            &section[..]
        );
    }

    #[test]
    fn xci_patches_magic_and_refreshes_hfs0_hashes() {
        let nca = build_meta_nca(
            0x0100_0000_0000_2000,
            3,
            CNMT_CONTENT_TYPE_PROGRAM,
            &[[0xCD; 16]],
        );
        let xci = build_test_xci(&[(
            "0000000000000000000000000000000b.cnmt.nca".into(),
            nca.clone(),
        )]);
        let dir = TempDir::new().unwrap();
        let input = write_temp(&dir, "game.xci", &xci);
        let output = dir.path().join("game.dxci");

        decrypt_container(&input, &output, &synthetic_keyset(), &NoProgress, None).unwrap();

        let out = std::fs::read(&output).unwrap();
        assert_eq!(out.len(), xci.len());
        assert_eq!(&out[0x100..0x104], b"DXCI");

        let hfs0_off = u64_le(&out, 0x130) as usize;
        let root_size = u64_le(&out, 0x138) as usize;
        let root_hash: [u8; 32] = Sha256::digest(&out[hfs0_off..hfs0_off + root_size]).into();
        assert_eq!(&out[0x140..0x160], &root_hash);

        let mut reader = std::io::Cursor::new(&out);
        reader.seek(SeekFrom::Start(hfs0_off as u64)).unwrap();
        let root = Hfs0::read(&mut reader).unwrap();
        let secure = root.files.iter().find(|f| f.name == "secure").unwrap();
        let part_abs = root.data_section_offset + secure.data_offset;
        reader.seek(SeekFrom::Start(part_abs)).unwrap();
        let sub = Hfs0::read(&mut reader).unwrap();
        let sub_header = &out[part_abs as usize..sub.data_section_offset as usize];
        assert_eq!(
            secure.sha256,
            hash_first_chunk(sub_header, secure.hashed_region_size)
        );

        let entry = &sub.files[0];
        let nca_abs = (sub.data_section_offset + entry.data_offset) as usize;
        let plain = &out[nca_abs..nca_abs + entry.size as usize];
        assert_eq!(plain, expected_plain_meta_nca(&dir, &nca));
        assert_eq!(
            entry.sha256,
            hash_first_chunk(plain, entry.hashed_region_size)
        );

        // Everything outside the patched headers and the NCA is untouched.
        assert_eq!(&out[0x160..hfs0_off], &xci[0x160..hfs0_off]);
        assert_eq!(&out[nca_abs + plain.len()..], &xci[nca_abs + plain.len()..]);
    }

    /// XCI whose update partition holds a rights-protected NCA with no
    /// bundled ticket and whose secure partition holds a decryptable
    /// meta NCA.
    fn build_xci_with_unplannable_update() -> Vec<u8> {
        let update_nca = build_synthetic_nca_with_rights_id(&[0x77; 0x200], [0xEE; 16]);
        let secure_nca = build_meta_nca(
            0x0100_0000_0000_4000,
            1,
            CNMT_CONTENT_TYPE_PROGRAM,
            &[[0xF1; 16]],
        );
        build_test_xci_partitions(&[
            (
                "update",
                &[(
                    "0000000000000000000000000000000c.nca".into(),
                    update_nca.clone(),
                )],
            ),
            (
                "secure",
                &[(
                    "0000000000000000000000000000000d.cnmt.nca".into(),
                    secure_nca.clone(),
                )],
            ),
        ])
    }

    /// An update NCA that cannot be planned (rights-protected with no
    /// bundled ticket) is copied through encrypted with a warning,
    /// while the secure NCA is still decrypted.
    #[test]
    fn xci_copies_unplannable_update_nca_through_with_warning() {
        let update_nca = build_synthetic_nca_with_rights_id(&[0x77; 0x200], [0xEE; 16]);
        let xci = build_xci_with_unplannable_update();
        let dir = TempDir::new().unwrap();
        let input = write_temp(&dir, "game.xci", &xci);
        let output = dir.path().join("game.dxci");

        let recorder = WarnRecorder::default();
        decrypt_container(&input, &output, &synthetic_keyset(), &recorder, None).unwrap();

        let out = std::fs::read(&output).unwrap();
        assert_eq!(out.len(), xci.len());

        let mut reader = std::io::Cursor::new(&out);
        let hfs0_off = u64_le(&out, 0x130) as usize;
        reader.seek(SeekFrom::Start(hfs0_off as u64)).unwrap();
        let root = Hfs0::read(&mut reader).unwrap();

        let update = root.files.iter().find(|f| f.name == "update").unwrap();
        let upd_abs = root.data_section_offset + update.data_offset;
        reader.seek(SeekFrom::Start(upd_abs)).unwrap();
        let upd = Hfs0::read(&mut reader).unwrap();
        let upd_end =
            (upd.data_section_offset + upd.files[0].data_offset + upd.files[0].size) as usize;
        // Partition header (entry hash untouched) and NCA bytes are
        // copied through verbatim.
        assert_eq!(
            &out[upd_abs as usize..upd_end],
            &xci[upd_abs as usize..upd_end]
        );
        assert_eq!(upd.files[0].size as usize, update_nca.len());
        let upd_nca_abs = (upd.data_section_offset + upd.files[0].data_offset) as usize;
        assert_eq!(
            &out[upd_nca_abs..upd_nca_abs + update_nca.len()],
            update_nca.as_slice()
        );

        let secure = root.files.iter().find(|f| f.name == "secure").unwrap();
        let sec_abs = root.data_section_offset + secure.data_offset;
        reader.seek(SeekFrom::Start(sec_abs)).unwrap();
        let sec = Hfs0::read(&mut reader).unwrap();
        let sec_nca_abs = (sec.data_section_offset + sec.files[0].data_offset) as usize + 0x200;
        assert_eq!(&out[sec_nca_abs..sec_nca_abs + 4], b"DNCA");

        let warnings = recorder.warnings.lock().expect("warning lock");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("update partition NCA") && w.contains(".nca")),
            "{warnings:?}"
        );
    }

    /// `nx info` labels our own output decrypted: the DNSP once every
    /// NCA is plaintext, and the DXCI even though its update NCA stayed
    /// encrypted.
    #[test]
    fn info_reports_decrypted_output() {
        use crate::nintendo::nx::info::read_info;
        let dir = TempDir::new().unwrap();
        let nca = build_meta_nca(
            0x0100_0000_0000_5000,
            1,
            CNMT_CONTENT_TYPE_PROGRAM,
            &[[0xA5; 16]],
        );
        let nsp = build_test_nsp(&[("0000000000000000000000000000000e.nca".into(), nca)]);
        let nsp = write_temp(&dir, "game.nsp", &nsp);
        let dnsp = dir.path().join("game.dnsp");
        decrypt_container(&nsp, &dnsp, &synthetic_keyset(), &NoProgress, None).unwrap();
        let xci = write_temp(&dir, "game.xci", &build_xci_with_unplannable_update());
        let dxci = dir.path().join("game.dxci");
        decrypt_container(&xci, &dxci, &synthetic_keyset(), &NoProgress, None).unwrap();

        assert!(!read_info(&nsp, None).unwrap().is_decrypted);
        assert!(read_info(&dnsp, None).unwrap().is_decrypted);
        assert!(!read_info(&xci, None).unwrap().is_decrypted);
        assert!(read_info(&dxci, None).unwrap().is_decrypted);
    }

    /// The async wrapper must replay the update-partition warning on
    /// the outer reporter; the GUI/runner only sees that one.
    #[tokio::test]
    async fn async_decrypt_replays_update_warning() {
        let xci = build_xci_with_unplannable_update();
        let dir = TempDir::new().unwrap();
        let input = write_temp(&dir, "game.xci", &xci);
        let output = dir.path().join("game.dxci");

        let recorder = WarnRecorder::default();
        decrypt_container_async(
            input.clone(),
            output.clone(),
            synthetic_keyset(),
            &recorder,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let warnings = recorder.warnings.lock().expect("warning lock");
        assert!(
            warnings.iter().any(|w| w.contains("update partition NCA")),
            "{warnings:?}"
        );
    }

    /// A plaintext NCA as other tools emit it: the XTS-decrypted
    /// header as is (`NCA3` magic, FS headers still claiming CTR) over
    /// decrypted sections. Only the header rewrite is left to do, and
    /// no key is needed for it.
    #[test]
    fn plaintext_nca3_gets_only_its_header_rewritten() {
        let nca = build_meta_nca(
            0x0100_0000_0000_3000,
            1,
            CNMT_CONTENT_TYPE_PROGRAM,
            &[[0xEF; 16]],
        );
        let dir = TempDir::new().unwrap();
        let expected = expected_plain_meta_nca(&dir, &nca);
        let mut plaintext = expected.clone();
        let mut header = [0u8; NCA_HEADER_SIZE];
        header.copy_from_slice(&nca[..NCA_HEADER_SIZE]);
        decrypt_nca_header(&mut header, &TEST_HEADER_KEY).unwrap();
        plaintext[..NCA_HEADER_SIZE].copy_from_slice(&header);
        assert_eq!(&plaintext[0x200..0x204], b"NCA3");

        let file = Arc::new(File::open(write_temp(&dir, "h.nca", &plaintext)).unwrap());
        let plan = NcaPlainPlan::open(
            file.clone(),
            0,
            plaintext.len() as u64,
            "h.nca",
            &KeySet::default(),
        )
        .unwrap();
        let mut out = Vec::new();
        plan.write_plain(&file, 0, &mut out, &NoProgress, None)
            .unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn compressed_input_is_rejected() {
        let dir = TempDir::new().unwrap();
        let input = write_temp(&dir, "game.nsz", &build_test_nsp(&[]));
        let err = decrypt_container(
            &input,
            &dir.path().join("game.dnsp"),
            &synthetic_keyset(),
            &NoProgress,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, NxError::CompressedInputUnsupported(_)));
    }

    /// An NSZ renamed to .nsp passes the extension gate; its .ncz
    /// entries must still be rejected instead of copied through.
    #[test]
    fn ncz_entry_is_rejected() {
        let nsp = build_test_nsp(&[("data.ncz".into(), vec![0xAB; 0x200])]);
        let dir = TempDir::new().unwrap();
        let input = write_temp(&dir, "game.nsp", &nsp);
        let err = decrypt_container(
            &input,
            &dir.path().join("game.dnsp"),
            &synthetic_keyset(),
            &NoProgress,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, NxError::CompressedInputUnsupported(path) if path == input));
    }
}
