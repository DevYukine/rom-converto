//! In-process cache keyed on (canonical path, mtime, keys path) so the GUI
//! can revisit a recently opened ROM without re-running the info extractor.
//! The keys path is part of the key because it changes what the extractor
//! can resolve (e.g. Switch CNMT/control data needs prod.keys).

use rom_converto_lib::info::InfoResult;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

type Key = (PathBuf, SystemTime, Option<PathBuf>);

/// Entry cap. An `InfoResult` carries decoded icon and banner PNGs, so an
/// unbounded map would hold every ROM the user ever inspected in memory.
const CAPACITY: usize = 64;

#[derive(Default)]
struct Entries {
    map: HashMap<Key, Arc<InfoResult>>,
    /// Insertion order, oldest first; drives eviction.
    order: VecDeque<Key>,
}

#[derive(Default)]
pub struct InfoCache {
    entries: Mutex<Entries>,
}

impl InfoCache {
    pub fn key_for(path: &Path, keys: Option<&Path>) -> Option<Key> {
        let canonical = std::fs::canonicalize(path).ok()?;
        let mtime = std::fs::metadata(&canonical).ok()?.modified().ok()?;
        Some((canonical, mtime, keys.map(Path::to_path_buf)))
    }

    pub fn get(&self, key: &Key) -> Option<Arc<InfoResult>> {
        self.entries.lock().ok()?.map.get(key).cloned()
    }

    pub fn insert(&self, key: Key, value: Arc<InfoResult>) {
        if let Ok(mut guard) = self.entries.lock() {
            if guard.map.insert(key.clone(), value).is_none() {
                guard.order.push_back(key);
            }
            while guard.order.len() > CAPACITY {
                if let Some(oldest) = guard.order.pop_front() {
                    guard.map.remove(&oldest);
                }
            }
        }
    }
}
