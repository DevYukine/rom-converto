//! XISO volume summary: what the probed base is and what the directory
//! tables account for.

use std::fs::File;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::XboxResult;
use crate::microsoft::xdvdfs::{PartitionKind, XBOX_PROBE_BASES, XdvdfsVolume, walk_dir_tables};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XisoInfo {
    /// Renamed on the wire: [`crate::info::InfoResult`] already tags its
    /// variants with a `kind` field, which would otherwise collide with
    /// this one when flattened into the same JSON object.
    #[serde(rename = "partition_kind")]
    pub kind: PartitionKind,
    pub base: u64,
    pub root_sector: u32,
    pub root_size: u32,
    pub file_count: u64,
    pub dir_count: u64,
    pub total_file_bytes: u64,
    pub image_size: u64,
}

pub fn read_info(path: &Path) -> XboxResult<XisoInfo> {
    let mut file = File::open(path)?;
    let image_size = file.metadata()?.len();
    let volume = XdvdfsVolume::probe(&mut file, &XBOX_PROBE_BASES)?;

    let mut file_count = 0;
    let mut dir_count = 0;
    let mut total_file_bytes = 0;
    walk_dir_tables(&mut file, &volume, |_, entry| {
        if entry.is_directory() {
            dir_count += 1;
        } else {
            file_count += 1;
            total_file_bytes += entry.size as u64;
        }
        Ok(())
    })?;

    Ok(XisoInfo {
        kind: volume.kind,
        base: volume.base,
        root_sector: volume.root_sector,
        root_size: volume.root_size,
        file_count,
        dir_count,
        total_file_bytes,
        image_size,
    })
}
