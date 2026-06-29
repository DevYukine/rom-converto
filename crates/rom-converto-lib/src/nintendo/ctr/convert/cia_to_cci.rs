use crate::nintendo::ctr::constants::{CTR_COMMON_KEYS_HEX, CTR_MEDIA_UNIT_SIZE};
use crate::nintendo::ctr::decrypt::util::{cbc_decrypt, gen_iv};
use crate::nintendo::ctr::models::cia::CiaFileWithoutContent;
use crate::nintendo::ctr::models::ncsd_header::{
    NCSD_FIRST_PARTITION_OFFSET, NCSD_HEADER_SIZE, NCSD_PARTITION_FS_TYPE_NORMAL, NcsdHeader,
    NcsdPartitionEntry,
};
use crate::nintendo::ctr::models::ticket::Ticket;
use crate::nintendo::ctr::util::align_64;
use crate::nintendo::ctr::error::NintendoCTRError;
use crate::util::{CancelToken, ProgressReporter, scratch_output_path};
use anyhow::{Context, Result, bail};
use binrw::{BinRead, BinWrite};
use log::{info, warn};
use std::io::{Cursor, SeekFrom};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};

const PREAMBLE_READ_LIMIT: usize = 256 * 1024;
const COPY_BUF: usize = 4 * 1024 * 1024;
const CARD1_MIN_IMAGE_SIZE: u64 = 0x8000000;
const NCSD_PADDING_BYTE: u8 = 0xFF;

pub async fn cia_to_cci(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Result<()> {
    cia_to_cci_cancellable(input, output, progress, CancelToken::new()).await
}

pub async fn cia_to_cci_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    let mut in_file = File::open(input).await.context("opening CIA input")?;
    let file_size = in_file.metadata().await?.len();

    let preamble_len = file_size.min(PREAMBLE_READ_LIMIT as u64) as usize;
    let mut preamble_buf = vec![0u8; preamble_len];
    in_file.read_exact(&mut preamble_buf).await?;

    let mut cur = Cursor::new(&preamble_buf);
    let pre = CiaFileWithoutContent::read_le(&mut cur).context("parsing CIA header/ticket/TMD")?;
    let content_start = align_64(cur.position());

    let title_key = derive_title_key(&pre.ticket).context("deriving title key")?;

    let mut partitions: Vec<PartitionLayout> = Vec::new();
    let mut byte_offset_into_content: u64 = 0;
    for chunk in &pre.tmd.content_chunk_records {
        let idx = chunk.content_index;
        if idx < 3 {
            partitions.push(PartitionLayout {
                content_index: idx,
                ncch_size: chunk.content_size,
                cia_offset: content_start + byte_offset_into_content,
                encrypted: chunk.content_type.is_encrypted(),
            });
        } else {
            warn!("Dropping content index {idx} (only indices 0/1/2 fit into NCSD)");
        }
        byte_offset_into_content += chunk.content_size;
    }

    if !partitions.iter().any(|p| p.content_index == 0) {
        bail!("CIA has no content index 0; cannot synthesize executable NCSD partition");
    }

    let media_unit = CTR_MEDIA_UNIT_SIZE as u64;
    let mut placements: Vec<PartitionPlacement> = Vec::new();
    let mut cur_pos = NCSD_FIRST_PARTITION_OFFSET;
    for p in &partitions {
        let aligned = align_to(p.ncch_size, media_unit);
        placements.push(PartitionPlacement {
            ncsd_offset: cur_pos,
            ncsd_size: aligned,
            layout: p.clone(),
        });
        cur_pos += aligned;
    }
    let used_size = cur_pos;
    let image_size = next_pow2_at_least(used_size, CARD1_MIN_IMAGE_SIZE);

    let mut ncsd = NcsdHeader::blank();
    ncsd.media_id = pre.tmd.header.title_id;
    ncsd.image_size = (image_size / media_unit) as u32;
    for pl in &placements {
        let i = pl.layout.content_index as usize;
        ncsd.partition_fs_types[i] = NCSD_PARTITION_FS_TYPE_NORMAL;
        ncsd.partition_table[i] = NcsdPartitionEntry {
            offset: (pl.ncsd_offset / media_unit) as u32,
            size: (pl.ncsd_size / media_unit) as u32,
        };
        ncsd.partition_id_table[i] = pre.tmd.header.title_id;
    }

    let mut header_buf = vec![0u8; NCSD_HEADER_SIZE];
    ncsd.write(&mut Cursor::new(&mut header_buf))?;

    let total_content_bytes: u64 = placements.iter().map(|p| p.layout.ncch_size).sum();
    progress.start(total_content_bytes, "Converting CIA to CCI...");

    let tmp = scratch_output_path(output);
    let out = File::create(&tmp).await.context("creating CCI output")?;
    let mut out = BufWriter::new(out);

    let stream = async {
        out.write_all(&header_buf).await?;
        pad_with(
            &mut out,
            NCSD_FIRST_PARTITION_OFFSET - NCSD_HEADER_SIZE as u64,
            0x00,
        )
        .await?;

        let mut buf = vec![0u8; COPY_BUF];
        for pl in &placements {
            in_file.seek(SeekFrom::Start(pl.layout.cia_offset)).await?;

            let mut cbc_iv = gen_iv(pl.layout.content_index);
            let mut remaining = pl.layout.ncch_size;
            while remaining > 0 {
                if cancel.is_cancelled() {
                    return Err(NintendoCTRError::Cancelled.into());
                }
                let to_read = remaining.min(buf.len() as u64) as usize;
                in_file.read_exact(&mut buf[..to_read]).await?;
                if pl.layout.encrypted {
                    if to_read < 16 {
                        bail!(
                            "encrypted content {} produced a sub-block read of {} bytes",
                            pl.layout.content_index,
                            to_read
                        );
                    }
                    let next_iv: [u8; 16] =
                        buf[to_read - 16..to_read].try_into().expect("16 bytes");
                    cbc_decrypt(&title_key, &cbc_iv, &mut buf[..to_read])?;
                    cbc_iv = next_iv;
                }
                out.write_all(&buf[..to_read]).await?;
                progress.inc(to_read as u64);
                remaining -= to_read as u64;
            }

            let pad = pl.ncsd_size - pl.layout.ncch_size;
            if pad > 0 {
                pad_with(&mut out, pad, NCSD_PADDING_BYTE).await?;
            }
        }

        if image_size > used_size {
            pad_with(&mut out, image_size - used_size, NCSD_PADDING_BYTE).await?;
        }

        out.flush().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(err) = stream {
        drop(out);
        tokio::fs::remove_file(&tmp).await.ok();
        return Err(err);
    }

    drop(out);
    tokio::fs::rename(&tmp, output).await?;
    progress.finish();

    info!(
        "Converted CIA to CCI: {} -> {}",
        input.display(),
        output.display()
    );
    Ok(())
}

#[derive(Debug, Clone)]
struct PartitionLayout {
    content_index: u16,
    ncch_size: u64,
    cia_offset: u64,
    encrypted: bool,
}

#[derive(Debug, Clone)]
struct PartitionPlacement {
    ncsd_offset: u64,
    ncsd_size: u64,
    layout: PartitionLayout,
}

fn derive_title_key(ticket: &Ticket) -> Result<[u8; 16]> {
    let td = &ticket.ticket_data;
    if td.title_key.len() != 16 {
        bail!("ticket has wrong title_key length: {}", td.title_key.len());
    }
    let idx = td.common_key_index as usize;
    let common = CTR_COMMON_KEYS_HEX
        .get(idx)
        .ok_or_else(|| anyhow::anyhow!("ticket common_key_index {idx} out of range"))?;
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(&td.title_id.to_be_bytes());
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&td.title_key);
    cbc_decrypt(common, &iv, &mut buf)?;
    Ok(buf)
}

fn align_to(n: u64, align: u64) -> u64 {
    n.div_ceil(align) * align
}

fn next_pow2_at_least(used: u64, min: u64) -> u64 {
    let target = used.max(min);
    target.next_power_of_two()
}

async fn pad_with(out: &mut BufWriter<File>, mut count: u64, byte: u8) -> Result<()> {
    let chunk = [byte; 4096];
    while count > 0 {
        let n = (count as usize).min(chunk.len());
        out.write_all(&chunk[..n]).await?;
        count -= n as u64;
    }
    Ok(())
}
