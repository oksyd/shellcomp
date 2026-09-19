use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

/// In-process serialization for paths in one lock domain.
#[derive(Default)]
pub(crate) struct PathLocks(Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>);

impl PathLocks {
    pub(crate) fn with_lock<R>(&self, path: &Path, run: impl FnOnce() -> R) -> R {
        let lock = {
            let mut locks = self.0.lock().unwrap_or_else(|error| error.into_inner());
            // Keep only active operations, rather than retaining every path ever used.
            locks.retain(|_, lock| lock.strong_count() != 0);
            if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(Mutex::new(()));
                locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
                lock
            }
        };
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        run()
    }
}
