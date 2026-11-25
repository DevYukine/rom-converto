//! RomFS and ExeFS extraction from NSP/XCI/NSZ/XCZ base games, updates
//! and DLC.
//!
//! [`plan_extract`] groups the inputs by their CNMTs, pairs every update
//! with its base game, and lists one target directory per title and
//! filesystem: `<output>/<TITLE ID>/romfs` and `/exefs`. An update's
//! RomFS is read through its indirect table over the base RomFS, so the
//! extracted tree is the patched game. [`extract_planned_async`] writes
//! every target to a scratch directory and publishes them together.
//! Sparse and compressed sections are not supported.

mod plan;
mod storage;
pub(crate) mod write;

use std::fmt;
use std::path::{Path, PathBuf};

use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::models::nca::FsHeader;
use crate::nintendo::nx::romfs::{Tree, read_romfs_header, walk_romfs};
use crate::nintendo::nx::walker::NcaInput;
use crate::util::{CancelToken, ProgressReporter, spawn_blocking_with_progress};
pub use plan::plan_extract;
use storage::{IndirectStorage, SectionRef, check_master_hash, data_layer};
use write::list_pfs0;

/// Which filesystems to write. The caller maps "neither flag" to both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NxExtractParts {
    pub romfs: bool,
    pub exefs: bool,
}

/// One filesystem of a title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NxExtractPart {
    Romfs,
    Exefs,
}

/// One output directory: `<output_dir>/<TITLE ID>/romfs` or `/exefs`.
#[derive(Debug, Clone)]
pub struct NxExtractTarget {
    /// Program id the folder is named after.
    pub title_id: u64,
    pub part: NxExtractPart,
    /// Final directory.
    pub dir: PathBuf,
    /// Preflight size estimate from the headers: the RomFS image or the
    /// PFS0 size, which bounds what the target writes. A plain target's
    /// is also at most its section size.
    pub bytes: u64,
    /// Replace `dir` when it already holds files. [`plan_extract`] leaves
    /// it off; without it only a missing or empty `dir` is written.
    pub overwrite: bool,
    source: SectionRef,
    /// The base program's RomFS section when `source` is the patch
    /// section of an update.
    base: Option<SectionRef>,
}

/// Targets plus the key set with every input's inline tickets folded in.
#[derive(Clone)]
pub struct NxExtractPlan {
    pub targets: Vec<NxExtractTarget>,
    pub warnings: Vec<String>,
    keys: KeySet,
}

/// Leaves out the keys, so logging a plan never prints them.
impl fmt::Debug for NxExtractPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NxExtractPlan")
            .field("targets", &self.targets)
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

impl NxExtractTarget {
    /// Opens the storage this target reads and lists the tree inside it.
    fn open(&self, keys: &KeySet, cancel: &CancelToken) -> NxResult<(Box<dyn NcaInput>, Tree)> {
        let source = self.source.open(keys)?;
        let fs = source.fs;
        let name = &self.source.nca.name;
        let (storage, size): (Box<dyn NcaInput>, u64) = match &self.base {
            None => {
                let size = source.size();
                (Box::new(source), size)
            }
            Some(base) => {
                let base_name = &base.nca.name;
                let base = base.open(keys)?;
                // The patch tables parse whatever the base holds, so a base
                // under the wrong key would only show as garbage files.
                let (offset, layer) = data_layer(&base.fs, base.index, base.size(), base_name)?;
                read_romfs_header(&base, offset, layer).map_err(|err| {
                    wrong_key_or(err, &base, base.size(), &base.fs, base.index, base_name)
                })?;
                let patched = IndirectStorage::open(base, source, name, cancel)?;
                let size = patched.size();
                (Box::new(patched), size)
            }
        };
        let (offset, layer_size) = data_layer(&fs, self.source.index, size, name)?;
        let listed = match self.part {
            NxExtractPart::Exefs => list_pfs0(&*storage, offset, layer_size),
            NxExtractPart::Romfs => walk_romfs(&*storage, offset, layer_size),
        };
        let tree = listed
            .map_err(|err| wrong_key_or(err, &*storage, size, &fs, self.source.index, name))?;
        Ok((storage, tree))
    }
}

/// A filesystem that does not parse is usually a wrong title key; only
/// the master hash can tell that apart from a malformed one. Returns
/// [`NxError::WrongKey`] on a mismatch and `err` otherwise.
fn wrong_key_or(
    err: NxError,
    storage: &dyn NcaInput,
    size: u64,
    fs: &FsHeader,
    index: usize,
    name: &str,
) -> NxError {
    match check_master_hash(storage, size, fs, index, name) {
        Err(wrong @ NxError::WrongKey { .. }) => wrong,
        _ => err,
    }
}

/// Writes every target in `plan` into scratch dirs first, then publishes
/// them all. A target whose `overwrite` is set replaces a dir that
/// already exists (moved aside, then deleted once every target is
/// published; restored on failure); otherwise an existing non-empty dir
/// is an error. Cancel or failure removes every scratch dir and every
/// parent dir this run created that is still empty. A cancel after
/// publishing started no longer stops the job. Returns the bytes written
/// per target, in `plan.targets` order.
///
/// # Errors
/// Fails when a target exists and its `overwrite` is off, when a source
/// NCA cannot be read, decrypts with the wrong key, or holds a malformed
/// filesystem, on I/O errors, and with
/// [`NxError::Cancelled`](crate::nintendo::nx::NxError::Cancelled) when
/// `cancel` fires before publishing.
pub async fn extract_planned_async(
    plan: NxExtractPlan,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> NxResult<Vec<u64>> {
    let total = plan
        .targets
        .iter()
        .fold(0u64, |sum, t| sum.saturating_add(t.bytes));
    spawn_blocking_with_progress(progress, move |progress| {
        progress.start(total, "Extracting Switch RomFS/ExeFS");
        let targets: Vec<(&Path, bool)> = plan
            .targets
            .iter()
            .map(|t| (t.dir.as_path(), t.overwrite))
            .collect();
        let result = write::write_targets(&targets, progress, &cancel, |i| {
            plan.targets[i].open(&plan.keys, &cancel)
        });
        progress.finish();
        result
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::compress::{NxCompressOptions, compress_container};
    use crate::nintendo::nx::container::list_container;
    use crate::nintendo::nx::error::NxError;
    use crate::nintendo::nx::models::cnmt::{
        CNMT_CONTENT_TYPE_DATA, CNMT_CONTENT_TYPE_PROGRAM, CNMT_TYPE_ADD_ON_CONTENT,
        CNMT_TYPE_APPLICATION, CNMT_TYPE_PATCH,
    };
    use crate::nintendo::nx::models::nca::{CONTENT_TYPE_DATA, CONTENT_TYPE_PROGRAM};
    use crate::nintendo::nx::ncz::NczMode;
    use crate::nintendo::nx::test_fixtures::{
        CnmtRecord, DATA_LEVEL, FixtureSection, TEST_BODY_KEY, build_common_ticket,
        build_meta_nca_records, build_nca, build_plaintext_nca, build_test_nsp,
        build_test_xci_partitions, noise, patch_romfs_section, pfs0_section, romfs_image,
        romfs_section, synthetic_keyset, synthetic_keyset_with_titlekek,
    };
    use crate::util::NoProgress;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fs;

    const APP: u64 = 0x0100_1234_AAAA_0000;
    const PATCH: u64 = APP | 0x800;
    const DLC: u64 = 0x0100_1234_AAAA_1001;
    const BOTH: NxExtractParts = NxExtractParts {
        romfs: true,
        exefs: true,
    };
    const ROMFS: NxExtractParts = NxExtractParts {
        romfs: true,
        exefs: false,
    };
    const EXEFS: NxExtractParts = NxExtractParts {
        romfs: false,
        exefs: true,
    };

    type Files = Vec<(String, Vec<u8>)>;

    fn exefs(seed: u32) -> Files {
        vec![
            ("main".into(), noise(0x400, seed)),
            ("main.npdm".into(), noise(0x80, seed + 1)),
        ]
    }

    fn base_romfs(seed: u32) -> Files {
        vec![
            ("a.bin".into(), noise(0x3000, seed)),
            ("dir/sub/b.txt".into(), b"base text\n".to_vec()),
            ("dir/c.dat".into(), noise(0x1234, seed + 1)),
        ]
    }

    /// What an update of `version` makes of [`base_romfs`]: `dir/c.dat`
    /// changed and moved first, `a.bin` read from the base, `new/d.bin`
    /// added.
    fn patched_romfs(seed: u32, version: u32) -> Files {
        let base = base_romfs(seed);
        vec![
            ("dir/c.dat".into(), noise(0x1500, seed + 40 + version)),
            base[0].clone(),
            base[1].clone(),
            ("new/d.bin".into(), noise(0x800, seed + 50 + version)),
        ]
    }

    fn dlc_romfs() -> Files {
        vec![
            ("item.bin".into(), noise(0x900, 60)),
            ("data/level.dat".into(), noise(0x40, 61)),
        ]
    }

    fn program(title_id: u64, rights_id: [u8; 16], seed: u32) -> Vec<u8> {
        build_nca(
            CONTENT_TYPE_PROGRAM,
            title_id,
            rights_id,
            &[
                pfs0_section(&exefs(seed)),
                romfs_section(&romfs_image(&base_romfs(seed)).0),
            ],
        )
    }

    /// The patch section of update `version` over [`base_romfs`]: its
    /// `a.bin` reads the base, everything else the patch.
    fn patch_romfs(seed: u32, version: u32) -> FixtureSection {
        let base_a = DATA_LEVEL + romfs_image(&base_romfs(seed)).1[0];
        let (image, offsets) = romfs_image(&patched_romfs(seed, version));
        let a_len = base_romfs(seed)[0].1.len() as u64;
        patch_romfs_section(&image, &[(DATA_LEVEL + offsets[1], a_len, base_a)])
    }

    fn patch_program(title_id: u64, seed: u32, version: u32) -> Vec<u8> {
        build_nca(
            CONTENT_TYPE_PROGRAM,
            title_id,
            [0; 16],
            &[
                pfs0_section(&exefs(seed + 20 + version)),
                patch_romfs(seed, version),
            ],
        )
    }

    /// NSP members of one title: its meta NCA, whose CNMT lists every
    /// `(nca, record type, id offset)` of `contents`, and those NCAs.
    fn title(
        meta_type: u8,
        title_id: u64,
        version: u32,
        contents: Vec<(Vec<u8>, u8, u8)>,
    ) -> Files {
        let mut members = Files::new();
        let mut records: Vec<CnmtRecord> = Vec::new();
        for (nca, record_type, id_offset) in contents {
            let id: [u8; 16] = Sha256::digest(&nca)[..16].try_into().unwrap();
            records.push((id, record_type, id_offset));
            members.push((format!("{}.nca", hex::encode(id)), nca));
        }
        let ext_len = if meta_type == CNMT_TYPE_PATCH {
            0x18
        } else {
            0x10
        };
        let mut ext = vec![0u8; ext_len];
        ext[..8].copy_from_slice(&APP.to_le_bytes());
        let meta = build_meta_nca_records(title_id, version, meta_type, &ext, &records);
        let id = hex::encode(&Sha256::digest(&meta)[..16]);
        members.insert(0, (format!("{id}.cnmt.nca"), meta));
        members
    }

    fn base_title() -> Files {
        title(
            CNMT_TYPE_APPLICATION,
            APP,
            0,
            vec![(program(APP, [0; 16], 1), CNMT_CONTENT_TYPE_PROGRAM, 0)],
        )
    }

    fn update_title(version: u32) -> Files {
        title(
            CNMT_TYPE_PATCH,
            PATCH,
            version << 16,
            vec![(
                patch_program(PATCH, 1, version),
                CNMT_CONTENT_TYPE_PROGRAM,
                0,
            )],
        )
    }

    fn dlc_title() -> Files {
        let data = build_nca(
            CONTENT_TYPE_DATA,
            DLC,
            [0; 16],
            &[romfs_section(&romfs_image(&dlc_romfs()).0)],
        );
        title(
            CNMT_TYPE_ADD_ON_CONTENT,
            DLC,
            0,
            vec![(data, CNMT_CONTENT_TYPE_DATA, 0)],
        )
    }

    fn write_input(dir: &Path, name: &str, members: &[Files]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, build_test_nsp(&members.concat())).unwrap();
        path
    }

    async fn extract(
        inputs: &[PathBuf],
        out: &Path,
        parts: NxExtractParts,
        keys: &KeySet,
    ) -> NxResult<Vec<String>> {
        let plan = plan_extract(inputs, out, parts, keys)?;
        let warnings = plan.warnings.clone();
        extract_planned_async(plan, &NoProgress, CancelToken::new()).await?;
        Ok(warnings)
    }

    fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let rel = path.strip_prefix(root).unwrap();
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    files.insert(rel, fs::read(&path).unwrap());
                }
            }
        }
        files
    }

    fn expected(parts: &[(u64, &str, Files)]) -> BTreeMap<String, Vec<u8>> {
        parts
            .iter()
            .flat_map(|(id, part, files)| {
                files
                    .iter()
                    .map(move |(path, data)| (format!("{id:016X}/{part}/{path}"), data.clone()))
            })
            .collect()
    }

    #[tokio::test]
    async fn base_game_extracts_the_requested_parts() {
        let cases = [
            (
                BOTH,
                vec![(APP, "exefs", exefs(1)), (APP, "romfs", base_romfs(1))],
            ),
            (ROMFS, vec![(APP, "romfs", base_romfs(1))]),
            (EXEFS, vec![(APP, "exefs", exefs(1))]),
        ];
        for (parts, want) in cases {
            let dir = tempfile::tempdir().unwrap();
            let base = write_input(dir.path(), "base.nsp", &[base_title()]);
            let out = dir.path().join("out");

            extract(&[base], &out, parts, &synthetic_keyset())
                .await
                .unwrap();

            assert_eq!(tree(&out), expected(&want), "{parts:?}");
        }
    }

    #[tokio::test]
    async fn update_patches_the_base_romfs_in_separate_and_merged_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let base = write_input(dir.path(), "base.nsp", &[base_title()]);
        let v1 = write_input(dir.path(), "v1.nsp", &[update_title(1)]);
        let v2 = write_input(dir.path(), "v2.nsp", &[update_title(2)]);
        // The newest update wins in either input order.
        for (i, updates) in [[&v2, &v1], [&v1, &v2]].into_iter().enumerate() {
            let out = dir.path().join(format!("separate{i}"));
            let inputs = [base.clone(), updates[0].clone(), updates[1].clone()];

            extract(&inputs, &out, BOTH, &synthetic_keyset())
                .await
                .unwrap();

            assert_eq!(
                tree(&out),
                expected(&[
                    (APP, "exefs", exefs(23)),
                    (APP, "romfs", patched_romfs(1, 2)),
                ]),
                "order {i}"
            );
        }

        let merged = write_input(
            dir.path(),
            "merged.nsp",
            &[base_title(), update_title(1), dlc_title()],
        );
        let out = dir.path().join("merged");
        extract(&[merged], &out, BOTH, &synthetic_keyset())
            .await
            .unwrap();
        assert_eq!(
            tree(&out),
            expected(&[
                (APP, "exefs", exefs(22)),
                (APP, "romfs", patched_romfs(1, 1)),
                (DLC, "romfs", dlc_romfs()),
            ])
        );
    }

    #[tokio::test]
    async fn dlc_in_its_own_input_extracts_beside_the_base() {
        let dir = tempfile::tempdir().unwrap();
        let base = write_input(dir.path(), "base.nsp", &[base_title()]);
        let dlc = write_input(dir.path(), "dlc.nsp", &[dlc_title()]);
        let out = dir.path().join("out");

        let warnings = extract(&[base, dlc], &out, ROMFS, &synthetic_keyset())
            .await
            .unwrap();

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            tree(&out),
            expected(&[(APP, "romfs", base_romfs(1)), (DLC, "romfs", dlc_romfs())])
        );
    }

    #[tokio::test]
    async fn superseded_update_content_is_never_opened() {
        // v1's program needs a ticket the inputs lack; v2 supersedes it.
        let locked = build_nca(
            CONTENT_TYPE_PROGRAM,
            PATCH,
            [0xC5; 16],
            &[pfs0_section(&exefs(9))],
        );
        let v1 = title(
            CNMT_TYPE_PATCH,
            PATCH,
            1 << 16,
            vec![(locked, CNMT_CONTENT_TYPE_PROGRAM, 0)],
        );
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "all.nsp", &[base_title(), v1, update_title(2)]);
        let out = dir.path().join("out");

        extract(&[input], &out, BOTH, &synthetic_keyset_with_titlekek())
            .await
            .unwrap();

        assert_eq!(
            tree(&out),
            expected(&[
                (APP, "exefs", exefs(23)),
                (APP, "romfs", patched_romfs(1, 2)),
            ])
        );
    }

    #[tokio::test]
    async fn update_without_exefs_keeps_the_base_exefs() {
        let update = build_nca(CONTENT_TYPE_PROGRAM, PATCH, [0; 16], &[patch_romfs(1, 1)]);
        let update = title(
            CNMT_TYPE_PATCH,
            PATCH,
            1 << 16,
            vec![(update, CNMT_CONTENT_TYPE_PROGRAM, 0)],
        );
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "game.nsp", &[base_title(), update]);
        let out = dir.path().join("out");

        extract(&[input], &out, BOTH, &synthetic_keyset())
            .await
            .unwrap();

        assert_eq!(
            tree(&out),
            expected(&[
                (APP, "exefs", exefs(1)),
                (APP, "romfs", patched_romfs(1, 1)),
            ])
        );
    }

    #[tokio::test]
    async fn dlc_with_a_program_title_id_keeps_the_program_romfs() {
        let data = build_nca(
            CONTENT_TYPE_DATA,
            APP,
            [0; 16],
            &[romfs_section(&romfs_image(&dlc_romfs()).0)],
        );
        let dlc = title(
            CNMT_TYPE_ADD_ON_CONTENT,
            DLC,
            0,
            vec![(data, CNMT_CONTENT_TYPE_DATA, 0)],
        );
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "clash.nsp", &[base_title(), dlc]);
        let out = dir.path().join("out");

        let warnings = extract(&[input], &out, ROMFS, &synthetic_keyset())
            .await
            .unwrap();

        assert_eq!(tree(&out), expected(&[(APP, "romfs", base_romfs(1))]));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn patched_target_bytes_are_the_patched_image_size() {
        // Every 4 KiB of a 1 MiB patched image maps the same base bytes,
        // so the image is far larger than the base and patch sections.
        let from_base: Vec<(u64, u64, u64)> = (0..256)
            .map(|k| (DATA_LEVEL + k * 0x1000, 0x1000, DATA_LEVEL))
            .collect();
        let patch = patch_romfs_section(&vec![0; 1 << 20], &from_base);
        let patch_size = patch.body.len() as u64;
        let update = title(
            CNMT_TYPE_PATCH,
            PATCH,
            1 << 16,
            vec![(
                build_nca(CONTENT_TYPE_PROGRAM, PATCH, [0; 16], &[patch]),
                CNMT_CONTENT_TYPE_PROGRAM,
                0,
            )],
        );
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "game.nsp", &[base_title(), update]);

        let plan = plan_extract(&[input], dir.path(), ROMFS, &synthetic_keyset()).unwrap();

        let base_size = romfs_section(&romfs_image(&base_romfs(1)).0).body.len() as u64;
        assert!(patch_size + base_size < 1 << 20);
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].bytes, 1 << 20);
    }

    #[tokio::test]
    async fn plaintext_update_still_applies_its_indirect_table() {
        let dir = tempfile::tempdir().unwrap();
        let base = build_plaintext_nca(
            CONTENT_TYPE_PROGRAM,
            APP,
            &[
                pfs0_section(&exefs(1)),
                romfs_section(&romfs_image(&base_romfs(1)).0),
            ],
        );
        let update = build_plaintext_nca(
            CONTENT_TYPE_PROGRAM,
            PATCH,
            &[pfs0_section(&exefs(22)), patch_romfs(1, 1)],
        );
        let input = write_input(
            dir.path(),
            "plain.nsp",
            &[
                title(
                    CNMT_TYPE_APPLICATION,
                    APP,
                    0,
                    vec![(base, CNMT_CONTENT_TYPE_PROGRAM, 0)],
                ),
                title(
                    CNMT_TYPE_PATCH,
                    PATCH,
                    1 << 16,
                    vec![(update, CNMT_CONTENT_TYPE_PROGRAM, 0)],
                ),
            ],
        );
        let out = dir.path().join("out");

        // Only the meta NCAs are encrypted; the content needs no keys.
        extract(&[input], &out, ROMFS, &synthetic_keyset())
            .await
            .unwrap();

        assert_eq!(tree(&out), expected(&[(APP, "romfs", patched_romfs(1, 1))]));
    }

    #[tokio::test]
    async fn rights_id_content_decrypts_with_the_inline_ticket() {
        let rights_id = [0xC5; 16];
        let mut members = title(
            CNMT_TYPE_APPLICATION,
            APP,
            0,
            vec![(program(APP, rights_id, 1), CNMT_CONTENT_TYPE_PROGRAM, 0)],
        );
        let dir = tempfile::tempdir().unwrap();
        let without = write_input(dir.path(), "no-ticket.nsp", &[members.clone()]);
        let out = dir.path().join("out");
        assert!(matches!(
            extract(&[without], &out, BOTH, &synthetic_keyset_with_titlekek()).await,
            Err(NxError::MissingKey { .. } | NxError::MissingTicket(_))
        ));

        members.push((
            format!("{}.tik", hex::encode(rights_id)),
            build_common_ticket(rights_id, TEST_BODY_KEY),
        ));
        let with = write_input(dir.path(), "ticket.nsp", &[members]);
        extract(&[with], &out, BOTH, &synthetic_keyset_with_titlekek())
            .await
            .unwrap();
        assert_eq!(
            tree(&out),
            expected(&[(APP, "exefs", exefs(1)), (APP, "romfs", base_romfs(1))])
        );
    }

    #[tokio::test]
    async fn wrong_title_key_is_reported_as_a_wrong_key() {
        let rights_id = [0xC5; 16];
        let mut members = title(
            CNMT_TYPE_APPLICATION,
            APP,
            0,
            vec![(program(APP, rights_id, 1), CNMT_CONTENT_TYPE_PROGRAM, 0)],
        );
        let mut ticket = build_common_ticket(rights_id, TEST_BODY_KEY);
        ticket[0x180] ^= 1;
        members.push((format!("{}.tik", hex::encode(rights_id)), ticket));
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "bad.nsp", &[members]);

        // The ExeFS checks its hash table, the RomFS its IVFC level 0.
        for (i, parts) in [EXEFS, ROMFS].into_iter().enumerate() {
            let out = dir.path().join(format!("out{i}"));
            let result = extract(
                std::slice::from_ref(&input),
                &out,
                parts,
                &synthetic_keyset_with_titlekek(),
            )
            .await;
            assert!(
                matches!(result, Err(NxError::WrongKey { .. })),
                "{parts:?}: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn lone_update_and_exefs_only_dlc_warn() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "u.nsp", &[update_title(1), dlc_title()]);
        let out = dir.path().join("out");

        let warnings = extract(
            std::slice::from_ref(&input),
            &out,
            BOTH,
            &synthetic_keyset(),
        )
        .await
        .unwrap();

        assert_eq!(
            tree(&out),
            expected(&[(APP, "exefs", exefs(22)), (DLC, "romfs", dlc_romfs())])
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains(&format!("{PATCH:016X}")),
            "{warnings:?}"
        );

        let plan = plan_extract(&[input], &out, EXEFS, &synthetic_keyset()).unwrap();
        assert_eq!(plan.targets.len(), 1);
        assert!(
            plan.warnings[0].contains(&format!("{DLC:016X}")),
            "{:?}",
            plan.warnings
        );
        let lone = write_input(dir.path(), "lone.nsp", &[update_title(1)]);
        assert!(matches!(
            plan_extract(&[lone], &out, ROMFS, &synthetic_keyset()),
            Err(NxError::NothingToExtract)
        ));
    }

    #[tokio::test]
    async fn multi_program_title_pairs_patches_per_program() {
        let base = title(
            CNMT_TYPE_APPLICATION,
            APP,
            0,
            vec![
                (program(APP, [0; 16], 1), CNMT_CONTENT_TYPE_PROGRAM, 0),
                (program(APP + 1, [0; 16], 7), CNMT_CONTENT_TYPE_PROGRAM, 1),
            ],
        );
        let update = title(
            CNMT_TYPE_PATCH,
            PATCH,
            1 << 16,
            vec![
                (patch_program(PATCH, 1, 1), CNMT_CONTENT_TYPE_PROGRAM, 0),
                (patch_program(PATCH + 1, 7, 1), CNMT_CONTENT_TYPE_PROGRAM, 1),
            ],
        );
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "multi.nsp", &[base, update]);
        let out = dir.path().join("out");

        extract(&[input], &out, BOTH, &synthetic_keyset())
            .await
            .unwrap();

        assert_eq!(
            tree(&out),
            expected(&[
                (APP, "exefs", exefs(22)),
                (APP, "romfs", patched_romfs(1, 1)),
                (APP + 1, "exefs", exefs(28)),
                (APP + 1, "romfs", patched_romfs(7, 1)),
            ])
        );
    }

    #[tokio::test]
    async fn nsz_entries_read_through_the_ncz_reader() {
        let dir = tempfile::tempdir().unwrap();
        let nsp = write_input(dir.path(), "merged.nsp", &[base_title(), update_title(1)]);
        let modes = [NczMode::Solid, NczMode::Block { size_exp: 14 }];
        for (i, mode) in modes.into_iter().enumerate() {
            let nsz = dir.path().join(format!("merged{i}.nsz"));
            let opts = NxCompressOptions { level: 1, mode };
            compress_container(&nsp, &nsz, opts, &synthetic_keyset(), &NoProgress, None).unwrap();
            let listing = list_container(&nsz).unwrap();
            let ncz = listing.entries.iter().filter(|e| e.name.ends_with(".ncz"));
            assert_eq!(ncz.count(), 2, "{mode:?}");
            let out = dir.path().join(format!("out{i}"));

            extract(&[nsz], &out, BOTH, &synthetic_keyset())
                .await
                .unwrap();

            assert_eq!(
                tree(&out),
                expected(&[
                    (APP, "exefs", exefs(22)),
                    (APP, "romfs", patched_romfs(1, 1)),
                ]),
                "{mode:?}"
            );
        }
    }

    #[tokio::test]
    async fn base_under_a_wrong_title_key_fails_the_patched_romfs() {
        let rights_id = [0xC5; 16];
        let mut base = title(
            CNMT_TYPE_APPLICATION,
            APP,
            0,
            vec![(program(APP, rights_id, 1), CNMT_CONTENT_TYPE_PROGRAM, 0)],
        );
        let mut ticket = build_common_ticket(rights_id, TEST_BODY_KEY);
        ticket[0x180] ^= 1;
        base.push((format!("{}.tik", hex::encode(rights_id)), ticket));
        let dir = tempfile::tempdir().unwrap();
        let input = write_input(dir.path(), "game.nsp", &[base, update_title(1)]);
        let out = dir.path().join("out");

        let result = extract(&[input], &out, BOTH, &synthetic_keyset_with_titlekek()).await;

        assert!(
            matches!(result, Err(NxError::WrongKey { .. })),
            "{result:?}"
        );
        assert!(tree(&out).is_empty());
    }

    #[tokio::test]
    async fn large_solid_nsz_and_xcz_inputs_extract() {
        // Over the 8 MiB eager-decode limit, so the solid NCZ reads
        // through its streaming cursor.
        let big: Files = vec![("big.bin".into(), noise(8 << 20, 70))];
        let (big_image, big_offsets) = romfs_image(&big);
        let program = build_nca(
            CONTENT_TYPE_PROGRAM,
            APP,
            [0; 16],
            &[pfs0_section(&exefs(1)), romfs_section(&big_image)],
        );
        let dir = tempfile::tempdir().unwrap();
        let nsp = write_input(
            dir.path(),
            "big.nsp",
            &[title(
                CNMT_TYPE_APPLICATION,
                APP,
                0,
                vec![(program, CNMT_CONTENT_TYPE_PROGRAM, 0)],
            )],
        );
        let xci = dir.path().join("game.xci");
        fs::write(
            &xci,
            build_test_xci_partitions(&[("secure", &base_title())]),
        )
        .unwrap();
        let cases = [
            (nsp, "big.nsz", NczMode::Solid, big.clone()),
            (
                xci,
                "game.xcz",
                NczMode::Block { size_exp: 14 },
                base_romfs(1),
            ),
        ];
        for (input, name, mode, romfs) in cases {
            let compressed = dir.path().join(name);
            let opts = NxCompressOptions { level: 1, mode };
            compress_container(
                &input,
                &compressed,
                opts,
                &synthetic_keyset(),
                &NoProgress,
                None,
            )
            .unwrap();
            let listing = list_container(&compressed).unwrap();
            assert!(
                listing.entries.iter().any(|e| e.name.ends_with(".ncz")),
                "{name}"
            );
            let out = dir.path().join(format!("{name}.out"));

            extract(&[compressed], &out, BOTH, &synthetic_keyset())
                .await
                .unwrap();

            assert_eq!(
                tree(&out),
                expected(&[(APP, "exefs", exefs(1)), (APP, "romfs", romfs)]),
                "{name}"
            );
        }

        // An update that changes the middle of big.bin reads the rest from
        // the solid base in order, which stays within the decode budget.
        let mut patched = big;
        let (mid, tail) = (4 << 20, (4 << 20) + 0x1000);
        patched[0].1[mid..tail].copy_from_slice(&noise(tail - mid, 71));
        let (image, offsets) = romfs_image(&patched);
        let (virt, base) = (DATA_LEVEL + offsets[0], DATA_LEVEL + big_offsets[0]);
        let (mid, tail) = (mid as u64, tail as u64);
        let patch = patch_romfs_section(
            &image,
            &[
                (virt, mid, base),
                (virt + tail, (8 << 20) - tail, base + tail),
            ],
        );
        let update = build_nca(
            CONTENT_TYPE_PROGRAM,
            PATCH,
            [0; 16],
            &[pfs0_section(&exefs(2)), patch],
        );
        let update = write_input(
            dir.path(),
            "update.nsp",
            &[title(
                CNMT_TYPE_PATCH,
                PATCH,
                1 << 16,
                vec![(update, CNMT_CONTENT_TYPE_PROGRAM, 0)],
            )],
        );
        let out = dir.path().join("patched.out");

        extract(
            &[dir.path().join("big.nsz"), update],
            &out,
            BOTH,
            &synthetic_keyset(),
        )
        .await
        .unwrap();

        assert_eq!(
            tree(&out),
            expected(&[(APP, "exefs", exefs(2)), (APP, "romfs", patched)])
        );
    }

    #[tokio::test]
    async fn xci_update_partition_is_ignored() {
        // A valid update in the update partition would patch the base.
        let dir = tempfile::tempdir().unwrap();
        let xci = dir.path().join("game.xci");
        fs::write(
            &xci,
            build_test_xci_partitions(&[("update", &update_title(1)), ("secure", &base_title())]),
        )
        .unwrap();
        let out = dir.path().join("out");

        extract(&[xci], &out, BOTH, &synthetic_keyset())
            .await
            .unwrap();

        assert_eq!(
            tree(&out),
            expected(&[(APP, "exefs", exefs(1)), (APP, "romfs", base_romfs(1))])
        );
    }

    #[tokio::test]
    async fn unreadable_meta_outside_the_secure_partition_warns() {
        let garbage = vec![(format!("{}.cnmt.nca", "0".repeat(32)), noise(0x4000, 9))];
        let dir = tempfile::tempdir().unwrap();
        let xci = dir.path().join("game.xci");
        fs::write(
            &xci,
            build_test_xci_partitions(&[("normal", &garbage), ("secure", &base_title())]),
        )
        .unwrap();
        let out = dir.path().join("out");

        let warnings = extract(&[xci], &out, BOTH, &synthetic_keyset())
            .await
            .unwrap();

        assert_eq!(
            tree(&out),
            expected(&[(APP, "exefs", exefs(1)), (APP, "romfs", base_romfs(1))])
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].starts_with("Skipped unreadable"),
            "{warnings:?}"
        );
    }
}
