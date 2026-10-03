pub(crate) mod pfs0_copy;

use std::io::Write;

use crate::nintendo::nx::error::{NxError, NxResult};

/// Writes zero padding in bounded chunks.
pub(crate) fn write_zeros<W: Write>(out: &mut W, len: u64) -> NxResult<()> {
    const CHUNK: usize = 1024 * 1024;
    if len == 0 {
        return Ok(());
    }
    let zeros = vec![0u8; len.min(CHUNK as u64) as usize];
    let mut remaining = len;
    while remaining > 0 {
        let take = remaining.min(CHUNK as u64) as usize;
        out.write_all(&zeros[..take])?;
        remaining -= take as u64;
    }
    Ok(())
}

pub(crate) use pfs0_copy::{Pfs0Source, copy_range, write_pfs0_from_sources};

/// Longest name a PFS0/HFS0 string table entry may hold, in bytes.
const MAX_NAME_LEN: usize = 1024;

/// Reads one NUL-terminated name from a PFS0/HFS0 string table at
/// `offset`, charging its raw on-disk byte length against `budget`,
/// which must stay within the table's length. Budgeting the raw bytes
/// rather than the decoded length matters because one invalid byte
/// decodes to a three-byte replacement character.
///
/// # Errors
///
/// Returns [`NxError::InvalidStringTable`] when `offset` is past the
/// table, the name exceeds [`MAX_NAME_LEN`], or the accumulated
/// budget exceeds the table length.
pub(crate) fn read_table_name(table: &[u8], offset: usize, budget: &mut usize) -> NxResult<String> {
    let rest = table.get(offset..).ok_or(NxError::InvalidStringTable)?;
    let end = rest
        .iter()
        .take(MAX_NAME_LEN + 1)
        .position(|&b| b == 0)
        .unwrap_or(rest.len());
    if end > MAX_NAME_LEN {
        return Err(NxError::InvalidStringTable);
    }
    *budget = budget
        .checked_add(end)
        .filter(|total| *total <= table.len())
        .ok_or(NxError::InvalidStringTable)?;
    Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_table_name_rejects_name_longer_than_limit() {
        let mut table = vec![b'a'; MAX_NAME_LEN];
        table.push(0);
        let mut budget = 0;
        assert_eq!(
            read_table_name(&table, 0, &mut budget).unwrap().len(),
            MAX_NAME_LEN
        );
        assert_eq!(budget, MAX_NAME_LEN);
        table.insert(MAX_NAME_LEN, b'a');
        let mut budget = 0;
        assert!(matches!(
            read_table_name(&table, 0, &mut budget),
            Err(NxError::InvalidStringTable)
        ));
    }

    #[test]
    fn read_table_name_budgets_raw_bytes_not_decoded_length() {
        // The invalid byte decodes to a three-byte replacement
        // character, so a decoded-length budget would overcount.
        let table = [0xFF, b'a', 0];
        let mut budget = 0;
        let name = read_table_name(&table, 0, &mut budget).unwrap();
        assert_eq!(name, "\u{FFFD}a");
        assert_eq!(budget, 2);
    }
}
