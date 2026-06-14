//! Small concurrency helpers shared across modules.

use std::sync::{LockResult, PoisonError};

/// Recover a lock guard even when the lock has been poisoned.
///
/// A lock becomes poisoned only if another thread panicked while holding it.
/// The data behind Archon's locks (resolution caches, action history, the
/// SQLite connection) stays structurally valid in that case, so recovering the
/// guard is preferable to propagating the panic and taking down the daemon or
/// resolver. Callers use it in place of `.expect("lock poisoned")`.
pub(crate) trait LockResultExt<G> {
    /// Return the guard, recovering the inner guard if the lock was poisoned.
    fn recover(self) -> G;
}

impl<G> LockResultExt<G> for LockResult<G> {
    fn recover(self) -> G {
        self.unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::LockResultExt;
    use std::sync::{Arc, Mutex, RwLock};

    #[test]
    fn recovers_mutex_after_poison() {
        let lock = Arc::new(Mutex::new(7u32));
        let poisoner = Arc::clone(&lock);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().recover();
            panic!("poison the mutex");
        })
        .join();

        // The mutex is now poisoned, but recover() still yields the value.
        assert!(lock.lock().is_err());
        assert_eq!(*lock.lock().recover(), 7);
    }

    #[test]
    fn recovers_rwlock_after_poison() {
        let lock = Arc::new(RwLock::new(vec![1, 2, 3]));
        let poisoner = Arc::clone(&lock);
        let _ = std::thread::spawn(move || {
            let mut guard = poisoner.write().recover();
            guard.push(4);
            panic!("poison the rwlock");
        })
        .join();

        assert!(lock.read().is_err());
        assert_eq!(lock.read().recover().len(), 4);
    }
}
