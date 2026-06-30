//! `MetaSource` impl that decrypts NUS content on demand.
//!
//! Pulling a Wii U meta file (e.g. `meta/iconTex.tga`) used to load
//! the whole `.app` cluster into memory and decrypt all of it before
//! slicing. This source instead opens the cluster file fresh per
//! request, decrypts only the byte range that backs the requested
//! virtual file, and reuses a cached FST across calls so we do not
//! re-decrypt the first chunk of content 0 for every meta read.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

use crate::nintendo::wup::meta_source::MetaSource;
use crate::nintendo::wup::models::tmd::WupTmd;
use crate::nintendo::wup::nus::content_reader::{decrypt_hashed_range, decrypt_raw_range};
use crate::nintendo::wup::nus::fst_parser::{FstClusterHashMode, VirtualFs, parse_fst};
use crate::nintendo::wup::nus::layout::{NusLayout, TicketSource};
use crate::nintendo::wup::nus::ticket_parser::{TitleKey, read_ticket_file};
use crate::nintendo::wup::nus::tmd_parser::read_tmd_file;
use crate::nintendo::wup::title_key_derive::derive_title_key;

const FST_INITIAL_PROBE: usize = 4 * 1024 * 1024;

pub struct NusSource {
    layout: NusLayout,
    title_key: TitleKey,
    tmd: WupTmd,
    fst_cache: Option<VirtualFs>,
}

impl NusSource {
    pub fn open(dir: &Path) -> Result<Self> {
        let layout = NusLayout::discover(dir).map_err(|e| anyhow!("nus layout: {}", e))?;
        let tmd = read_tmd_file(&layout.tmd_path).map_err(|e| anyhow!("read tmd: {}", e))?;
        let title_key = match &layout.ticket_source {
            TicketSource::OnDisk(path) => {
                let (_ticket, key) =
                    read_ticket_file(path).map_err(|e| anyhow!("read ticket: {}", e))?;
                key
            }
            TicketSource::Derive => TitleKey(derive_title_key(tmd.title_id)),
        };
        Ok(Self {
            layout,
            title_key,
            tmd,
            fst_cache: None,
        })
    }

    pub fn tmd(&self) -> &WupTmd {
        &self.tmd
    }

    /// Decrypted title key for this title's content.
    pub fn title_key(&self) -> TitleKey {
        self.title_key
    }

    /// Parse and return the title's FST (decrypted from content 0).
    pub fn virtual_fs(&self) -> Result<VirtualFs> {
        self.load_fst()
    }

    /// A content-bytes source over this title's on-disk `.app` files.
    pub fn content_source(
        &self,
    ) -> crate::nintendo::wup::nus::content_stream::DirectoryContentSource {
        crate::nintendo::wup::nus::content_stream::DirectoryContentSource::with_resolver(
            self.layout.content.clone(),
        )
    }

    fn load_fst(&self) -> Result<VirtualFs> {
        let content_0 = self
            .tmd
            .contents
            .first()
            .ok_or_else(|| anyhow!("tmd has no content 0"))?;
        let path = self
            .layout
            .content
            .resolve(content_0.content_id)
            .ok_or_else(|| anyhow!("content 0 missing on disk"))?;
        let mut file = File::open(&path).context("open content 0")?;
        let file_size = file.metadata()?.len();
        let probe = (FST_INITIAL_PROBE as u64).min(file_size) as usize;
        let probe_aligned = probe & !15;
        let decrypted = decrypt_raw_range(&mut file, &self.title_key, 0, 0, probe_aligned)
            .map_err(|e| anyhow!("decrypt content 0 probe: {}", e))?;
        parse_fst(&decrypted).map_err(|e| anyhow!("parse fst: {}", e))
    }
}

impl MetaSource for NusSource {
    fn read(&mut self, virtual_path: &str) -> Result<Option<Vec<u8>>> {
        if self.fst_cache.is_none() {
            self.fst_cache = Some(self.load_fst()?);
        }
        let fst = self.fst_cache.as_ref().expect("just set");

        let (cluster_index, byte_offset, byte_len, hash_mode) = {
            let entry = match fst.files.iter().find(|f| f.path == virtual_path) {
                Some(e) => e,
                None => return Ok(None),
            };
            let cluster = fst
                .clusters
                .get(entry.cluster_index as usize)
                .ok_or_else(|| anyhow!("cluster {} missing from FST", entry.cluster_index))?;
            (
                entry.cluster_index,
                u64::from(entry.file_offset) * u64::from(fst.offset_factor),
                entry.file_size as usize,
                cluster.hash_mode,
            )
        };

        let tmd_entry = self
            .tmd
            .content_by_index(cluster_index)
            .ok_or_else(|| anyhow!("tmd missing cluster {}", cluster_index))?;
        let cluster_path = self
            .layout
            .content
            .resolve(tmd_entry.content_id)
            .ok_or_else(|| anyhow!("content {} not on disk", tmd_entry.content_id))?;
        let mut file = File::open(&cluster_path)
            .with_context(|| format!("open content {}", tmd_entry.content_id))?;

        let bytes = match hash_mode {
            FstClusterHashMode::Raw | FstClusterHashMode::RawStream => decrypt_raw_range(
                &mut file,
                &self.title_key,
                cluster_index,
                byte_offset,
                byte_len,
            )
            .map_err(|e| anyhow!("decrypt raw range: {}", e))?,
            FstClusterHashMode::HashInterleaved => {
                decrypt_hashed_range(&mut file, &self.title_key, byte_offset, byte_len)
                    .map_err(|e| anyhow!("decrypt hashed range: {}", e))?
            }
            FstClusterHashMode::Unknown(b) => {
                return Err(anyhow!(
                    "unsupported cluster hash mode {} for {}",
                    b,
                    virtual_path
                ));
            }
        };
        Ok(Some(bytes))
    }
}
