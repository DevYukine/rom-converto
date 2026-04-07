use crate::nintendo::ctr::z3ds::error::{Z3dsError, Z3dsResult};
use crate::nintendo::ctr::z3ds::models::Z3dsHeader;
use crate::nintendo::ctr::z3ds::seekable::decode_seekable;
use crate::util::{BYTES_PER_MB, ProgressReporter};
use binrw::BinRead;
use log::info;
use std::io::Cursor;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter, SeekFrom};
use tokio::task;

pub async fn decompress_rom(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Z3dsResult<()> {
    let mut file = File::open(input).await?;

    // Read and parse the header
    let mut header_buf = vec![0u8; 0x20];
    file.read_exact(&mut header_buf).await?;
    let mut cursor = Cursor::new(&header_buf);
    let header = Z3dsHeader::read(&mut cursor)?;

    if header.version != 1 {
        return Err(Z3dsError::UnsupportedVersion(header.version));
    }

    // Seek past the metadata block to the compressed payload
    let payload_offset = header.header_size as u64 + header.metadata_size as u64;
    file.seek(SeekFrom::Start(payload_offset)).await?;

    // Read compressed payload
    let mut compressed = vec![0u8; header.compressed_size as usize];
    file.read_exact(&mut compressed).await?;

    // Phase 1: Decompress
    // We estimate total as compressed + uncompressed (decompress + write phases)
    let total_work = header.compressed_size + header.uncompressed_size;
    progress.start(
        total_work,
        &format!(
            "Decompressing {} ({:.2} MB compressed)",
            input.file_name().unwrap_or_default().to_string_lossy(),
            header.compressed_size as f64 / BYTES_PER_MB,
        ),
    );

    let decompressed = task::spawn_blocking(move || decode_seekable(&compressed)).await??;
    progress.inc(header.compressed_size);

    let actual_size = decompressed.len() as u64;
    if actual_size != header.uncompressed_size {
        return Err(Z3dsError::DecompressedSizeMismatch {
            expected: header.uncompressed_size,
            actual: actual_size,
        });
    }

    // Phase 2: Write to disk in chunks for progress
    let out_file = File::create(output).await?;
    let mut out = BufWriter::new(out_file);

    const WRITE_CHUNK: usize = 4 * 1024 * 1024; // 4 MB
    for chunk in decompressed.chunks(WRITE_CHUNK) {
        out.write_all(chunk).await?;
        progress.inc(chunk.len() as u64);
    }
    out.flush().await?;
    progress.finish();

    info!(
        "Decompressed {} -> {} ({:.2} MB)",
        input.display(),
        output.display(),
        actual_size as f64 / BYTES_PER_MB,
    );

    Ok(())
}
