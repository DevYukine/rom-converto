pub(crate) mod pfs0_copy;

use std::io::Write;

use crate::nintendo::nx::error::NxResult;

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
