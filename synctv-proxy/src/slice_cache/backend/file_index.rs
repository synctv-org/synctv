use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use dashmap::DashMap;

/// A single entry in the in-memory file index.
pub(super) struct FileIndexEntry {
    pub path: PathBuf,
    pub data_size: u64,
    pub inserted_at_millis: u64,
    pub ttl_secs: u64,
    pub last_accessed: AtomicU64,
}

/// The in-memory index that mirrors what is on disk.
pub(super) struct FileIndex {
    pub entries: DashMap<String, FileIndexEntry>,
    total_size: AtomicU64,
    mutation_lock: Mutex<()>,
}

impl FileIndex {
    pub(super) fn new() -> Self {
        Self {
            entries: DashMap::new(),
            total_size: AtomicU64::new(0),
            mutation_lock: Mutex::new(()),
        }
    }

    pub(super) fn insert(&self, key: String, entry: FileIndexEntry) {
        let _guard = self
            .mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let new_size = entry.data_size;
        let old_size = self
            .entries
            .insert(key, entry)
            .map_or(0, |old| old.data_size);
        let current = self.total_size.load(Ordering::Relaxed);
        let base = current.saturating_sub(old_size);
        if current == u64::MAX || base.checked_add(new_size).is_none() {
            self.recompute_total_size();
        } else {
            self.total_size.store(base + new_size, Ordering::Relaxed);
        }
    }

    pub(super) fn remove(&self, key: &str) -> Option<(String, FileIndexEntry)> {
        let _guard = self
            .mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((key, value)) = self.entries.remove(key) {
            let current = self.total_size.load(Ordering::Relaxed);
            if current == u64::MAX {
                self.recompute_total_size();
            } else {
                self.total_size
                    .store(current.saturating_sub(value.data_size), Ordering::Relaxed);
            }
            Some((key, value))
        } else {
            None
        }
    }

    pub(super) fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub(super) fn total_size(&self) -> u64 {
        self.total_size.load(Ordering::Relaxed)
    }

    fn recompute_total_size(&self) {
        let total = self
            .entries
            .iter()
            .fold(0u64, |total, entry| total.saturating_add(entry.data_size));
        self.total_size.store(total, Ordering::Relaxed);
    }
}

/// Summary returned by `FileBackend::load_index`.
#[derive(Debug, Default)]
pub struct LoadResult {
    pub loaded: u64,
    pub deleted: u64,
    pub errors: u64,
    pub total_bytes: u64,
}
