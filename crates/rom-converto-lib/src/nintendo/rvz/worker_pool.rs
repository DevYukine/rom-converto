//! Generic persistent worker pool shared by the RVZ encode and decode
//! pipelines.
//!
//! # Shape
//!
//! A [`Pool<W, O>`] owns `n_threads` worker threads, one bounded channel
//! per worker for back-pressure, and a shared result channel. Each
//! worker holds a user-supplied state value that implements [`Worker`],
//! typically a struct with a long-lived `zstd::bulk::Compressor` or
//! `Decompressor` plus scratch buffers, so expensive per-thread setup
//! (`ZSTD_createCCtx`, scratch allocations) happens exactly once per
//! pool lifetime instead of once per work item.
//!
//! # Ordering
//!
//! Work items are submitted with a monotonically increasing `seq` and
//! dispatched round-robin (`seq % n_threads`). Results come back in
//! any order; the [`drive`] helper hides that with a small
//! `HashMap<u64, O>` reorder buffer, calling the caller's `consume`
//! closure only on contiguous runs starting at the next expected
//! sequence number. This keeps output byte-for-byte reproducible
//! regardless of which worker finishes first.
//!
//! # Threading model
//!
//! The pool lives inside `tokio::task::spawn_blocking` (see
//! `compress_disc` / `decompress_disc`), so the worker threads are
//! plain `std::thread::spawn` workers communicating via
//! `std::sync::mpsc`. Do NOT switch the channels to
//! `tokio::sync::mpsc`; the workers are synchronous by design and
//! pay no async runtime cost.

use crate::nintendo::rvz::error::{RvzError, RvzResult};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, SyncSender, channel, sync_channel};
use std::thread;

/// Worker thread count for the RVZ pools: `available_parallelism()`,
/// clamped to at least 1.
pub fn parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(1)
}

/// Per-thread worker state. One instance lives for the lifetime of a
/// pool thread; `process` is called once per submitted work item.
///
/// Implementations should own any expensive, reusable state (zstd
/// contexts, scratch buffers) so the hot loop never allocates.
pub trait Worker<W, O> {
    fn process(&mut self, work: W) -> RvzResult<O>;
}

/// Persistent worker pool. Generic over the work-item and output
/// types; the worker type is erased at spawn time so one [`Pool<W, O>`]
/// can wrap either an encoder or decoder worker set.
pub struct Pool<W: Send + 'static, O: Send + 'static> {
    n_threads: usize,
    work_txs: Vec<SyncSender<Option<(u64, W)>>>,
    result_rx: Receiver<(u64, RvzResult<O>)>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl<W: Send + 'static, O: Send + 'static> Pool<W, O> {
    /// Spawn `workers.len()` threads, each owning one worker state
    /// instance. Workers are consumed by value; the caller is
    /// responsible for any fallible construction (e.g. initializing
    /// a `zstd::bulk::Compressor`) before calling [`Pool::spawn`].
    ///
    /// Back-pressure: each worker's inbound channel has capacity 2,
    /// so the dispatcher can run at most one work item ahead of the
    /// item the worker is currently processing. This caps per-worker
    /// memory pressure without starving the pipeline.
    pub fn spawn<Wk>(workers: Vec<Wk>) -> Self
    where
        Wk: Worker<W, O> + Send + 'static,
    {
        let n_threads = workers.len();
        let (result_tx, result_rx) = channel::<(u64, RvzResult<O>)>();
        let mut work_txs = Vec::with_capacity(n_threads);
        let mut handles = Vec::with_capacity(n_threads);

        for mut worker in workers {
            let (work_tx, work_rx) = sync_channel::<Option<(u64, W)>>(2);
            work_txs.push(work_tx);
            let result_tx = result_tx.clone();
            let handle = thread::spawn(move || {
                while let Ok(Some((seq, work))) = work_rx.recv() {
                    let result = worker.process(work);
                    if result_tx.send((seq, result)).is_err() {
                        // Result channel closed, dispatcher is
                        // unwinding. Stop silently.
                        break;
                    }
                }
            });
            handles.push(handle);
        }
        drop(result_tx);

        Self {
            n_threads,
            work_txs,
            result_rx,
            handles,
        }
    }

    /// Route `work` to worker `seq % n_threads`. Returns an error if
    /// the target worker's channel has closed (i.e. the worker thread
    /// has panicked or exited) so the dispatcher can surface the
    /// failure instead of hanging on the next `recv`.
    pub fn submit(&self, seq: u64, work: W) -> RvzResult<()> {
        let worker = (seq as usize) % self.n_threads;
        self.work_txs[worker]
            .send(Some((seq, work)))
            .map_err(|_| RvzError::Custom("worker pool channel closed".into()))
    }

    /// Block until any worker produces a result. Returns the
    /// submission sequence number and the worker's `RvzResult<O>`.
    ///
    /// Panics only if every worker has exited without producing
    /// any output, which only happens if the pool was shut down
    /// prematurely (a programming error).
    pub fn recv(&self) -> (u64, RvzResult<O>) {
        self.result_rx
            .recv()
            .expect("worker pool result channel closed unexpectedly")
    }

    /// Signal all workers to exit (`None` sentinel) and join their
    /// threads. Must be called after the caller has drained every
    /// result it expects via [`Self::recv`]; workers still holding
    /// unprocessed items will process them before exiting.
    pub fn shutdown(self) {
        for tx in self.work_txs {
            let _ = tx.send(None);
            drop(tx);
        }
        for h in self.handles {
            let _ = h.join();
        }
    }
}

/// Pump a pool with back-pressured submit + ordered flush.
///
/// Calls `produce(seq)` for each submission in order, routes results
/// back to `consume(seq, out)` in strict order so the caller can
/// append bytes to a sequential writer without worrying about worker
/// interleaving. Caps in-flight work at `max_in_flight`.
///
/// On any error (produce, submit, worker, or consume), drains the
/// remaining in-flight jobs before returning so no thread is left
/// holding work. The first error wins; subsequent errors are
/// discarded.
pub fn drive<W, O, Produce, Consume>(
    pool: &Pool<W, O>,
    total: u64,
    max_in_flight: usize,
    mut produce: Produce,
    mut consume: Consume,
) -> RvzResult<()>
where
    W: Send + 'static,
    O: Send + 'static,
    Produce: FnMut(u64) -> RvzResult<W>,
    Consume: FnMut(u64, O) -> RvzResult<()>,
{
    let mut pending: HashMap<u64, O> = HashMap::new();
    let mut submit_seq: u64 = 0;
    let mut write_seq: u64 = 0;
    let mut in_flight: usize = 0;
    let mut run_result: RvzResult<()> = Ok(());

    while write_seq < total {
        // Submit as much as back-pressure allows.
        while run_result.is_ok() && in_flight < max_in_flight && submit_seq < total {
            match produce(submit_seq) {
                Ok(work) => match pool.submit(submit_seq, work) {
                    Ok(()) => {
                        submit_seq += 1;
                        in_flight += 1;
                    }
                    Err(e) => run_result = Err(e),
                },
                Err(e) => run_result = Err(e),
            }
        }
        if run_result.is_err() {
            break;
        }

        // Receive one result, stash, drain contiguous runs.
        let (seq, result) = pool.recv();
        in_flight -= 1;
        match result {
            Ok(out) => {
                pending.insert(seq, out);
            }
            Err(e) => {
                run_result = Err(e);
                break;
            }
        }
        while let Some(out) = pending.remove(&write_seq) {
            if let Err(e) = consume(write_seq, out) {
                run_result = Err(e);
                break;
            }
            write_seq += 1;
        }
        if run_result.is_err() {
            break;
        }
    }

    // Drain still-running work before letting the caller tear the
    // pool down. Without this, `shutdown()` would race workers that
    // are mid-process.
    while in_flight > 0 {
        let (_seq, result) = pool.recv();
        in_flight -= 1;
        if run_result.is_ok()
            && let Err(e) = result
        {
            run_result = Err(e);
        }
    }

    run_result
}
