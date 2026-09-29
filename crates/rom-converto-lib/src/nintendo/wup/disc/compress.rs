//! Stream a Wii U disc image into a ZArchive writer.
//!
//! Decryption walks the disc layer by layer:
//!
//! 1. Partition TOC at `0x18000` (disc key, zero IV).
//! 2. SI partition header (plaintext) + SI FST (disc key, zero IV).
//! 3. Per-title `title.tik` and `title.tmd` from inside the SI FST
//!    (disc key, per-file-offset IV).
//! 4. Title key from the ticket (Wii U common key).
//! 5. GM partition header (plaintext) + content 0 (title key, raw
//!    mode) to produce the game's FST.
//! 6. Each virtual file decrypted on demand through the shared
//!    [`ContentLoader`].

use std::io::Write;
use std::path::Path;

use crate::nintendo::wup::disc::disc_key::{DiscKey, load_disc_key};
use crate::nintendo::wup::disc::partition::{
    PartitionContentLocation, PartitionContentSource, compute_content_location,
    read_disc_decrypted_file_iv, read_disc_decrypted_zero_iv_range, read_partition_header,
};
use crate::nintendo::wup::disc::partition_table::{
    PartitionEntry, PartitionKind, PartitionTable, parse_partition_table,
};
use crate::nintendo::wup::disc::sector_stream::{DiscSectorSource, SECTOR_SIZE};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::WupTmd;
use crate::nintendo::wup::models::ticket::WUP_TICKET_BASE_SIZE;
use crate::nintendo::wup::models::tmd::WUP_TMD_HEADER_SIZE;
use crate::nintendo::wup::nus::content_reader::decrypt_raw_range;
use crate::nintendo::wup::nus::content_stream::ContentLoader;
use crate::nintendo::wup::nus::fst_parser::{VirtualFs, parse_fst_ranges};
use crate::nintendo::wup::nus::ticket_parser::{TitleKey, parse_ticket_bytes};
use crate::util::Cancelled;
use crate::util::ProgressReporter;
use crate::zar::ZarWriter;

fn parse_disc_content0_fst(
    disc: &mut dyn DiscSectorSource,
    offset: u64,
    size: u64,
    title_key: &TitleKey,
) -> WupResult<VirtualFs> {
    // The TMD only declares content0's size; bound it against the bytes
    // actually remaining in the disc image so a crafted small image with
    // an inflated declared size cannot drive parse_fst_ranges into
    // allocating for entries/names that could never physically fit.
    let disc_len = disc.total_sectors() * SECTOR_SIZE as u64;
    let remaining = disc_len.saturating_sub(offset);
    let size = size.min(remaining);
    parse_fst_ranges(size, |byte_offset, len| {
        decrypt_raw_range(
            |relative, output| {
                let at = offset.checked_add(relative).ok_or(WupError::InvalidFst)?;
                disc.read_bytes(at, output)?;
                Ok(())
            },
            title_key,
            0,
            byte_offset,
            len,
        )
    })
}

/// Prepared per-partition plans for one disc: the decrypted FST and
/// content location map for every content partition backed by a real
/// SI title, computed once and shared between size estimation and
/// compression.
pub(crate) struct DiscTitlePlan {
    partitions: Vec<PartitionPlan>,
}

impl DiscTitlePlan {
    pub(crate) fn uncompressed_bytes(&self) -> u64 {
        self.partitions
            .iter()
            .flat_map(|plan| &plan.fs.files)
            .filter(|file| !file.is_shared)
            .map(|file| u64::from(file.file_size))
            .fold(0, u64::saturating_add)
    }
}

pub(crate) fn prepare_disc_title(
    disc_path: &Path,
    key_override: Option<&Path>,
) -> WupResult<DiscTitlePlan> {
    let mut disc = crate::nintendo::wup::disc::sector_stream::open_disc(disc_path)?;
    let key = load_disc_key(disc_path, key_override)?;
    let table = parse_partition_table(&mut *disc, &key)?;
    let si = table
        .find_si()
        .cloned()
        .ok_or(WupError::InvalidPartitionHeader)?;
    let si_titles = parse_si_titles(&mut *disc, &si, &key)?;
    if !table
        .content_partitions()
        .any(|p| matches!(p.kind, PartitionKind::Game))
    {
        return Err(WupError::NoGamePartitionFound);
    }
    let mut partitions = Vec::new();
    for (toc_index, partition) in content_partitions_with_index(&table) {
        if let Some(plan) = build_partition_plan(&mut *disc, partition, toc_index, &si_titles)? {
            partitions.push(plan);
        }
    }
    if partitions.is_empty() {
        return Err(WupError::NoGamePartitionFound);
    }
    Ok(DiscTitlePlan { partitions })
}

/// Resolve one content partition's SI title by TOC index and build its
/// [`PartitionPlan`]. A partition whose SI directory is absent (a
/// stripped update on a game disc, for example) is skipped (`Ok(None)`).
pub(crate) fn build_partition_plan(
    disc: &mut dyn DiscSectorSource,
    partition: &PartitionEntry,
    toc_index: usize,
    titles: &[SiTitle],
) -> WupResult<Option<PartitionPlan>> {
    match find_matching_title(titles, toc_index) {
        Some(si_title) => plan_partition(disc, partition, si_title).map(Some),
        None => Ok(None),
    }
}

/// Iterate the disc's content partitions (GM/UP/UC) paired with the
/// TOC index that [`find_matching_title`] keys on.
pub(crate) fn content_partitions_with_index(
    table: &PartitionTable,
) -> impl Iterator<Item = (usize, &PartitionEntry)> {
    table.entries.iter().enumerate().filter(|(_, e)| {
        matches!(
            e.kind,
            PartitionKind::Game | PartitionKind::Update | PartitionKind::Dlc
        )
    })
}

/// Compresses one Wii U disc using its previously prepared partition/FST plan.
pub(crate) fn compress_prepared_disc_title<W: Write>(
    disc_path: &Path,
    plan: DiscTitlePlan,
    sink: &mut ZarWriter<'_, W>,
    progress: &dyn ProgressReporter,
    cancelled: Option<&AtomicBool>,
) -> WupResult<Vec<(u64, u16)>> {
    let mut disc = crate::nintendo::wup::disc::sector_stream::open_disc(disc_path)?;
    let mut results = Vec::with_capacity(plan.partitions.len());
    for partition in plan.partitions {
        if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Cancelled.into());
        }
        results.push(compress_one_partition_with_cancel(
            &mut *disc, partition, sink, progress, cancelled,
        )?);
    }
    Ok(results)
}

/// Decrypted ticket + TMD bytes for one title pulled from the SI
/// FST. Parsing happens on demand so the SI walk stays cheap.
pub(crate) struct SiTitle {
    /// SI FST directory this title was read from, named
    /// `{partition_index:02x}` after the partition's TOC position.
    pub(crate) dir: String,
    pub(crate) title_id: u64,
    pub(crate) ticket_bytes: Vec<u8>,
    pub(crate) tmd_bytes: Vec<u8>,
}

/// Walk the SI partition's FST and pull out every title's ticket and
/// TMD pair. A directory backing a real title (both files present)
/// whose TMD fails its preflight check is kept with empty
/// `tmd_bytes`, so [`plan_partition`]'s `WupTmd::parse` reports
/// `InvalidTmd` just as it would from a fully-read malformed TMD; a
/// short ticket on a real title aborts the whole scan, matching a
/// `parse_ticket_bytes` failure on a fully-read ticket.
pub(crate) fn parse_si_titles(
    disc: &mut dyn DiscSectorSource,
    si: &PartitionEntry,
    key: &DiscKey,
) -> WupResult<Vec<SiTitle>> {
    // Partition header (plaintext) gives the FST offset + size.
    let header = read_partition_header(disc, si.byte_offset())?;
    let header_size = header.header_size as u64;
    let fst_abs_offset = si.byte_offset() + header_size;
    // The header only declares fst_size; bound it against the bytes
    // actually remaining in the disc image for the same reason as
    // parse_disc_content0_fst above.
    let disc_len = disc.total_sectors() * SECTOR_SIZE as u64;
    let fst_size = (header.fst_size as u64).min(disc_len.saturating_sub(fst_abs_offset));

    // Parse the SI FST by decrypting only its header, entry tables, and
    // required strings instead of allocating its full declared extent.
    let si_fs = parse_fst_ranges(fst_size, |range_offset, range_len| {
        read_disc_decrypted_zero_iv_range(
            disc,
            key,
            fst_abs_offset,
            fst_size,
            range_offset,
            range_len,
        )
    })?;

    // Collect title.tik / title.tmd read results by their parent
    // directory. A failed read is kept alongside a successful one
    // (rather than recorded eagerly) so a directory that never gets
    // both files -- an unrelated SI entry, not a real title -- is
    // dropped below instead of surfacing a spurious failure.
    let mut by_dir: std::collections::HashMap<String, TicketTmdPair> =
        std::collections::HashMap::new();

    for vfile in &si_fs.files {
        let (dir, fname) = split_parent(&vfile.path);
        if fname != "title.tik" && fname != "title.tmd" {
            continue;
        }
        let cluster = si_fs
            .clusters
            .get(vfile.cluster_index as usize)
            .ok_or(WupError::InvalidFst)?;

        // Absolute disc byte offset for an SI cluster file:
        //   partition_offset + header_size
        //   + (cluster.offset - 1) * SECTOR_SIZE  (0 if cluster.offset == 0)
        //   + file_offset * offset_factor.
        let cluster_disc_off = if cluster.offset == 0 {
            si.byte_offset() + header_size
        } else {
            si.byte_offset() + header_size + (cluster.offset as u64 - 1) * SECTOR_SIZE as u64
        };
        let file_abs_off =
            cluster_disc_off + (vfile.file_offset as u64) * si_fs.offset_factor as u64;
        // Only a per-title preflight failure (short ticket, or a TMD
        // whose header outruns the file) is deferred; any other error
        // (disc I/O, FST) aborts the whole SI scan.
        let result =
            match read_si_metadata_file(disc, key, file_abs_off, fname, u64::from(vfile.file_size))
            {
                Err(err @ (WupError::InvalidTicket | WupError::InvalidTmd)) => Err(err),
                other => Ok(other?),
            };
        let entry = by_dir.entry(dir.to_string()).or_insert((None, None));
        if fname == "title.tik" {
            entry.0 = Some(result);
        } else {
            entry.1 = Some(result);
        }
    }

    resolve_si_titles(by_dir)
}

type MetaResult = WupResult<Vec<u8>>;
type TicketTmdPair = (Option<MetaResult>, Option<MetaResult>);

/// Turn collected title.tik / title.tmd read results into paired
/// titles, matching develop: a directory with just one of the two
/// files never backed a real title and is dropped entirely. Among
/// paired dirs, a TMD preflight failure is kept as an empty-TMD
/// title (deferred to `plan_partition`'s `WupTmd::parse`, like
/// develop's full-read TMD parse would fail); a ticket preflight
/// failure aborts the whole scan, like develop's `parse_ticket_bytes`
/// call.
fn resolve_si_titles(
    by_dir: std::collections::HashMap<String, TicketTmdPair>,
) -> WupResult<Vec<SiTitle>> {
    let mut titles = Vec::new();
    for (dir, (tik, tmd)) in by_dir {
        let (Some(tik), Some(tmd)) = (tik, tmd) else {
            continue;
        };
        let tik = tik?;
        let (ticket, _) = parse_ticket_bytes(&tik)?;
        titles.push(SiTitle {
            dir,
            title_id: ticket.title_id,
            ticket_bytes: tik,
            tmd_bytes: tmd.unwrap_or_default(),
        });
    }
    Ok(titles)
}

fn read_si_metadata_file(
    disc: &mut dyn DiscSectorSource,
    key: &DiscKey,
    file_offset: u64,
    filename: &str,
    file_size: u64,
) -> WupResult<Vec<u8>> {
    let required_len = match filename {
        "title.tik" => {
            if file_size < WUP_TICKET_BASE_SIZE as u64 {
                return Err(WupError::InvalidTicket);
            }
            WUP_TICKET_BASE_SIZE
        }
        "title.tmd" => {
            if file_size < WUP_TMD_HEADER_SIZE as u64 {
                return Err(WupError::InvalidTmd);
            }
            let header = read_disc_decrypted_file_iv(disc, key, file_offset, WUP_TMD_HEADER_SIZE)?;
            WupTmd::required_len(&header)?
        }
        _ => return Err(WupError::InvalidFst),
    };
    if required_len as u64 > file_size {
        return Err(if filename == "title.tik" {
            WupError::InvalidTicket
        } else {
            WupError::InvalidTmd
        });
    }
    read_disc_decrypted_file_iv(disc, key, file_offset, required_len)
}

/// Match a content partition to its SI ticket/TMD by the partition's
/// index in the disc TOC. The SI FST stores each title under a
/// directory named `{partition_index:02x}` (the partition's position
/// in the TOC), so the lookup is positional rather than name-based.
/// Partitions whose SI directory is absent (a stripped update on a
/// game disc, for example) return `None` and are skipped by callers.
pub(crate) fn find_matching_title(titles: &[SiTitle], toc_index: usize) -> Option<&SiTitle> {
    let dir = format!("{toc_index:02x}");
    titles.iter().find(|t| t.dir == dir)
}

/// Everything needed to read the content of one GM/UP/UC partition:
/// the decrypted title key, parsed TMD, parsed FST, and the
/// `content_id -> (disc offset, size)` location map. Shared by the
/// compressor, the disc `info` reader, and the disc `verify` path.
pub(crate) struct PartitionPlan {
    pub(crate) title_id: u64,
    pub(crate) title_version: u16,
    pub(crate) title_key: TitleKey,
    pub(crate) tmd: WupTmd,
    pub(crate) fs: VirtualFs,
    pub(crate) locations: Vec<(u32, PartitionContentLocation)>,
}

/// Decrypt a content partition's FST and build the location map, the
/// shared head of every partition walk.
fn plan_partition(
    disc: &mut dyn DiscSectorSource,
    partition: &PartitionEntry,
    si_title: &SiTitle,
) -> WupResult<PartitionPlan> {
    let (ticket, title_key) = parse_ticket_bytes(&si_title.ticket_bytes)?;
    let tmd = WupTmd::parse(&si_title.tmd_bytes)?;

    let header = read_partition_header(disc, partition.byte_offset())?;
    let gm_header_size = header.header_size as u64;

    // Content 0 sits at the start of the content area. Size on disc
    // is TMD.contents[0].size (encrypted, padded). It must be
    // decrypted up front so its FST can produce the location map.
    let content0 = tmd.contents.first().ok_or(WupError::InvalidTmd)?;
    let content0_offset = partition.byte_offset() + gm_header_size;
    let fs = parse_disc_content0_fst(disc, content0_offset, content0.size, &title_key)?;

    let mut locations: Vec<(u32, PartitionContentLocation)> = Vec::new();
    for (cluster_idx, cluster) in fs.clusters.iter().enumerate() {
        let tmd_entry = tmd
            .content_by_index(cluster_idx as u16)
            .ok_or(WupError::InvalidTmd)?;
        let loc = compute_content_location(
            partition.byte_offset(),
            gm_header_size,
            cluster.offset as u64,
            tmd_entry.size,
        );
        locations.push((tmd_entry.content_id, loc));
    }

    Ok(PartitionPlan {
        title_id: ticket.title_id,
        title_version: ticket.title_version,
        title_key,
        tmd,
        fs,
        locations,
    })
}

fn compress_one_partition_with_cancel<W: Write>(
    disc: &mut dyn DiscSectorSource,
    plan: PartitionPlan,
    sink: &mut ZarWriter<'_, W>,
    progress: &dyn ProgressReporter,
    cancelled: Option<&AtomicBool>,
) -> WupResult<(u64, u16)> {
    let archive_folder = format!("{:016x}_v{}", plan.title_id, plan.title_version);
    let mut source = PartitionContentSource::new(disc, plan.locations);
    let mut loader = ContentLoader::new(&mut source, plan.title_key, &plan.tmd, &plan.fs);
    for vfile in &plan.fs.files {
        if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Cancelled.into());
        }
        let archive_path = format!("{archive_folder}/{}", vfile.path);
        let mut started = false;
        loader.stream_file(vfile, |bytes| {
            if !started {
                sink.start_file(&archive_path)?;
                started = true;
            }
            sink.append_data(bytes)?;
            progress.inc(bytes.len() as u64);
            Ok(())
        })?;
        if !started {
            sink.start_file(&archive_path)?;
        }
    }
    Ok((plan.title_id, plan.title_version))
}

fn split_parent(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", path),
    }
}

/// Blanket impl so `ContentLoader` can take a
/// `&mut PartitionContentSource` directly.
impl crate::nintendo::wup::nus::content_stream::ContentBytesSource
    for &mut PartitionContentSource<'_>
{
    fn encrypted_content_len(&mut self, content_id: u32) -> WupResult<u64> {
        (**self).encrypted_content_len(content_id)
    }

    fn read_encrypted_range(
        &mut self,
        content_id: u32,
        offset: u64,
        output: &mut [u8],
    ) -> WupResult<()> {
        (**self).read_encrypted_range(content_id, offset, output)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn si_tmd_metadata_round_trips_with_content_entry() {
        use crate::nintendo::wup::disc::sector_stream::InMemoryDisc;
        use crate::nintendo::wup::models::tmd::WUP_TMD_CONTENT_ENTRY_SIZE;
        use aes::Aes128;
        use block_padding::NoPadding;
        use cbc::Encryptor;
        use cbc::cipher::{BlockModeEncrypt, KeyIvInit};

        let key = DiscKey::from_file_contents(&[0; 16]).unwrap();
        let plaintext_len = WUP_TMD_HEADER_SIZE + WUP_TMD_CONTENT_ENTRY_SIZE;
        let mut plaintext = vec![0; plaintext_len.next_multiple_of(16)];
        plaintext[0x1DE..0x1E0].copy_from_slice(&1u16.to_be_bytes());
        type Aes128CbcEnc = Encryptor<Aes128>;
        let plaintext_size = plaintext.len();
        Aes128CbcEnc::new_from_slices(key.as_bytes(), &[0; 16])
            .unwrap()
            .encrypt_padded::<NoPadding>(&mut plaintext, plaintext_size)
            .unwrap();

        let mut disc_data = vec![0; SECTOR_SIZE];
        disc_data[..plaintext.len()].copy_from_slice(&plaintext);
        let mut disc = InMemoryDisc::new(disc_data);
        let tmd =
            read_si_metadata_file(&mut disc, &key, 0, "title.tmd", plaintext_len as u64).unwrap();
        let parsed = WupTmd::parse(&tmd).unwrap();
        assert_eq!(parsed.contents.len(), 1);
    }

    #[test]
    fn resolve_si_titles_drops_unpaired_dir_even_when_its_only_file_failed() {
        // A directory that only ever has one of title.tik/title.tmd never
        // backed a real title (develop drops it silently); a preflight
        // failure on that lone file must not surface as if the directory
        // had been a matched-but-corrupt title.
        let mut by_dir: std::collections::HashMap<String, TicketTmdPair> =
            std::collections::HashMap::new();
        by_dir.insert("00".to_string(), (Some(Err(WupError::InvalidTicket)), None));
        let titles = resolve_si_titles(by_dir).unwrap();
        assert!(titles.is_empty());
    }

    #[test]
    fn resolve_si_titles_keeps_paired_title_with_empty_tmd_on_tmd_preflight_failure() {
        // A directory with both files present, whose TMD failed its
        // preflight check, is a real (malformed) title: it is kept
        // with empty `tmd_bytes` so `plan_partition`'s `WupTmd::parse`
        // reports `InvalidTmd`, the outcome develop gets from parsing
        // a fully-read malformed TMD.
        let mut by_dir: std::collections::HashMap<String, TicketTmdPair> =
            std::collections::HashMap::new();
        by_dir.insert(
            "01".to_string(),
            (
                Some(Ok(vec![0u8; WUP_TICKET_BASE_SIZE])),
                Some(Err(WupError::InvalidTmd)),
            ),
        );
        let titles = resolve_si_titles(by_dir).unwrap();
        assert_eq!(titles.len(), 1);
        assert!(titles[0].tmd_bytes.is_empty());
    }

    #[test]
    fn resolve_si_titles_aborts_on_paired_ticket_preflight_failure() {
        // A short ticket on a directory that does back a real title
        // aborts the whole scan, matching develop's `parse_ticket_bytes`
        // failure on a fully-read ticket.
        let mut by_dir: std::collections::HashMap<String, TicketTmdPair> =
            std::collections::HashMap::new();
        by_dir.insert(
            "01".to_string(),
            (Some(Err(WupError::InvalidTicket)), Some(Ok(vec![0u8; 4]))),
        );
        let result = resolve_si_titles(by_dir);
        assert!(matches!(result, Err(WupError::InvalidTicket)));
    }

    #[test]
    fn split_parent_extracts_basename() {
        assert_eq!(split_parent("a/b/c"), ("a/b", "c"));
        assert_eq!(split_parent("bare"), ("", "bare"));
        assert_eq!(split_parent(""), ("", ""));
    }

    #[test]
    fn find_matching_title_by_toc_index() {
        let titles = vec![
            SiTitle {
                dir: "02".to_string(),
                title_id: 0x0005_0000_1019_E600,
                ticket_bytes: vec![],
                tmd_bytes: vec![],
            },
            SiTitle {
                dir: "03".to_string(),
                title_id: 0x0005_0010_1006_0000,
                ticket_bytes: vec![],
                tmd_bytes: vec![],
            },
        ];
        assert_eq!(
            find_matching_title(&titles, 2).unwrap().title_id,
            0x0005_0000_1019_E600
        );
        assert_eq!(
            find_matching_title(&titles, 3).unwrap().title_id,
            0x0005_0010_1006_0000
        );
    }

    #[test]
    fn find_matching_title_returns_none_when_si_dir_absent() {
        // An update partition at TOC index 1 with no `01/` directory
        // in the SI FST has no ticket/TMD and must be skipped rather
        // than mismatched to another title.
        let titles = vec![SiTitle {
            dir: "02".to_string(),
            title_id: 0x0005_0000_1019_E600,
            ticket_bytes: vec![],
            tmd_bytes: vec![],
        }];
        assert!(find_matching_title(&titles, 1).is_none());
    }

    #[test]
    fn find_matching_title_returns_none_for_empty() {
        let titles: Vec<SiTitle> = Vec::new();
        assert!(find_matching_title(&titles, 2).is_none());
    }

    #[test]
    fn parse_disc_content0_fst_clamps_declared_size_to_disc_image_length() {
        use crate::nintendo::wup::disc::sector_stream::InMemoryDisc;
        use crate::nintendo::wup::nus::fst_parser::{
            FST_FILE_ENTRY_SIZE, FST_HEADER_SIZE, FST_MAGIC,
        };
        use aes::Aes128;
        use block_padding::NoPadding;
        use cbc::Encryptor;
        use cbc::cipher::{BlockModeEncrypt, KeyIvInit};

        let key = TitleKey([0u8; 16]);

        // FST header (0x20 bytes) declaring zero clusters, followed by a
        // root directory entry (0x10 bytes) whose entry count is huge
        // enough that the resulting entry table would be tens of
        // gigabytes -- exactly the shape a crafted TMD content0.size
        // would need to smuggle past an unclamped bound check.
        let mut plaintext = vec![0u8; FST_HEADER_SIZE + FST_FILE_ENTRY_SIZE];
        plaintext[0..4].copy_from_slice(&FST_MAGIC.to_be_bytes());
        plaintext[4..8].copy_from_slice(&1u32.to_be_bytes()); // offset_factor
        let root = FST_HEADER_SIZE;
        plaintext[root..root + 4].copy_from_slice(&0x0100_0000u32.to_be_bytes()); // directory
        plaintext[root + 8..root + 12].copy_from_slice(&u32::MAX.to_be_bytes()); // entry count

        type Aes128CbcEnc = Encryptor<Aes128>;
        let plaintext_len = plaintext.len();
        Aes128CbcEnc::new_from_slices(&key.0, &[0; 16])
            .unwrap()
            .encrypt_padded::<NoPadding>(&mut plaintext, plaintext_len)
            .unwrap();

        // A tiny one-sector disc image: nowhere near large enough to back
        // the entry table a declared multi-gigabyte content0 size would
        // otherwise permit an allocation attempt for.
        let mut disc_data = vec![0u8; SECTOR_SIZE];
        disc_data[..plaintext.len()].copy_from_slice(&plaintext);
        let mut disc = InMemoryDisc::new(disc_data);

        // TMD-declared content0 size far exceeds both the disc image and
        // the (huge) entry table the crafted root entry claims.
        let result = parse_disc_content0_fst(&mut disc, 0, 200_000_000_000, &key);
        assert!(matches!(result, Err(WupError::InvalidFst)));
    }
}
