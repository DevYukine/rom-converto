pub mod group_reader;
pub mod maker_codes;
pub mod pixel;
pub mod pread;
pub mod worker_pool;

pub const BYTES_PER_MB: f64 = 1_000_000.0;

/// Trait for reporting progress from library operations.
///
/// Consumers implement this to bridge progress updates to their
/// preferred UI (CLI progress bars, GUI events, etc.).
pub trait ProgressReporter: Send + Sync {
    fn start(&self, total: u64, msg: &str);
    fn inc(&self, delta: u64);
    fn finish(&self);
}

pub struct NoProgress;

impl ProgressReporter for NoProgress {
    fn start(&self, _: u64, _: &str) {}
    fn inc(&self, _: u64) {}
    fn finish(&self) {}
}
