//! XISO extraction: walk the dirtabs, mirror the tree on disk, and
//! stream each file out of its data sectors.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::error::{XboxError, XboxResult};
use crate::microsoft::xdvdfs::{
    XBOX_PROBE_BASES, XdvdfsError, XdvdfsVolume, data_offset, walk_dir_tables,
};
use crate::util::CancelToken;

const COPY_BUF: usize = 1024 * 1024;

/// A dirent name safe to append to a real filesystem path: an untrusted
/// image can otherwise plant an absolute or `..` name and escape
/// `output_dir` (zip-slip).
fn is_safe_dirent_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.contains(['/', '\\', '\0']) {
        return false;
    }
    if cfg!(windows) && name.contains(':') {
        return false;
    }
    true
}

pub(super) fn extract_blocking(
    input: &Path,
    output_dir: &Path,
    bytes_done: Arc<AtomicU64>,
    cancel: &CancelToken,
) -> XboxResult<()> {
    let mut image = File::open(input)?;
    let volume = XdvdfsVolume::probe(&mut image, &XBOX_PROBE_BASES)?;
    fs::create_dir_all(output_dir)?;

    // The walk visits a directory's own entry before its contents, so
    // every parent exists by the time a file under it is queued.
    let mut files: Vec<(PathBuf, u32, u64)> = Vec::new();
    let mut unsafe_name: Option<String> = None;
    let walked = walk_dir_tables(&mut image, &volume, |parent, entry| {
        let name = entry.name_str();
        if !is_safe_dirent_name(&name) {
            // walk_dir_tables requires an XdvdfsError here; the real
            // XboxError::UnsafeName is raised below once the walk halts.
            unsafe_name.get_or_insert(name);
            return Err(XdvdfsError::InvalidDirent {
                offset: 0,
                reason: "unsafe dirent name",
            });
        }
        let mut dest = output_dir.to_path_buf();
        dest.extend(parent);
        dest.push(&name);
        if entry.is_directory() {
            fs::create_dir_all(&dest)?;
        } else {
            files.push((dest, entry.start_sector, entry.size as u64));
        }
        Ok(())
    });
    if let Some(name) = unsafe_name {
        return Err(XboxError::UnsafeName { name });
    }
    walked?;

    let mut buf = vec![0u8; COPY_BUF];
    for (dest, sector, size) in files {
        if cancel.is_cancelled() {
            return Err(XboxError::Cancelled);
        }
        image.seek(SeekFrom::Start(data_offset(&volume, sector)))?;
        let mut out = io::BufWriter::with_capacity(COPY_BUF, File::create(&dest)?);
        let mut left = size;
        while left > 0 {
            if cancel.is_cancelled() {
                return Err(XboxError::Cancelled);
            }
            let take = left.min(buf.len() as u64) as usize;
            image.read_exact(&mut buf[..take])?;
            out.write_all(&buf[..take])?;
            bytes_done.fetch_add(take as u64, Ordering::Relaxed);
            left -= take as u64;
        }
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::xdvdfs::{SECTOR_SIZE, VOLUME_DESCRIPTOR_SECTOR, VOLUME_MAGIC};

    fn build_descriptor(root_sector: u32, root_size: u32) -> Vec<u8> {
        let mut d = vec![0u8; 0x800];
        d[0..20].copy_from_slice(VOLUME_MAGIC);
        d[0x14..0x18].copy_from_slice(&root_sector.to_le_bytes());
        d[0x18..0x1C].copy_from_slice(&root_size.to_le_bytes());
        d[0x1C..0x24].copy_from_slice(&0u64.to_le_bytes());
        d[0x7EC..0x800].copy_from_slice(VOLUME_MAGIC);
        d
    }

    /// A single root-level plain-file dirent with no children.
    fn encode_root_file_dirent(name: &[u8]) -> Vec<u8> {
        let mut e = Vec::with_capacity(14 + name.len());
        e.extend_from_slice(&0u16.to_le_bytes()); // left
        e.extend_from_slice(&0u16.to_le_bytes()); // right
        e.extend_from_slice(&0u32.to_le_bytes()); // start_sector
        e.extend_from_slice(&0u32.to_le_bytes()); // size
        e.push(0); // attributes: plain file
        e.push(name.len() as u8);
        e.extend_from_slice(name);
        e
    }

    #[test]
    fn traversal_dirent_name_is_rejected() {
        let root_sector = 40u32;
        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        let entry = encode_root_file_dirent(b"..");
        root[0..entry.len()].copy_from_slice(&entry);

        let mut image = vec![0u8; ((root_sector as u64 + 1) * SECTOR_SIZE) as usize];
        let descriptor_off = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        image[descriptor_off..descriptor_off + 0x800]
            .copy_from_slice(&build_descriptor(root_sector, SECTOR_SIZE as u32));
        let root_off = (root_sector as u64 * SECTOR_SIZE) as usize;
        image[root_off..root_off + root.len()].copy_from_slice(&root);

        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("evil.iso");
        std::fs::write(&image_path, &image).unwrap();
        let output_dir = dir.path().join("out");

        let err = extract_blocking(
            &image_path,
            &output_dir,
            Arc::new(AtomicU64::new(0)),
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, XboxError::UnsafeName { .. }), "{err}");
    }
}
