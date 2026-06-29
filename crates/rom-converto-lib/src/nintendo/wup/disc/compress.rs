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

use std::path::Path;

use crate::nintendo::wup::disc::disc_key::{DiscKey, load_disc_key};
use crate::nintendo::wup::disc::partition::{
    PartitionContentLocation, PartitionContentSource, compute_content_location,
    read_disc_decrypted_file_iv, read_disc_decrypted_zero_iv, read_partition_header,
};
use crate::nintendo::wup::disc::partition_table::{
    PartitionEntry, PartitionKind, PartitionTable, parse_partition_table,
};
use crate::nintendo::wup::disc::sector_stream::{DiscSectorSource, SECTOR_SIZE};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::WupTmd;
use crate::nintendo::wup::nus::content_stream::{ContentLoader, decrypt_content_0};
use crate::nintendo::wup::nus::fst_parser::{VirtualFs, parse_fst};
use crate::nintendo::wup::nus::ticket_parser::{TitleKey, parse_ticket_bytes};
use crate::nintendo::wup::zarchive_writer::ArchiveSink;
use crate::util::ProgressReporter;

/// Sum the decrypted byte size of every FST file this disc would
/// actually emit across all content partitions, skipping shared
/// entries (FST type bit 7). Does the same partition walk and
/// content-0 decrypt as [`compress_disc_title`] but stops before
/// per-file streaming so the caller can seed the progress bar with
/// a real byte total.
pub fn estimate_disc_uncompressed_bytes(
    disc_path: &Path,
    key_override: Option<&Path>,
) -> WupResult<u64> {
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

    let mut total: u64 = 0;
    for (toc_index, partition) in content_partitions_with_index(&table) {
        let si_title = match find_matching_title(&si_titles, toc_index) {
            Some(t) => t,
            None => continue,
        };
        total = total.saturating_add(estimate_one_partition(&mut *disc, partition, si_title)?);
    }
    Ok(total)
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

fn estimate_one_partition(
    disc: &mut dyn DiscSectorSource,
    partition: &PartitionEntry,
    si_title: &SiTitle,
) -> WupResult<u64> {
    let (_, title_key) = parse_ticket_bytes(&si_title.ticket_bytes)?;
    let tmd = WupTmd::parse(&si_title.tmd_bytes)?;
    let header = read_partition_header(disc, partition.byte_offset())?;
    let gm_header_size = header.header_size as u64;
    let content0 = tmd.contents.first().ok_or(WupError::InvalidTmd)?;
    let content0_offset = partition.byte_offset() + gm_header_size;
    let mut encrypted_content0 = vec![0u8; content0.size as usize];
    disc.read_bytes(content0_offset, &mut encrypted_content0)?;
    let fst_bytes = decrypt_content_0(encrypted_content0, &title_key)?;
    let fs = parse_fst(&fst_bytes)?;
    let mut total: u64 = 0;
    for vfile in &fs.files {
        if !vfile.is_shared {
            total = total.saturating_add(u64::from(vfile.file_size));
        }
    }
    Ok(total)
}

/// Compress one Wii U disc (WUD or WUX) into `sink`. Bundles the
/// game plus any UP/UC partitions that share the disc. Returns a
/// `(title_id, version)` pair for every title written so the caller
/// can log what landed in the archive.
pub fn compress_disc_title(
    disc_path: &Path,
    key_override: Option<&Path>,
    sink: &mut dyn ArchiveSink,
    progress: &dyn ProgressReporter,
) -> WupResult<Vec<(u64, u16)>> {
    compress_disc_title_with_cancel(disc_path, key_override, sink, progress, None)
}

pub(crate) fn compress_disc_title_with_cancel(
    disc_path: &Path,
    key_override: Option<&Path>,
    sink: &mut dyn ArchiveSink,
    progress: &dyn ProgressReporter,
    cancelled: Option<&AtomicBool>,
) -> WupResult<Vec<(u64, u16)>> {
    let mut disc = crate::nintendo::wup::disc::sector_stream::open_disc(disc_path)?;
    let key = load_disc_key(disc_path, key_override)?;

    let table = parse_partition_table(&mut *disc, &key)?;
    let si = table
        .find_si()
        .cloned()
        .ok_or(WupError::InvalidPartitionHeader)?;

    // Pull ticket + TMD for every title advertised in the SI FST.
    let si_titles = parse_si_titles(&mut *disc, &si, &key)?;

    let mut results = Vec::new();
    if !table
        .content_partitions()
        .any(|p| matches!(p.kind, PartitionKind::Game))
    {
        return Err(WupError::NoGamePartitionFound);
    }

    let indexed: Vec<(usize, PartitionEntry)> = content_partitions_with_index(&table)
        .map(|(i, p)| (i, p.clone()))
        .collect();
    for (toc_index, partition) in &indexed {
        if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(WupError::Cancelled);
        }
        let si_title = match find_matching_title(&si_titles, *toc_index) {
            Some(t) => t,
            None => continue,
        };
        let (title_id, version) =
            compress_one_partition_with_cancel(&mut *disc, partition, si_title, sink, progress, cancelled)?;
        results.push((title_id, version));
    }

    if results.is_empty() {
        return Err(WupError::NoGamePartitionFound);
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
/// TMD pair.
pub(crate) fn parse_si_titles(
    disc: &mut dyn DiscSectorSource,
    si: &PartitionEntry,
    key: &DiscKey,
) -> WupResult<Vec<SiTitle>> {
    // Partition header (plaintext) gives the FST offset + size.
    let header = read_partition_header(disc, si.byte_offset())?;
    let header_size = header.header_size as u64;
    let fst_size = header.fst_size as usize;

    // Read and decrypt the SI FST.
    let fst_abs_offset = si.byte_offset() + header_size;
    let fst_bytes = read_disc_decrypted_zero_iv(disc, key, fst_abs_offset, fst_size)?;
    let si_fs = parse_fst(&fst_bytes)?;

    // Collect title.tik / title.tmd pairs by their parent directory.
    type TicketTmdPair = (Option<Vec<u8>>, Option<Vec<u8>>);
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
        let bytes = read_disc_decrypted_file_iv(disc, key, file_abs_off, vfile.file_size as usize)?;

        let entry = by_dir.entry(dir.to_string()).or_insert((None, None));
        if fname == "title.tik" {
            entry.0 = Some(bytes);
        } else {
            entry.1 = Some(bytes);
        }
    }

    // Keep only dirs that have both ticket and TMD. Read the ticket
    // once to pull title_id out for the matching step below.
    let mut titles = Vec::new();
    for (dir, (tik, tmd)) in by_dir {
        if let (Some(tik), Some(tmd)) = (tik, tmd) {
            let (ticket, _) = parse_ticket_bytes(&tik)?;
            titles.push(SiTitle {
                dir,
                title_id: ticket.title_id,
                ticket_bytes: tik,
                tmd_bytes: tmd,
            });
        }
    }
    Ok(titles)
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
pub(crate) fn plan_partition(
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
    let mut encrypted_content0 = vec![0u8; content0.size as usize];
    disc.read_bytes(content0_offset, &mut encrypted_content0)?;
    let fst_bytes = decrypt_content_0(encrypted_content0, &title_key)?;
    let fs = parse_fst(&fst_bytes)?;

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

fn compress_one_partition_with_cancel(
    disc: &mut dyn DiscSectorSource,
    partition: &PartitionEntry,
    si_title: &SiTitle,
    sink: &mut dyn ArchiveSink,
    progress: &dyn ProgressReporter,
    cancelled: Option<&AtomicBool>,
) -> WupResult<(u64, u16)> {
    let plan = plan_partition(disc, partition, si_title)?;
    let archive_folder = format!("{:016x}_v{}", plan.title_id, plan.title_version);
    let mut source = PartitionContentSource::new(disc, plan.locations);
    let mut loader = ContentLoader::new(&mut source, plan.title_key, &plan.tmd, &plan.fs);
    for vfile in &plan.fs.files {
        if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(WupError::Cancelled);
        }
        let bytes = loader.extract_file(vfile)?;
        let archive_path = format!("{archive_folder}/{}", vfile.path);
        sink.start_new_file(&archive_path)?;
        sink.append_data(&bytes)?;
        progress.inc(bytes.len() as u64);
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
    fn read_encrypted_content(&mut self, content_id: u32) -> WupResult<Vec<u8>> {
        (**self).read_encrypted_content(content_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
