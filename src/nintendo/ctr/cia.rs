use crate::nintendo::ctr::decrypt::cia::parse_and_decrypt_cia;
use crate::nintendo::ctr::models::certificate::Certificate;
use crate::nintendo::ctr::models::cia::{
    CIA_HEADER_SIZE, CiaFile, CiaFileWithoutContent, CiaHeader,
};
use crate::nintendo::ctr::models::ticket::Ticket;
use crate::nintendo::ctr::models::title_metadata::TitleMetadata;
use binrw::{BinRead, BinWrite, Endian};
use byteorder::{BigEndian, ReadBytesExt};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Seek, SeekFrom};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};

pub async fn decrypt_from_encrypted_cia(
    input: &Path,
    out_writer: &mut BufWriter<File>,
) -> anyhow::Result<()> {
    // 1) Decrypt NCCH files inside the CIA
    parse_and_decrypt_cia(input, None).await?;

    // 2) Read original cia without content
    let data = tokio::fs::read(input).await?;
    let original_cia = CiaFileWithoutContent::read_le(&mut Cursor::new(data))?;

    let mut decrypted_cia = CiaFile {
        header: original_cia.header,
        cert_chain: original_cia.cert_chain,
        ticket: original_cia.ticket,
        tmd: original_cia.tmd,
        content_data: vec![],
        meta_data: None,
    };

    // 3) Update Hashes and set content_type to unencrypted

    for content_chunk_record in &mut decrypted_cia.tmd.content_chunk_records {
        content_chunk_record.content_type.set_encrypted(false);

        let parent = input.parent().unwrap_or_else(|| Path::new("."));
        let stem = input.file_stem().unwrap().to_str().unwrap();

        let new_file_name = format!(
            "{stem}.{index}.{id:08x}.ncch",
            stem = stem,
            index = content_chunk_record.content_index,
            id = content_chunk_record.content_id
        );

        let file_path = parent.join(new_file_name);

        let data = tokio::fs::read(&file_path).await?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        content_chunk_record.hash = hasher.finalize().to_vec();
    }

    for content_info_record in &mut decrypted_cia.tmd.content_info_records {
        let start = content_info_record.content_index_offset as usize;
        let count = content_info_record.content_command_count as usize;
        let mut hasher = Sha256::new();

        for chunk in &decrypted_cia.tmd.content_chunk_records[start..start + count] {
            let mut buf = Cursor::new(Vec::new());
            chunk.write_be(&mut buf)?;
            hasher.update(buf.get_ref());
        }

        content_info_record.hash = hasher.finalize().to_vec();
    }

    let mut hasher = Sha256::new();

    for content_info_record in &mut decrypted_cia.tmd.content_info_records {
        // Serialize each ContentInfoRecord as big-endian
        let mut cursor = Cursor::new(Vec::new());
        // write offset and count
        content_info_record
            .content_index_offset
            .write_be(&mut cursor)?;
        content_info_record
            .content_command_count
            .write_be(&mut cursor)?;
        // then raw hash bytes
        cursor.get_mut().extend(content_info_record.hash.clone());

        hasher.update(cursor.get_ref());
    }

    decrypted_cia.tmd.header.content_info_records_hash = hasher.finalize().to_vec();

    // 4) Write the decrypted CIA file
    let mut data = Cursor::new(Vec::new());

    decrypted_cia.write_le(&mut data)?;

    out_writer.write_all(data.get_ref()).await?;

    for content_chunk_record in decrypted_cia.tmd.content_chunk_records {
        let parent = input.parent().unwrap_or_else(|| Path::new("."));
        let stem = input.file_stem().unwrap().to_str().unwrap();

        let new_file_name = format!(
            "{stem}.{index}.{id:08x}.ncch",
            stem = stem,
            index = content_chunk_record.content_index,
            id = content_chunk_record.content_id
        );

        let file_path = parent.join(new_file_name);

        let content = tokio::fs::read(&file_path).await?;
        out_writer.write_all(&content).await?;

        // Clean up the temporary file
        tokio::fs::remove_file(&file_path).await?;
    }

    Ok(())
}

/// Writes out the CIA file
pub async fn write_cia(
    path: &Path,
    out: &mut BufWriter<File>,
    tmd_path: &Path,
    tik_path: &Path,
    tmd: TitleMetadata,
    tik: Ticket,
) -> anyhow::Result<()> {
    // Read all content files
    let mut content = vec![];
    for entry in &tmd.content_chunk_records {
        let content_file = format!("{:08x}", entry.content_id);

        let content_path = path.join(&content_file);
        let mut content_file = File::open(content_path).await?;
        let mut bytes = Vec::new();
        content_file.read_to_end(&mut bytes).await?;
        content.extend_from_slice(&bytes);
    }

    // Extract certificate chains from TMD and Ticket files
    let mut cert_chain = Vec::new();

    // Read certificate chain from TMD file
    let tmd_certs = read_certificate_chain(tmd_path).await?;

    // Read certificate chain from Ticket file
    let tik_certs = read_certificate_chain(tik_path).await?;

    cert_chain.extend(merge_certificate_chains(tmd_certs, tik_certs));

    // Calculate sizes
    let mut tmd_buf = Vec::new();
    tmd.write_options(&mut Cursor::new(&mut tmd_buf), Endian::Big, ())?;
    let tmd_size = tmd_buf.len() as u32;

    let mut tik_buf = Vec::new();
    tik.write_options(&mut Cursor::new(&mut tik_buf), Endian::Big, ())?;
    let ticket_size = tik_buf.len() as u32;

    const CERT_CHAIN_SIZE: u32 = 2560u32;

    // Create the CIA structure
    let mut cia = CiaFile {
        header: CiaHeader {
            header_size: CIA_HEADER_SIZE,
            cia_type: 0, // 0 = Normal
            version: 0,  // CIA format version
            cert_chain_size: CERT_CHAIN_SIZE,
            ticket_size,
            tmd_size,
            meta_size: 0, // No metadata
            content_size: content.len() as u64,
            content_index: vec![0u8; 0x2000],
        },
        cert_chain,
        ticket: tik,
        tmd,
        content_data: content,
        meta_data: None,
    };

    cia.apply_content_indexes();

    // Write the CIA file
    let mut cia_buf = Vec::new();
    cia.write_options(&mut Cursor::new(&mut cia_buf), Endian::Little, ())?;

    // Write to output
    out.write_all(&cia_buf).await?;
    out.flush().await?;

    Ok(())
}

/// Reads certificate chain from the end of a TMD or Ticket file
async fn read_certificate_chain(file_path: &Path) -> anyhow::Result<Vec<Certificate>> {
    let content = tokio::fs::read(file_path).await?;
    let mut cursor = Cursor::new(&content);

    // First, parse the main structure to find where certificates start
    let _ = {
        let start_pos = cursor.position();

        // Try to read as TMD first
        if let Ok(_tmd) = TitleMetadata::read_options(&mut cursor, Endian::Big, ()) {
            cursor.position()
        } else {
            // Reset and try as Ticket
            cursor.seek(SeekFrom::Start(start_pos))?;
            if let Ok(_ticket) = Ticket::read_options(&mut cursor, Endian::Big, ()) {
                cursor.position()
            } else {
                return Err(anyhow::anyhow!("File is neither TMD nor Ticket"));
            }
        }
    };

    let mut certificates = Vec::new();

    // Read all certificates until EOF or invalid data
    while cursor.position() < content.len() as u64 {
        // Check if there's enough data for at least a signature type
        if content.len() as u64 - cursor.position() < 4 {
            break;
        }

        // Peek at signature type
        let pos = cursor.position();
        let sig_type_bytes = match ReadBytesExt::read_u32::<BigEndian>(&mut cursor) {
            Ok(val) => val,
            Err(_) => break,
        };
        cursor.seek(SeekFrom::Start(pos))?;

        // Check if it's a valid certificate signature type
        if !matches!(sig_type_bytes, 0x010000..=0x010005) {
            break;
        }

        // Try to read the certificate
        match Certificate::read_options(&mut cursor, Endian::Big, ()) {
            Ok(cert) => {
                certificates.push(cert);
            }
            Err(_) => {
                break;
            }
        }
    }

    Ok(certificates)
}

/// Merges certificate chains from TMD and Ticket, avoiding duplicates
fn merge_certificate_chains(
    tmd_certs: Vec<Certificate>,
    tik_certs: Vec<Certificate>,
) -> Vec<Certificate> {
    let mut merged = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    // Helper function to get certificate name as string
    fn get_cert_name(cert: &Certificate) -> String {
        String::from_utf8_lossy(&cert.name)
            .trim_end_matches('\0')
            .to_string()
    }

    // First, find and add the CA certificate (should be the same in both)
    for cert in tmd_certs.iter().chain(tik_certs.iter()) {
        let name = get_cert_name(cert);
        if name.starts_with("CA") && !seen_names.contains(&name) {
            seen_names.insert(name.clone());
            merged.push(cert.clone());
            break;
        }
    }

    // Then add the Ticket certificate (XS)
    for cert in tik_certs.iter() {
        let name = get_cert_name(cert);
        if name.starts_with("XS") && !seen_names.contains(&name) {
            seen_names.insert(name.clone());
            merged.push(cert.clone());
            break;
        }
    }

    // Then add the TMD certificate (CP)
    for cert in tmd_certs.iter() {
        let name = get_cert_name(cert);
        if name.starts_with("CP") && !seen_names.contains(&name) {
            seen_names.insert(name.clone());
            merged.push(cert.clone());
            break;
        }
    }

    merged
}
