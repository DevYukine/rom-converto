use crate::nintendo::ctr::constants::{
    CTR_MEDIA_UNIT_SIZE, NCCH_FLAGS_OFFSET, NCCH_FLAGS7_NOCRYPTO, NCCH_MAGIC_OFFSET,
    NCSD_PARTITION_TABLE_OFFSET,
};
use crate::nintendo::ctr::models::title_metadata::TitleMetadata;
use crate::nintendo::ctr::util::align_64_usize;
use crate::nintendo::ctr::z3ds::compress_worker::{
    Z3dsCompressWork, Z3dsCompressedFrame, encode_seekable, make_z3ds_compress_workers,
};
use crate::nintendo::ctr::z3ds::error::{Z3dsError, Z3dsResult};
use crate::nintendo::ctr::z3ds::models::{
    Z3DS_HEADER_SIZE, Z3dsHeader, Z3dsMetadata, Z3dsMetadataItem, underlying_magic,
};
use crate::nintendo::ctr::z3ds::seekable::{FRAME_SIZE_CIA, FRAME_SIZE_DEFAULT};
use crate::util::worker_pool::{Pool, parallelism};
use crate::util::{BYTES_PER_MB, CancelToken, ProgressReporter, await_with_progress_cancel};
use binrw::{BinRead, BinWrite, Endian};
use chrono::Utc;
use log::{info, warn};
use std::io::{BufReader, BufWriter as StdBufWriter, Cursor, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use tokio::task;

/// Zstd level used when the caller does not request one. Level 0
/// asks libzstd for its own default (currently level 3) and picks
/// up any future zstd tuning for free.
pub const DEFAULT_ZSTD_LEVEL: i32 = 0;

/// Maximum accepted zstd level. libzstd clamps anything above 22
/// down to 22 internally but over-max values are rejected up front so
/// the CLI fails before opening the input file.
pub const MAX_ZSTD_LEVEL: i32 = 22;

/// Minimum accepted zstd level. Negative levels are valid zstd
/// tunings but never a good fit for ROM data, so they are capped off
/// at the lower end.
pub const MIN_ZSTD_LEVEL: i32 = 0;

/// Size of the probe buffer used for the encryption check. Must cover the
/// furthest offset the check reads: NCSD partition 0 plus its NCCH header
/// flags. Real-world NCSD images can place partition 0 at media-unit offsets
/// well past 0x80 (which is 64 KB), so a 64 KB probe would silently let
/// encrypted ROMs through `check_ncch_not_encrypted` (which skips the check
/// on EOF). 1 MB is still negligible RAM and covers any realistic partition
/// 0 offset.
const ENCRYPTION_PROBE_SIZE: usize = 1024 * 1024;

/// Compile-time guard. `ENCRYPTION_PROBE_SIZE` must be large enough to reach
/// an NCSD partition 0 placed at MU=0x100 (offset 0x20000). Shrinking the
/// constant below this bound fails the build before any silent
/// encryption-check regression can ship.
const _: () = assert!(
    ENCRYPTION_PROBE_SIZE >= 0x20000 + 0x200,
    "ENCRYPTION_PROBE_SIZE too small for high-MU NCSD partitions",
);

pub async fn compress_rom(
    input: &Path,
    output: &Path,
    level: Option<i32>,
    allow_encrypted: bool,
    progress: &dyn ProgressReporter,
) -> Z3dsResult<()> {
    compress_rom_cancellable(
        input,
        output,
        level,
        allow_encrypted,
        progress,
        CancelToken::new(),
    )
    .await
}

/// A sibling temp path so an interrupted write never lands on the final
/// name.
fn scratch_output_path(output: &Path) -> std::path::PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    output.with_file_name(name)
}

/// Like [`compress_rom`] but observes `cancel` at every frame boundary;
/// on cancel the partial output is removed.
pub async fn compress_rom_cancellable(
    input: &Path,
    output: &Path,
    level: Option<i32>,
    allow_encrypted: bool,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Z3dsResult<()> {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let (underlying_magic, frame_size) = match ext.as_str() {
        "cia" => (underlying_magic::CIA, FRAME_SIZE_CIA),
        "cci" | "3ds" => (underlying_magic::NCSD, FRAME_SIZE_DEFAULT),
        "cxi" => (underlying_magic::NCCH, FRAME_SIZE_DEFAULT),
        "3dsx" => (underlying_magic::THREEDSX, FRAME_SIZE_DEFAULT),
        other => return Err(Z3dsError::UnsupportedInputFormat(other.to_string())),
    };

    let zstd_level = level.unwrap_or(DEFAULT_ZSTD_LEVEL);
    if !(MIN_ZSTD_LEVEL..=MAX_ZSTD_LEVEL).contains(&zstd_level) {
        return Err(Z3dsError::InvalidCompressionLevel {
            level: zstd_level,
            min: MIN_ZSTD_LEVEL,
            max: MAX_ZSTD_LEVEL,
        });
    }

    let uncompressed_size = tokio::fs::metadata(input).await?.len();

    // Read only the bytes the encryption check needs. See ENCRYPTION_PROBE_SIZE.
    let probe = {
        let probe_len = std::cmp::min(uncompressed_size, ENCRYPTION_PROBE_SIZE as u64) as usize;
        let mut buf = vec![0u8; probe_len];
        let mut f = tokio::fs::File::open(input).await?;
        use tokio::io::AsyncReadExt as _;
        f.read_exact(&mut buf).await?;
        buf
    };
    match check_not_encrypted(&probe, &ext) {
        Ok(()) => {}
        Err(e @ (Z3dsError::InputNotDecrypted | Z3dsError::EncryptionStateUnknown)) => {
            if allow_encrypted {
                warn!(
                    "{}: {e}. Compressing anyway because --allow-encrypted was set; the output may be near the same size as the input.",
                    input.display()
                );
            } else {
                return Err(e);
            }
        }
        Err(e) => return Err(e),
    }
    drop(probe);

    let version = env!("CARGO_PKG_VERSION");
    let metadata = Z3dsMetadata::new(vec![
        Z3dsMetadataItem::new_str("compressor", &format!("rom-converto ({version})")),
        Z3dsMetadataItem::new_str("date", &Utc::now().to_rfc3339()),
        Z3dsMetadataItem::new_str("maxframesize", &frame_size.to_string()),
        Z3dsMetadataItem::new_str("zstdlevel", &zstd_level.to_string()),
    ]);
    let metadata_bytes = metadata.to_bytes()?;
    let metadata_size = metadata_bytes.len() as u32;

    progress.start(
        uncompressed_size,
        &format!(
            "Compressing {} ({:.2} MB)",
            input.file_name().unwrap_or_default().to_string_lossy(),
            uncompressed_size as f64 / BYTES_PER_MB,
        ),
    );

    // Atomic counter to relay progress out of the blocking thread.
    use std::sync::atomic::AtomicU64;
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_clone = bytes_done.clone();

    // The paths are moved into the blocking task; borrows do not cross await.
    let write_path = scratch_output_path(output);
    let input_owned = input.to_path_buf();
    let write_owned = write_path.clone();
    let cancel_bg = cancel.clone();
    let metadata_bytes_owned = metadata_bytes;

    let handle = task::spawn_blocking(move || -> Z3dsResult<(u64, u64)> {
        // std::fs (not tokio) lets the reader and writer hand off directly to zstd.
        let in_file = std::fs::File::open(&input_owned)?;
        let mut reader = BufReader::with_capacity(4 * 1024 * 1024, in_file);

        let out_file = std::fs::File::create(&write_owned)?;
        let mut writer = StdBufWriter::with_capacity(4 * 1024 * 1024, out_file);

        // Placeholder header. The real one is written after the payload, by
        // seeking back to offset 0 once compressed_size is known.
        let placeholder_header = vec![0u8; Z3DS_HEADER_SIZE as usize];
        writer.write_all(&placeholder_header)?;
        writer.write_all(&metadata_bytes_owned)?;

        // Spawn a worker pool with one persistent zstd encoder per
        // thread. The pool is torn down at the end of this closure
        // so its lifetime is bounded by one compress invocation.
        let n_threads = parallelism();
        let workers = make_z3ds_compress_workers(n_threads, zstd_level)?;
        let pool: Pool<Z3dsCompressWork, Z3dsCompressedFrame, Z3dsError> = Pool::spawn(workers);

        let compressed_size = encode_seekable(
            &pool,
            &mut reader,
            &mut writer,
            frame_size,
            uncompressed_size,
            &bytes_done_clone,
            &cancel_bg,
        )?;

        pool.shutdown();

        // Flush before seeking back so the BufWriter doesn't leak buffered
        // payload bytes past the rewritten header.
        writer.flush()?;

        let header = Z3dsHeader::new(
            underlying_magic,
            metadata_size,
            compressed_size,
            uncompressed_size,
        );
        let mut header_buf = Cursor::new(Vec::with_capacity(Z3DS_HEADER_SIZE as usize));
        header.write(&mut header_buf)?;
        let header_bytes = header_buf.into_inner();
        debug_assert_eq!(header_bytes.len(), Z3DS_HEADER_SIZE as usize);

        writer.seek(SeekFrom::Start(0))?;
        writer.write_all(&header_bytes)?;
        writer.flush()?;

        Ok((compressed_size, uncompressed_size))
    });

    let cleanup = {
        let write_path = write_path.clone();
        move || -> Z3dsError {
            let _ = std::fs::remove_file(&write_path);
            Z3dsError::Cancelled
        }
    };
    let (compressed_size, _) =
        match await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await {
            Ok(sizes) => sizes,
            Err(err) => {
                let _ = tokio::fs::remove_file(&write_path).await;
                return Err(err);
            }
        };
    tokio::fs::rename(&write_path, output).await?;

    let ratio = (1.0 - compressed_size as f64 / uncompressed_size as f64) * 100.0;
    info!(
        "Compressed {} -> {} ({:.1}% reduction)",
        input.display(),
        output.display(),
        ratio
    );

    Ok(())
}

/// Returns an error if the input ROM appears to be encrypted.
///
/// For NCCH and NCSD files the flags are at a known offset inside the header.
/// For CIA files the check looks at the first NCCH content block.
/// 3DSX files have no encryption, so the check is a no-op.
pub(crate) fn check_not_encrypted(data: &[u8], ext: &str) -> Z3dsResult<()> {
    match ext {
        "cci" | "3ds" => check_ncsd_not_encrypted(data),
        "cxi" => check_ncch_not_encrypted(data, 0),
        "cia" => check_cia_not_encrypted(data),
        _ => Ok(()),
    }
}

/// NCCH header flags are at offset 0x100 (signature) + 0x188 = 0x188 from the
/// start of the NCCH block. Bit 2 of flags[7] being set means NoCrypto.
/// If that bit is clear the partition is encrypted. When the header cannot be
/// read or the NCCH magic is absent, the crypto state is unknown and the check
/// fails safe rather than letting a possibly encrypted ROM through.
pub(crate) fn check_ncch_not_encrypted(data: &[u8], ncch_offset: usize) -> Z3dsResult<()> {
    let magic_start = ncch_offset + NCCH_MAGIC_OFFSET;
    if data.len() < magic_start + 4 {
        return Err(Z3dsError::EncryptionStateUnknown);
    }
    if data[magic_start..magic_start + 4] != underlying_magic::NCCH {
        return Err(Z3dsError::EncryptionStateUnknown);
    }

    let flags_offset = ncch_offset + NCCH_FLAGS_OFFSET;
    if data.len() <= flags_offset + 7 {
        return Err(Z3dsError::EncryptionStateUnknown);
    }
    let flags7 = data[flags_offset + 7];
    if flags7 & NCCH_FLAGS7_NOCRYPTO == 0 {
        return Err(Z3dsError::InputNotDecrypted);
    }
    Ok(())
}

/// NCSD header at offset 0x100 contains the NCCH header for partition 0.
/// The first partition starts right after the NCSD header at offset 0x4000 by default.
pub(crate) fn check_ncsd_not_encrypted(data: &[u8]) -> Z3dsResult<()> {
    let magic_end = NCCH_MAGIC_OFFSET + 4;
    if data.len() < magic_end || data[NCCH_MAGIC_OFFSET..magic_end] != underlying_magic::NCSD {
        return Err(Z3dsError::EncryptionStateUnknown);
    }
    // Partition 0 NCCH starts at the offset stored in NCSD partition table.
    if data.len() < NCSD_PARTITION_TABLE_OFFSET + 8 {
        return Err(Z3dsError::EncryptionStateUnknown);
    }
    let partition_offset_mu = u32::from_le_bytes([
        data[NCSD_PARTITION_TABLE_OFFSET],
        data[NCSD_PARTITION_TABLE_OFFSET + 1],
        data[NCSD_PARTITION_TABLE_OFFSET + 2],
        data[NCSD_PARTITION_TABLE_OFFSET + 3],
    ]);
    let partition_offset = partition_offset_mu as usize * CTR_MEDIA_UNIT_SIZE as usize;
    check_ncch_not_encrypted(data, partition_offset)
}

/// CIA files have no magic at offset 0. Per-content encryption is recorded in
/// the TMD, which sits in plaintext before the content section. In an encrypted
/// CIA the whole content (the NCCH header and its magic included) is title-key
/// encrypted, so the NCCH magic is absent at the content offset; the TMD
/// content-chunk flags are the only reliable signal there. A decrypted CIA still
/// falls through to the plaintext NCCH NoCrypto check.
pub(crate) fn check_cia_not_encrypted(data: &[u8]) -> Z3dsResult<()> {
    if data.len() < 0x20 {
        return Err(Z3dsError::EncryptionStateUnknown);
    }
    // CIA header layout (little-endian):
    //   0x00  u32  header_size
    //   0x04  u16  type
    //   0x06  u16  version
    //   0x08  u32  cert_chain_size
    //   0x0C  u32  ticket_size
    //   0x10  u32  tmd_size
    //   0x14  u32  meta_size
    //   0x18  u64  content_size
    let header_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let cert_chain_size = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    let ticket_size = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
    let tmd_size = u32::from_le_bytes([data[16], data[17], data[18], data[19]]) as usize;

    let tmd_offset =
        align_64_usize(header_size) + align_64_usize(cert_chain_size) + align_64_usize(ticket_size);
    let content_offset = tmd_offset + align_64_usize(tmd_size);

    if tmd_size == 0 || data.len() < tmd_offset + tmd_size {
        return Err(Z3dsError::EncryptionStateUnknown);
    }

    let mut cursor = Cursor::new(&data[tmd_offset..tmd_offset + tmd_size]);
    let tmd = TitleMetadata::read_options(&mut cursor, Endian::Big, ())
        .map_err(|_| Z3dsError::EncryptionStateUnknown)?;

    if tmd
        .content_chunk_records
        .iter()
        .any(|r| r.content_type.is_encrypted())
    {
        return Err(Z3dsError::InputNotDecrypted);
    }

    check_ncch_not_encrypted(data, content_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::constants::{
        NCCH_FLAGS_OFFSET, NCCH_FLAGS7_NOCRYPTO, NCCH_MAGIC_OFFSET, NCSD_PARTITION_TABLE_OFFSET,
    };
    use crate::nintendo::ctr::models::signature::{SignatureData, SignatureType};
    use crate::nintendo::ctr::models::title_metadata::{
        ContentChunkRecord, ContentInfoRecord, ContentType, TitleMetadataHeader,
    };
    use crate::nintendo::ctr::z3ds::error::Z3dsError;

    // Builds a fake NCCH block starting at `offset` within a zeroed buffer of `total_size`.
    // `decrypted` controls whether the NoCrypto flag (flags[7] bit 2) is set.
    fn make_ncch_at(total_size: usize, offset: usize, decrypted: bool) -> Vec<u8> {
        let mut data = vec![0u8; total_size];
        let magic_start = offset + NCCH_MAGIC_OFFSET;
        data[magic_start..magic_start + 4].copy_from_slice(&underlying_magic::NCCH);
        if decrypted {
            data[offset + NCCH_FLAGS_OFFSET + 7] = NCCH_FLAGS7_NOCRYPTO;
        }
        data
    }

    // Builds a fake NCSD ROM with a partition 0 NCCH at media-unit offset `partition_mu`.
    fn make_ncsd(partition_mu: u32, ncch_decrypted: bool) -> Vec<u8> {
        let partition_offset = partition_mu as usize * 0x200;
        let total = partition_offset + 0x200;
        let mut data = make_ncch_at(total, partition_offset, ncch_decrypted);
        let magic_start = NCCH_MAGIC_OFFSET;
        data[magic_start..magic_start + 4].copy_from_slice(&underlying_magic::NCSD);
        data[NCSD_PARTITION_TABLE_OFFSET..NCSD_PARTITION_TABLE_OFFSET + 4]
            .copy_from_slice(&partition_mu.to_le_bytes());
        data
    }

    // Serializes a minimal single-content TMD whose content chunk carries the
    // chosen Encrypted flag. Mirrors the struct literal in title_metadata tests.
    fn serialize_tmd(content_encrypted: bool) -> Vec<u8> {
        let tmd = TitleMetadata {
            signature_data: SignatureData {
                signature_type: SignatureType::Rsa2048Sha256,
                signature: vec![0xBB; 0x100],
                padding: vec![0x00; 0x3C],
            },
            header: TitleMetadataHeader {
                signature_issuer: vec![0x00; 0x40],
                version: 1,
                ca_crl_version: 0,
                signer_crl_version: 0,
                reserved1: 0,
                system_version: 0,
                title_id: 0x0004000000125600,
                title_type: 0x00040010,
                group_id: 0,
                save_data_size: 0,
                srl_private_save_data_size: 0,
                reserved2: 0,
                srl_flag: 0,
                reserved3: vec![0x00; 0x31],
                access_rights: 0,
                title_version: 0x0100,
                content_count: 1,
                boot_content: 0,
                padding: 0,
                content_info_records_hash: vec![0x00; 0x20],
            },
            content_info_records: vec![
                ContentInfoRecord {
                    content_index_offset: 0,
                    content_command_count: 1,
                    hash: vec![0x00; 0x20],
                };
                64
            ],
            content_chunk_records: vec![ContentChunkRecord {
                content_id: 0,
                content_index: 0,
                content_type: ContentType(if content_encrypted { 0x0001 } else { 0x0000 }),
                content_size: 0x00400000,
                hash: vec![0x00; 0x20],
            }],
        };
        let mut buf = Vec::new();
        tmd.write(&mut Cursor::new(&mut buf)).unwrap();
        buf
    }

    // Builds a fake CIA probe with a real serialized TMD at tmd_offset (0x40)
    // and an NCCH block at content_offset. `content_encrypted` drives the TMD
    // content-chunk Encrypted flag; `ncch_decrypted` drives the NCCH NoCrypto
    // flag at content_offset.
    fn make_cia_with_tmd(content_encrypted: bool, ncch_decrypted: bool) -> Vec<u8> {
        let tmd_bytes = serialize_tmd(content_encrypted);
        let tmd_offset = 0x40usize;
        let tmd_size = tmd_bytes.len();
        let content_offset = tmd_offset + align_64_usize(tmd_size);
        let total = content_offset + 0x200;

        let mut data = make_ncch_at(total, content_offset, ncch_decrypted);
        data[tmd_offset..tmd_offset + tmd_size].copy_from_slice(&tmd_bytes);
        data[0..4].copy_from_slice(&0x20u32.to_le_bytes());
        data[16..20].copy_from_slice(&(tmd_size as u32).to_le_bytes());
        data
    }

    // Like make_cia_with_tmd but leaves the content region zeroed (no NCCH
    // magic), matching a real encrypted CIA where the NCCH is ciphertext.
    fn make_cia_tmd_only(content_encrypted: bool) -> Vec<u8> {
        let tmd_bytes = serialize_tmd(content_encrypted);
        let tmd_offset = 0x40usize;
        let tmd_size = tmd_bytes.len();
        let content_offset = tmd_offset + align_64_usize(tmd_size);
        let total = content_offset + 0x200;

        let mut data = vec![0u8; total];
        data[tmd_offset..tmd_offset + tmd_size].copy_from_slice(&tmd_bytes);
        data[0..4].copy_from_slice(&0x20u32.to_le_bytes());
        data[16..20].copy_from_slice(&(tmd_size as u32).to_le_bytes());
        data
    }

    fn make_cia(ncch_decrypted: bool) -> Vec<u8> {
        make_cia_with_tmd(false, ncch_decrypted)
    }

    #[test]
    fn ncch_decrypted_passes() {
        let data = make_ncch_at(0x200, 0, true);
        assert!(check_ncch_not_encrypted(&data, 0).is_ok());
    }

    #[test]
    fn ncch_encrypted_fails() {
        let data = make_ncch_at(0x200, 0, false);
        let err = check_ncch_not_encrypted(&data, 0).unwrap_err();
        assert!(matches!(err, Z3dsError::InputNotDecrypted));
    }

    #[test]
    fn ncch_wrong_magic_fails_safe() {
        // No NCCH magic → crypto state is unknown, fail safe.
        let data = vec![0u8; 0x200];
        assert!(matches!(
            check_ncch_not_encrypted(&data, 0).unwrap_err(),
            Z3dsError::EncryptionStateUnknown
        ));
    }

    #[test]
    fn ncch_too_short_for_magic_fails_safe() {
        let data = vec![0u8; 0x50]; // shorter than 0x100 + 4
        assert!(matches!(
            check_ncch_not_encrypted(&data, 0).unwrap_err(),
            Z3dsError::EncryptionStateUnknown
        ));
    }

    #[test]
    fn ncch_too_short_for_flags_fails_safe() {
        // Has the magic but not enough bytes to reach flags[7].
        let mut data = vec![0u8; 0x18F]; // flags[7] is at 0x18F, need at least 0x190
        data[0x100..0x104].copy_from_slice(b"NCCH");
        assert!(matches!(
            check_ncch_not_encrypted(&data, 0).unwrap_err(),
            Z3dsError::EncryptionStateUnknown
        ));
    }

    #[test]
    fn ncch_with_nonzero_offset_decrypted_passes() {
        let data = make_ncch_at(0x4200, 0x4000, true);
        assert!(check_ncch_not_encrypted(&data, 0x4000).is_ok());
    }

    #[test]
    fn ncch_with_nonzero_offset_encrypted_fails() {
        let data = make_ncch_at(0x4200, 0x4000, false);
        let err = check_ncch_not_encrypted(&data, 0x4000).unwrap_err();
        assert!(matches!(err, Z3dsError::InputNotDecrypted));
    }

    #[test]
    fn ncsd_decrypted_passes() {
        let data = make_ncsd(1, true); // partition at MU=1 → offset 0x200
        assert!(check_ncsd_not_encrypted(&data).is_ok());
    }

    #[test]
    fn ncsd_encrypted_fails() {
        let data = make_ncsd(1, false);
        let err = check_ncsd_not_encrypted(&data).unwrap_err();
        assert!(matches!(err, Z3dsError::InputNotDecrypted));
    }

    #[test]
    fn ncsd_wrong_magic_fails_safe() {
        let data = vec![0u8; 0x200];
        assert!(matches!(
            check_ncsd_not_encrypted(&data).unwrap_err(),
            Z3dsError::EncryptionStateUnknown
        ));
    }

    #[test]
    fn cia_decrypted_passes() {
        let data = make_cia(true);
        assert!(check_cia_not_encrypted(&data).is_ok());
    }

    #[test]
    fn cia_encrypted_fails() {
        let data = make_cia(false);
        let err = check_cia_not_encrypted(&data).unwrap_err();
        assert!(matches!(err, Z3dsError::InputNotDecrypted));
    }

    #[test]
    fn cia_too_short_fails_safe() {
        let data = vec![0u8; 0x10]; // shorter than 0x20
        assert!(matches!(
            check_cia_not_encrypted(&data).unwrap_err(),
            Z3dsError::EncryptionStateUnknown
        ));
    }

    #[test]
    fn cia_tmd_encrypted_flag_returns_not_decrypted() {
        // TMD marks the content encrypted and the content region is junk (no
        // NCCH magic), as in a real encrypted CIA. The TMD flag is the only
        // signal, so this must be InputNotDecrypted, not EncryptionStateUnknown.
        let data = make_cia_tmd_only(true);
        assert!(matches!(
            check_cia_not_encrypted(&data).unwrap_err(),
            Z3dsError::InputNotDecrypted
        ));
    }

    #[test]
    fn cia_tmd_decrypted_with_plain_ncch_passes() {
        let data = make_cia_with_tmd(false, true);
        assert!(check_cia_not_encrypted(&data).is_ok());
    }

    #[test]
    fn cia_truncated_tmd_fails_safe() {
        // Header advertises a TMD the probe is too short to contain.
        let mut data = vec![0u8; 0x40];
        data[0..4].copy_from_slice(&0x20u32.to_le_bytes());
        data[16..20].copy_from_slice(&0x800u32.to_le_bytes());
        assert!(matches!(
            check_cia_not_encrypted(&data).unwrap_err(),
            Z3dsError::EncryptionStateUnknown
        ));
    }

    #[test]
    fn dispatch_cxi_decrypted_passes() {
        let data = make_ncch_at(0x200, 0, true);
        assert!(check_not_encrypted(&data, "cxi").is_ok());
    }

    #[test]
    fn dispatch_cxi_encrypted_fails() {
        let data = make_ncch_at(0x200, 0, false);
        assert!(matches!(
            check_not_encrypted(&data, "cxi").unwrap_err(),
            Z3dsError::InputNotDecrypted
        ));
    }

    #[test]
    fn dispatch_3dsx_always_passes() {
        // 3DSX has no encryption, so any content is accepted.
        let data = vec![0xFFu8; 128];
        assert!(check_not_encrypted(&data, "3dsx").is_ok());
    }

    #[test]
    fn dispatch_cia_decrypted_passes() {
        let data = make_cia(true);
        assert!(check_not_encrypted(&data, "cia").is_ok());
    }

    #[test]
    fn dispatch_cci_decrypted_passes() {
        let data = make_ncsd(1, true);
        assert!(check_not_encrypted(&data, "cci").is_ok());
    }

    #[test]
    fn dispatch_3ds_decrypted_passes() {
        let data = make_ncsd(1, true);
        assert!(check_not_encrypted(&data, "3ds").is_ok());
    }

    /// Regression for the encryption-probe size: NCSD images can have
    /// partition 0 at media-unit offsets well past 0x80 (past 64 KB).
    /// The probe must be large enough that `check_ncch_not_encrypted` reaches
    /// the NCCH header at the partition 0 offset, otherwise the check
    /// silently passes on encrypted ROMs.
    #[test]
    fn ncsd_partition_at_high_mu_decrypted_passes() {
        // Partition 0 at MU=0x100 → offset 0x20000 (128 KB), well above the
        // old 64 KB probe limit but inside the new 1 MB probe.
        let data = make_ncsd(0x100, true);
        assert!(data.len() >= 0x20000 + 0x200);
        assert!(check_ncsd_not_encrypted(&data).is_ok());
    }

    #[test]
    fn ncsd_partition_at_high_mu_encrypted_fails() {
        let data = make_ncsd(0x100, false);
        let err = check_ncsd_not_encrypted(&data).unwrap_err();
        assert!(matches!(err, Z3dsError::InputNotDecrypted));
    }
}
