//! Shared test-only helpers.

use std::env;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Process-wide lock that serializes environment-variable mutation across tests.
///
/// The process environment is global mutable state; `cargo test` runs tests on
/// multiple threads, so concurrent `set_var`/`remove_var` calls would race.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// RAII guard for mutating environment variables inside a test.
///
/// Acquiring a guard takes the process-wide environment lock, so only one guard
/// is active at a time. Every mutation records the previous value and is undone
/// when the guard drops — including on panic — leaving the environment exactly
/// as it was found. This centralizes the single `unsafe` required by Rust 2024's
/// `std::env::set_var`, keeping individual tests free of unsafe blocks.
pub struct EnvVarGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
}

impl Default for EnvVarGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvVarGuard {
    pub fn new() -> Self {
        Self {
            _lock: env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
            saved: Vec::new(),
        }
    }

    /// Set `key` to `value`, remembering the prior value for restoration.
    pub fn set(&mut self, key: &str, value: &str) -> &mut Self {
        self.saved.push((key.to_string(), env::var(key).ok()));
        // SAFETY: all environment access in tests is serialized by the held lock.
        unsafe { env::set_var(key, value) };
        self
    }

    /// Remove `key`, remembering the prior value for restoration.
    pub fn remove(&mut self, key: &str) -> &mut Self {
        self.saved.push((key.to_string(), env::var(key).ok()));
        // SAFETY: all environment access in tests is serialized by the held lock.
        unsafe { env::remove_var(key) };
        self
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // Restore in reverse order so repeated mutations of the same key unwind
        // back to the original value. The lock is still held here because field
        // drop (which releases it) runs after this method.
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                // SAFETY: still serialized by the held lock.
                Some(v) => unsafe { env::set_var(&key, v) },
                None => unsafe { env::remove_var(&key) },
            }
        }
    }
}
