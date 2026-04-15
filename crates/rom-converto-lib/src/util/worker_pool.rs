//! Generic persistent worker pool shared by every format's
//! compress / decompress pipelines.
//!
//! # Shape
//!
//! A [`Pool<W, O, E>`] owns `n_threads` worker threads, one bounded
//! channel per worker for back-pressure, and a shared result channel.
//! Each worker holds a user-supplied state value that implements
//! [`Worker`], typically a struct with long-lived codec contexts and
//! scratch buffers, so expensive per-thread setup (`ZSTD_createCCtx`,
//! LZMA probability tables, deflate dictionaries, etc.) happens
//! exactly once per pool lifetime instead of once per work item.
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
//! # Error model
//!
//! The pool is generic over a worker error type `E`. Pool-internal
//! failures (worker thread panic → dead channel) surface as
//! [`PoolChannelClosed`], which the caller's error type must be able
//! to absorb via `From<PoolChannelClosed>`. Worker errors from
//! `process` flow through unchanged.
//!
//! # Threading model
//!
//! The pool lives inside `tokio::task::spawn_blocking` at the
//! outermost layer (see the compress / decompress entry points in
//! each format module), so the worker threads are plain
//! `std::thread::spawn` workers communicating via `std::sync::mpsc`.
//! Do NOT switch the channels to `tokio::sync::mpsc`; the workers
//! are synchronous by design and pay no async runtime cost.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, SyncSender, channel, sync_channel};
use std::thread;

/// Worker thread count: `available_parallelism()`, clamped to at
/// least 1.
pub fn parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(1)
}

/// Pool-internal error returned by [`Pool::submit`] when a worker's
/// inbound channel has closed (usually because the worker thread
/// panicked). Consumers map this into their own error type via
/// `From<PoolChannelClosed>`.
#[derive(Debug, Clone, Copy)]
pub struct PoolChannelClosed;

impl std::fmt::Display for PoolChannelClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("worker pool channel closed")
    }
}

impl std::error::Error for PoolChannelClosed {}

/// Per-thread worker state. One instance lives for the lifetime of a
/// pool thread; `process` is called once per submitted work item.
///
/// Implementations should own any expensive, reusable state (codec
/// contexts, scratch buffers) so the hot loop never allocates.
pub trait Worker<W, O, E> {
    fn process(&mut self, work: W) -> Result<O, E>;
}

/// Persistent worker pool. Generic over the work-item, output, and
/// error types; the worker type is erased at spawn time so one
/// [`Pool`] can wrap any encoder or decoder worker set.
pub struct Pool<W: Send + 'static, O: Send + 'static, E: Send + 'static> {
    n_threads: usize,
    work_txs: Vec<SyncSender<Option<(u64, W)>>>,
    result_rx: Receiver<(u64, Result<O, E>)>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl<W: Send + 'static, O: Send + 'static, E: Send + 'static> Pool<W, O, E> {
    /// Spawn `workers.len()` threads, each owning one worker state
    /// instance. Workers are consumed by value; the caller is
    /// responsible for any fallible construction (e.g. initialising
    /// a codec context) before calling [`Pool::spawn`].
    ///
    /// Back-pressure: each worker's inbound channel has capacity 2,
    /// so the dispatcher can run at most one work item ahead of the
    /// item the worker is currently processing. This caps per-worker
    /// memory pressure without starving the pipeline.
    pub fn spawn<Wk>(workers: Vec<Wk>) -> Self
    where
        Wk: Worker<W, O, E> + Send + 'static,
    {
        let n_threads = workers.len();
        let (result_tx, result_rx) = channel::<(u64, Result<O, E>)>();
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

    /// Route `work` to any worker that has capacity, starting at
    /// the preferred slot `seq % n_threads`. On congestion (every
    /// worker's channel is full) blocks on the preferred slot.
    ///
    /// Non-strict routing matters when per-item processing cost is
    /// uneven: CD codec trials, for example, spend 4-10× longer on
    /// LZMA-heavy data hunks than on all-zero hunks, so a strict
    /// round-robin would stall the dispatcher behind the slowest
    /// worker while peer workers sit idle. Ordering is preserved by
    /// [`drive`]'s reorder HashMap regardless of which worker ran
    /// each item, so smart routing is safe.
    ///
    /// Returns [`PoolChannelClosed`] only if every worker's channel
    /// has closed (i.e. all worker threads have exited).
    pub fn submit(&self, seq: u64, work: W) -> Result<(), PoolChannelClosed> {
        use std::sync::mpsc::TrySendError;

        let start = (seq as usize) % self.n_threads;
        let mut pending = Some((seq, work));
        for i in 0..self.n_threads {
            let idx = (start + i) % self.n_threads;
            let item = pending.take().expect("pending set on every loop iteration");
            match self.work_txs[idx].try_send(Some(item)) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(Some(inner))) => {
                    pending = Some(inner);
                }
                Err(TrySendError::Full(None)) => unreachable!("None sentinel is never sent here"),
                Err(TrySendError::Disconnected(_)) => {
                    // Move on; another worker may still be alive.
                    pending = None;
                }
            }
            if pending.is_none() {
                // Only reachable on a disconnected worker; rebuild
                // a fresh work item from the original. Since we
                // consumed it we have to fail the whole submit.
                return Err(PoolChannelClosed);
            }
        }
        // Every worker is busy: block on the preferred slot.
        let item = pending.take().expect("pending still set after loop");
        self.work_txs[start]
            .send(Some(item))
            .map_err(|_| PoolChannelClosed)
    }

    /// Block until any worker produces a result. Returns the
    /// submission sequence number and the worker's `Result<O, E>`.
    ///
    /// Panics only if every worker has exited without producing
    /// any output, which only happens if the pool was shut down
    /// prematurely (a programming error).
    pub fn recv(&self) -> (u64, Result<O, E>) {
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
pub fn drive<W, O, E, Produce, Consume>(
    pool: &Pool<W, O, E>,
    total: u64,
    max_in_flight: usize,
    mut produce: Produce,
    mut consume: Consume,
) -> Result<(), E>
where
    W: Send + 'static,
    O: Send + 'static,
    E: Send + 'static + From<PoolChannelClosed>,
    Produce: FnMut(u64) -> Result<W, E>,
    Consume: FnMut(u64, O) -> Result<(), E>,
{
    let mut pending: HashMap<u64, O> = HashMap::new();
    let mut submit_seq: u64 = 0;
    let mut write_seq: u64 = 0;
    let mut in_flight: usize = 0;
    let mut run_result: Result<(), E> = Ok(());

    while write_seq < total {
        // Submit as much as back-pressure allows.
        while run_result.is_ok() && in_flight < max_in_flight && submit_seq < total {
            match produce(submit_seq) {
                Ok(work) => match pool.submit(submit_seq, work) {
                    Ok(()) => {
                        submit_seq += 1;
                        in_flight += 1;
                    }
                    Err(e) => run_result = Err(e.into()),
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
