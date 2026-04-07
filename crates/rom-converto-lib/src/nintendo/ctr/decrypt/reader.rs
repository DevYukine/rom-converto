use crate::nintendo::ctr::decrypt::util::{cbc_decrypt, gen_iv};
use byteorder::{BigEndian, ByteOrder};
use std::io::SeekFrom;
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

#[derive(Debug)]
pub struct CiaReader {
    pub file: File,
    encrypted: bool,
    pub path: PathBuf,
    pub key: [u8; 16],
    pub content_id: u32,
    pub cidx: u16,
    iv: [u8; 16],
    contentoff: u64,
    pub single_ncch: bool,
    pub from_ncsd: bool,
    last_enc_block: u128,
}

#[allow(clippy::too_many_arguments)]
impl CiaReader {
    pub fn new(
        file: File,
        encrypted: bool,
        path: PathBuf,
        key: [u8; 16],
        content_id: u32,
        cidx: u16,
        contentoff: u64,
        single_ncch: bool,
        from_ncsd: bool,
    ) -> CiaReader {
        CiaReader {
            file,
            encrypted,
            path,
            key,
            content_id,
            cidx,
            iv: gen_iv(cidx),
            contentoff,
            single_ncch,
            from_ncsd,
            last_enc_block: 0,
        }
    }

    pub async fn seek(&mut self, offs: u64) -> anyhow::Result<()> {
        if self.single_ncch || self.from_ncsd {
            self.file.seek(SeekFrom::Start(offs)).await?;
        } else if offs == 0 {
            self.file.seek(SeekFrom::Start(self.contentoff)).await?;
            self.iv = gen_iv(self.cidx);
        } else {
            self.file
                .seek(SeekFrom::Start(self.contentoff + offs - 16))
                .await?;
            self.file.read_exact(&mut self.iv).await?;
        }

        Ok(())
    }

    pub async fn read(&mut self, data: &mut [u8]) -> anyhow::Result<()> {
        self.file.read_exact(data).await?;

        if self.encrypted {
            let last_enc_block = BigEndian::read_u128(&data[(data.len() - 16)..]);
            cbc_decrypt(&self.key, &self.iv, data)?;
            let first_dec_block = BigEndian::read_u128(&data[0..16]);

            // XOR the last encrypted block with the first decrypted block
            BigEndian::write_u128(&mut data[0..16], first_dec_block ^ self.last_enc_block);

            self.last_enc_block = last_enc_block;
        }

        Ok(())
    }
}
