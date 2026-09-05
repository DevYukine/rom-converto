//! Meta-NCA helpers shared by `info`, `merge`, and `split`: inline ticket
//! harvesting and CNMT extraction.

use crate::nintendo::nx::container::ContainerListing;
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::keys::KeySet;
use crate::nintendo::nx::models::cnmt::Cnmt;
use crate::nintendo::nx::models::nca::CONTENT_TYPE_META;
use crate::nintendo::nx::models::pfs0::Pfs0;
use crate::nintendo::nx::models::ticket::Ticket;
use crate::nintendo::nx::walker::NcaWalker;
use crate::util::pread::file_read_exact_at;
use std::fs::File;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

/// Folds every `.tik` inside `listing` into `keys`, ignoring tickets that
/// cannot be read or parsed.
pub(crate) fn merge_inline_tickets(path: &Path, listing: &ContainerListing, keys: &mut KeySet) {
    let Ok(file) = File::open(path) else {
        return;
    };
    let file = Arc::new(file);
    for entry in &listing.entries {
        if !entry.name.to_ascii_lowercase().ends_with(".tik") {
            continue;
        }
        let mut buf = vec![0u8; entry.size as usize];
        if file_read_exact_at(&file, &mut buf, entry.abs_offset).is_err() {
            continue;
        }
        if let Ok(ticket) = Ticket::parse(&buf) {
            keys.title_keys
                .insert(ticket.rights_id, ticket.encrypted_title_key);
        }
    }
}

/// Decrypts the meta NCA at `nca_offset` and parses the CNMT inside its
/// first section.
pub(crate) fn read_meta_cnmt(
    file: Arc<File>,
    nca_offset: u64,
    nca_size: u64,
    keys: &KeySet,
) -> NxResult<Cnmt> {
    let walker = NcaWalker::open(file, nca_offset, nca_size, keys)?;
    if walker.header.content_type != CONTENT_TYPE_META {
        return Err(NxError::NotMetaNca {
            content_type: walker.header.content_type,
        });
    }
    let section = walker.sections.first().ok_or(NxError::MetaNcaNoSections)?;

    // Hash region precedes the PFS0; scan the first chunk for the PFS0
    // magic instead of plumbing through HashedHierarchicalSha256 offsets.
    let scan_len = section.raw_size.min(0x4000);
    let scan_len_aligned = (scan_len + 15) & !15;
    let mut scan = vec![0u8; scan_len_aligned as usize];
    walker.read_section_plain(section, 0, &mut scan)?;
    let pfs0_off = scan
        .windows(4)
        .position(|w| w == b"PFS0")
        .ok_or(NxError::Pfs0BadMagic)? as u64;

    let pfs0_section_offset = pfs0_off;
    // Read enough bytes to cover the PFS0 header + string table + at
    // least one .cnmt file. 64 KB is conservative; meta NCAs are tiny.
    let read_len = section
        .raw_size
        .saturating_sub(pfs0_section_offset)
        .min(0x10000);
    let read_len_aligned = (read_len + 15) & !15;
    let read_start = pfs0_section_offset & !15;
    let read_offset_in_data = (pfs0_section_offset - read_start) as usize;
    let mut buf = vec![0u8; read_len_aligned as usize];
    walker.read_section_plain(section, read_start, &mut buf)?;
    let pfs0_bytes = &buf[read_offset_in_data..];

    let mut cur = Cursor::new(pfs0_bytes);
    let pfs0 = Pfs0::read(&mut cur)?;
    let cnmt_entry = pfs0
        .files
        .iter()
        .find(|f| f.name.to_ascii_lowercase().ends_with(".cnmt"))
        .ok_or(NxError::MetaMissingCnmt)?;

    // The PFS0 was read at `read_start`; file payload starts at the
    // PFS0's reported data_section_offset (relative to its own start).
    let data_start_in_buf = (read_offset_in_data as u64)
        .checked_add(pfs0.data_section_offset)
        .ok_or(NxError::MetaCnmtTruncated)?;
    let cnmt_start = data_start_in_buf
        .checked_add(cnmt_entry.data_offset)
        .ok_or(NxError::MetaCnmtTruncated)?;
    let cnmt_end = cnmt_start
        .checked_add(cnmt_entry.size)
        .ok_or(NxError::MetaCnmtTruncated)?;
    if cnmt_end > buf.len() as u64 {
        return Err(NxError::MetaCnmtTruncated);
    }
    let cnmt_bytes = &buf[cnmt_start as usize..cnmt_end as usize];
    Cnmt::parse(cnmt_bytes)
}
