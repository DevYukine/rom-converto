//! [`MetaSource`] over one decrypted Wii U disc partition.
//!
//! The source-agnostic metadata extractor reads only the requested FST file
//! extents; encrypted content ranges are decrypted in bounded chunks.

use anyhow::{Result, anyhow};

use crate::nintendo::wup::disc::partition::PartitionContentSource;
use crate::nintendo::wup::meta_source::MetaSource;
use crate::nintendo::wup::models::WupTmd;
use crate::nintendo::wup::nus::content_stream::ContentLoader;
use crate::nintendo::wup::nus::fst_parser::VirtualFs;
use crate::nintendo::wup::nus::ticket_parser::TitleKey;

/// [`MetaSource`] backed by one decrypted Wii U disc partition.
pub struct DiscMetaSource<'d> {
    source: PartitionContentSource<'d>,
    title_key: TitleKey,
    tmd: WupTmd,
    fs: VirtualFs,
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
        }
    }
}

impl<'d> MetaSource for DiscMetaSource<'d> {
    fn read(&mut self, virtual_path: &str) -> Result<Option<Vec<u8>>> {
        let file = match self.fs.files.iter().find(|f| f.path == virtual_path) {
            Some(f) => f.clone(),
            None => return Ok(None),
        };
        let mut loader = ContentLoader::new(&mut self.source, self.title_key, &self.tmd, &self.fs);
        // Validate the extent (shared / out-of-extent entries) before
        // reserving file_size bytes, so a crafted FST that declares a
        // huge size for a file this title never actually shipped fails
        // cheaply instead of allocating first.
        let extent = match loader.validate_file(&file) {
            Ok(extent) => extent,
            Err(crate::nintendo::wup::error::WupError::FileInheritedFromOtherTitle { .. }) => {
                return Ok(None);
            }
            Err(error) => return Err(anyhow!("disc meta: {error}")),
        };
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(file.file_size as usize)
            .map_err(|e| anyhow!("disc metadata allocation failed: {e}"))?;
        loader
            .stream_prepared_file(&file, extent, |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            })
            .map_err(|error| anyhow!("disc meta: {error}"))?;
        Ok(Some(bytes))
    }
    fn exists(&mut self, virtual_path: &str) -> Result<bool> {
        let file = match self.fs.files.iter().find(|f| f.path == virtual_path) {
            Some(f) => f.clone(),
            None => return Ok(false),
        };
        // Same predicate as read(): a shared or out-of-extent entry has
        // no own bytes on this partition and should not be reported as
        // present.
        let mut loader = ContentLoader::new(&mut self.source, self.title_key, &self.tmd, &self.fs);
        match loader.validate_file(&file) {
            Ok(_) => Ok(true),
            Err(crate::nintendo::wup::error::WupError::FileInheritedFromOtherTitle { .. }) => {
                Ok(false)
            }
            Err(error) => Err(anyhow!("disc meta: {error}")),
        }
    }
}
