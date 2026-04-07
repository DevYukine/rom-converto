use rom_converto_lib::util::ProgressReporter;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Serialize)]
struct ProgressEvent {
    task_id: String,
    kind: String,
    total: u64,
    current: u64,
    message: String,
}

/// Bridges the library's `ProgressReporter` trait to Tauri events.
///
/// Emits `progress` events that the Vue frontend listens to.
pub struct TauriProgress {
    app: AppHandle,
    task_id: String,
    current: AtomicU64,
}

impl TauriProgress {
    pub fn new(app: AppHandle, task_id: impl Into<String>) -> Self {
        Self {
            app,
            task_id: task_id.into(),
            current: AtomicU64::new(0),
        }
    }
}

impl ProgressReporter for TauriProgress {
    fn start(&self, total: u64, msg: &str) {
        self.current.store(0, Ordering::Relaxed);
        let _ = self.app.emit(
            "progress",
            ProgressEvent {
                task_id: self.task_id.clone(),
                kind: "start".to_string(),
                total,
                current: 0,
                message: msg.to_string(),
            },
        );
    }

    fn inc(&self, delta: u64) {
        let current = self.current.fetch_add(delta, Ordering::Relaxed) + delta;
        let _ = self.app.emit(
            "progress",
            ProgressEvent {
                task_id: self.task_id.clone(),
                kind: "inc".to_string(),
                total: 0,
                current,
                message: String::new(),
            },
        );
    }

    fn finish(&self) {
        let _ = self.app.emit(
            "progress",
            ProgressEvent {
                task_id: self.task_id.clone(),
                kind: "finish".to_string(),
                total: 0,
                current: 0,
                message: String::new(),
            },
        );
    }
}
