//! V1-V4 to V5 migration: the decoded raw stream and every metadata
//! entry are copied through unchanged so the header hashes survive.

use crate::cd::FRAME_SIZE;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::legacy::LEGACY_COMPRESSION_AV;
use crate::chd::models::{
    CHD_METADATA_FLAG_HASHED, CHD_METADATA_RESERVED_BYTES, CHD_METADATA_TAG_CD,
    CHD_METADATA_TAG_HARD_DISK, ChdMetadataHeader,
};
use crate::chd::reader::cue_generator::parse_chd_track_metadata;
use crate::chd::writer::ChdWriter;
use crate::util::{BYTES_PER_MB, CancelToken, ProgressReporter, run_scratch_write};
use log::{info, warn};
use std::path::PathBuf;
use tokio::fs;

use super::*;

/// Default destination for a migrated CHD. Input and output share the
/// `.chd` extension, so the derived name gets a `v5` infix to keep the
/// V5 output off its own source.
pub fn migrated_chd_path(input: &std::path::Path) -> PathBuf {
    input.with_extension("v5.chd")
}

/// Rewrite the v1-v4 CHD at `input_path` as a v5 CHD at `output_path`;
/// on cancel the scratch CHD is removed and the destination is left
/// untouched.
pub async fn migrate_chd_to_v5(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    opts: ChdOptions,
    cancel: CancelToken,
) -> ChdResult<()> {
    match legacy::peek_chd_version(&input_path)? {
        Some(5) => return Err(ChdError::ChdAlreadyV5),
        Some(version) if version > 5 => return Err(ChdError::UnsupportedChdVersion),
        // Not a CHD at all: fall through and let the reader report the bad magic.
        _ => {}
    }
    if fs::metadata(&output_path).await.is_ok() && !opts.force {
        return Err(ChdError::ChdFileAlreadyExists);
    }

    let open_path = input_path.clone();
    let source = tokio::task::spawn_blocking(move || legacy::LegacyChd::open(&open_path)).await??;
    // V1/V2 keep their geometry in header fields V5 dropped, so it rides
    // along as the `GDDD` entry chdman writes.
    let geometry = source
        .header()
        .chs
        .as_ref()
        .map(|chs| (chs.cylinders, chs.heads, chs.sectors, chs.sector_bytes));
    let unit_bytes = source.header().unit_bytes;
    let logical_bytes = source.header().logical_bytes;
    let is_dvd = unit_bytes != FRAME_SIZE as u32;
    // A/V hunks are `avhu` frames; the generic slots store them faithfully but
    // far larger than chdman would.
    if source.header().compression == LEGACY_COMPRESSION_AV {
        warn!(
            "{} is an A/V CHD; the V5 output will not be avhuff-compressed and will be much larger",
            input_path.display()
        );
    }
    validate_chd_options(&opts, is_dvd)?;

    // Re-hunking would rewrite the map for no gain, so the source hunk
    // size carries over unless the caller overrides it.
    let hunk_bytes = match opts.hunk_size {
        Some(size) if size == 0 || !size.is_multiple_of(unit_bytes) => {
            return Err(ChdError::InvalidHunkSize);
        }
        Some(size) => size,
        None => source.header().hunk_bytes,
    };
    let codecs = opts.codecs.clone().unwrap_or_else(|| {
        if is_dvd {
            default_dvd_codecs()
        } else {
            default_cd_codecs()
        }
    });

    let total_mb = logical_bytes as f64 / BYTES_PER_MB;
    progress.start(
        logical_bytes,
        &format!("Migrating to CHD V5 (~{:.2} MB)", total_mb),
    );

    let level = opts.level;
    run_scratch_write(&output_path, true, progress, &cancel, move |write_path, bytes_done, cancel| -> ChdResult<()> {
        // Copied out field by field before `into_raw_reader` consumes the
        // source; ChdMetadataHeader is not Clone.
        let mut metadata: Vec<ChdMetadataHeader> = source
            .metadata()
            .iter()
            .map(|entry| ChdMetadataHeader {
                tag: entry.tag,
                flags: entry.flags,
                reserved: entry.reserved,
                data: entry.data.clone(),
            })
            .collect();
        if let Some((cylinders, heads, sectors, sector_bytes)) = geometry {
            let mut data =
                format!("CYLS:{cylinders},HEADS:{heads},SECS:{sectors},BPS:{sector_bytes}")
                    .into_bytes();
            data.push(0);
            metadata.push(ChdMetadataHeader {
                tag: CHD_METADATA_TAG_HARD_DISK,
                flags: CHD_METADATA_FLAG_HASHED,
                reserved: [0; CHD_METADATA_RESERVED_BYTES],
                data,
            });
        }
        // chdman's `copy` rewrites the pre-CHT2 `CHTR` entries as hashed
        // CHT2, so the migrated disc hashes like a current createcd and
        // matches MAME's checksums. `CHGD` stays verbatim: its upgrade
        // also byte-swaps the audio frames.
        if metadata.iter().any(|m| m.tag == CHD_METADATA_TAG_CD_TRACK)
            && !metadata.iter().any(|m| m.tag == CHD_METADATA_TAG_CD)
        {
            let text = cd_track_metadata_text(&metadata).unwrap_or_default();
            let tracks = parse_chd_track_metadata(&text)?;
            metadata.retain(|m| m.tag != CHD_METADATA_TAG_CD_TRACK);
            metadata.extend(tracks.iter().map(|t| {
                ChdMetadataHeader::new_cd_metadata(format!(
                    "TRACK:{} TYPE:{} SUBTYPE:{} FRAMES:{} PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0",
                    t.track_number,
                    t.track_type,
                    t.subtype.as_deref().unwrap_or("NONE"),
                    t.frames
                ))
            }));
        }
        let mut writer = ChdWriter::create_raw(
            &write_path,
            logical_bytes,
            hunk_bytes,
            unit_bytes,
            metadata,
            codecs,
            level,
        )?;
        let mut reader = source.into_raw_reader();
        writer.compress_all_hunks_raw(&mut reader, &bytes_done, &cancel)?;
        writer.finalize()?;
        Ok(())
    })
    .await?;

    let chd_size = fs::metadata(&output_path).await?.len();
    info!(
        "Migrated to CHD V5: {:.2} MB raw, {:.2} MB written",
        total_mb,
        chd_size as f64 / BYTES_PER_MB
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cd::CD_HUNK_BYTES;
    use crate::chd::models::SHA1_BYTES;
    use crate::chd::reader::ChdFlavor;
    use crate::chd::writer::metadata::MetadataHash;
    use sha1::{Digest, Sha1};

    use crate::util::NoProgress;
    use test_fixtures::mixed_iso;

    /// Hand-built V4 CHD holding `raw` as uncompressed `hunk_bytes` hunks,
    /// with `metadata` chained after the map and the header hashes filled
    /// in the way chdman would. An empty list leaves the chain absent, as
    /// chdman's `createraw` does.
    fn v4_raw_image(raw: &[u8], hunk_bytes: usize, metadata: &[ChdMetadataHeader]) -> Vec<u8> {
        const V4_HEADER_BYTES: usize = 108;

        let hunks = raw.len() / hunk_bytes;
        let raw_sha1: [u8; SHA1_BYTES] = Sha1::digest(raw).into();
        let hashes: Vec<MetadataHash> = metadata
            .iter()
            .filter(|m| m.flags & CHD_METADATA_FLAG_HASHED != 0)
            .map(|m| MetadataHash {
                tag: m.tag,
                sha1: Sha1::digest(&m.data).into(),
            })
            .collect();

        let mut image = vec![0u8; V4_HEADER_BYTES + hunks * 16];
        image[0..8].copy_from_slice(b"MComprHD");
        image[8..12].copy_from_slice(&(V4_HEADER_BYTES as u32).to_be_bytes());
        image[12..16].copy_from_slice(&4u32.to_be_bytes());
        // zlib, though every map entry below stores its hunk uncompressed.
        image[20..24].copy_from_slice(&1u32.to_be_bytes());
        image[24..28].copy_from_slice(&(hunks as u32).to_be_bytes());
        image[28..36].copy_from_slice(&(raw.len() as u64).to_be_bytes());
        image[44..48].copy_from_slice(&(hunk_bytes as u32).to_be_bytes());
        image[48..68].copy_from_slice(&compute_overall_sha1(raw_sha1, &hashes));
        image[88..108].copy_from_slice(&raw_sha1);

        if !metadata.is_empty() {
            let meta_offset = image.len() as u64;
            image[36..44].copy_from_slice(&meta_offset.to_be_bytes());
        }
        for (i, m) in metadata.iter().enumerate() {
            // Tag, flags, 24-bit length, next pointer (zero on the last).
            image.extend_from_slice(&m.tag);
            image.push(m.flags);
            image.extend_from_slice(&(m.data.len() as u32).to_be_bytes()[1..]);
            let next = if i + 1 == metadata.len() {
                0
            } else {
                image.len() as u64 + 8 + m.data.len() as u64
            };
            image.extend_from_slice(&next.to_be_bytes());
            image.extend_from_slice(&m.data);
        }

        let data_offset = image.len();
        for hunk in 0..hunks {
            let entry = V4_HEADER_BYTES + hunk * 16;
            image[entry..entry + 8]
                .copy_from_slice(&((data_offset + hunk * hunk_bytes) as u64).to_be_bytes());
            image[entry + 12..entry + 14].copy_from_slice(&(hunk_bytes as u16).to_be_bytes());
            // UNCOMPRESSED, CRC check suppressed.
            image[entry + 15] = 0x12;
        }
        image.extend_from_slice(raw);
        image
    }

    async fn migrate_image(image: &[u8]) -> crate::chd::reader::SyncChdHandle {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("legacy.chd");
        std::fs::write(&src, image).unwrap();
        let out = dir.path().join("migrated.chd");

        migrate_chd_to_v5(
            &NoProgress,
            src,
            out.clone(),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        verify_chd(&NoProgress, out.clone(), None, false, CancelToken::new())
            .await
            .unwrap();
        crate::chd::reader::open_chd_sync(&out).unwrap()
    }

    /// A V4 header stores the SHA-1 of the decoded raw data and the
    /// chained overall digest, so a migration that copies the stream and
    /// the metadata through byte for byte must reproduce both.
    #[tokio::test]
    async fn migrate_v4_chd_preserves_header_hashes() {
        let raw = mixed_iso(6);
        let image = v4_raw_image(&raw, 4096, &[ChdMetadataHeader::new_dvd_metadata()]);

        let handle = migrate_image(&image).await;
        assert_eq!(handle.header.raw_sha1, image[88..108]);
        assert_eq!(handle.header.sha1, image[48..68]);
        assert_eq!(handle.header.logical_bytes, raw.len() as u64);
        assert_eq!(handle.flavor(), ChdFlavor::Dvd);
    }

    /// chdman's `createraw` writes no metadata at all. The migrated V5
    /// must then carry a zero metadata offset, or readers walk the first
    /// hunk as a metadata entry and the overall SHA-1 stops verifying.
    #[tokio::test]
    async fn migrate_metadata_less_chd_leaves_meta_offset_zero() {
        let raw = mixed_iso(6);
        let image = v4_raw_image(&raw, 4096, &[]);

        let handle = migrate_image(&image).await;
        assert_eq!(handle.header.meta_offset, 0);
        assert!(handle.metadata.is_empty());
        assert_eq!(handle.header.sha1, image[48..68]);
    }

    /// Pre-2009 CD CHDs carry unhashed `CHTR` track entries. chdman's
    /// `copy` rewrites them as hashed CHT2 with the pregap fields at their
    /// defaults, and the migration has to do the same so the overall
    /// SHA-1 lands on the value current chdman builds produce.
    #[tokio::test]
    async fn migrate_upgrades_chtr_track_metadata_to_cht2() {
        // Two legacy CD hunks of four 2448-byte frames each.
        let raw: Vec<u8> = (0..2 * CD_HUNK_BYTES as usize)
            .map(|i| (i % 251) as u8)
            .collect();
        let chtr = ChdMetadataHeader {
            tag: CHD_METADATA_TAG_CD_TRACK,
            flags: 0,
            reserved: [0; CHD_METADATA_RESERVED_BYTES],
            data: b"TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:8\0".to_vec(),
        };
        let image = v4_raw_image(&raw, CD_HUNK_BYTES as usize, &[chtr]);

        let handle = migrate_image(&image).await;
        let cht2 = ChdMetadataHeader::new_cd_metadata(
            "TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:8 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"
                .to_string(),
        );
        assert_eq!(handle.metadata.len(), 1);
        assert_eq!(handle.metadata[0].tag, CHD_METADATA_TAG_CD);
        assert_eq!(handle.metadata[0].flags, CHD_METADATA_FLAG_HASHED);
        assert_eq!(handle.metadata[0].data, cht2.data);
        let expected = compute_overall_sha1(
            handle.header.raw_sha1,
            &[MetadataHash {
                tag: cht2.tag,
                sha1: Sha1::digest(&cht2.data).into(),
            }],
        );
        assert_eq!(handle.header.sha1, expected);
    }

    /// V1 keeps its geometry in header fields V5 does not have, so migrating
    /// has to re-express it as the `GDDD` entry or the image stops being
    /// recognizable as a hard disk.
    #[tokio::test]
    async fn migrate_v1_chd_carries_geometry_into_gddd() {
        const V1_HEADER_BYTES: usize = 76;
        const SECTOR_BYTES: u32 = 512;
        const HUNK_SECTORS: u32 = 8;
        const CYLINDERS: u32 = 2;
        const HEADS: u32 = 1;
        const SECTORS: u32 = 8;

        let hunk_bytes = (SECTOR_BYTES * HUNK_SECTORS) as usize;
        let raw = mixed_iso(4)[..hunk_bytes * 2].to_vec();
        let hunks = raw.len() / hunk_bytes;
        let data_offset = V1_HEADER_BYTES + hunks * 8;

        let mut image = vec![0u8; data_offset];
        image[0..8].copy_from_slice(b"MComprHD");
        image[8..12].copy_from_slice(&(V1_HEADER_BYTES as u32).to_be_bytes());
        image[12..16].copy_from_slice(&1u32.to_be_bytes());
        image[20..24].copy_from_slice(&1u32.to_be_bytes());
        image[24..28].copy_from_slice(&HUNK_SECTORS.to_be_bytes());
        image[28..32].copy_from_slice(&(hunks as u32).to_be_bytes());
        image[32..36].copy_from_slice(&CYLINDERS.to_be_bytes());
        image[36..40].copy_from_slice(&HEADS.to_be_bytes());
        image[40..44].copy_from_slice(&SECTORS.to_be_bytes());

        // V1 entries pack the length in the top 20 bits; length == hunk_bytes
        // marks the hunk uncompressed.
        for hunk in 0..hunks {
            let offset = data_offset + hunk * hunk_bytes;
            let packed = ((hunk_bytes as u64) << 44) | offset as u64;
            let base = V1_HEADER_BYTES + hunk * 8;
            image[base..base + 8].copy_from_slice(&packed.to_be_bytes());
        }
        image.extend_from_slice(&raw);

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("legacy.chd");
        std::fs::write(&src, &image).unwrap();
        let out = dir.path().join("migrated.chd");

        migrate_chd_to_v5(
            &NoProgress,
            src,
            out.clone(),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let handle = crate::chd::reader::open_chd_sync(&out).unwrap();
        assert_eq!(handle.header.unit_bytes, SECTOR_BYTES);
        assert_eq!(handle.header.logical_bytes, raw.len() as u64);
        let gddd = handle
            .metadata
            .iter()
            .find(|entry| entry.tag == CHD_METADATA_TAG_HARD_DISK)
            .expect("geometry carried over");
        assert_eq!(
            String::from_utf8_lossy(&gddd.data).trim_end_matches('\0'),
            "CYLS:2,HEADS:1,SECS:8,BPS:512"
        );
    }
}
