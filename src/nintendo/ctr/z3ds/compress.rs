use crate::nintendo::ctr::constants::{
    CTR_MEDIA_UNIT_SIZE, NCCH_FLAGS_OFFSET, NCCH_FLAGS7_NOCRYPTO, NCCH_MAGIC_OFFSET,
    NCSD_PARTITION0_OFFSET_FIELD,
};
use crate::nintendo::ctr::util::align_64_usize;
use crate::nintendo::ctr::z3ds::error::{Z3dsError, Z3dsResult};
use crate::nintendo::ctr::z3ds::models::{
    Z3dsHeader, Z3dsMetadata, Z3dsMetadataItem, underlying_magic,
};
use crate::nintendo::ctr::z3ds::seekable::{FRAME_SIZE_CIA, FRAME_SIZE_DEFAULT, encode_seekable};
use crate::util::{BYTES_PER_MB, create_standalone_progress_bar};
use binrw::BinWrite;
use chrono::Utc;
use log::info;
use std::io::Cursor;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::task;

pub async fn compress_rom(input: &Path, output: &Path) -> Z3dsResult<()> {
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

    let input_data = tokio::fs::read(input).await?;

    // Encryption check: for NCCH/NCSD files the NcchHeader flags byte at offset
    // 0x100 + 0x188 (= 0x288) bit 2 being clear means the content is encrypted.
    // A simpler heuristic: check the NCCH/NCSD magic at offset 0x100, then check
    // flags[7] (cryptomethod). For CIA files we look at the first NCCH inside.
    check_not_encrypted(&input_data, &ext)?;

    let uncompressed_size = input_data.len() as u64;

    let pg = create_standalone_progress_bar(
        uncompressed_size,
        format!(
            "Compressing {} ({:.2} MB)",
            input.file_name().unwrap_or_default().to_string_lossy(),
            uncompressed_size as f64 / BYTES_PER_MB,
        ),
    )?;

    // Build metadata
    let version = crate::built_info::PKG_VERSION;
    let metadata = Z3dsMetadata::new(vec![
        Z3dsMetadataItem::new_str("compressor", &format!("rom-converto ({version})")),
        Z3dsMetadataItem::new_str("date", &Utc::now().to_rfc3339()),
        Z3dsMetadataItem::new_str("maxframesize", &frame_size.to_string()),
    ]);
    let metadata_bytes = metadata.to_bytes()?;
    let metadata_size = metadata_bytes.len() as u32;

    // Compress on a blocking thread
    let data_clone = input_data.clone();
    let compressed =
        task::spawn_blocking(move || encode_seekable(&data_clone, frame_size, 0)).await??;

    pg.finish_and_clear();

    let compressed_size = compressed.len() as u64;

    // Build and serialise the header
    let header = Z3dsHeader::new(
        underlying_magic,
        metadata_size,
        compressed_size,
        uncompressed_size,
    );

    let mut header_buf = Cursor::new(Vec::new());
    header.write(&mut header_buf)?;
    let header_bytes = header_buf.into_inner();

    // Write header + metadata + compressed data
    let file = File::create(output).await?;
    let mut out = BufWriter::new(file);
    out.write_all(&header_bytes).await?;
    out.write_all(&metadata_bytes).await?;
    out.write_all(&compressed).await?;
    out.flush().await?;

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
/// For CIA files we look at the first NCCH content block.
/// For 3DSX files there is no encryption, so the check is skipped.
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
/// If that bit is clear the partition is encrypted.
pub(crate) fn check_ncch_not_encrypted(data: &[u8], ncch_offset: usize) -> Z3dsResult<()> {
    let magic_start = ncch_offset + NCCH_MAGIC_OFFSET;
    if data.len() < magic_start + 4 {
        return Ok(()); // can't check, let it through
    }
    if &data[magic_start..magic_start + 4] != b"NCCH" {
        return Ok(());
    }

    let flags_offset = ncch_offset + NCCH_FLAGS_OFFSET;
    if data.len() <= flags_offset + 7 {
        return Ok(());
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
    if data.len() < magic_end || &data[NCCH_MAGIC_OFFSET..magic_end] != b"NCSD" {
        return Ok(());
    }
    // Partition 0 NCCH starts at the offset stored in NCSD partition table.
    if data.len() < NCSD_PARTITION0_OFFSET_FIELD + 8 {
        return Ok(());
    }
    let partition_offset_mu = u32::from_le_bytes([
        data[NCSD_PARTITION0_OFFSET_FIELD],
        data[NCSD_PARTITION0_OFFSET_FIELD + 1],
        data[NCSD_PARTITION0_OFFSET_FIELD + 2],
        data[NCSD_PARTITION0_OFFSET_FIELD + 3],
    ]);
    let partition_offset = partition_offset_mu as usize * CTR_MEDIA_UNIT_SIZE as usize;
    check_ncch_not_encrypted(data, partition_offset)
}

/// CIA files don't have a magic at offset 0. We locate the first NCCH by
/// parsing the CIA header sizes to find the content section.
pub(crate) fn check_cia_not_encrypted(data: &[u8]) -> Z3dsResult<()> {
    if data.len() < 0x20 {
        return Ok(());
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

    let content_offset = align_64_usize(header_size)
        + align_64_usize(cert_chain_size)
        + align_64_usize(ticket_size)
        + align_64_usize(tmd_size);

    check_ncch_not_encrypted(data, content_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::constants::{
        NCCH_FLAGS_OFFSET, NCCH_FLAGS7_NOCRYPTO, NCCH_MAGIC_OFFSET,
    };
    use crate::nintendo::ctr::z3ds::error::Z3dsError;

    // Builds a fake NCCH block starting at `offset` within a zeroed buffer of `total_size`.
    // `decrypted` controls whether the NoCrypto flag (flags[7] bit 2) is set.
    fn make_ncch_at(total_size: usize, offset: usize, decrypted: bool) -> Vec<u8> {
        let mut data = vec![0u8; total_size];
        let magic_start = offset + NCCH_MAGIC_OFFSET;
        data[magic_start..magic_start + 4].copy_from_slice(b"NCCH");
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
        data[magic_start..magic_start + 4].copy_from_slice(b"NCSD");
        data[0x120..0x124].copy_from_slice(&partition_mu.to_le_bytes());
        data
    }

    // Builds a fake CIA header pointing to an NCCH at the computed content offset.
    // Uses header_size=0x20, all section sizes 0 → content at align64(0x20) = 0x40.
    fn make_cia(ncch_decrypted: bool) -> Vec<u8> {
        // content_offset = align64(0x20) = 0x40
        let content_offset: usize = 0x40;
        let total = content_offset + 0x200;
        let mut data = make_ncch_at(total, content_offset, ncch_decrypted);
        // header_size = 0x20 (LE u32)
        data[0..4].copy_from_slice(&0x20u32.to_le_bytes());
        // cert_chain_size, ticket_size, tmd_size all stay 0
        data
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
    fn ncch_wrong_magic_skips_check() {
        // No NCCH magic → check is silently skipped.
        let data = vec![0u8; 0x200];
        assert!(check_ncch_not_encrypted(&data, 0).is_ok());
    }

    #[test]
    fn ncch_too_short_for_magic_skips_check() {
        let data = vec![0u8; 0x50]; // shorter than 0x100 + 4
        assert!(check_ncch_not_encrypted(&data, 0).is_ok());
    }

    #[test]
    fn ncch_too_short_for_flags_skips_check() {
        // Has the magic but not enough bytes to reach flags[7].
        let mut data = vec![0u8; 0x18F]; // flags[7] is at 0x18F, need at least 0x190
        data[0x100..0x104].copy_from_slice(b"NCCH");
        assert!(check_ncch_not_encrypted(&data, 0).is_ok());
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
    fn ncsd_wrong_magic_skips_check() {
        let data = vec![0u8; 0x200];
        assert!(check_ncsd_not_encrypted(&data).is_ok());
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
    fn cia_too_short_skips_check() {
        let data = vec![0u8; 0x10]; // shorter than 0x20
        assert!(check_cia_not_encrypted(&data).is_ok());
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
}
