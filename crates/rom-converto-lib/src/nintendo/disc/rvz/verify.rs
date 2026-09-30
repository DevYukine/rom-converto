//! Structural (fast) verification of an RVZ container's stored SHA-1 hashes.
//!
//! Shared by the GameCube ([`crate::nintendo::dol::verify`]) and Wii
//! ([`crate::nintendo::rvl::verify`]) verify paths. Re-reads the file header,
//! disc struct and partition table and checks all three stored SHA-1 digests
//! without decompressing any group data, and reads the group descriptor table
//! to check that the file covers the stored bytes of every group with data.
//! Unlike [`super::decompress::RvzDiscReader`]
//! this reports each hash independently instead of erroring on the first
//! mismatch, and it also checks the partition-table hash that the reader skips.

use binrw::{BinRead, Endian};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use crate::util::CancelToken;

use crate::nintendo::disc::rvz::constants::RVZ_MAGIC;
use crate::nintendo::disc::rvz::error::{RvzError, RvzResult};
use crate::nintendo::disc::rvz::format::sha1::{
    compute_disc_hash, compute_file_head_hash, compute_part_hash,
};
use crate::nintendo::disc::rvz::format::{
    WIA_DISC_SIZE, WIA_FILE_HEAD_SIZE, WIA_PART_SIZE, WiaDisc, WiaFileHead, WiaPart,
};
use crate::nintendo::disc::rvz::read_group_table;
use crate::util::Cancelled;

/// Result of verifying the three SHA-1 hashes an RVZ container stores over its
/// own metadata structs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "runner.ts"))]
pub struct RvzStructuralVerify {
    pub file_head_hash_ok: bool,
    pub disc_hash_ok: bool,
    /// Partition-table hash verdict. `false` means the stored hash does
    /// not match; `None` means it was not checked (the container
    /// declares no partition table, or an earlier stored hash already
    /// failed).
    pub part_hash_ok: Option<bool>,
    /// 1 = GameCube, 2 = Wii.
    pub disc_type: u32,
    pub iso_size: u64,
    pub n_part: u32,
}

impl RvzStructuralVerify {
    /// True if every stored hash present matched (a missing partition
    /// table hash does not fail the check).
    pub fn ok(&self) -> bool {
        self.file_head_hash_ok && self.disc_hash_ok && self.part_hash_ok != Some(false)
    }
}

/// Check the RVZ at `path` against its own header, disc and group
/// hashes, without decompressing the disc data.
pub fn verify_rvz_structure(path: &Path, cancel: &CancelToken) -> RvzResult<RvzStructuralVerify> {
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let mut reader = BufReader::with_capacity(64 * 1024, File::open(path)?);

    let mut head_bytes = vec![0u8; WIA_FILE_HEAD_SIZE];
    reader.read_exact(&mut head_bytes)?;
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let head = WiaFileHead::read_options(&mut Cursor::new(&head_bytes), Endian::Big, ())?;
    if head.magic != RVZ_MAGIC {
        return Err(RvzError::InvalidMagic(head.magic));
    }
    let file_head_hash_ok = compute_file_head_hash(&head) == head.file_head_hash;

    // The size checks run only on an authenticated head: a flipped head
    // must produce a hash report, not an error computed from its
    // unverified fields.
    let file_len = reader.get_ref().metadata()?.len();
    if file_head_hash_ok {
        // The container declares its own size; falling short of that is
        // truncation no matter what the tables still manage to parse.
        if file_len < head.wia_file_size {
            return Err(RvzError::Truncated {
                expected: head.wia_file_size,
                actual: file_len,
            });
        }
        super::check_disc_size(&head, file_len)?;
        if head.disc_size < WIA_DISC_SIZE as u32 {
            return Err(RvzError::Custom(
                "disc struct is smaller than the fixed RVZ structure".into(),
            ));
        }
    }

    // The disc struct is a fixed size, so the read is bounded whatever
    // the (possibly unauthenticated) head claims; the hash verdict
    // needs its bytes either way.
    let mut disc_bytes = vec![0u8; WIA_DISC_SIZE];
    reader.read_exact(&mut disc_bytes)?;
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let disc = WiaDisc::read_options(&mut Cursor::new(&disc_bytes), Endian::Big, ())?;
    let disc_hash_ok = compute_disc_hash(&disc) == head.disc_hash;
    // A failed stored hash means every field below it is unauthenticated:
    // report the hashes instead of erroring on tables parsed from them.
    if !file_head_hash_ok || !disc_hash_ok {
        return Ok(RvzStructuralVerify {
            file_head_hash_ok,
            disc_hash_ok,
            part_hash_ok: None,
            disc_type: disc.disc_type,
            iso_size: head.iso_file_size,
            n_part: disc.n_part,
        });
    }
    super::check_table_bounds(&disc, file_len, head.iso_file_size)?;

    let mut parts = Vec::new();
    let mut part_hash_ok = None;
    if disc.n_part > 0 {
        reader.seek(SeekFrom::Start(disc.part_off))?;
        let mut buf = vec![0u8; disc.n_part as usize * WIA_PART_SIZE];
        reader.read_exact(&mut buf)?;
        let mut cur = Cursor::new(&buf);
        parts = Vec::with_capacity(disc.n_part as usize);
        for _ in 0..disc.n_part {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            parts.push(WiaPart::read_options(&mut cur, Endian::Big, ())?);
        }
        part_hash_ok = Some(compute_part_hash(&parts) == disc.part_hash);
        // A failed partition-table hash is a verdict: report it instead
        // of letting a later table error (for example the decode
        // limitation of an oversized partitioned chunk) replace it.
        if part_hash_ok == Some(false) {
            return Ok(RvzStructuralVerify {
                file_head_hash_ok,
                disc_hash_ok,
                part_hash_ok,
                disc_type: disc.disc_type,
                iso_size: head.iso_file_size,
                n_part: disc.n_part,
            });
        }
    }

    // A data-truncated container must never verify: the file has to at
    // least cover every group's stored bytes. Headers and tables hashing
    // clean says nothing about the chunk data, so read the group table
    // (metadata only; no group data is decompressed) and bound it. The
    // bound runs over all groups with stored data, not just the last
    // entry: tables can close with zero sentinels that would hide an
    // earlier group running past EOF.
    let groups = read_group_table(&mut reader, &disc, head.iso_file_size)?;
    super::check_group_data_bounds(&groups, file_len)?;

    // The region and partition group ranges must stay inside the table
    // that was just parsed, so no later lookup can index past it.
    let raw_data = super::read_raw_data_table(&mut reader, &disc)?;
    super::check_group_indices(
        &raw_data,
        &parts,
        &groups,
        head.iso_file_size,
        disc.chunk_size,
    )?;

    Ok(RvzStructuralVerify {
        file_head_hash_ok,
        disc_hash_ok,
        part_hash_ok,
        disc_type: disc.disc_type,
        iso_size: head.iso_file_size,
        n_part: disc.n_part,
    })
}

/// Structural RVZ check for the disc verifiers: returns the report for
/// RVZ input, `(None, None)` for non-RVZ input, the reason string for a
/// structurally broken container, and propagates errors the fail-closed
/// classifier attributes to the environment rather than the container.
pub fn verify_rvz_structure_reported(
    path: &Path,
    cancel: &CancelToken,
) -> Result<(Option<RvzStructuralVerify>, Option<String>), RvzError> {
    match verify_rvz_structure(path, cancel) {
        Ok(structure) => Ok((Some(structure), None)),
        // Not an RVZ container (plain ISO/GCM, GCZ, …): nothing to check.
        Err(RvzError::InvalidMagic(_)) => Ok((None, None)),
        Err(e) if cancel.is_cancelled() => Err(e),
        // Fail-closed: an error the shared classifier deems
        // "nobody checked" (environment, cancellation, infrastructure)
        // aborts the run instead of reading as a broken container.
        Err(e) if crate::util::verify::unverifiable(&e) => Err(e),
        // A structurally broken RVZ fails the verify without aborting
        // the run; the reason rides along for `rvz_note`.
        Err(e) => Ok((None, Some(e.to_string()))),
    }
}

/// Hand-built minimal containers for the verifier tests here and in the
/// GameCube/Wii verify suites: file head + disc struct + compressed
/// `groups` table + a valid empty raw-data table + `trailing` bytes.
/// Both stored hashes are computed, so only the group bounds can be
/// wrong; the raw table keeps `RvzDiscReader::open` working without
/// touching any group data.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::nintendo::disc::rvz::format::{RvzGroup, WIA_DISC_SIZE, WiaRawData};
    use binrw::BinWrite;

    pub(crate) fn build_rvz(dhead: [u8; 128], groups: &[RvzGroup], trailing: &[u8]) -> Vec<u8> {
        build_rvz_with(dhead, 0, groups.len() as u32, groups, trailing)
    }

    /// [`build_rvz`] with `disc.n_part`/`disc.n_groups` overridden: both
    /// stored hashes are recomputed, so only the header bounds can reject
    /// the container. `n_groups` need not match `groups`: an inflated
    /// count with an empty table is exactly the corrupt-header shape the
    /// bounds exist to catch.
    pub(crate) fn build_rvz_with(
        dhead: [u8; 128],
        n_part: u32,
        n_groups: u32,
        groups: &[RvzGroup],
        trailing: &[u8],
    ) -> Vec<u8> {
        build_rvz_custom(dhead, n_part, n_groups, &[], groups, trailing, |_| {})
    }

    /// [`build_rvz_with`] with explicit raw-data table entries and a
    /// closure that mutates the disc struct before its hash is computed,
    /// so tests can forge corrupt field values (chunk size, raw-data
    /// count, ...) that keep the stored hashes otherwise valid.
    pub(crate) fn build_rvz_custom(
        dhead: [u8; 128],
        n_part: u32,
        n_groups: u32,
        raw_data: &[WiaRawData],
        groups: &[RvzGroup],
        trailing: &[u8],
        tweak: impl FnOnce(&mut WiaDisc),
    ) -> Vec<u8> {
        let mut table_raw = Vec::new();
        {
            let mut table_cursor = Cursor::new(&mut table_raw);
            for group in groups {
                group
                    .write_options(&mut table_cursor, Endian::Big, ())
                    .unwrap();
            }
        }
        let table = zstd::bulk::compress(&table_raw, 3).unwrap();

        let mut raw_raw = Vec::new();
        {
            let mut raw_cursor = Cursor::new(&mut raw_raw);
            for entry in raw_data {
                entry
                    .write_options(&mut raw_cursor, Endian::Big, ())
                    .unwrap();
            }
        }
        let raw_table = zstd::bulk::compress(&raw_raw, 3).unwrap();

        let group_off = (WIA_FILE_HEAD_SIZE + WIA_DISC_SIZE) as u64;
        let raw_data_off = group_off + table.len() as u64;
        let mut disc = WiaDisc {
            disc_type: 1,
            compression: 5,
            compr_level: 3,
            chunk_size: 2 * 1024 * 1024,
            dhead,
            n_part,
            part_t_size: WIA_PART_SIZE as u32,
            part_off: 0,
            part_hash: [0u8; 20],
            n_raw_data: raw_data.len() as u32,
            raw_data_off,
            raw_data_size: raw_table.len() as u32,
            n_groups,
            group_off,
            group_size: table.len() as u32,
            compr_data_len: 0,
            compr_data: [0u8; 7],
        };
        tweak(&mut disc);
        let disc_hash = compute_disc_hash(&disc);
        let file_len =
            (WIA_FILE_HEAD_SIZE + WIA_DISC_SIZE + table.len() + raw_table.len() + trailing.len())
                as u64;
        let head = WiaFileHead {
            magic: RVZ_MAGIC,
            version: 0x0100_0000,
            version_compatible: 0x0003_0000,
            disc_size: WIA_DISC_SIZE as u32,
            disc_hash,
            iso_file_size: 0x1_0000,
            wia_file_size: file_len,
            file_head_hash: [0u8; 20],
        };
        let head = WiaFileHead {
            file_head_hash: compute_file_head_hash(&head),
            ..head
        };

        let mut file = Vec::new();
        {
            let mut cur = Cursor::new(&mut file);
            head.write_options(&mut cur, Endian::Big, ()).unwrap();
            disc.write_options(&mut cur, Endian::Big, ()).unwrap();
        }
        file.extend_from_slice(&table);
        file.extend_from_slice(&raw_table);
        file.extend_from_slice(trailing);
        file
    }

    /// [`build_rvz`] with the last `cut` bytes removed and
    /// `wia_file_size` (plus the file-head hash) rewritten to describe
    /// the shortened file, so only the group bound can flag the missing
    /// data bytes.
    pub(crate) fn build_rvz_cut(
        dhead: [u8; 128],
        groups: &[RvzGroup],
        trailing: &[u8],
        cut: usize,
    ) -> Vec<u8> {
        let mut file = build_rvz(dhead, groups, trailing);
        file.truncate(file.len() - cut);
        let mut head = {
            let mut cur = Cursor::new(&file[..]);
            WiaFileHead::read_options(&mut cur, Endian::Big, ()).unwrap()
        };
        head.wia_file_size = file.len() as u64;
        head.file_head_hash = compute_file_head_hash(&head);
        head.write_options(&mut Cursor::new(&mut file), Endian::Big, ())
            .unwrap();
        file
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::disc::rvz::format::RvzGroup;
    use binrw::BinWrite;

    fn mk(file: bool, disc: bool, part: Option<bool>) -> RvzStructuralVerify {
        RvzStructuralVerify {
            file_head_hash_ok: file,
            disc_hash_ok: disc,
            part_hash_ok: part,
            disc_type: 2,
            iso_size: 0,
            n_part: part.map(|_| 1).unwrap_or(0),
        }
    }

    #[test]
    fn ok_requires_head_and_disc_and_part_not_false() {
        assert!(mk(true, true, Some(true)).ok());
        assert!(mk(true, true, None).ok());
        assert!(!mk(false, true, Some(true)).ok());
        assert!(!mk(true, false, Some(true)).ok());
        assert!(!mk(true, true, Some(false)).ok());
    }

    /// A minimal container with clean hashes but no group data: the bounds
    /// check must reject a group whose stored bytes run past EOF, and
    /// accept the same container when the group fits inside the file.
    #[test]
    fn last_group_must_fit_inside_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok.rvz");
        // The group holds 8 bytes at offset 0: the tables alone cover it.
        std::fs::write(
            &ok,
            test_support::build_rvz([0u8; 128], &[RvzGroup::new_compressed(0, 8, 0)], &[]),
        )
        .unwrap();
        assert!(verify_rvz_structure(&ok, &CancelToken::new()).unwrap().ok());

        let truncated = dir.path().join("truncated.rvz");
        // The same container with the group's data starting at 16 KiB,
        // well past the ~300-byte file: clean hashes are not enough.
        std::fs::write(
            &truncated,
            test_support::build_rvz(
                [0u8; 128],
                &[RvzGroup::new_compressed(0x4000 / 4, 8, 0)],
                &[],
            ),
        )
        .unwrap();
        let err = verify_rvz_structure(&truncated, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, RvzError::Truncated { .. }), "{err}");
    }

    /// The truncation bound covers EVERY group with stored data, not just
    /// the table's last entry: a zero sentinel closing the table must not
    /// hide a middle group running past EOF.
    #[test]
    fn middle_group_past_eof_is_truncated_despite_zero_sentinel_last() {
        let groups = [
            RvzGroup::new_compressed(0, 8, 0),
            RvzGroup::new_compressed(0x4000 / 4, 8, 0),
            RvzGroup {
                data_off4: 0,
                data_size: 0,
                rvz_packed_size: 0,
            },
        ];
        let file = test_support::build_rvz([0u8; 128], &groups, &[]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sentinel.rvz");
        std::fs::write(&path, file).unwrap();
        let err = verify_rvz_structure(&path, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, RvzError::Truncated { .. }), "{err}");
    }

    /// A corrupt partition count must read as Truncated before the
    /// partition-array allocation: `0xFFFFFFFF * WIA_PART_SIZE` bytes
    /// would never be allocated, let alone read.
    #[test]
    fn huge_n_part_is_truncated_not_allocated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge_n_part.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_with([0u8; 128], u32::MAX, 0, &[], &[]),
        )
        .unwrap();
        let err = verify_rvz_structure(&path, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, RvzError::Truncated { .. }), "{err}");
    }

    /// Symmetrically, the group-table bound must cap the decompressed
    /// allocation: a 27-byte table claiming `u32::MAX` 12-byte entries
    /// reads as TableTooLarge from the geometric bound instead of
    /// asking zstd for 48 GiB.
    #[test]
    fn huge_n_groups_is_table_too_large_not_allocated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge_n_groups.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_with([0u8; 128], 0, u32::MAX, &[], &[]),
        )
        .unwrap();
        let err = verify_rvz_structure(&path, &CancelToken::new()).unwrap_err();
        assert!(
            matches!(
                err,
                RvzError::TableTooLarge {
                    table: "group",
                    entries: u32::MAX
                }
            ),
            "{err}"
        );
    }

    /// The 16 KiB RLE-frame shape: an inflated count whose table the
    /// zstd expansion ratio would still allow (a tiny RLE frame can
    /// legitimately expand that far) must read as TableTooLarge from
    /// the geometric bound, before the compressed bytes are read or
    /// expanded, so no table-sized allocation ever happens.
    #[test]
    fn inflated_n_groups_passing_ratio_gate_is_table_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inflated_groups.rvz");
        // 100k groups × 12 bytes = 1.2 MiB of raw table behind an empty
        // frame: the ratio cap (~8 MiB) allows it, the geometry does not.
        std::fs::write(
            &path,
            test_support::build_rvz_with([0u8; 128], 0, 100_000, &[], &[]),
        )
        .unwrap();
        let err = verify_rvz_structure(&path, &CancelToken::new()).unwrap_err();
        assert!(
            matches!(
                err,
                RvzError::TableTooLarge {
                    table: "group",
                    entries: 100_000
                }
            ),
            "{err}"
        );
    }

    /// Tables-first layout: the group table sits before the data, so
    /// truncating the data tail leaves a fully parseable table with clean
    /// hashes; the bounds must still catch the missing group bytes. The
    /// header describes the shortened length, so only the group bound can
    /// fire.
    #[test]
    fn tables_first_layout_truncated_in_data_is_truncated() {
        const TRAILING_LEN: usize = 0x8000;
        const CUT: usize = 0x1000;
        // The table length does not depend on the descriptor values, so a
        // throwaway build locates the data region for the real offsets.
        let probe = test_support::build_rvz([0u8; 128], &[], &[0u8; TRAILING_LEN]);
        let data_start = probe.len() - TRAILING_LEN;
        let groups = [
            RvzGroup::new_compressed(0, 8, 0),
            RvzGroup::new_compressed(((data_start + TRAILING_LEN - 8) / 4) as u32, 8, 0),
        ];
        let file = test_support::build_rvz_cut([0u8; 128], &groups, &[0u8; TRAILING_LEN], CUT);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tables_first.rvz");
        std::fs::write(&path, &file).unwrap();
        let err = verify_rvz_structure(&path, &CancelToken::new()).unwrap_err();
        match err {
            RvzError::Truncated { expected, actual } => {
                // The group bound reports the last group's end, and the
                // file really is the shortened length the header claims.
                assert_eq!(expected, (u64::from(groups[1].data_off4) << 2) + 8);
                assert_eq!(actual, file.len() as u64);
            }
            other => panic!("expected Truncated, got {other}"),
        }
    }

    /// End to end: a real container verifies, and cutting the file below
    /// the groups' stored bytes fails verification (an error classified
    /// as the container being broken, which overwrite flows treat as a
    /// rewrite trigger).
    #[tokio::test]
    async fn truncated_container_does_not_verify() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");

        // 5 MiB at the 128 KiB default chunk size.
        let original =
            crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso(5 * 1024 * 1024 + 123);
        std::fs::write(&iso, &original).unwrap();
        crate::nintendo::disc::rvz::compress_disc(
            &iso,
            &rvz,
            crate::nintendo::disc::rvz::RvzCompressOptions::default(),
            &crate::util::NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert!(
            verify_rvz_structure(&rvz, &CancelToken::new())
                .unwrap()
                .ok()
        );

        // The group table closes the file, so cutting the tail removes it
        // along with the last group's data: either way, not valid.
        let full = std::fs::read(&rvz).unwrap();
        std::fs::write(&rvz, &full[..full.len() - 4096]).unwrap();
        assert!(
            !verify_rvz_structure(&rvz, &CancelToken::new())
                .map(|r| r.ok())
                .unwrap_or(false)
        );
    }

    /// A failed stored hash makes every field below it unauthenticated:
    /// the verifier reports the failing hashes instead of erroring on
    /// tables parsed from them, no matter how hostile those tables are.
    #[test]
    fn failed_hash_reports_before_table_reads() {
        // Corrupt head hash + a group count no table could back: without
        // the early report this reads as TableTooLarge and hides the
        // hash verdict.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_head_hash.rvz");
        let mut file = test_support::build_rvz_with([0u8; 128], 0, u32::MAX, &[], &[]);
        let mut head =
            WiaFileHead::read_options(&mut Cursor::new(&file[..]), Endian::Big, ()).unwrap();
        head.file_head_hash = [0x5Au8; 20];
        head.write_options(&mut Cursor::new(&mut file), Endian::Big, ())
            .unwrap();
        std::fs::write(&path, &file).unwrap();
        let report = verify_rvz_structure(&path, &CancelToken::new()).unwrap();
        assert!(!report.file_head_hash_ok);
        assert!(report.disc_hash_ok);
        assert_eq!(report.part_hash_ok, None);
        assert!(!report.ok());

        // Corrupt disc hash + a hostile group table (a declared size no
        // zstd frame could back): same shape, the disc verdict must
        // reach the caller. The head hash is recomputed so only the
        // disc verdict fails.
        let path = dir.path().join("bad_disc_hash.rvz");
        let mut file = test_support::build_rvz_custom(
            [0u8; 128],
            0,
            1,
            &[],
            &[RvzGroup::new_compressed(0, 8, 0)],
            &[],
            |disc| disc.group_size = 0x100_0000,
        );
        let mut head =
            WiaFileHead::read_options(&mut Cursor::new(&file[..]), Endian::Big, ()).unwrap();
        head.disc_hash = [0xFFu8; 20];
        head.file_head_hash = compute_file_head_hash(&head);
        head.write_options(&mut Cursor::new(&mut file), Endian::Big, ())
            .unwrap();
        std::fs::write(&path, &file).unwrap();
        let report = verify_rvz_structure(&path, &CancelToken::new()).unwrap();
        assert!(report.file_head_hash_ok);
        assert!(!report.disc_hash_ok);
        assert!(!report.ok());
    }

    /// A partitioned container whose chunk size exceeds the 2 MiB
    /// partition-decode limit is a decode limitation, not corruption:
    /// verification reports an unverifiable-classified error, never an
    /// invalid-container error.
    #[test]
    fn partitioned_oversized_chunk_is_unverifiable_not_invalid() {
        use crate::nintendo::disc::rvz::format::sha1::compute_part_hash;
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [crate::nintendo::disc::rvz::format::WiaPartData {
                first_sector: 0,
                n_sectors: 0,
                group_index: 0,
                n_groups: 0,
            }; 2],
        };
        let mut part_bytes = Vec::new();
        part.write_options(&mut Cursor::new(&mut part_bytes), Endian::Big, ())
            .unwrap();

        // 6 MiB: not a power of two, but a multiple of 2 MiB, so the
        // container-level chunk rule accepts it.
        for chunk_size in [4 * 1024 * 1024u32, 6 * 1024 * 1024] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("part_chunk_{chunk_size}.rvz"));
            std::fs::write(
                &path,
                test_support::build_rvz_custom([0u8; 128], 1, 0, &[], &[], &part_bytes, |disc| {
                    disc.part_hash = compute_part_hash(std::slice::from_ref(&part));
                    disc.chunk_size = chunk_size;
                    disc.part_off = disc.raw_data_off + u64::from(disc.raw_data_size);
                }),
            )
            .unwrap();
            let err = verify_rvz_structure(&path, &CancelToken::new()).unwrap_err();
            assert!(
                matches!(err, RvzError::PartitionChunkTooLarge(_, _)),
                "{err}"
            );
            // The classifier routes this to Unverified, not Invalid.
            assert!(crate::util::verify::unverifiable(&err), "{err}");
        }
    }

    /// A flipped head must yield the hash report even when its size
    /// fields are hostile: the size checks run only on an authenticated
    /// head and the disc struct is read at its fixed size.
    #[test]
    fn flipped_head_reports_hash_despite_hostile_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hostile_head.rvz");
        let mut file =
            test_support::build_rvz([0u8; 128], &[RvzGroup::new_compressed(0, 8, 0)], &[]);
        let mut head =
            WiaFileHead::read_options(&mut Cursor::new(&file[..]), Endian::Big, ()).unwrap();
        head.disc_size = u32::MAX;
        head.wia_file_size = u64::MAX;
        head.file_head_hash = [0x5Au8; 20];
        head.write_options(&mut Cursor::new(&mut file), Endian::Big, ())
            .unwrap();
        std::fs::write(&path, &file).unwrap();

        let report = verify_rvz_structure(&path, &CancelToken::new()).unwrap();
        assert!(!report.file_head_hash_ok);
        assert!(report.disc_hash_ok);
        assert!(!report.ok());
    }

    /// An authenticated head whose `disc_size` is smaller than the fixed
    /// disc struct is corrupt and reads a clear error instead of a short
    /// parse.
    #[test]
    fn shrunk_disc_struct_size_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small_disc_struct.rvz");
        let mut file =
            test_support::build_rvz([0u8; 128], &[RvzGroup::new_compressed(0, 8, 0)], &[]);
        let mut head =
            WiaFileHead::read_options(&mut Cursor::new(&file[..]), Endian::Big, ()).unwrap();
        head.disc_size = 4;
        head.file_head_hash = compute_file_head_hash(&head);
        head.write_options(&mut Cursor::new(&mut file), Endian::Big, ())
            .unwrap();
        std::fs::write(&path, &file).unwrap();

        let err = verify_rvz_structure(&path, &CancelToken::new()).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m)
                if m.contains("disc struct is smaller than the fixed RVZ structure")),
            "{err}"
        );
    }

    /// A failed partition-table hash is a verdict on its own: a
    /// partitioned container whose chunk exceeds the decode limit must
    /// report the hash failure instead of an error that classifies as
    /// merely unverifiable.
    #[test]
    fn failed_part_hash_reports_before_chunk_check() {
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [crate::nintendo::disc::rvz::format::WiaPartData {
                first_sector: 0,
                n_sectors: 0,
                group_index: 0,
                n_groups: 0,
            }; 2],
        };
        let mut part_bytes = Vec::new();
        part.write_options(&mut Cursor::new(&mut part_bytes), Endian::Big, ())
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_part_hash.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom([0u8; 128], 1, 0, &[], &[], &part_bytes, |disc| {
                // The stored part hash stays zero while the parsed table
                // hashes differently: the part verdict is a failure.
                disc.chunk_size = 6 * 1024 * 1024;
                disc.part_off = disc.raw_data_off + u64::from(disc.raw_data_size);
            }),
        )
        .unwrap();

        let report = verify_rvz_structure(&path, &CancelToken::new()).unwrap();
        assert_eq!(report.part_hash_ok, Some(false));
        assert!(report.file_head_hash_ok);
        assert!(report.disc_hash_ok);
        assert!(!report.ok());
    }
}
