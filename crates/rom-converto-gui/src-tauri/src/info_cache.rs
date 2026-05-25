//! In-process cache keyed on (canonical path, mtime) so the GUI can
//! revisit a recently opened ROM without re-running the info extractor.

use rom_converto_lib::info::InfoResult;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

type Key = (PathBuf, SystemTime);

#[derive(Default)]
pub struct InfoCache {
    entries: Mutex<HashMap<Key, Arc<InfoResult>>>,
}

impl InfoCache {
    pub fn key_for(path: &Path) -> Option<Key> {
        let canonical = std::fs::canonicalize(path).ok()?;
        let mtime = std::fs::metadata(&canonical).ok()?.modified().ok()?;
        Some((canonical, mtime))
    }

    pub fn get(&self, key: &Key) -> Option<Arc<InfoResult>> {
        self.entries.lock().ok()?.get(key).cloned()
    }

    pub fn insert(&self, key: Key, value: Arc<InfoResult>) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.insert(key, value);
        }
    }
}
