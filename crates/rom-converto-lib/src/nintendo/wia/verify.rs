//! WIA integrity verification.
//!
//! The standard pass validates the SHA-1 header chain (file head,
//! disc struct, partition table) plus the declared file size, and
//! decodes both metadata tables; corruption there fails immediately.
//! The deep pass additionally decodes every group through the codec
//! on the worker pool, catching payload corruption that the header
//! chain cannot see (codec checksums, Purge SHA-1 trailers, truncated
//! group data).

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::error::{WiaError, WiaResult};
use super::format::exception_lists_per_group;
use super::reader::{
    Segment, SegmentKind, WiaLayout, WiaSegmentWorker, build_segments, read_segment_work,
};
use crate::util::worker_pool::{Pool, drive, parallelism};

/// Bytes covered by the non-deep pass (header chain plus tables).
fn header_bytes(layout: &WiaLayout) -> u64 {
    crate::nintendo::rvz::format::WIA_FILE_HEAD_SIZE as u64
        + layout.head.disc_size as u64
        + layout.disc.n_part as u64 * layout.disc.part_t_size as u64
        + layout.disc.raw_data_size as u64
        + layout.disc.group_size as u64
}

pub fn verify_total(path: &Path, deep: bool) -> WiaResult<u64> {
    let mut f = File::open(path)?;
    let layout = WiaLayout::parse(&mut f)?;
    Ok(if deep {
        layout.stored_bytes()
    } else {
        header_bytes(&layout)
    })
}

pub fn verify_wia_blocking(path: &Path, deep: bool, bytes_done: Arc<AtomicU64>) -> WiaResult<()> {
    let mut f = File::open(path)?;
    let layout = WiaLayout::parse(&mut f)?;
    bytes_done.fetch_add(header_bytes(&layout), Ordering::Relaxed);
    if !deep {
        return Ok(());
    }

    let segments = build_segments(&layout)?;
    let n_lists = exception_lists_per_group(layout.disc.chunk_size);
    let group_size_of = |seg: &Segment| -> u64 {
        match seg.kind {
            SegmentKind::RawGroup { group_index, .. }
            | SegmentKind::PartGroup { group_index, .. } => {
                layout.groups[group_index as usize].data_size as u64
            }
            _ => 0,
        }
    };

    let workers: WiaResult<Vec<WiaSegmentWorker>> = (0..parallelism())
        .map(|_| WiaSegmentWorker::new(&layout.disc))
        .collect();
    let pool = Pool::spawn(workers?);

    let mut seg_iter = segments.iter();
    let sizes: Vec<u64> = segments.iter().map(group_size_of).collect();
    let result = drive(
        &pool,
        segments.len() as u64,
        parallelism() * 2,
        |_seq| {
            let seg = seg_iter
                .next()
                .ok_or_else(|| WiaError::Custom("segment iterator exhausted".into()))?;
            read_segment_work(&mut f, seg, &layout.groups, &layout.disc.dhead, n_lists)
        },
        |seq, _decoded| {
            bytes_done.fetch_add(sizes[seq as usize], Ordering::Relaxed);
            Ok(())
        },
    );
    pool.shutdown();
    result
}
