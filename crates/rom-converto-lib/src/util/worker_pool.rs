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
//! LZMA probability tables, deflate dictionaries, and similar) happens
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
//! failures surface as [`PoolChannelClosed`] once every worker has
//! exited (the caller's error type must be able to absorb it via
//! `From<PoolChannelClosed>`), or as [`PoolOutcome::Panicked`] when a
//! single job's worker panicked. Worker errors from `process` flow
//! through unchanged.
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
use std::io::Write;
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
/// The per-operation working-set target, excluding final indexes and results.
pub const MEMORY_TARGET_BYTES: usize = 512 * 1024 * 1024;

/// Queue admission derived from codec, producer, item and writer memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admission {
    pub workers: usize,
    pub max_in_flight: usize,
    /// Bounded writer-channel capacity; the writer's in-hand buffer is
    /// reserved separately, so callers pass this value unchanged.
    pub writer_capacity: usize,
}

impl Admission {
    /// One worker, one job, and a rendezvous writer channel: the writer
    /// holds one unit while the worker decodes the next, so a unit that
    /// exceeds the target on its own is never live more than twice.
    pub const DEGRADED: Self = Self {
        workers: 1,
        max_in_flight: 1,
        writer_capacity: 0,
    };
}

/// Memory inputs to [`Budget::admit`], in bytes.
pub struct Budget {
    /// Persistent per-worker codec context (zstd cctx, LZMA state, ...).
    pub codec_per_worker: usize,
    /// Each job's input, output, and per-job scratch.
    pub per_job: usize,
    /// One bounded writer-channel slot.
    pub writer_slot: usize,
    /// Producer/consumer items held outside channel occupancy.
    pub fixed: usize,
}

impl Budget {
    /// Derive worker and queue counts while reserving all simultaneously-live
    /// buffers, or `None` when one queued unit plus the codec context and the
    /// writer reserve exceeds [`MEMORY_TARGET_BYTES`].
    ///
    /// Callers fall back in two tiers when this returns `None`: formats whose
    /// units are header-capped (CSO, CHD, RVZ partition clusters, Z3DS
    /// compress, ZAR) take [`Admission::DEGRADED`], while formats that can
    /// stream a unit (NCZ, plain and packed RVZ chunks, Z3DS decompress)
    /// treat `None` as "stream instead".
    ///
    /// `per_job` includes each job's input, output, and per-job scratch.
    /// `fixed` includes producer/consumer items held outside channel occupancy.
    /// The 512 MiB target applies to one operation; up to eight GUI operations
    /// may run concurrently, so aggregate process memory can be higher.
    ///
    /// Workers never exceed `max_in_flight` or `jobs`: an idle worker only
    /// pins a codec context, and effective concurrency is `min(workers,
    /// in_flight)` anyway.
    pub fn admit(&self, requested_workers: usize, jobs: u64) -> Option<Admission> {
        // A hostile table can declare zero-byte units; one byte keeps the
        // divisions below meaningful without changing any real admission.
        let per_job = self.per_job.max(1);
        let target = MEMORY_TARGET_BYTES;
        let jobs = usize::try_from(jobs).unwrap_or(usize::MAX);
        // One channel slot plus the writer's in-hand buffer.
        let reserve = self.writer_slot.saturating_mul(2);
        let spare = target.checked_sub(self.fixed.saturating_add(reserve))?;
        let unit = self.codec_per_worker.saturating_add(per_job);
        // Largest worker count whose best in-flight depth keeps every worker
        // busy; one worker with one job always fits once `spare >= unit`.
        let workers = requested_workers.min(jobs).min(spare / unit);
        if workers == 0 {
            return None;
        }
        let after = spare - self.codec_per_worker.saturating_mul(workers);
        let max_in_flight = jobs.min(workers.saturating_mul(2)).min(after / per_job);
        // Twice the in-flight depth lets the writer absorb bursts of
        // small units without stalling decoders (develop's 4x depth).
        // `reserve` sits in the numerator, so the quotient is at least 2.
        let writer_capacity = (after + reserve - per_job * max_in_flight)
            .checked_div(self.writer_slot)
            .map_or(0, |slots| (slots - 1).min(max_in_flight.saturating_mul(2)));
        Some(Admission {
            workers,
            max_in_flight,
            writer_capacity,
        })
    }
}

/// Native zstd one-shot compression-context estimate for a block size and level.
pub fn zstd_cctx_estimate(level: i32, max_input: usize) -> usize {
    // SAFETY: these native estimate functions accept arbitrary level/input hints.
    let params = unsafe { zstd_sys::ZSTD_getCParams(level, max_input as u64, 0) };
    unsafe { zstd_sys::ZSTD_estimateCCtxSize_usingCParams(params) }
}

/// Native zstd decompression-context estimate (window buffers excluded).
pub fn zstd_dctx_estimate() -> usize {
    // SAFETY: pure size query with no arguments.
    unsafe { zstd_sys::ZSTD_estimateDCtxSize() }
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    const MIB: usize = 1024 * 1024;

    #[test]
    fn zero_byte_units_admit_without_dividing_by_zero() {
        let admission = Budget {
            codec_per_worker: MIB,
            per_job: 0,
            writer_slot: 0,
            fixed: 0,
        }
        .admit(4, 8)
        .expect("zero-byte units fit");
        assert_eq!((admission.workers, admission.max_in_flight), (4, 8));
    }

    #[test]
    fn admission_counts_writer_buffers_and_reports_unfit_single_worker() {
        let fits = Budget {
            codec_per_worker: 200 * MIB,
            per_job: 100 * MIB,
            writer_slot: 50 * MIB,
            fixed: 10 * MIB,
        }
        .admit(8, 64)
        .expect("one worker fits");
        assert_eq!(fits.workers, 1);
        assert!(fits.writer_capacity >= 1);
        // The writer's in-hand buffer is reserved on top of the channel slots.
        assert!(
            10 * MIB
                + fits.workers * 200 * MIB
                + fits.max_in_flight * 100 * MIB
                + (fits.writer_capacity + 1) * 50 * MIB
                <= MEMORY_TARGET_BYTES
        );

        let too_large = Budget {
            codec_per_worker: MEMORY_TARGET_BYTES,
            per_job: 1,
            writer_slot: 0,
            fixed: 1,
        }
        .admit(8, 1);
        assert!(too_large.is_none());
    }

    #[test]
    fn admission_never_leaves_workers_idle() {
        // 3 workers fit with one job in flight, but 2 workers with two jobs
        // is the same concurrency without an idle codec context.
        let a = Budget {
            codec_per_worker: 100 * MIB,
            per_job: 128 * MIB,
            writer_slot: 0,
            fixed: 0,
        }
        .admit(8, 64)
        .expect("fits");
        assert_eq!((a.workers, a.max_in_flight), (2, 2));

        let few_jobs = Budget {
            codec_per_worker: MIB,
            per_job: MIB,
            writer_slot: 0,
            fixed: 0,
        }
        .admit(8, 3)
        .expect("fits");
        assert_eq!((few_jobs.workers, few_jobs.max_in_flight), (3, 3));

        // Default-sized units keep full parallelism with in_flight >= workers.
        let full = Budget {
            codec_per_worker: MIB,
            per_job: 2 * MIB,
            writer_slot: 2 * MIB,
            fixed: 0,
        }
        .admit(64, u64::MAX)
        .expect("fits");
        assert_eq!(
            (full.workers, full.max_in_flight, full.writer_capacity),
            (64, 128, 95)
        );
    }

    struct SlowEvens;

    impl Worker<u64, u64, PoolChannelClosed> for SlowEvens {
        fn process(&mut self, seq: u64) -> Result<u64, PoolChannelClosed> {
            if seq.is_multiple_of(2) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(seq)
        }
    }

    #[test]
    fn drive_consumes_in_sequence_when_workers_finish_out_of_order() {
        let pool = Pool::spawn(vec![SlowEvens, SlowEvens, SlowEvens, SlowEvens]);
        let mut seen = Vec::new();
        drive(&pool, 32, 8, Ok, |seq, out| {
            assert_eq!(seq, out);
            seen.push(seq);
            Ok(())
        })
        .expect("drive completes");
        pool.shutdown();
        assert_eq!(seen, (0..32).collect::<Vec<_>>());
    }
}

/// Pool-internal error returned by [`Pool::submit`] when a worker's
/// inbound channel has closed, that is, its worker thread has exited.
/// A panicked worker does not close its channel (see
/// [`PoolOutcome::Panicked`]); consumers map this into their own error
/// type via `From<PoolChannelClosed>`.
#[derive(Debug, Clone, Copy)]
pub struct PoolChannelClosed;

impl std::fmt::Display for PoolChannelClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("worker pool channel closed")
    }
}

impl std::error::Error for PoolChannelClosed {}

/// One result delivered on a pool's result channel: either the
/// worker's own outcome for a submitted job, or a [`PoolOutcome::Panicked`]
/// marker. A worker that panics stays alive and answers that job and
/// every later one routed to it with the marker until shutdown, so
/// exactly one result arrives per submitted job and draining the
/// channel ends only when every worker has exited.
pub enum PoolOutcome<O, E> {
    /// The job ran and the worker returned its own outcome.
    Done(Result<O, E>),
    /// The job never produced output: its worker panicked.
    Panicked,
}

/// Per-thread worker state. One instance lives for the lifetime of a
/// pool thread; `process` is called once per submitted work item.
///
/// Implementations should own any expensive, reusable state (codec
/// contexts, scratch buffers) so the hot loop never allocates.
pub trait Worker<W, O, E> {
    /// Processes one work item on the calling worker thread.
    fn process(&mut self, work: W) -> Result<O, E>;
}

/// Persistent worker pool. Generic over the work-item, output, and
/// error types; the worker type is erased at spawn time so one
/// [`Pool`] can wrap any encoder or decoder worker set.
pub struct Pool<W: Send + 'static, O: Send + 'static, E: Send + 'static> {
    n_threads: usize,
    work_txs: Vec<SyncSender<Option<(u64, W)>>>,
    result_rx: Receiver<(u64, PoolOutcome<O, E>)>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl<W: Send + 'static, O: Send + 'static, E: Send + 'static> Pool<W, O, E> {
    /// Spawn `workers.len()` threads, each owning one worker state
    /// instance. Workers are consumed by value; the caller is
    /// responsible for any fallible construction (such as initializing
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
        assert!(!workers.is_empty(), "pool needs at least one worker");
        let n_threads = workers.len();
        let (result_tx, result_rx) = channel::<(u64, PoolOutcome<O, E>)>();
        let mut work_txs = Vec::with_capacity(n_threads);
        let mut handles = Vec::with_capacity(n_threads);

        for mut worker in workers {
            let (work_tx, work_rx) = sync_channel::<Option<(u64, W)>>(2);
            work_txs.push(work_tx);
            let result_tx = result_tx.clone();
            let handle = thread::spawn(move || {
                // A panic poisons this worker instead of killing it: the
                // thread keeps receiving and answers every later item
                // with a Panicked marker without running it, so each
                // submit still gets exactly one result and none is
                // dropped while the thread exits. Exiting on the
                // shutdown sentinel re-raises the original panic.
                let mut poisoned: Option<Box<dyn std::any::Any + Send>> = None;
                while let Ok(Some((seq, work))) = work_rx.recv() {
                    let outcome = if poisoned.is_some() {
                        PoolOutcome::Panicked
                    } else {
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            worker.process(work)
                        })) {
                            Ok(result) => PoolOutcome::Done(result),
                            Err(payload) => {
                                poisoned = Some(payload);
                                PoolOutcome::Panicked
                            }
                        }
                    };
                    if result_tx.send((seq, outcome)).is_err() {
                        // Result channel closed, dispatcher is
                        // unwinding. Stop silently.
                        break;
                    }
                }
                if let Some(payload) = poisoned {
                    std::panic::resume_unwind(payload);
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
    /// A worker whose job panicked is not disconnected: it stays in
    /// its poison loop and later items routed to it are answered with
    /// [`PoolOutcome::Panicked`] instead of running, each still
    /// yielding exactly one result. [`PoolChannelClosed`] is returned
    /// when a slot's channel is disconnected, that is, its worker
    /// thread has exited for a reason other than a job panic.
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
                    // This slot's worker thread has exited; the job
                    // would never be answered, so fail the submit.
                    pending = None;
                }
            }
            if pending.is_none() {
                // Only reachable on a disconnected worker: its item is
                // already consumed, so the whole submit fails.
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
    /// submission sequence number and the worker's outcome.
    ///
    /// Panics only if every worker has exited without producing
    /// any output, which only happens if the pool was shut down
    /// prematurely (a programming error).
    pub fn recv(&self) -> (u64, PoolOutcome<O, E>) {
        self.result_rx
            .recv()
            .expect("worker pool result channel closed unexpectedly")
    }

    /// Block until any worker produces a result, or return `None`
    /// once every worker has exited. A panicked worker keeps answering
    /// with [`PoolOutcome::Panicked`] until shutdown, so `None` means
    /// every submitted job has been delivered.
    pub(crate) fn recv_opt(&self) -> Option<(u64, PoolOutcome<O, E>)> {
        self.result_rx.recv().ok()
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
    debug_assert!(max_in_flight > 0);
    let mut pending: HashMap<u64, O> = HashMap::new();
    let mut submit_seq: u64 = 0;
    let mut write_seq: u64 = 0;
    let mut in_flight: usize = 0;
    let mut run_result: Result<(), E> = Ok(());

    while write_seq < total {
        // Bound in_flight + pending so out-of-order results can't pile up past
        // max_in_flight. Safe: at the cap the drain loop below always frees a slot.
        while run_result.is_ok() && in_flight + pending.len() < max_in_flight && submit_seq < total
        {
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
        let (seq, outcome) = pool.recv();
        in_flight -= 1;
        match outcome {
            PoolOutcome::Done(Ok(out)) => {
                pending.insert(seq, out);
            }
            PoolOutcome::Done(Err(e)) => {
                run_result = Err(e);
                break;
            }
            PoolOutcome::Panicked => {
                run_result = Err(PoolChannelClosed.into());
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
        let (_seq, outcome) = pool.recv();
        in_flight -= 1;
        if run_result.is_ok()
            && let PoolOutcome::Done(Err(e)) = outcome
        {
            run_result = Err(e);
        }
    }

    run_result
}

/// Run `body` with a dedicated writer thread draining a bounded
/// channel into `writer`, so pool decoding and disk writes overlap.
///
/// `body` drives the pool and sends each ordered output chunk into the
/// channel; the channel is closed and the writer joined before this
/// returns, and a panicked writer surfaces as `on_panic`.
pub fn with_writer_thread<W, E, F>(
    writer: &mut W,
    capacity: usize,
    on_panic: E,
    body: F,
) -> Result<(), E>
where
    W: Write + Send,
    E: Send + From<std::io::Error>,
    F: FnOnce(&SyncSender<Vec<u8>>) -> Result<(), E>,
{
    let (tx, rx) = sync_channel::<Vec<u8>>(capacity);

    thread::scope(|s| {
        let writer_slot = writer;
        let handle = s.spawn(move || -> Result<(), E> {
            while let Ok(bytes) = rx.recv() {
                writer_slot.write_all(&bytes)?;
            }
            Ok(())
        });

        let body_result = body(&tx);
        drop(tx);
        // A failed write closes `rx`, which the body only sees as a closed
        // channel; report the writer's I/O error, not that symptom.
        handle.join().unwrap_or(Err(on_panic))?;
        body_result
    })
}

#[cfg(test)]
mod pool_tests {
    use super::*;

    /// Panics on its first job; the pool must keep answering later
    /// items from the poisoned worker instead of dropping them.
    struct PanicFirstWorker;

    impl Worker<u64, u64, std::io::Error> for PanicFirstWorker {
        fn process(&mut self, work: u64) -> Result<u64, std::io::Error> {
            if work == 0 {
                panic!("first job explodes");
            }
            Ok(work)
        }
    }

    #[test]
    fn poisoned_worker_answers_every_later_submit() {
        const TOTAL: u64 = 64;
        let (done_tx, done_rx) = channel::<usize>();
        let collector = std::thread::spawn(move || {
            let pool = Pool::spawn(vec![PanicFirstWorker, PanicFirstWorker, PanicFirstWorker]);
            // Push well past the per-slot buffer of 2 so some submits
            // land on the poisoned worker after its panic.
            for seq in 0..TOTAL {
                pool.submit(seq, seq)
                    .expect("poisoned worker stays connected");
            }
            let mut panicked = 0;
            for _ in 0..TOTAL {
                let (_, outcome) = pool
                    .recv_opt()
                    .expect("worker exited before every result arrived");
                if matches!(outcome, PoolOutcome::Panicked) {
                    panicked += 1;
                }
            }
            pool.shutdown();
            done_tx.send(panicked).ok();
        });
        // A missing outcome means the pool dropped a submit and the
        // old hang is back, so cap the wait instead of blocking forever.
        let panicked = done_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("pool hung: not every job after the panic got an outcome");
        collector.join().expect("collector thread panicked");
        assert!(panicked >= 1, "the panicked job's marker never arrived");
    }
}
