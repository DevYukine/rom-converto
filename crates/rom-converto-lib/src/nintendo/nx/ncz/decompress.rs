//! NCZ -> NCA streaming decompression. Pulls from `input`, applies
//! `ReencryptWriter` over the decompressed payload, and forwards
//! re-encrypted bytes to `out`. Memory stays bounded by buffer sizes
//! regardless of input length.

use std::io::{self, Cursor, Read, Write};

use byteorder::{LE, ReadBytesExt};

use crate::nintendo::nx::constants::{
    MAX_BLOCK_SIZE_EXP, MIN_BLOCK_SIZE_EXP, NCA_PREFIX_SIZE, NCZ_SECTION_ENTRY_SIZE,
    NCZBLOCK_MAGIC, NCZSECTN_MAGIC,
};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::ncz::decompress_worker::{
    NczDecompressWork, default_thread_count, spawn_ncz_decompress_pool,
};
use crate::nintendo::nx::ncz::header::{NczBlockInfo, NczSectionEntry};
use crate::nintendo::nx::ncz::reencrypt::ReencryptWriter;
use crate::util::worker_pool::drive;
use crate::util::{CancelToken, ProgressReporter};

const STREAM_CHUNK: usize = 256 * 1024;
const READ_BUFFER: usize = 4 * 1024 * 1024;

pub fn ncz_to_nca<R: Read + Send, W: Write>(
    input: &mut R,
    out: &mut W,
    progress: &dyn ProgressReporter,
) -> NxResult<()> {
    ncz_to_nca_cancellable(input, out, progress, &CancelToken::new())
}

pub fn ncz_to_nca_cancellable<R: Read + Send, W: Write>(
    input: &mut R,
    out: &mut W,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    check_cancel(cancel)?;
    let mut input = io::BufReader::with_capacity(READ_BUFFER, input);

    let mut prefix = [0u8; NCA_PREFIX_SIZE];
    input.read_exact(&mut prefix)?;
    out.write_all(&prefix)?;
    progress.inc(NCA_PREFIX_SIZE as u64);

    let sections = read_sections(&mut input, cancel)?;
    let (block, payload_prefix) = read_block_or_payload_start(&mut input, cancel)?;

    let mut reenc = ReencryptWriter::new(out, &sections, NCA_PREFIX_SIZE as u64);
    match block {
        Some(info) => decode_blocks_stream(&mut input, &info, &mut reenc, progress, cancel)?,
        None => {
            let stash = payload_prefix.unwrap_or_default();
            let chained = Cursor::new(stash).chain(input);
            decode_solid_stream(chained, &mut reenc, progress, cancel)?;
        }
    }
    Ok(())
}

fn read_sections<R: Read>(input: &mut R, cancel: &CancelToken) -> NxResult<Vec<NczSectionEntry>> {
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic)?;
    if magic != NCZSECTN_MAGIC {
        return Err(NxError::NczBadMagic(magic));
    }
    let count = input.read_i64::<LE>()?;
    if count < 0 {
        return Err(NxError::IncompleteSection);
    }
    let mut sections = Vec::with_capacity(count as usize);
    let mut entry = vec![0u8; NCZ_SECTION_ENTRY_SIZE];
    for _ in 0..count {
        check_cancel(cancel)?;
        input.read_exact(&mut entry)?;
        let mut cur = Cursor::new(&entry);
        let offset = cur.read_i64::<LE>()?;
        let size = cur.read_i64::<LE>()?;
        let crypto_type = cur.read_i64::<LE>()?;
        let _padding = cur.read_i64::<LE>()?;
        let mut crypto_key = [0u8; 16];
        cur.read_exact(&mut crypto_key)?;
        let mut crypto_counter = [0u8; 16];
        cur.read_exact(&mut crypto_counter)?;
        sections.push(NczSectionEntry {
            offset,
            size,
            crypto_type,
            crypto_key,
            crypto_counter,
        });
    }
    Ok(sections)
}

fn read_block_or_payload_start<R: Read>(
    input: &mut R,
    cancel: &CancelToken,
) -> NxResult<(Option<NczBlockInfo>, Option<[u8; 8]>)> {
    check_cancel(cancel)?;
    let mut peek = [0u8; 8];
    if let Err(e) = input.read_exact(&mut peek) {
        return if e.kind() == io::ErrorKind::UnexpectedEof {
            Ok((None, None))
        } else {
            Err(e.into())
        };
    }
    if peek != NCZBLOCK_MAGIC {
        return Ok((None, Some(peek)));
    }
    let version = input.read_u8()?;
    let kind = input.read_u8()?;
    let _u8 = input.read_u8()?;
    let block_size_exp = input.read_u8()?;
    if !(MIN_BLOCK_SIZE_EXP..=MAX_BLOCK_SIZE_EXP).contains(&block_size_exp) {
        return Err(NxError::BlockSizeOutOfRange(block_size_exp));
    }
    let num_blocks = input.read_u32::<LE>()?;
    let decompressed_size = input.read_i64::<LE>()?;
    let mut compressed_block_sizes = Vec::with_capacity(num_blocks as usize);
    for _ in 0..num_blocks {
        check_cancel(cancel)?;
        compressed_block_sizes.push(input.read_u32::<LE>()?);
    }
    Ok((
        Some(NczBlockInfo {
            version,
            kind,
            block_size_exp,
            decompressed_size,
            compressed_block_sizes,
        }),
        None,
    ))
}

fn decode_solid_stream<R: Read + Send, W: Write>(
    input: R,
    reenc: &mut ReencryptWriter<W>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    // Solid-mode bottleneck on a single thread is the chain
    // `zstd_decode -> CTR encrypt -> file write`, all CPU/IO bound
    // and all serial. Splitting decode (Thread A) from encrypt+write
    // (Thread B) lets the OS overlap libzstd's read syscalls and
    // arithmetic with the AES + write pipeline on a second core. On
    // a 14 GB single-NCA NSZ this trims ~30% off serial wall time.
    use std::sync::mpsc::sync_channel;
    use std::thread::scope;

    let (tx, rx) = sync_channel::<Vec<u8>>(8);

    scope(|s| -> NxResult<()> {
        let decode_handle = s.spawn(move || -> NxResult<()> {
            let mut decoder = zstd::stream::read::Decoder::new(input)
                .map_err(|e| NxError::ZstdError(format!("zstd decoder init: {e}")))?;
            loop {
                check_cancel(cancel)?;
                let mut buf = vec![0u8; STREAM_CHUNK];
                let n = decoder
                    .read(&mut buf)
                    .map_err(|e| NxError::ZstdError(format!("zstd read: {e}")))?;
                if n == 0 {
                    break;
                }
                buf.truncate(n);
                if tx.send(buf).is_err() {
                    break;
                }
            }
            Ok(())
        });

        while let Ok(chunk) = rx.recv() {
            check_cancel(cancel)?;
            reenc.write_all(&chunk)?;
            progress.inc(chunk.len() as u64);
        }
        decode_handle
            .join()
            .map_err(|_| NxError::WorkerPoolClosed)??;
        Ok(())
    })
}

fn decode_blocks_stream<R: Read, W: Write>(
    input: &mut R,
    info: &NczBlockInfo,
    reenc: &mut ReencryptWriter<W>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    let block_size = info.block_size_bytes() as usize;
    let num_blocks = info.compressed_block_sizes.len();
    let n_threads = default_thread_count().min(num_blocks.max(1));
    let pool = spawn_ncz_decompress_pool(n_threads);

    let drive_result = drive(
        &pool,
        num_blocks as u64,
        n_threads * 2,
        |seq| -> NxResult<NczDecompressWork> {
            check_cancel(cancel)?;
            let i = seq as usize;
            let csz = info.compressed_block_sizes[i] as usize;
            let is_last = i + 1 == num_blocks;
            let logical_size = if is_last {
                (info.decompressed_size as usize) - i * block_size
            } else {
                block_size
            };
            let mut compressed = vec![0u8; csz];
            input.read_exact(&mut compressed)?;
            Ok(NczDecompressWork {
                compressed,
                logical_size,
                raw: csz == logical_size,
            })
        },
        |_seq, out_block| -> NxResult<()> {
            check_cancel(cancel)?;
            reenc.write_all(&out_block.bytes)?;
            progress.inc(out_block.bytes.len() as u64);
            Ok(())
        },
    );
    pool.shutdown();
    drive_result?;
    Ok(())
}

fn check_cancel(cancel: &CancelToken) -> NxResult<()> {
    if cancel.is_cancelled() {
        return Err(NxError::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::compress::{NxCompressOptions, compress_container};
    use crate::nintendo::nx::constants::{NCA_FS_ENTRY_OFFSET, NCA_FS_HEADER_OFFSET, NCA3_MAGIC};
    use crate::nintendo::nx::crypto::aes_ctr::apply_ctr;
    use crate::nintendo::nx::crypto::aes_xts::encrypt_nca_header;
    use crate::nintendo::nx::models::nca::{FsHeader, initial_ctr_for_offset};
    use crate::nintendo::nx::models::pfs0;
    use crate::nintendo::nx::ncz::compress::{NcaToNczOptions, NczMode, nca_to_ncz};
    use crate::nintendo::nx::test_fixtures::{
        TEST_BODY_KEY, encrypt_key_area_block, synthetic_keyset,
    };
    use crate::nintendo::nx::walker::NcaWalker;
    use crate::util::NoProgress;
    use sha2::{Digest, Sha256};
    use std::fs::{self, File};
    use std::io::{Cursor, Write};
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    fn build_synthetic_nca(plaintext_section: &[u8]) -> Vec<u8> {
        const ENC_AES_CTR: u8 = 3;
        let mut header = [0u8; 0xC00];
        header[0x200..0x204].copy_from_slice(&NCA3_MAGIC);
        header[0x207] = 0;
        header[0x220] = 1;

        let section_start_byte = 0x4000u64;
        let section_size = plaintext_section.len() as u64;
        let section_end_byte = section_start_byte + section_size;
        let start_sector = (section_start_byte / 0x200) as u32;
        let end_sector = (section_end_byte / 0x200) as u32;

        header[NCA_FS_ENTRY_OFFSET..NCA_FS_ENTRY_OFFSET + 4]
            .copy_from_slice(&start_sector.to_le_bytes());
        header[NCA_FS_ENTRY_OFFSET + 4..NCA_FS_ENTRY_OFFSET + 8]
            .copy_from_slice(&end_sector.to_le_bytes());

        let fs0_off = NCA_FS_HEADER_OFFSET;
        header[fs0_off + 4] = ENC_AES_CTR;
        let ctr_low: u32 = 0x12345678;
        let ctr_high: u32 = 0x9ABCDEF0;
        header[fs0_off + 0x140..fs0_off + 0x144].copy_from_slice(&ctr_low.to_le_bytes());
        header[fs0_off + 0x144..fs0_off + 0x148].copy_from_slice(&ctr_high.to_le_bytes());

        let key_area = encrypt_key_area_block([[0x11; 16], [0x22; 16], TEST_BODY_KEY, [0x44; 16]]);
        header[0x300..0x340].copy_from_slice(&key_area);

        let keys = synthetic_keyset();
        encrypt_nca_header(&mut header, keys.header_key().unwrap()).unwrap();

        let mut nca = vec![0u8; section_start_byte as usize];
        nca[..0xC00].copy_from_slice(&header);

        let mut encrypted = plaintext_section.to_vec();
        let counter = initial_ctr_for_offset(
            &FsHeader {
                section_ctr_low: ctr_low,
                section_ctr_high: ctr_high,
                ..Default::default()
            },
            section_start_byte,
        );
        apply_ctr(&TEST_BODY_KEY, &counter, &mut encrypted).unwrap();
        nca.extend_from_slice(&encrypted);
        nca
    }

    fn round_trip_with_mode(mode: NczMode, plaintext_size: usize) {
        let plaintext: Vec<u8> = (0..plaintext_size).map(|i| (i & 0xFF) as u8).collect();
        let nca_bytes = build_synthetic_nca(&plaintext);

        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&nca_bytes).unwrap();
        tmp.flush().unwrap();

        let file = Arc::new(File::open(tmp.path()).unwrap());
        let keys = synthetic_keyset();
        let walker = NcaWalker::open(file, 0, nca_bytes.len() as u64, &keys).unwrap();

        let mut ncz_blob_cursor = Cursor::new(Vec::new());
        nca_to_ncz(
            &walker,
            &mut ncz_blob_cursor,
            NcaToNczOptions { mode, level: 3 },
            &NoProgress,
        )
        .unwrap();
        let ncz_blob = ncz_blob_cursor.into_inner();

        let mut cur = Cursor::new(&ncz_blob);
        let mut recovered = Vec::new();
        ncz_to_nca(&mut cur, &mut recovered, &NoProgress).unwrap();
        assert_eq!(recovered.len(), nca_bytes.len(), "size mismatch");
        let mismatch = recovered.iter().zip(&nca_bytes).position(|(a, b)| a != b);
        if let Some(p) = mismatch {
            panic!(
                "first mismatch at byte 0x{p:X} (recovered=0x{:02X} expected=0x{:02X})",
                recovered[p], nca_bytes[p]
            );
        }
    }

    #[test]
    fn round_trip_solid_small() {
        round_trip_with_mode(NczMode::Solid, 0x10000);
    }

    #[test]
    fn round_trip_block_aligned() {
        round_trip_with_mode(NczMode::Block { size_exp: 14 }, 0x40000);
    }

    #[test]
    fn round_trip_block_unaligned() {
        round_trip_with_mode(NczMode::Block { size_exp: 14 }, 0x40200);
    }

    fn build_synthetic_nsp(nca_bytes: &[u8]) -> Vec<u8> {
        let specs = vec![
            ("game.nca".to_string(), nca_bytes.len() as u64),
            ("ticket.tik".to_string(), 16),
        ];
        let hdr = pfs0::build_header(&specs, &pfs0::Pfs0LayoutHints::default()).unwrap();
        let mut out = hdr.bytes;
        out.extend_from_slice(nca_bytes);
        out.extend_from_slice(&[0xAB; 16]);
        out
    }

    #[test]
    fn nsp_round_trip_through_files() {
        let nca = build_synthetic_nca(&(0..0x40200).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>());
        let nsp_blob = build_synthetic_nsp(&nca);

        let dir = tempfile::tempdir().unwrap();
        let nsp_path = dir.path().join("game.nsp");
        let nsz_path = dir.path().join("game.nsz");
        let recovered_path = dir.path().join("recovered.nsp");
        fs::write(&nsp_path, &nsp_blob).unwrap();

        let keys = synthetic_keyset();
        compress_container(
            &nsp_path,
            &nsz_path,
            NxCompressOptions {
                level: 3,
                mode: NczMode::Solid,
            },
            &keys,
            &NoProgress,
            None,
        )
        .unwrap();

        crate::nintendo::nx::decompress::decompress_container(
            &nsz_path,
            &recovered_path,
            &keys,
            &NoProgress,
            None,
        )
        .unwrap();

        let recovered = fs::read(&recovered_path).unwrap();
        let original_sha = Sha256::digest(&nsp_blob);
        let recovered_sha = Sha256::digest(&recovered);
        assert_eq!(
            original_sha.as_slice(),
            recovered_sha.as_slice(),
            "round trip lost bytes (orig={}, rec={})",
            nsp_blob.len(),
            recovered.len()
        );
    }
}
