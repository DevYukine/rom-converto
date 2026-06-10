//! Pull-based pipelined group reader.
//!
//! [`PipelinedGroupReader`] presents `Read + Seek` over a logical byte
//! stream that is materialized group-by-group on a [`Pool`]. The caller
//! supplies the spawned pool, an ordered list of [`GroupSpan`]s covering
//! the logical stream, and a `produce` closure that loads the raw bytes
//! for a group index (run on the reader thread, so disk access stays
//! sequential). Workers decode raw groups into logical bytes; the
//! adapter keeps up to `in_flight_cap` groups in flight and reassembles
//! results in order with a reorder map, the same scheme as
//! [`crate::util::worker_pool::drive`] but pull-based so it works
//! incrementally inside `Read::read`.
//!
//! Seeks inside the submission window are served by draining results
//! until the wanted group arrives; seeks outside it (rare: header
//! probing at open) drain and discard in-flight work, then resubmit
//! from the new position.

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};

use super::worker_pool::{Pool, Worker, parallelism};

/// Placement of one decoded group inside the logical output stream.
/// Spans must be contiguous and ordered: span `i + 1` starts where
/// span `i` ends.
#[derive(Debug, Clone, Copy)]
pub struct GroupSpan {
    pub logical_offset: u64,
    pub logical_size: u32,
}

/// Soft ceiling for raw + decoded group bytes held by the pipeline.
/// Keeps worst-case memory bounded on low-end machines even for
/// formats with very large groups (wit writes WIA chunks of 40 MiB).
const IN_FLIGHT_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

/// In-flight group cap for a given maximum group size: at least 2 so
/// the pipeline overlaps produce/decode/consume, at most one slot per
/// worker thread, scaled down for huge groups so raw + decoded bytes
/// stay near [`IN_FLIGHT_BUDGET_BYTES`].
pub fn in_flight_cap(max_group_bytes: u64) -> usize {
    let by_budget = (IN_FLIGHT_BUDGET_BYTES / (2 * max_group_bytes.max(1))) as usize;
    parallelism().min(by_budget.max(1)).max(2)
}

pub struct PipelinedGroupReader<W, E, P>
where
    W: Send + 'static,
    E: Send + 'static,
    P: FnMut(u64) -> Result<W, E>,
{
    pool: Option<Pool<W, Vec<u8>, E>>,
    produce: P,
    spans: Vec<GroupSpan>,
    logical_size: u64,
    cap: usize,
    /// First group index that may still be needed; everything below it
    /// has been consumed or skipped.
    window_base: u64,
    next_submit: u64,
    in_flight: usize,
    pending: HashMap<u64, Vec<u8>>,
    current: Option<(u64, Vec<u8>)>,
    pos: u64,
}

impl<W, E, P> PipelinedGroupReader<W, E, P>
where
    W: Send + 'static,
    E: Send + Sync + 'static + std::error::Error,
    P: FnMut(u64) -> Result<W, E>,
{
    /// `spans` must be non-empty, ordered, and contiguous from logical
    /// offset 0. `pool` must produce, for work item `produce(i)`, the
    /// decoded bytes of group `i` with exactly `spans[i].logical_size`
    /// bytes.
    pub fn new<Wk>(workers: Vec<Wk>, spans: Vec<GroupSpan>, cap: usize, produce: P) -> Self
    where
        Wk: Worker<W, Vec<u8>, E> + Send + 'static,
    {
        debug_assert!(!spans.is_empty(), "group plan must cover the stream");
        debug_assert!(spans[0].logical_offset == 0);
        debug_assert!(
            spans
                .windows(2)
                .all(|w| w[0].logical_offset + w[0].logical_size as u64 == w[1].logical_offset),
            "group spans must be contiguous"
        );
        let logical_size = spans
            .last()
            .map(|s| s.logical_offset + s.logical_size as u64)
            .unwrap_or(0);
        Self {
            pool: Some(Pool::spawn(workers)),
            produce,
            spans,
            logical_size,
            cap: cap.max(2),
            window_base: 0,
            next_submit: 0,
            in_flight: 0,
            pending: HashMap::new(),
            current: None,
            pos: 0,
        }
    }

    pub fn logical_size(&self) -> u64 {
        self.logical_size
    }

    fn group_index_for(&self, pos: u64) -> u64 {
        self.spans
            .partition_point(|s| s.logical_offset + s.logical_size as u64 <= pos) as u64
    }

    fn pool(&self) -> &Pool<W, Vec<u8>, E> {
        self.pool.as_ref().expect("pool alive until drop")
    }

    /// Discard everything in flight. Worker errors for discarded groups
    /// are dropped: they belong to work the caller no longer wants.
    fn reset_window(&mut self, base: u64) {
        while self.in_flight > 0 {
            let _ = self.pool().recv();
            self.in_flight -= 1;
        }
        self.pending.clear();
        self.window_base = base;
        self.next_submit = base;
    }

    fn top_up(&mut self) -> io::Result<()> {
        while self.in_flight < self.cap && self.next_submit < self.spans.len() as u64 {
            let work = (self.produce)(self.next_submit).map_err(io::Error::other)?;
            self.pool()
                .submit(self.next_submit, work)
                .map_err(io::Error::other)?;
            self.next_submit += 1;
            self.in_flight += 1;
        }
        Ok(())
    }

    fn ensure_group(&mut self, idx: u64) -> io::Result<()> {
        if matches!(self.current, Some((cur, _)) if cur == idx) {
            return Ok(());
        }
        if !self.pending.contains_key(&idx) && !(self.window_base..self.next_submit).contains(&idx)
        {
            self.reset_window(idx);
        }
        self.top_up()?;
        while !self.pending.contains_key(&idx) {
            debug_assert!(
                self.in_flight > 0,
                "wanted group neither pending nor in flight"
            );
            let (seq, result) = self.pool().recv();
            self.in_flight -= 1;
            let bytes = result.map_err(io::Error::other)?;
            if seq >= self.window_base {
                self.pending.insert(seq, bytes);
            }
            self.top_up()?;
        }
        let bytes = self.pending.remove(&idx).expect("checked in loop");
        let expected = self.spans[idx as usize].logical_size as usize;
        if bytes.len() != expected {
            return Err(io::Error::other(format!(
                "group {idx} decoded to {} bytes, expected {expected}",
                bytes.len()
            )));
        }
        self.window_base = idx + 1;
        self.pending.retain(|k, _| *k > idx);
        self.current = Some((idx, bytes));
        Ok(())
    }

    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.logical_size {
            return Ok(0);
        }
        let idx = self.group_index_for(self.pos);
        self.ensure_group(idx)?;
        let span = self.spans[idx as usize];
        let in_group = (self.pos - span.logical_offset) as usize;
        // Never serve across a group boundary in one call; the next
        // group may still be in flight.
        let take = buf
            .len()
            .min(span.logical_size as usize - in_group)
            .min((self.logical_size - self.pos) as usize);
        let (_, bytes) = self.current.as_ref().expect("ensured above");
        buf[..take].copy_from_slice(&bytes[in_group..in_group + take]);
        self.pos += take as u64;
        Ok(take)
    }
}

impl<W, E, P> Read for PipelinedGroupReader<W, E, P>
where
    W: Send + 'static,
    E: Send + Sync + 'static + std::error::Error,
    P: FnMut(u64) -> Result<W, E>,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_some(buf)
    }
}

impl<W, E, P> Seek for PipelinedGroupReader<W, E, P>
where
    W: Send + 'static,
    E: Send + Sync + 'static + std::error::Error,
    P: FnMut(u64) -> Result<W, E>,
{
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let new_pos: i128 = match from {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.logical_size as i128 + d as i128,
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to negative offset",
            ));
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

impl<W, E, P> Drop for PipelinedGroupReader<W, E, P>
where
    W: Send + 'static,
    E: Send + 'static,
    P: FnMut(u64) -> Result<W, E>,
{
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            while self.in_flight > 0 {
                let _ = pool.recv();
                self.in_flight -= 1;
            }
            pool.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test worker: work item is (group index, size, fill byte); output
    /// is `size` copies of the fill byte, slowed down for odd groups so
    /// results come back out of order.
    struct FillWorker;

    impl Worker<(u64, u32, u8), Vec<u8>, std::io::Error> for FillWorker {
        fn process(&mut self, (idx, size, byte): (u64, u32, u8)) -> io::Result<Vec<u8>> {
            if idx % 2 == 1 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(vec![byte; size as usize])
        }
    }

    type FillWork = (u64, u32, u8);

    fn make_reader(
        sizes: &[u32],
    ) -> PipelinedGroupReader<FillWork, std::io::Error, impl FnMut(u64) -> io::Result<FillWork>>
    {
        let mut spans = Vec::new();
        let mut off = 0u64;
        for &s in sizes {
            spans.push(GroupSpan {
                logical_offset: off,
                logical_size: s,
            });
            off += s as u64;
        }
        let sizes: Vec<u32> = sizes.to_vec();
        PipelinedGroupReader::new(
            vec![FillWorker, FillWorker, FillWorker],
            spans,
            4,
            move |idx| Ok((idx, sizes[idx as usize], idx as u8)),
        )
    }

    #[test]
    fn sequential_read_reassembles_in_order() {
        let sizes = [100u32, 50, 200, 10, 75, 130, 60, 90];
        let mut r = make_reader(&sizes);
        let total: usize = sizes.iter().map(|&s| s as usize).sum();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), total);
        let mut off = 0;
        for (i, &s) in sizes.iter().enumerate() {
            assert!(
                out[off..off + s as usize].iter().all(|&b| b == i as u8),
                "group {i} bytes wrong"
            );
            off += s as usize;
        }
    }

    #[test]
    fn seek_and_partial_reads() {
        let sizes = [64u32; 16];
        let mut r = make_reader(&sizes);
        r.seek(SeekFrom::Start(64 * 5 + 10)).unwrap();
        let mut buf = [0u8; 20];
        r.read_exact(&mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 5));
        // Backward to group 1 (outside window): pipeline restarts.
        r.seek(SeekFrom::Start(64 + 3)).unwrap();
        r.read_exact(&mut buf).unwrap();
        assert!(buf.iter().all(|&b| b == 1));
        r.seek(SeekFrom::End(-4)).unwrap();
        let mut tail = [0u8; 4];
        r.read_exact(&mut tail).unwrap();
        assert!(tail.iter().all(|&b| b == 15));
        assert_eq!(r.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn read_never_crosses_group_boundary() {
        let sizes = [8u32, 8, 8];
        let mut r = make_reader(&sizes);
        let mut buf = [0u8; 64];
        let n = r.read(&mut buf).unwrap();
        assert_eq!(n, 8);
        assert!(buf[..8].iter().all(|&b| b == 0));
    }

    #[test]
    fn worker_error_surfaces_as_io_error() {
        struct FailWorker;
        impl Worker<u64, Vec<u8>, std::io::Error> for FailWorker {
            fn process(&mut self, idx: u64) -> io::Result<Vec<u8>> {
                if idx == 2 {
                    Err(io::Error::other("decode failed"))
                } else {
                    Ok(vec![0u8; 16])
                }
            }
        }
        let spans: Vec<GroupSpan> = (0..4)
            .map(|i| GroupSpan {
                logical_offset: i * 16,
                logical_size: 16,
            })
            .collect();
        let mut r = PipelinedGroupReader::new(
            vec![FailWorker, FailWorker],
            spans,
            2,
            Ok::<u64, std::io::Error>,
        );
        let mut out = Vec::new();
        let err = r.read_to_end(&mut out).unwrap_err();
        assert!(err.to_string().contains("decode failed"));
    }

    #[test]
    fn wrong_decoded_size_is_an_error() {
        struct ShortWorker;
        impl Worker<u64, Vec<u8>, std::io::Error> for ShortWorker {
            fn process(&mut self, _: u64) -> io::Result<Vec<u8>> {
                Ok(vec![0u8; 3])
            }
        }
        let spans = vec![GroupSpan {
            logical_offset: 0,
            logical_size: 16,
        }];
        let mut r =
            PipelinedGroupReader::new(vec![ShortWorker], spans, 2, Ok::<u64, std::io::Error>);
        let mut out = Vec::new();
        let err = r.read_to_end(&mut out).unwrap_err();
        assert!(err.to_string().contains("expected 16"));
    }

    #[test]
    fn in_flight_cap_scales_with_group_size() {
        assert!(in_flight_cap(128 * 1024) >= 2);
        assert_eq!(in_flight_cap(40 * 1024 * 1024).min(2), 2);
        assert!(in_flight_cap(40 * 1024 * 1024) <= parallelism().max(2));
        let _ = in_flight_cap(0);
    }
}
