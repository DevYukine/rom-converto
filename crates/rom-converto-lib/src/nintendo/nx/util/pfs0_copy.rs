//! Verbatim byte-range copying into a freshly built PFS0. Shared by the
//! super-NSP merge and the container split.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::models::pfs0::{Pfs0LayoutHints, build_header};
use crate::util::pread::file_read_exact_at;
use crate::util::{CancelToken, ProgressReporter};

/// A byte range in one opened input, copied verbatim into an output PFS0.
pub(crate) struct Pfs0Source {
    pub file: Arc<File>,
    pub abs_offset: u64,
    pub size: u64,
    pub name: String,
}

/// Builds a PFS0 over `sources` in order at `output` and copies each
/// source's byte range verbatim.
///
/// # Errors
/// Returns an error if `output` cannot be written or a source range
/// cannot be read, and [`NxError::Cancelled`] if `cancel` fires mid-copy.
pub(crate) fn write_pfs0_from_sources(
    output: &Path,
    sources: &[Pfs0Source],
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    let specs: Vec<(String, u64)> = sources.iter().map(|s| (s.name.clone(), s.size)).collect();
    let header = build_header(&specs, &Pfs0LayoutHints::default())?;

    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)?;
    out.write_all(&header.bytes)?;
    for s in sources {
        copy_range(&s.file, s.abs_offset, s.size, &mut out, progress, cancel)?;
    }
    out.flush()?;
    Ok(())
}

/// Copies `size` bytes from `abs_offset` in `file` to `out`, reporting
/// progress per chunk and aborting when `cancel` fires.
///
/// # Errors
/// Propagates I/O errors and returns [`NxError::Cancelled`] on cancel.
pub(crate) fn copy_range<W: Write>(
    file: &File,
    abs_offset: u64,
    size: u64,
    out: &mut W,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    const CHUNK: usize = 4 * 1024 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut remaining = size;
    let mut at = abs_offset;
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(NxError::Cancelled);
        }
        let take = (CHUNK as u64).min(remaining) as usize;
        file_read_exact_at(file, &mut buf[..take], at)?;
        out.write_all(&buf[..take])?;
        at += take as u64;
        remaining -= take as u64;
        progress.inc(take as u64);
    }
    Ok(())
}
