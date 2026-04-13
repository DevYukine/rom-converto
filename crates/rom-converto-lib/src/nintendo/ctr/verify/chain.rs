use crate::nintendo::ctr::constants::{
    CTR_MEDIA_UNIT_SIZE, CTR_NCSD_PARTITIONS, NCCH_MAGIC_OFFSET, NCSD_PARTITION_COUNT,
    NCSD_PARTITION_ENTRY_SIZE, NCSD_PARTITION_TABLE_OFFSET,
};
use crate::nintendo::ctr::models::certificate::{Certificate, PublicKey};
use crate::nintendo::ctr::models::cia::{CIA_HEADER_SIZE, CiaFileWithoutContent, CiaHeader};
use crate::nintendo::ctr::models::ncch_header::NcchHeader;
use crate::nintendo::ctr::models::title_metadata::TitleMetadata;
use crate::nintendo::ctr::util::align_64;
use crate::nintendo::ctr::verify::root_key::{ROOT_CA_EXPONENT, ROOT_CA_MODULUS};
use crate::nintendo::ctr::z3ds::models::Z3dsHeader;
use crate::util::ProgressReporter;
use anyhow::{Context, Result};
use binrw::{BinRead, BinWrite, Endian};
use rsa::pkcs1v15::VerifyingKey;
use rsa::signature::Verifier;
use rsa::{BigUint, RsaPublicKey};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Cursor, SeekFrom};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

#[derive(Debug, Clone, Default)]
pub struct CtrVerifyOptions {
    pub verify_content_hashes: bool,
}

pub type CiaVerifyOptions = CtrVerifyOptions;

#[derive(Debug, Clone, Serialize)]
pub struct CiaVerifyResult {
    pub legitimacy: CiaLegitimacy,
    pub ca_cert_valid: bool,
    pub tmd_signer_cert_valid: bool,
    pub ticket_signer_cert_valid: bool,
    pub tmd_signature_valid: bool,
    pub ticket_signature_valid: bool,
    pub content_hashes_valid: Option<bool>,
    pub title_id: String,
    pub console_id: u32,
    pub title_version: u16,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CiaLegitimacy {
    Legit(CiaLegitimacySubType),
    Piratelegit,
    Standard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CiaLegitimacySubType {
    Global,
    Personalized,
}

impl std::fmt::Display for CiaLegitimacy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CiaLegitimacy::Legit(CiaLegitimacySubType::Global) => write!(f, "Legit (Global)"),
            CiaLegitimacy::Legit(CiaLegitimacySubType::Personalized) => {
                write!(f, "Legit (Personalized)")
            }
            CiaLegitimacy::Piratelegit => write!(f, "Piratelegit"),
            CiaLegitimacy::Standard => write!(f, "Standard"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NcchPartitionResult {
    pub index: usize,
    pub name: String,
    pub title_id: String,
    pub product_code: String,
    pub encrypted: bool,
    pub ncch_magic_valid: bool,
    pub exheader_hash_valid: Option<bool>,
    pub logo_hash_valid: Option<bool>,
    pub exefs_hash_valid: Option<bool>,
    pub romfs_hash_valid: Option<bool>,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NcsdVerifyResult {
    pub ncsd_magic_valid: bool,
    pub title_id: String,
    pub partition_count: usize,
    pub partitions: Vec<NcchPartitionResult>,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "format")]
pub enum CtrVerifyResult {
    Cia(CiaVerifyResult),
    Ncsd(NcsdVerifyResult),
}

pub async fn verify_ctr(
    input: &Path,
    options: &CtrVerifyOptions,
    progress: &dyn ProgressReporter,
) -> Result<CtrVerifyResult> {
    let mut file = tokio::fs::File::open(input).await?;
    let file_size = file.metadata().await?.len();

    let mut probe = [0u8; 0x104];
    let probe_len = std::cmp::min(file_size, probe.len() as u64) as usize;
    file.read_exact(&mut probe[..probe_len]).await?;
    file.seek(SeekFrom::Start(0)).await?;

    if probe_len >= 4 && &probe[0..4] == b"Z3DS" {
        return verify_compressed(input, options, progress).await;
    }

    if probe_len >= 0x104 {
        let magic = &probe[0x100..0x104];
        if magic == b"NCSD" {
            let result = verify_ncsd_file(input, progress).await?;
            return Ok(CtrVerifyResult::Ncsd(result));
        }
        if magic == b"NCCH" {
            // Standalone NCCH gets reported as a single-partition NCSD result.
            let result = verify_standalone_ncch_file(input, progress).await?;
            return Ok(CtrVerifyResult::Ncsd(result));
        }
    }

    if probe_len >= 4 {
        let header_size = u32::from_le_bytes(probe[0..4].try_into()?);
        if header_size == CIA_HEADER_SIZE {
            let result = verify_cia(input, options, progress).await?;
            return Ok(CtrVerifyResult::Cia(result));
        }
    }

    Err(anyhow::anyhow!(
        "Unrecognized format: not a CIA, NCSD, NCCH, or Z3DS file"
    ))
}

async fn verify_compressed(
    input: &Path,
    options: &CtrVerifyOptions,
    progress: &dyn ProgressReporter,
) -> Result<CtrVerifyResult> {
    let mut file = tokio::fs::File::open(input).await?;

    let mut header_buf = vec![0u8; 0x20];
    file.read_exact(&mut header_buf).await?;
    let mut cursor = Cursor::new(&header_buf);
    let header = Z3dsHeader::read(&mut cursor).context("Failed to parse Z3DS header")?;

    let payload_offset = header.header_size as u64 + header.metadata_size as u64;
    let compressed_size = header.compressed_size;
    drop(file);

    progress.start(
        header.uncompressed_size,
        "Decompressing for verification...",
    );

    // Stream the compressed payload from disk through the zstd decoder into
    // a temp file. Peak heap is BufReader (4 MB) + BufWriter (4 MB) + libzstd
    // working state, regardless of file size.
    //
    // The seek-table footer is a zstd skippable frame, which libzstd skips on
    // its own (covered by `zstd_streaming_skips_skippable_frame_natively`).
    let temp_dir = tempfile::tempdir()?;
    let ext = match &header.underlying_magic {
        b"CIA\0" => "cia",
        b"NCSD" => "3ds",
        b"NCCH" => "cxi",
        _ => "bin",
    };
    let temp_path = temp_dir.path().join(format!("verify_temp.{ext}"));
    let temp_path_for_blocking = temp_path.clone();
    let input_path = input.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        use std::io::{BufReader, BufWriter, Read as _, Seek as _};
        let mut in_file = std::fs::File::open(&input_path)?;
        in_file.seek(SeekFrom::Start(payload_offset))?;
        // Cap the read at the declared compressed payload size to avoid
        // feeding trailing garbage or another concatenated section into the
        // decoder.
        let limited = in_file.take(compressed_size);
        let mut reader = BufReader::with_capacity(4 * 1024 * 1024, limited);
        let mut writer = BufWriter::with_capacity(
            4 * 1024 * 1024,
            std::fs::File::create(&temp_path_for_blocking)?,
        );
        zstd::stream::copy_decode(&mut reader, &mut writer)?;
        writer
            .into_inner()
            .map_err(|e| anyhow::anyhow!("failed to flush decompressed output: {e}"))?
            .sync_all()?;
        Ok(())
    })
    .await??;
    progress.inc(header.uncompressed_size / 4);

    progress.finish();

    // temp_dir's Drop removes the file after this function returns.
    let result = Box::pin(verify_ctr(&temp_path, options, progress)).await?;
    Ok(result)
}

pub async fn verify_cia(
    input: &Path,
    options: &CtrVerifyOptions,
    progress: &dyn ProgressReporter,
) -> Result<CiaVerifyResult> {
    let mut file = tokio::fs::File::open(input).await?;
    let file_size = file.metadata().await?.len();
    progress.start(file_size, "Verifying CIA signatures...");

    // The header's declared sizes drive the rest of the layout walk.
    let mut header_buf = vec![0u8; CIA_HEADER_SIZE as usize];
    file.read_exact(&mut header_buf).await?;
    let cia_header = CiaHeader::read_le(&mut Cursor::new(&header_buf))
        .context("Failed to parse CIA header")?;

    let header_end: u64 = CIA_HEADER_SIZE as u64;
    let cert_start = align_64(header_end);
    let cert_end = cert_start + cia_header.cert_chain_size as u64;
    let ticket_start = align_64(cert_end);
    let ticket_end = ticket_start + cia_header.ticket_size as u64;
    let tmd_start = align_64(ticket_end);
    let tmd_end = tmd_start + cia_header.tmd_size as u64;
    let content_start = align_64(tmd_end);

    if content_start > file_size {
        anyhow::bail!("CIA preamble exceeds file size (corrupt header)");
    }

    // Read only the preamble [0..content_start] (few MB at most); content
    // bytes will be hashed later by streaming directly from the file.
    let mut preamble = vec![0u8; content_start as usize];
    preamble[..CIA_HEADER_SIZE as usize].copy_from_slice(&header_buf);
    file.seek(SeekFrom::Start(CIA_HEADER_SIZE as u64)).await?;
    file.read_exact(&mut preamble[CIA_HEADER_SIZE as usize..])
        .await?;

    let mut details = Vec::new();

    let mut cursor = Cursor::new(&preamble);
    let cia_without_content = CiaFileWithoutContent::read_options(&mut cursor, Endian::Little, ())
        .context("Failed to parse CIA file")?;

    let title_id = format!("{:016X}", cia_without_content.tmd.header.title_id);
    let console_id = cia_without_content.ticket.ticket_data.console_id;
    let title_version = cia_without_content.tmd.header.title_version;

    details.push(format!("Title ID: {title_id}"));
    details.push(format!("Title Version: {title_version}"));
    details.push(format!(
        "Console ID: {console_id:#010X}{}",
        if console_id == 0 {
            " (global)"
        } else {
            " (personalized)"
        }
    ));
    details.push(format!(
        "Content Count: {}",
        cia_without_content.tmd.header.content_count
    ));

    progress.inc(file_size / 4);

    let ca_cert = find_cert_by_name_prefix(&cia_without_content.cert_chain, "CA");
    let cp_cert = find_cert_by_name_prefix(&cia_without_content.cert_chain, "CP");
    let xs_cert = find_cert_by_name_prefix(&cia_without_content.cert_chain, "XS");

    // Verify CA cert using Root key
    let ca_cert_valid = if let Some(ca) = &ca_cert {
        let body = serialize_cert_body(ca);
        let valid = verify_rsa_signature(&ROOT_CA_MODULUS, ROOT_CA_EXPONENT, &ca.signature, &body);
        details.push(format!(
            "CA certificate (Root -> CA): {}",
            if valid { "VALID" } else { "INVALID" }
        ));
        valid
    } else {
        details.push("CA certificate: NOT FOUND".to_string());
        false
    };

    // Verify CP cert (TMD signer)
    let tmd_signer_cert_valid = if let (Some(ca), Some(cp)) = (&ca_cert, &cp_cert) {
        if let Some((modulus, exponent)) = extract_rsa_key(&ca.public_key) {
            let body = serialize_cert_body(cp);
            let valid = verify_rsa_signature(modulus, exponent, &cp.signature, &body);
            details.push(format!(
                "TMD signer cert (CA -> CP): {}",
                if valid { "VALID" } else { "INVALID" }
            ));
            ca_cert_valid && valid
        } else {
            details.push("TMD signer cert: CA has unsupported key type".to_string());
            false
        }
    } else {
        details.push("TMD signer cert (CP): NOT FOUND".to_string());
        false
    };

    // Verify XS cert (ticket signer)
    let ticket_signer_cert_valid = if let (Some(ca), Some(xs)) = (&ca_cert, &xs_cert) {
        if let Some((modulus, exponent)) = extract_rsa_key(&ca.public_key) {
            let body = serialize_cert_body(xs);
            let valid = verify_rsa_signature(modulus, exponent, &xs.signature, &body);
            details.push(format!(
                "Ticket signer cert (CA -> XS): {}",
                if valid { "VALID" } else { "INVALID" }
            ));
            ca_cert_valid && valid
        } else {
            details.push("Ticket signer cert: CA has unsupported key type".to_string());
            false
        }
    } else {
        details.push("Ticket signer cert (XS): NOT FOUND".to_string());
        false
    };

    progress.inc(file_size / 4);

    // Verify TMD signature
    let tmd_signature_valid = if let Some(cp) = &cp_cert {
        if let Some((modulus, exponent)) = extract_rsa_key(&cp.public_key) {
            let body = serialize_tmd_body(&cia_without_content.tmd);
            let valid = verify_rsa_signature(
                modulus,
                exponent,
                &cia_without_content.tmd.signature_data.signature,
                &body,
            );
            details.push(format!(
                "TMD signature: {}",
                if valid { "VALID" } else { "INVALID" }
            ));
            tmd_signer_cert_valid && valid
        } else {
            details.push("TMD signature: CP has unsupported key type".to_string());
            false
        }
    } else {
        details.push("TMD signature: CP cert not found".to_string());
        false
    };

    // Verify ticket signature
    let ticket_signature_valid = if let Some(xs) = &xs_cert {
        if let Some((modulus, exponent)) = extract_rsa_key(&xs.public_key) {
            let body = serialize_ticket_body(&cia_without_content.ticket);
            let valid = verify_rsa_signature(
                modulus,
                exponent,
                &cia_without_content.ticket.signature_data.signature,
                &body,
            );
            details.push(format!(
                "Ticket signature: {}",
                if valid { "VALID" } else { "INVALID" }
            ));
            ticket_signer_cert_valid && valid
        } else {
            details.push("Ticket signature: XS has unsupported key type".to_string());
            false
        }
    } else {
        details.push("Ticket signature: XS cert not found".to_string());
        false
    };

    progress.inc(file_size / 4);

    let content_hashes_valid = if options.verify_content_hashes {
        match verify_content_hashes_streaming(
            &mut file,
            content_start,
            file_size,
            &cia_without_content.tmd,
            &mut details,
        )
        .await
        {
            Ok(valid) => Some(valid),
            Err(e) => {
                details.push(format!("Content hash verification failed: {e}"));
                Some(false)
            }
        }
    } else {
        None
    };

    progress.inc(file_size - (file_size / 4) * 3);
    progress.finish();

    let legitimacy = classify(
        tmd_signature_valid,
        ticket_signature_valid,
        content_hashes_valid,
        console_id,
    );

    details.push(format!("Classification: {legitimacy}"));

    Ok(CiaVerifyResult {
        legitimacy,
        ca_cert_valid,
        tmd_signer_cert_valid,
        ticket_signer_cert_valid,
        tmd_signature_valid,
        ticket_signature_valid,
        content_hashes_valid,
        title_id,
        console_id,
        title_version,
        details,
    })
}

async fn verify_ncsd_file(
    input: &Path,
    progress: &dyn ProgressReporter,
) -> Result<NcsdVerifyResult> {
    let mut file = tokio::fs::File::open(input).await?;
    let file_size = file.metadata().await?.len();
    progress.start(file_size, "Verifying NCSD integrity...");

    let mut details = Vec::new();

    let mut magic_buf = [0u8; 4];
    file.seek(SeekFrom::Start(NCCH_MAGIC_OFFSET as u64)).await?;
    file.read_exact(&mut magic_buf).await?;
    let ncsd_magic_valid = magic_buf == *b"NCSD";
    details.push(format!(
        "NCSD magic: {}",
        if ncsd_magic_valid { "VALID" } else { "INVALID" }
    ));

    // Title ID lives at NCSD header offset 0x108 (u64 LE).
    file.seek(SeekFrom::Start(0x108)).await?;
    let mut tid_buf = [0u8; 8];
    file.read_exact(&mut tid_buf).await?;
    let title_id = format!("{:016X}", u64::from_le_bytes(tid_buf));
    details.push(format!("Title ID: {title_id}"));

    file.seek(SeekFrom::Start(NCSD_PARTITION_TABLE_OFFSET as u64))
        .await?;
    let mut table_buf = [0u8; NCSD_PARTITION_COUNT * NCSD_PARTITION_ENTRY_SIZE];
    file.read_exact(&mut table_buf).await?;

    let mut partitions = Vec::new();
    let mut partition_count = 0usize;

    for i in 0..NCSD_PARTITION_COUNT {
        let entry_offset = i * NCSD_PARTITION_ENTRY_SIZE;
        let offset_mu = u32::from_le_bytes(table_buf[entry_offset..entry_offset + 4].try_into()?);
        let size_mu = u32::from_le_bytes(table_buf[entry_offset + 4..entry_offset + 8].try_into()?);

        if offset_mu == 0 && size_mu == 0 {
            continue;
        }

        partition_count += 1;
        let part_offset = offset_mu as u64 * CTR_MEDIA_UNIT_SIZE as u64;
        let part_size = size_mu as u64 * CTR_MEDIA_UNIT_SIZE as u64;
        let name = CTR_NCSD_PARTITIONS.get(i).unwrap_or(&"Unknown").to_string();

        details.push(format!(
            "Partition {i} ({name}): offset=0x{part_offset:X}, size=0x{part_size:X}"
        ));

        let result =
            verify_ncch_partition_from_file(&mut file, part_offset, part_size, i, &name, file_size)
                .await?;
        partitions.push(result);

        progress.inc(part_size);
    }

    progress.finish();

    details.push(format!("Partitions found: {partition_count}"));

    Ok(NcsdVerifyResult {
        ncsd_magic_valid,
        title_id,
        partition_count,
        partitions,
        details,
    })
}

async fn verify_standalone_ncch_file(
    input: &Path,
    progress: &dyn ProgressReporter,
) -> Result<NcsdVerifyResult> {
    let mut file = tokio::fs::File::open(input).await?;
    let file_size = file.metadata().await?.len();
    progress.start(file_size, "Verifying NCCH integrity...");

    let result =
        verify_ncch_partition_from_file(&mut file, 0, file_size, 0, "Main", file_size).await?;

    let title_id = result.title_id.clone();

    progress.finish();

    Ok(NcsdVerifyResult {
        ncsd_magic_valid: true,
        title_id,
        partition_count: 1,
        partitions: vec![result],
        details: vec!["Standalone NCCH file".to_string()],
    })
}

async fn verify_ncch_partition_from_file(
    file: &mut tokio::fs::File,
    offset: u64,
    _size: u64,
    index: usize,
    name: &str,
    file_size: u64,
) -> Result<NcchPartitionResult> {
    let mut details = Vec::new();

    if offset + 512 > file_size {
        return Ok(NcchPartitionResult {
            index,
            name: name.to_string(),
            title_id: String::new(),
            product_code: String::new(),
            encrypted: false,
            ncch_magic_valid: false,
            exheader_hash_valid: None,
            logo_hash_valid: None,
            exefs_hash_valid: None,
            romfs_hash_valid: None,
            details: vec!["Partition data truncated".to_string()],
        });
    }

    file.seek(SeekFrom::Start(offset)).await?;
    let mut header_buf = [0u8; 512];
    file.read_exact(&mut header_buf).await?;
    let header = NcchHeader::read(&mut Cursor::new(&header_buf))?;

    let ncch_magic_valid = header.magic == *b"NCCH";
    details.push(format!(
        "NCCH magic: {}",
        if ncch_magic_valid { "VALID" } else { "INVALID" }
    ));

    let mut tid_bytes = header.titleid;
    tid_bytes.reverse();
    let title_id = hex::encode(tid_bytes);
    let product_code = String::from_utf8_lossy(&header.productcode)
        .trim_end_matches('\0')
        .to_string();
    let encrypted = header.is_encrypted();

    details.push(format!("Title ID: {title_id}"));
    details.push(format!("Product Code: {product_code}"));
    details.push(format!(
        "Encryption: {}",
        if encrypted { "Encrypted" } else { "Decrypted" }
    ));

    if encrypted {
        details.push("Hash verification skipped (content is encrypted)".to_string());
        return Ok(NcchPartitionResult {
            index,
            name: name.to_string(),
            title_id,
            product_code,
            encrypted,
            ncch_magic_valid,
            exheader_hash_valid: None,
            logo_hash_valid: None,
            exefs_hash_valid: None,
            romfs_hash_valid: None,
            details,
        });
    }

    let mu = CTR_MEDIA_UNIT_SIZE as u64;

    // ExHeader hash covers `exhdrsize` bytes starting at NCCH offset 0x200.
    let exheader_hash_valid = if header.exhdrsize > 0 {
        let exhdr_offset = offset + 0x200;
        let exhdr_size = header.exhdrsize as u64;
        match read_and_hash(file, exhdr_offset, exhdr_size, file_size).await {
            Some(hash) => {
                let valid = hash == header.exhdrhash;
                details.push(format!(
                    "ExHeader hash: {}",
                    if valid { "OK" } else { "MISMATCH" }
                ));
                Some(valid)
            }
            None => {
                details.push("ExHeader hash: data truncated".to_string());
                Some(false)
            }
        }
    } else {
        None
    };

    let logo_hash_valid = if header.logosize > 0 {
        let logo_offset = offset + header.logooffset as u64 * mu;
        let logo_size = header.logosize as u64 * mu;
        match read_and_hash(file, logo_offset, logo_size, file_size).await {
            Some(hash) => {
                let valid = hash == header.logohash;
                details.push(format!(
                    "Logo hash: {}",
                    if valid { "OK" } else { "MISMATCH" }
                ));
                Some(valid)
            }
            None => {
                details.push("Logo hash: data truncated".to_string());
                Some(false)
            }
        }
    } else {
        None
    };

    // ExeFS hash: first exefshashsize * mu bytes
    let exefs_hash_valid = if header.exefssize > 0 && header.exefshashsize > 0 {
        let exefs_offset = offset + header.exefsoffset as u64 * mu;
        let hash_size = header.exefshashsize as u64 * mu;
        match read_and_hash(file, exefs_offset, hash_size, file_size).await {
            Some(hash) => {
                let valid = hash == header.exefshash;
                details.push(format!(
                    "ExeFS hash: {}",
                    if valid { "OK" } else { "MISMATCH" }
                ));
                Some(valid)
            }
            None => {
                details.push("ExeFS hash: data truncated".to_string());
                Some(false)
            }
        }
    } else {
        None
    };

    // RomFS hash: first romfshashsize * mu bytes
    let romfs_hash_valid = if header.romfssize > 0 && header.romfshashsize > 0 {
        let romfs_offset = offset + header.romfsoffset as u64 * mu;
        let hash_size = header.romfshashsize as u64 * mu;
        match read_and_hash(file, romfs_offset, hash_size, file_size).await {
            Some(hash) => {
                let valid = hash == header.romfshash;
                details.push(format!(
                    "RomFS hash: {}",
                    if valid { "OK" } else { "MISMATCH" }
                ));
                Some(valid)
            }
            None => {
                details.push("RomFS hash: data truncated".to_string());
                Some(false)
            }
        }
    } else {
        None
    };

    Ok(NcchPartitionResult {
        index,
        name: name.to_string(),
        title_id,
        product_code,
        encrypted,
        ncch_magic_valid,
        exheader_hash_valid,
        logo_hash_valid,
        exefs_hash_valid,
        romfs_hash_valid,
        details,
    })
}

/// Read a section from file and compute SHA-256. Returns None if out of bounds.
async fn read_and_hash(
    file: &mut tokio::fs::File,
    offset: u64,
    size: u64,
    file_size: u64,
) -> Option<[u8; 32]> {
    if offset + size > file_size || size == 0 {
        return None;
    }

    file.seek(SeekFrom::Start(offset)).await.ok()?;

    // Read in chunks to avoid allocating huge buffers
    const CHUNK_SIZE: usize = 4 * 1024 * 1024;
    let mut hasher = Sha256::new();
    let mut remaining = size as usize;

    while remaining > 0 {
        let to_read = remaining.min(CHUNK_SIZE);
        let mut buf = vec![0u8; to_read];
        file.read_exact(&mut buf).await.ok()?;
        hasher.update(&buf);
        remaining -= to_read;
    }

    let hash = hasher.finalize();
    Some(hash.into())
}

fn classify(
    tmd_valid: bool,
    ticket_valid: bool,
    content_hashes: Option<bool>,
    console_id: u32,
) -> CiaLegitimacy {
    if tmd_valid && ticket_valid {
        let sub = if console_id == 0 {
            CiaLegitimacySubType::Global
        } else {
            CiaLegitimacySubType::Personalized
        };
        CiaLegitimacy::Legit(sub)
    } else if tmd_valid && !ticket_valid {
        match content_hashes {
            Some(true) => CiaLegitimacy::Piratelegit,
            None => CiaLegitimacy::Piratelegit,
            Some(false) => CiaLegitimacy::Standard,
        }
    } else {
        CiaLegitimacy::Standard
    }
}

fn find_cert_by_name_prefix<'a>(certs: &'a [Certificate], prefix: &str) -> Option<&'a Certificate> {
    certs.iter().find(|c| {
        let name = String::from_utf8_lossy(&c.name);
        let name = name.trim_end_matches('\0');
        name.starts_with(prefix)
    })
}

fn extract_rsa_key(key: &PublicKey) -> Option<(&[u8], u32)> {
    match key {
        PublicKey::Rsa4096 {
            modulus,
            public_exponent,
            ..
        } => Some((modulus, *public_exponent)),
        PublicKey::Rsa2048 {
            modulus,
            public_exponent,
            ..
        } => Some((modulus, *public_exponent)),
        PublicKey::EllipticCurve { .. } => None,
    }
}

fn verify_rsa_signature(modulus: &[u8], exponent: u32, signature: &[u8], data: &[u8]) -> bool {
    let n = BigUint::from_bytes_be(modulus);
    let e = BigUint::from(exponent);

    let public_key = match RsaPublicKey::new(n, e) {
        Ok(k) => k,
        Err(_) => return false,
    };

    let verifying_key = VerifyingKey::<Sha256>::new(public_key);

    match rsa::pkcs1v15::Signature::try_from(signature) {
        Ok(sig) => verifying_key.verify(data, &sig).is_ok(),
        Err(_) => false,
    }
}

fn serialize_cert_body(cert: &Certificate) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);
    let _ = cert.issuer.write_options(&mut cursor, Endian::Big, ());
    let _ = cert.key_type.write_options(&mut cursor, Endian::Big, ());
    let _ = cert.name.write_options(&mut cursor, Endian::Big, ());
    let _ = cert
        .expiration_time
        .write_options(&mut cursor, Endian::Big, ());
    let _ = cert.public_key.write_options(&mut cursor, Endian::Big, ());
    buf
}

fn serialize_tmd_body(
    tmd: &crate::nintendo::ctr::models::title_metadata::TitleMetadata,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);
    let _ = tmd.header.write_options(&mut cursor, Endian::Big, ());
    for record in &tmd.content_info_records {
        let _ = record.write_options(&mut cursor, Endian::Big, ());
    }
    for record in &tmd.content_chunk_records {
        let _ = record.write_options(&mut cursor, Endian::Big, ());
    }
    buf
}

fn serialize_ticket_body(ticket: &crate::nintendo::ctr::models::ticket::Ticket) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);
    let _ = ticket
        .ticket_data
        .write_options(&mut cursor, Endian::Big, ());
    buf
}

/// Streaming content hash verify: seeks the file to each content chunk's offset
/// and computes SHA-256 incrementally with a reusable 4 MB buffer instead of
/// loading the entire CIA into memory.
async fn verify_content_hashes_streaming(
    file: &mut tokio::fs::File,
    content_start: u64,
    file_size: u64,
    tmd: &TitleMetadata,
    details: &mut Vec<String>,
) -> Result<bool> {
    const CHUNK_BUF: usize = 4 * 1024 * 1024;
    let mut buf = vec![0u8; CHUNK_BUF];
    let mut all_valid = true;
    let mut offset = content_start;

    for record in &tmd.content_chunk_records {
        let size = record.content_size;
        if offset + size > file_size {
            details.push(format!(
                "Content {}: data truncated (need {} bytes at {:#x}, file is {})",
                record.content_id,
                size,
                offset,
                file_size
            ));
            all_valid = false;
            break;
        }

        file.seek(SeekFrom::Start(offset)).await?;
        let mut hasher = Sha256::new();
        let mut remaining = size;
        while remaining > 0 {
            let to_read = remaining.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..to_read]).await?;
            hasher.update(&buf[..to_read]);
            remaining -= to_read as u64;
        }
        let hash = hasher.finalize();

        if hash.as_slice() == record.hash.as_slice() {
            details.push(format!("Content {}: hash OK", record.content_id));
        } else {
            details.push(format!("Content {}: hash MISMATCH", record.content_id));
            all_valid = false;
        }

        offset += size;
    }

    // Verify content info records hash chain (operates on TMD, no file I/O).
    let mut info_buf = Vec::new();
    let mut info_cursor = Cursor::new(&mut info_buf);
    for record in &tmd.content_info_records {
        let _ = record.write_options(&mut info_cursor, Endian::Big, ());
    }
    let info_hash = Sha256::digest(&info_buf);

    if info_hash.as_slice() == tmd.header.content_info_records_hash.as_slice() {
        details.push("Content info records hash: OK".to_string());
    } else {
        details.push("Content info records hash: MISMATCH".to_string());
        all_valid = false;
    }

    Ok(all_valid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::test_fixtures::{synth_cia, TestProgress};
    use std::io::Write as _;

    #[tokio::test]
    async fn verify_cia_streaming_parses_header_and_tmd() {
        let (_tmp, path, _) = synth_cia(0x2000);
        let progress = TestProgress::default();
        let result = verify_cia(
            &path,
            &CtrVerifyOptions {
                verify_content_hashes: false,
            },
            &progress,
        )
        .await
        .expect("verify should parse the synthetic CIA");

        assert_eq!(result.title_id, "0004000000030000");
        assert_eq!(result.title_version, 0x0100);
        assert_eq!(result.console_id, 0);
        // Content hash check wasn't requested.
        assert_eq!(result.content_hashes_valid, None);
        // Signatures are forged → all false, classification Standard.
        assert!(!result.tmd_signature_valid);
        assert!(!result.ticket_signature_valid);
    }

    #[tokio::test]
    async fn verify_cia_streaming_validates_content_hash() {
        let (_tmp, path, _) = synth_cia(0x4000);
        let progress = TestProgress::default();
        let result = verify_cia(
            &path,
            &CtrVerifyOptions {
                verify_content_hashes: true,
            },
            &progress,
        )
        .await
        .unwrap();

        assert_eq!(result.content_hashes_valid, Some(true));
    }

    #[tokio::test]
    async fn verify_compressed_streams_into_temp_without_full_buffer() {
        use crate::nintendo::ctr::z3ds::{compress_rom, decompress_rom};

        let (_tmp, cia_path, _) = synth_cia(0x3000);

        let zcia_path = cia_path.with_extension("zcia");
        let prog = crate::util::NoProgress;
        compress_rom(&cia_path, &zcia_path, &prog).await.unwrap();

        // Sanity: round-trip the compressed file back and compare to original.
        let decompressed_path = cia_path.with_extension("roundtrip.cia");
        decompress_rom(&zcia_path, &decompressed_path, &prog)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(&cia_path).unwrap(),
            std::fs::read(&decompressed_path).unwrap(),
            "decompress of compressed synthetic CIA must round-trip"
        );

        // Verify the compressed path through the streaming temp-file pipeline.
        let result = verify_ctr(
            &zcia_path,
            &CtrVerifyOptions {
                verify_content_hashes: true,
            },
            &prog,
        )
        .await
        .unwrap();

        match result {
            CtrVerifyResult::Cia(cia) => {
                assert_eq!(cia.title_id, "0004000000030000");
                assert_eq!(cia.content_hashes_valid, Some(true));
            }
            CtrVerifyResult::Ncsd(_) => panic!("expected Cia result"),
        }
    }

    #[tokio::test]
    async fn verify_cia_streaming_detects_content_hash_mismatch() {
        let (_tmp, path, _) = synth_cia(0x4000);

        // Corrupt one byte in the content region. Content starts somewhere after
        // the TMD; scan for the deterministic pattern and flip a byte there.
        // Using known layout: content_start = align_64(CIA_HEADER_SIZE +
        // cert_chain_size + ticket_size + tmd_size). For this fixture the
        // content is at the tail of the file.
        let len = std::fs::metadata(&path).unwrap().len();
        let corrupt_offset = len - 0x100; // inside the content region
        {
            use std::io::{Seek as _, SeekFrom};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap();
            f.seek(SeekFrom::Start(corrupt_offset)).unwrap();
            f.write_all(&[0xFFu8; 1]).unwrap();
        }

        let progress = TestProgress::default();
        let result = verify_cia(
            &path,
            &CtrVerifyOptions {
                verify_content_hashes: true,
            },
            &progress,
        )
        .await
        .unwrap();

        assert_eq!(result.content_hashes_valid, Some(false));
        assert!(
            result.details.iter().any(|s| s.contains("MISMATCH")),
            "details should report MISMATCH, got: {:?}",
            result.details
        );
    }
}
