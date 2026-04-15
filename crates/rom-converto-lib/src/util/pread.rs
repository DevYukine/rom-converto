//! Positional `read_exact` across a shared `File`.
//!
//! On Windows uses [`std::os::windows::fs::FileExt::seek_read`] which
//! issues `ReadFile` with an `OVERLAPPED` offset and does NOT move
//! the file-handle cursor, so multiple threads can safely read
//! disjoint regions of the same file concurrently. On Unix uses
//! [`std::os::unix::fs::FileExt::read_exact_at`].
//!
//! Both backends loop until the full buffer is satisfied, turning
//! short reads at end-of-file into [`std::io::ErrorKind::UnexpectedEof`].
//! Shared by every decompress worker pool that wants a single
//! `Arc<File>` backing many concurrent readers.

#[cfg(windows)]
pub fn file_read_exact_at(
    file: &std::fs::File,
    buf: &mut [u8],
    mut offset: u64,
) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut pos = 0usize;
    while pos < buf.len() {
        let n = file.seek_read(&mut buf[pos..], offset)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "short positional read",
            ));
        }
        pos += n;
        offset += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
pub fn file_read_exact_at(
    file: &std::fs::File,
    buf: &mut [u8],
    offset: u64,
) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}
