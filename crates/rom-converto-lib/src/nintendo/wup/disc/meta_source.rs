//! [`MetaSource`] over one decrypted Wii U disc partition.
//!
//! Lets the source-agnostic `read_loadiine` path in [`super::super::info`]
//! pull `code/app.xml`, `meta/meta.xml`, and `meta/iconTex.tga` straight off a
//! WUD/WUX disc without first extracting the whole title. Decrypted clusters
//! are cached so repeated meta reads from the same cluster skip the AES work.

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use crate::nintendo::wup::disc::partition::PartitionContentSource;
use crate::nintendo::wup::meta_source::MetaSource;
use crate::nintendo::wup::models::WupTmd;
use crate::nintendo::wup::nus::content_stream::{
    ContentBytesSource, decrypt_hashed_content, decrypt_raw_content,
};
use crate::nintendo::wup::nus::fst_parser::{FstClusterHashMode, VirtualFs};
use crate::nintendo::wup::nus::ticket_parser::TitleKey;

pub struct DiscMetaSource<'d> {
    source: PartitionContentSource<'d>,
    title_key: TitleKey,
    tmd: WupTmd,
    fs: VirtualFs,
    cache: HashMap<u16, Vec<u8>>,
}

impl<'d> DiscMetaSource<'d> {
    pub fn new(
        source: PartitionContentSource<'d>,
        title_key: TitleKey,
        tmd: WupTmd,
        fs: VirtualFs,
    ) -> Self {
        Self {
            source,
            title_key,
            tmd,
            fs,
            cache: HashMap::new(),
        }
    }

    fn decrypted_cluster(&mut self, cluster_index: u16) -> Result<&[u8]> {
        if !self.cache.contains_key(&cluster_index) {
            let hash_mode = self
                .fs
                .clusters
                .get(cluster_index as usize)
                .ok_or_else(|| anyhow!("disc meta: cluster {cluster_index} missing from FST"))?
                .hash_mode;
            let content_id = self
                .tmd
                .content_by_index(cluster_index)
                .ok_or_else(|| anyhow!("disc meta: tmd missing cluster {cluster_index}"))?
                .content_id;
            let encrypted = self
                .source
                .read_encrypted_content(content_id)
                .map_err(|e| anyhow!("disc meta: read content {content_id}: {e}"))?;
            let decrypted = match hash_mode {
                FstClusterHashMode::HashInterleaved => {
                    decrypt_hashed_content(&encrypted, &self.title_key).map_err(|e| {
                        anyhow!("disc meta: decrypt hashed cluster {cluster_index}: {e}")
                    })?
                }
                FstClusterHashMode::Raw | FstClusterHashMode::RawStream => {
                    decrypt_raw_content(encrypted, &self.title_key, cluster_index).map_err(|e| {
                        anyhow!("disc meta: decrypt raw cluster {cluster_index}: {e}")
                    })?
                }
                FstClusterHashMode::Unknown(b) => {
                    return Err(anyhow!(
                        "disc meta: unsupported hash mode {b} for cluster {cluster_index}"
                    ));
                }
            };
            self.cache.insert(cluster_index, decrypted);
        }
        Ok(self.cache.get(&cluster_index).expect("just inserted"))
    }
}

impl<'d> MetaSource for DiscMetaSource<'d> {
    fn read(&mut self, virtual_path: &str) -> Result<Option<Vec<u8>>> {
        let file = match self.fs.files.iter().find(|f| f.path == virtual_path) {
            Some(f) => f.clone(),
            None => return Ok(None),
        };
        // Meta/code files on a base game partition are never shared, but be
        // defensive: a shared entry has no own bytes here.
        if file.is_shared {
            return Ok(None);
        }
        let offset_factor = self.fs.offset_factor as u64;
        let start = u64::from(file.file_offset) * offset_factor;
        let end = start + u64::from(file.file_size);
        let cluster = self.decrypted_cluster(file.cluster_index)?;
        if end as usize > cluster.len() {
            return Ok(None);
        }
        Ok(Some(cluster[start as usize..end as usize].to_vec()))
    }
}
