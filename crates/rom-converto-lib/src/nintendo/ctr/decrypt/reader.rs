use crate::nintendo::ctr::decrypt::util::{cbc_decrypt, gen_iv};
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
    pub cidx: u16,
    iv: [u8; 16],
    contentoff: u64,
    pub single_ncch: bool,
    pub from_ncsd: bool,
}

/// Inputs for [`CiaReader::new`].
pub struct CiaReaderArgs {
    pub file: File,
    pub encrypted: bool,
    pub path: PathBuf,
    pub key: [u8; 16],
    pub cidx: u16,
    pub contentoff: u64,
    pub single_ncch: bool,
    pub from_ncsd: bool,
}

impl CiaReader {
    pub fn new(args: CiaReaderArgs) -> CiaReader {
        let CiaReaderArgs {
            file,
            encrypted,
            path,
            key,
            cidx,
            contentoff,
            single_ncch,
            from_ncsd,
        } = args;
        CiaReader {
            file,
            encrypted,
            path,
            key,
            cidx,
            iv: gen_iv(cidx),
            contentoff,
            single_ncch,
            from_ncsd,
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
            if data.is_empty() || !data.len().is_multiple_of(16) {
                anyhow::bail!(
                    "CIA content read of {} bytes is not a whole number of AES blocks",
                    data.len()
                );
            }
            let mut next_iv = [0u8; 16];
            next_iv.copy_from_slice(&data[data.len() - 16..]);
            cbc_decrypt(&self.key, &self.iv, data)?;
            self.iv = next_iv;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::Aes128;
    use block_padding::NoPadding;
    use cbc::cipher::{BlockModeEncrypt, KeyIvInit};

    #[tokio::test]
    async fn encrypted_reads_chain_blocks_and_reject_partial_blocks() {
        let key = [0x5A; 16];
        let cidx = 0x0301;
        let plaintext: Vec<u8> = (0..16 + 0x200 + 32).map(|i| (i % 251) as u8).collect();
        let mut ciphertext = plaintext.clone();
        ciphertext.resize(plaintext.len() + 16, 0);
        let ciphertext = cbc::Encryptor::<Aes128>::new_from_slices(&key, &gen_iv(cidx))
            .unwrap()
            .encrypt_padded::<NoPadding>(&mut ciphertext, plaintext.len())
            .unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("content.bin");
        std::fs::write(&path, ciphertext).unwrap();
        let mut reader = CiaReader::new(CiaReaderArgs {
            file: File::open(&path).await.unwrap(),
            encrypted: true,
            path,
            key,
            cidx,
            contentoff: 0,
            single_ncch: false,
            from_ncsd: false,
        });

        reader.seek(0).await.unwrap();
        let mut one_shot = vec![0; plaintext.len()];
        reader.read(&mut one_shot).await.unwrap();
        assert_eq!(one_shot, plaintext);

        reader.seek(0).await.unwrap();
        let mut split = vec![0; plaintext.len()];
        let mut offset = 0;
        for size in [16, 0x200, 32] {
            reader
                .read(&mut split[offset..offset + size])
                .await
                .unwrap();
            offset += size;
        }
        assert_eq!(split, one_shot);

        reader.seek(0).await.unwrap();
        assert!(reader.read(&mut [0; 8]).await.is_err());
    }
}
