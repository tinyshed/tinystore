//! What every part of the store takes a mutex with.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Takes `mutex`, poisoned or not: a panic is caught where it happens, and
/// whoever holds the lock next goes on with what the mutex holds.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
