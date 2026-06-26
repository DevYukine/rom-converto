pub mod fs;
pub mod iso9660;
pub mod maker_codes;
pub mod pixel;
pub mod pread;
pub mod worker_pool;

pub const BYTES_PER_MB: f64 = 1_000_000.0;

/// Re-root a derived output filename into `output_dir`, or return it unchanged
/// when no directory is given.
pub fn place_in_dir(
    derived: &std::path::Path,
    output_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    match output_dir {
        Some(dir) => dir.join(
            derived
                .file_name()
                .expect("a derived output path always has a file name"),
        ),
        None => derived.to_path_buf(),
    }
}

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

/// Await a blocking pipeline while draining its shared byte counter
/// into `progress` every 100 ms; calls `progress.finish()` at the end
/// either way.
pub(crate) async fn await_with_progress<T, E>(
    progress: &dyn ProgressReporter,
    bytes_done: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    mut handle: tokio::task::JoinHandle<Result<T, E>>,
) -> Result<T, E>
where
    E: From<tokio::task::JoinError>,
{
    use std::sync::atomic::Ordering;

    let result = loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => break result,
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    };
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();
    result?
}
