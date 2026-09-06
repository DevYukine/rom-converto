//! Raw-deflate round trips over reusable `flate2` state.
//!
//! Both CHD's `zlib` hunk codec and CSO's per-block codec compress
//! independent chunks with one long-lived compressor per worker, so
//! the state is reset before each call and the whole chunk has to
//! finish in a single `Finish` pass.

use std::io;

/// Compress `data` as one raw-deflate stream, reusing `compressor`.
pub(crate) fn deflate_with_reset(
    compressor: &mut flate2::Compress,
    data: &[u8],
) -> io::Result<Vec<u8>> {
    compressor.reset();
    // Deflate worst case is slightly larger than input
    let max_out = data.len() + data.len() / 100 + 600;
    let mut output = vec![0u8; max_out];
    let before_out = compressor.total_out();
    let status = compressor
        .compress(data, &mut output, flate2::FlushCompress::Finish)
        .map_err(|e| io::Error::other(format!("deflate compress error: {e}")))?;
    match status {
        flate2::Status::StreamEnd => {}
        _ => {
            return Err(io::Error::other(
                "deflate compression did not finish in one call",
            ));
        }
    }
    let written = (compressor.total_out() - before_out) as usize;
    output.truncate(written);
    Ok(output)
}

/// Inflate one raw-deflate stream of at most `expected_len` bytes,
/// reusing `decompress`.
pub(crate) fn deflate_decompress_with(
    decompress: &mut flate2::Decompress,
    src: &[u8],
    expected_len: usize,
) -> io::Result<Vec<u8>> {
    decompress.reset(false);
    let mut output = vec![0u8; expected_len];
    let before_out = decompress.total_out();
    let status = decompress
        .decompress(src, &mut output, flate2::FlushDecompress::Finish)
        .map_err(|e| io::Error::other(format!("deflate decompress error: {e}")))?;
    match status {
        flate2::Status::StreamEnd | flate2::Status::Ok => {}
        flate2::Status::BufError => {
            return Err(io::Error::other("deflate decompress buffer error"));
        }
    }
    let written = (decompress.total_out() - before_out) as usize;
    output.truncate(written);
    Ok(output)
}
