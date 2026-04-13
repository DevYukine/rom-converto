use crate::nintendo::ctr::constants::{
    CERT_SIG_TYPE_MAX, CERT_SIG_TYPE_MIN, CIA_CERT_CHAIN_SIZE, CIA_CONTENT_INDEX_SIZE,
};
use crate::nintendo::ctr::decrypt::cia::parse_and_decrypt_cia;
use crate::nintendo::ctr::models::certificate::Certificate;
use crate::nintendo::ctr::models::cia::{
    CIA_HEADER_SIZE, CiaFile, CiaFileWithoutContent, CiaHeader,
};
use crate::nintendo::ctr::models::ticket::Ticket;
use crate::nintendo::ctr::models::title_metadata::TitleMetadata;
use crate::util::ProgressReporter;
use binrw::{BinRead, BinWrite, Endian};
use byteorder::{BigEndian, ReadBytesExt};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Seek, SeekFrom};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};

/// Buffer size for streaming content files from disk to the CIA output.
const CONTENT_COPY_BUF: usize = 4 * 1024 * 1024;

pub async fn decrypt_from_encrypted_cia(
    input: &Path,
    out_writer: &mut BufWriter<File>,
    progress: &dyn ProgressReporter,
) -> anyhow::Result<()> {
    let input_size = tokio::fs::metadata(input).await?.len();
    progress.start(input_size, "Decrypting CIA...");
    parse_and_decrypt_cia(input, None, progress).await?;
    progress.finish();

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

    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("input path has no valid filename stem"))?;

    for content_chunk_record in &mut decrypted_cia.tmd.content_chunk_records {
        content_chunk_record.content_type.set_encrypted(false);

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
        let mut cursor = Cursor::new(Vec::new());
        content_info_record
            .content_index_offset
            .write_be(&mut cursor)?;
        content_info_record
            .content_command_count
            .write_be(&mut cursor)?;
        cursor
            .get_mut()
            .extend_from_slice(&content_info_record.hash);

        hasher.update(cursor.get_ref());
    }

    decrypted_cia.tmd.header.content_info_records_hash = hasher.finalize().to_vec();

    let mut data = Cursor::new(Vec::new());

    decrypted_cia.write_le(&mut data)?;

    out_writer.write_all(data.get_ref()).await?;

    for content_chunk_record in decrypted_cia.tmd.content_chunk_records {
        let new_file_name = format!(
            "{stem}.{index}.{id:08x}.ncch",
            stem = stem,
            index = content_chunk_record.content_index,
            id = content_chunk_record.content_id
        );

        let file_path = parent.join(new_file_name);

        let content = tokio::fs::read(&file_path).await?;
        out_writer.write_all(&content).await?;

        tokio::fs::remove_file(&file_path).await?;
    }

    Ok(())
}

/// Writes out the CIA file by streaming content files from disk directly to
/// the output, avoiding the previous behavior of loading every `.app` into
/// memory and then serializing a full in-memory CIA. Peak memory is bounded by
/// the TMD/ticket preamble (a few KB) plus a 4 MB copy buffer.
pub async fn write_cia(
    path: &Path,
    out: &mut BufWriter<File>,
    tmd_path: &Path,
    tik_path: &Path,
    tmd: TitleMetadata,
    tik: Ticket,
    progress: &dyn ProgressReporter,
) -> anyhow::Result<()> {
    let total_content_size: u64 = tmd
        .content_chunk_records
        .iter()
        .map(|e| e.content_size)
        .sum();
    progress.start(total_content_size, "Building CIA...");

    let tmd_certs = read_certificate_chain(tmd_path).await?;
    let tik_certs = read_certificate_chain(tik_path).await?;
    let cert_chain: Vec<Certificate> = merge_certificate_chains(tmd_certs, tik_certs);

    // Ticket and TMD have variable BinWrite sizes. Serialize them to scratch
    // buffers so the CIA header declares lengths that match BinRead.
    let mut tmd_buf = Vec::new();
    tmd.write_options(&mut Cursor::new(&mut tmd_buf), Endian::Big, ())?;
    let tmd_size = tmd_buf.len() as u32;

    let mut tik_buf = Vec::new();
    tik.write_options(&mut Cursor::new(&mut tik_buf), Endian::Big, ())?;
    let ticket_size = tik_buf.len() as u32;

    // CiaFileWithoutContent::write_options emits header → cert → ticket →
    // TMD → padding up to content_start, which is the full preamble.
    let mut cia_wo = CiaFileWithoutContent {
        header: CiaHeader {
            header_size: CIA_HEADER_SIZE,
            cia_type: 0,
            version: 0,
            cert_chain_size: CIA_CERT_CHAIN_SIZE,
            ticket_size,
            tmd_size,
            meta_size: 0,
            content_size: total_content_size,
            content_index: vec![0u8; CIA_CONTENT_INDEX_SIZE],
        },
        cert_chain,
        ticket: tik,
        tmd,
    };
    for record in &cia_wo.tmd.content_chunk_records {
        cia_wo.header.set_content_index(record.content_index as usize);
    }

    let mut preamble = Vec::new();
    cia_wo.write_options(&mut Cursor::new(&mut preamble), Endian::Little, ())?;
    out.write_all(&preamble).await?;

    // Stream each content file directly to the output. The size check below
    // is what stops a truncated/corrupt .app from producing a CIA whose
    // header content_size disagrees with the bytes actually written.
    let mut buf = vec![0u8; CONTENT_COPY_BUF];
    for entry in &cia_wo.tmd.content_chunk_records {
        let content_file = format!("{:08x}", entry.content_id);
        let content_path = path.join(&content_file);

        let actual_size = tokio::fs::metadata(&content_path).await?.len();
        if actual_size != entry.content_size {
            anyhow::bail!(
                "content file {} size mismatch: TMD declares {} bytes but file is {} bytes",
                content_path.display(),
                entry.content_size,
                actual_size,
            );
        }

        let mut f = File::open(&content_path).await?;
        let mut written: u64 = 0;
        loop {
            let n = f.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).await?;
            progress.inc(n as u64);
            written += n as u64;
        }
        if written != entry.content_size {
            anyhow::bail!(
                "content file {} short read: expected {} bytes, got {}",
                content_path.display(),
                entry.content_size,
                written,
            );
        }
    }

    out.flush().await?;
    progress.finish();

    Ok(())
}

/// Reads a certificate chain from the end of a TMD or Ticket file
async fn read_certificate_chain(file_path: &Path) -> anyhow::Result<Vec<Certificate>> {
    let content = tokio::fs::read(file_path).await?;
    let mut cursor = Cursor::new(&content);

    // First, parse the main structure to find where certificates start
    let _ = {
        let start_pos = cursor.position();

        // The file starts with either a TMD or a Ticket; certificates follow.
        if let Ok(_tmd) = TitleMetadata::read_options(&mut cursor, Endian::Big, ()) {
            cursor.position()
        } else {
            cursor.seek(SeekFrom::Start(start_pos))?;
            if let Ok(_ticket) = Ticket::read_options(&mut cursor, Endian::Big, ()) {
                cursor.position()
            } else {
                return Err(anyhow::anyhow!("File is neither TMD nor Ticket"));
            }
        }
    };

    let mut certificates = Vec::new();

    while cursor.position() < content.len() as u64 {
        if content.len() as u64 - cursor.position() < 4 {
            break;
        }

        // Peek at the sig type so the loop stops cleanly when the trailing
        // region hits padding or unrelated data instead of a valid cert.
        let pos = cursor.position();
        let sig_type_bytes = match ReadBytesExt::read_u32::<BigEndian>(&mut cursor) {
            Ok(val) => val,
            Err(_) => break,
        };
        cursor.seek(SeekFrom::Start(pos))?;

        if !matches!(sig_type_bytes, CERT_SIG_TYPE_MIN..=CERT_SIG_TYPE_MAX) {
            break;
        }

        match Certificate::read_options(&mut cursor, Endian::Big, ()) {
            Ok(cert) => certificates.push(cert),
            Err(_) => break,
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

    fn get_cert_name(cert: &Certificate) -> String {
        String::from_utf8_lossy(&cert.name)
            .trim_end_matches('\0')
            .to_string()
    }

    // CIA cert chain order is CA → XS → CP. CA may live in either source.
    for cert in tmd_certs.iter().chain(tik_certs.iter()) {
        let name = get_cert_name(cert);
        if name.starts_with("CA") && !seen_names.contains(&name) {
            seen_names.insert(name.clone());
            merged.push(cert.clone());
            break;
        }
    }

    for cert in tik_certs.iter() {
        let name = get_cert_name(cert);
        if name.starts_with("XS") && !seen_names.contains(&name) {
            seen_names.insert(name.clone());
            merged.push(cert.clone());
            break;
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::models::cia::CiaFile;
    use crate::nintendo::ctr::test_fixtures::{append_be, make_cert, make_tmd, make_ticket};
    use crate::util::NoProgress;

    #[test]
    fn tmd_with_trailing_certs_parses_as_tmd() {
        let tmd = make_tmd(
            0x0004000000030000,
            vec![(0, 0, vec![0u8; 16], [0u8; 32])],
        );
        let mut buf = Vec::new();
        append_be(&mut buf, &tmd);
        append_be(&mut buf, &make_cert(b"CP0000000b", 0xBB));
        append_be(&mut buf, &make_cert(b"CA00000003", 0xAA));

        let mut cursor = Cursor::new(&buf);
        let tmd_read = TitleMetadata::read_options(&mut cursor, Endian::Big, ())
            .expect("TMD should parse");
        assert_eq!(tmd_read.header.content_count, 1);
    }

    #[tokio::test]
    async fn write_cia_streams_content_and_parses_back() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn = tmp.path();

        let make_content = |seed: u8, len: usize| -> Vec<u8> {
            (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
        };

        let content_a = make_content(0x11, 0x1800);
        let content_b = make_content(0x77, 0x900);

        let hash_of = |data: &[u8]| -> [u8; 32] {
            let mut h = Sha256::new();
            h.update(data);
            let d = h.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&d);
            arr
        };
        let hash_a = hash_of(&content_a);
        let hash_b = hash_of(&content_b);

        // Content files named by content_id (lowercase hex, 8 chars).
        std::fs::write(cdn.join("00000000"), &content_a).unwrap();
        std::fs::write(cdn.join("00000001"), &content_b).unwrap();

        let title_id = 0x0004000000030000u64;
        let tmd = make_tmd(
            title_id,
            vec![
                (0, 0, content_a.clone(), hash_a),
                (1, 1, content_b.clone(), hash_b),
            ],
        );
        let ticket = make_ticket(title_id);

        // Write TMD file: serialized TMD + trailing CP + CA cert.
        let tmd_path = cdn.join("tmd");
        {
            let mut buf = Vec::new();
            append_be(&mut buf, &tmd);
            append_be(&mut buf, &make_cert(b"CP0000000b", 0xBB));
            append_be(&mut buf, &make_cert(b"CA00000003", 0xAA));
            std::fs::write(&tmd_path, &buf).unwrap();
        }

        // Write Ticket file: serialized Ticket + trailing XS cert.
        let tik_path = cdn.join("cetk");
        {
            let mut buf = Vec::new();
            append_be(&mut buf, &ticket);
            append_be(&mut buf, &make_cert(b"XS0000000c", 0xCC));
            std::fs::write(&tik_path, &buf).unwrap();
        }

        let out_path = cdn.join("out.cia");
        {
            let f = File::create(&out_path).await.unwrap();
            let mut out = BufWriter::new(f);
            write_cia(
                cdn,
                &mut out,
                &tmd_path,
                &tik_path,
                tmd.clone(),
                ticket,
                &NoProgress,
            )
            .await
            .unwrap();
            out.flush().await.unwrap();
        }

        // Parse the result with the normal CIA reader and assert layout.
        let bytes = std::fs::read(&out_path).unwrap();
        let cia = CiaFile::read_options(&mut Cursor::new(&bytes), Endian::Little, ())
            .expect("streamed CIA must round-trip via BinRead");

        assert_eq!(cia.tmd.header.title_id, title_id);
        assert_eq!(cia.tmd.content_chunk_records.len(), 2);
        assert_eq!(cia.header.content_size as usize, content_a.len() + content_b.len());
        assert_eq!(&cia.content_data[..content_a.len()], content_a.as_slice());
        assert_eq!(&cia.content_data[content_a.len()..], content_b.as_slice());
        // Cert chain should be non-empty; exact count depends on
        // merge_certificate_chains ordering and BinRead's cert-chain parse.
        assert!(!cia.cert_chain.is_empty());
    }

    #[tokio::test]
    async fn write_cia_verifies_via_streaming_verify() {
        // End-to-end: write_cia → verify_cia (streaming content hashes).
        let tmp = tempfile::tempdir().unwrap();
        let cdn = tmp.path();

        let content_a: Vec<u8> = (0u16..0x2000).map(|i| (i as u8).wrapping_mul(7)).collect();
        let hash_a = {
            let mut h = Sha256::new();
            h.update(&content_a);
            let d = h.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&d);
            arr
        };
        std::fs::write(cdn.join("00000000"), &content_a).unwrap();

        let title_id = 0x0004000000110000u64;
        let tmd = make_tmd(title_id, vec![(0, 0, content_a.clone(), hash_a)]);
        let ticket = make_ticket(title_id);

        let tmd_path = cdn.join("tmd");
        {
            let mut buf = Vec::new();
            append_be(&mut buf, &tmd);
            append_be(&mut buf, &make_cert(b"CP0000000b", 0xBB));
            append_be(&mut buf, &make_cert(b"CA00000003", 0xAA));
            std::fs::write(&tmd_path, &buf).unwrap();
        }
        let tik_path = cdn.join("cetk");
        {
            let mut buf = Vec::new();
            append_be(&mut buf, &ticket);
            append_be(&mut buf, &make_cert(b"XS0000000c", 0xCC));
            std::fs::write(&tik_path, &buf).unwrap();
        }

        let out_path = cdn.join("streamed.cia");
        {
            let f = File::create(&out_path).await.unwrap();
            let mut out = BufWriter::new(f);
            write_cia(
                cdn,
                &mut out,
                &tmd_path,
                &tik_path,
                tmd,
                ticket,
                &NoProgress,
            )
            .await
            .unwrap();
            out.flush().await.unwrap();
        }

        use crate::nintendo::ctr::verify::{verify_cia, CtrVerifyOptions};
        let result = verify_cia(
            &out_path,
            &CtrVerifyOptions {
                verify_content_hashes: true,
            },
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(result.title_id, format!("{title_id:016X}"));
        assert_eq!(result.content_hashes_valid, Some(true));
    }

    #[tokio::test]
    async fn write_cia_rejects_truncated_content_file() {
        // Defensive boundary check: if a content file on disk is shorter than
        // what the TMD declares, write_cia must surface an error rather than
        // produce a corrupt CIA whose header content_size disagrees with the
        // bytes actually written.
        let tmp = tempfile::tempdir().unwrap();
        let cdn = tmp.path();

        let declared: Vec<u8> = (0..0x1000u16).map(|i| (i as u8).wrapping_mul(11)).collect();
        let hash = {
            let mut h = Sha256::new();
            h.update(&declared);
            let d = h.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&d);
            arr
        };
        // Write only HALF the declared bytes to disk.
        std::fs::write(cdn.join("00000000"), &declared[..0x800]).unwrap();

        let title_id = 0x0004000000220000u64;
        let tmd = make_tmd(title_id, vec![(0, 0, declared, hash)]);
        let ticket = make_ticket(title_id);

        let tmd_path = cdn.join("tmd");
        {
            let mut buf = Vec::new();
            append_be(&mut buf, &tmd);
            append_be(&mut buf, &make_cert(b"CP0000000b", 0xBB));
            append_be(&mut buf, &make_cert(b"CA00000003", 0xAA));
            std::fs::write(&tmd_path, &buf).unwrap();
        }
        let tik_path = cdn.join("cetk");
        {
            let mut buf = Vec::new();
            append_be(&mut buf, &ticket);
            append_be(&mut buf, &make_cert(b"XS0000000c", 0xCC));
            std::fs::write(&tik_path, &buf).unwrap();
        }

        let out_path = cdn.join("truncated.cia");
        let f = File::create(&out_path).await.unwrap();
        let mut out = BufWriter::new(f);
        let err = write_cia(
            cdn,
            &mut out,
            &tmd_path,
            &tik_path,
            tmd,
            ticket,
            &NoProgress,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("size mismatch"),
            "expected size-mismatch error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn write_cia_rejects_oversized_content_file() {
        // Same defensive check, opposite direction: a file that's longer than
        // the TMD declares must also fail loudly.
        let tmp = tempfile::tempdir().unwrap();
        let cdn = tmp.path();

        let declared: Vec<u8> = (0..0x800u16).map(|i| (i as u8).wrapping_mul(13)).collect();
        let hash = {
            let mut h = Sha256::new();
            h.update(&declared);
            let d = h.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&d);
            arr
        };
        // Write 0x1000 bytes when TMD declares 0x800.
        let on_disk: Vec<u8> = (0..0x1000u16).map(|i| i as u8).collect();
        std::fs::write(cdn.join("00000000"), &on_disk).unwrap();

        let title_id = 0x0004000000330000u64;
        let tmd = make_tmd(title_id, vec![(0, 0, declared, hash)]);
        let ticket = make_ticket(title_id);

        let tmd_path = cdn.join("tmd");
        {
            let mut buf = Vec::new();
            append_be(&mut buf, &tmd);
            append_be(&mut buf, &make_cert(b"CP0000000b", 0xBB));
            append_be(&mut buf, &make_cert(b"CA00000003", 0xAA));
            std::fs::write(&tmd_path, &buf).unwrap();
        }
        let tik_path = cdn.join("cetk");
        {
            let mut buf = Vec::new();
            append_be(&mut buf, &ticket);
            append_be(&mut buf, &make_cert(b"XS0000000c", 0xCC));
            std::fs::write(&tik_path, &buf).unwrap();
        }

        let out_path = cdn.join("oversized.cia");
        let f = File::create(&out_path).await.unwrap();
        let mut out = BufWriter::new(f);
        let err = write_cia(
            cdn,
            &mut out,
            &tmd_path,
            &tik_path,
            tmd,
            ticket,
            &NoProgress,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("size mismatch"),
            "expected size-mismatch error, got: {err}"
        );
    }
}
