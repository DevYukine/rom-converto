use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn resolve_log_levels(quiet: bool, verbose: u8) -> (log::LevelFilter, log::LevelFilter) {
    use log::LevelFilter::{Debug, Info, Trace, Warn};
    if quiet {
        return (Warn, Warn);
    }
    match verbose {
        0 => (Info, Warn),
        1 => (Debug, Warn),
        2 => (Trace, Warn),
        _ => (Trace, Trace),
    }
}

pub struct DualLogger {
    console: env_logger::Logger,
    file: Mutex<BufWriter<File>>,
}

impl DualLogger {
    pub fn new(console: env_logger::Logger, file: BufWriter<File>) -> Self {
        Self {
            console,
            file: Mutex::new(file),
        }
    }
}

impl log::Log for DualLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.console.enabled(metadata) || metadata.level() <= log::Level::Trace
    }

    fn log(&self, record: &log::Record) {
        self.console.log(record);
        if let Ok(mut guard) = self.file.lock() {
            let _ = writeln!(
                guard,
                "[{}] [{}] {}: {}",
                format_timestamp(),
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        self.console.flush();
        if let Ok(mut guard) = self.file.lock() {
            let _ = guard.flush();
        }
    }
}

fn format_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// Standard days-to-civil-date conversion (Howard Hinnant's algorithm); the
// arithmetic is non-obvious so it lives in one place rather than inline.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::LevelFilter::{Debug, Info, Trace, Warn};

    #[test]
    fn quiet_wins_over_verbose() {
        assert_eq!(resolve_log_levels(true, 0), (Warn, Warn));
        assert_eq!(resolve_log_levels(true, 3), (Warn, Warn));
    }

    #[test]
    fn default_is_info_project_warn_global() {
        assert_eq!(resolve_log_levels(false, 0), (Info, Warn));
    }

    #[test]
    fn single_v_is_project_debug() {
        assert_eq!(resolve_log_levels(false, 1), (Debug, Warn));
    }

    #[test]
    fn double_v_is_project_trace() {
        assert_eq!(resolve_log_levels(false, 2), (Trace, Warn));
    }

    #[test]
    fn triple_v_and_above_opens_global_trace() {
        assert_eq!(resolve_log_levels(false, 3), (Trace, Trace));
        assert_eq!(resolve_log_levels(false, 255), (Trace, Trace));
    }
}
