pub mod http;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rom_converto_lib::util::ProgressReporter;
use std::sync::Mutex;

const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})";

/// Bridges the library's `ProgressReporter` trait to indicatif `ProgressBar`.
pub struct IndicatifProgress {
    mp: MultiProgress,
    bar: Mutex<Option<ProgressBar>>,
}

impl IndicatifProgress {
    pub fn new(mp: MultiProgress) -> Self {
        Self {
            mp,
            bar: Mutex::new(None),
        }
    }
}

impl ProgressReporter for IndicatifProgress {
    fn start(&self, total: u64, msg: &str) {
        let pg = self.mp.add(ProgressBar::new(total));
        let style = ProgressStyle::default_bar()
            .template(PROGRESS_TEMPLATE)
            .expect("valid progress template")
            .progress_chars("#>-");
        pg.set_style(style);
        pg.set_message(msg.to_string());
        *self.bar.lock().unwrap() = Some(pg);
    }

    fn inc(&self, delta: u64) {
        if let Some(bar) = self.bar.lock().unwrap().as_ref() {
            bar.inc(delta);
        }
    }

    fn finish(&self) {
        if let Some(bar) = self.bar.lock().unwrap().take() {
            bar.finish_and_clear();
        }
    }
}
