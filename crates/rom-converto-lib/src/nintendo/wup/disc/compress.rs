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
    PartitionEntry, PartitionKind, parse_partition_table,
};
use crate::nintendo::wup::disc::sector_stream::{DiscSectorSource, SECTOR_SIZE};
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::WupTmd;
use crate::nintendo::wup::nus::content_stream::{ContentLoader, decrypt_content_0};
use crate::nintendo::wup::nus::fst_parser::parse_fst;
use crate::nintendo::wup::nus::ticket_parser::parse_ticket_bytes;
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

    let content_partitions: Vec<PartitionEntry> = table.content_partitions().cloned().collect();
    if !content_partitions
        .iter()
        .any(|p| matches!(p.kind, PartitionKind::Game))
    {
        return Err(WupError::NoGamePartitionFound);
    }

    let mut total: u64 = 0;
    for partition in &content_partitions {
        let si_title = match find_matching_title(&si_titles, &partition.name) {
            Some(t) => t,
            None => continue,
        };
        total = total.saturating_add(estimate_one_partition(&mut *disc, partition, si_title)?);
    }
    Ok(total)
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
    let mut disc = crate::nintendo::wup::disc::sector_stream::open_disc(disc_path)?;
    let key = load_disc_key(disc_path, key_override)?;

    let table = parse_partition_table(&mut *disc, &key)?;
    let si = table
        .find_si()
        .cloned()
        .ok_or(WupError::InvalidPartitionHeader)?;

    // Pull ticket + TMD for every title advertised in the SI FST.
    let si_titles = parse_si_titles(&mut *disc, &si, &key)?;

    // Match each GM/UP/UC partition to its ticket + TMD by title id.
    let mut results = Vec::new();
    let content_partitions: Vec<PartitionEntry> = table.content_partitions().cloned().collect();
    if !content_partitions
        .iter()
        .any(|p| matches!(p.kind, PartitionKind::Game))
    {
        return Err(WupError::NoGamePartitionFound);
    }

    for partition in &content_partitions {
        let si_title = match find_matching_title(&si_titles, &partition.name) {
            Some(t) => t,
            None => continue,
        };
        let (title_id, version) =
            compress_one_partition(&mut *disc, partition, si_title, sink, progress)?;
        results.push((title_id, version));
    }

    if results.is_empty() {
        return Err(WupError::NoGamePartitionFound);
    }
    Ok(results)
}

/// Decrypted ticket + TMD bytes for one title pulled from the SI
/// FST. Parsing happens on demand so the SI walk stays cheap.
struct SiTitle {
    title_id: u64,
    ticket_bytes: Vec<u8>,
    tmd_bytes: Vec<u8>,
}

/// Walk the SI partition's FST and pull out every title's ticket and
/// TMD pair.
fn parse_si_titles(
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
    for (_, (tik, tmd)) in by_dir {
        if let (Some(tik), Some(tmd)) = (tik, tmd) {
            let (ticket, _) = parse_ticket_bytes(&tik)?;
            titles.push(SiTitle {
                title_id: ticket.title_id,
                ticket_bytes: tik,
                tmd_bytes: tmd,
            });
        }
    }
    Ok(titles)
}

/// Match a partition name like `GM12345678` or `UP` to one SI title.
/// GM names carry the low 8 hex digits of the title id as suffix. UP
/// names have no suffix, so fall back to the first non-game title
/// (title-type high half != 0x0005_000E).
fn find_matching_title<'a>(titles: &'a [SiTitle], partition_name: &str) -> Option<&'a SiTitle> {
    if partition_name.len() >= 10 {
        let suffix = &partition_name[2..10];
        if let Ok(low) = u32::from_str_radix(suffix, 16)
            && let Some(t) = titles.iter().find(|t| (t.title_id as u32) == low)
        {
            return Some(t);
        }
    }
    if partition_name.starts_with("UP") {
        return titles.iter().find(|t| {
            let mid = (t.title_id >> 32) as u32;
            mid != 0x0005_000E
        });
    }
    None
}

/// Compress one GM/UP/UC partition: decrypt its FST, build a content
/// location map, stream every virtual file through the shared
/// `ContentLoader`.
fn compress_one_partition(
    disc: &mut dyn DiscSectorSource,
    partition: &PartitionEntry,
    si_title: &SiTitle,
    sink: &mut dyn ArchiveSink,
    progress: &dyn ProgressReporter,
) -> WupResult<(u64, u16)> {
    let (ticket, title_key) = parse_ticket_bytes(&si_title.ticket_bytes)?;
    let tmd = WupTmd::parse(&si_title.tmd_bytes)?;

    let header = read_partition_header(disc, partition.byte_offset())?;
    let gm_header_size = header.header_size as u64;

    // Content 0 sits at the start of the content area. Size on disc
    // is TMD.contents[0].size (encrypted, padded).
    let content0 = tmd.contents.first().ok_or(WupError::InvalidTmd)?;
    let content0_offset = partition.byte_offset() + gm_header_size;
    let content0_size = content0.size;

    // Content 0 must be decrypted up front (outside ContentLoader) so
    // its FST can produce the location map ContentLoader needs.
    let mut encrypted_content0 = vec![0u8; content0_size as usize];
    disc.read_bytes(content0_offset, &mut encrypted_content0)?;
    let fst_bytes = decrypt_content_0(encrypted_content0, &title_key)?;
    let fs = parse_fst(&fst_bytes)?;

    // Build the content_id -> (disc offset, size) map used by
    // PartitionContentSource.
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

    let archive_folder = format!("{:016x}_v{}", ticket.title_id, ticket.title_version);
    let mut source = PartitionContentSource::new(disc, locations);
    let mut loader = ContentLoader::new(&mut source, title_key, &tmd, &fs);
    for vfile in &fs.files {
        let bytes = loader.extract_file(vfile)?;
        let archive_path = format!("{archive_folder}/{}", vfile.path);
        sink.start_new_file(&archive_path)?;
        sink.append_data(&bytes)?;
        progress.inc(bytes.len() as u64);
    }
    Ok((ticket.title_id, ticket.title_version))
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
    fn find_matching_title_by_gm_suffix() {
        let titles = vec![
            SiTitle {
                title_id: 0x0005_000E_0000_BEEF,
                ticket_bytes: vec![],
                tmd_bytes: vec![],
            },
            SiTitle {
                title_id: 0x0005_000E_0000_CAFE,
                ticket_bytes: vec![],
                tmd_bytes: vec![],
            },
        ];
        let t = find_matching_title(&titles, "GM0000CAFE").unwrap();
        assert_eq!(t.title_id, 0x0005_000E_0000_CAFE);
    }

    #[test]
    fn find_matching_title_up_fallback() {
        let titles = vec![
            SiTitle {
                title_id: 0x0005_000E_0000_BEEF,
                ticket_bytes: vec![],
                tmd_bytes: vec![],
            },
            SiTitle {
                title_id: 0x0005_001B_0000_0001,
                ticket_bytes: vec![],
                tmd_bytes: vec![],
            },
        ];
        let t = find_matching_title(&titles, "UP").unwrap();
        assert_eq!(t.title_id, 0x0005_001B_0000_0001);
    }

    #[test]
    fn find_matching_title_returns_none_for_unknown() {
        let titles: Vec<SiTitle> = Vec::new();
        assert!(find_matching_title(&titles, "GM12345678").is_none());
    }
}
