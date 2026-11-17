//! A set of files as a cache, awaited: a change of its ceiling and a trim,
//! each run to its end on the store's pool once polled, as the blocking calls
//! of [`crate::blobs::SetMaxSize`] and [`crate::blobs::Files::trim`] run.

use std::future::Future;

use super::{Files, describe};
use crate::Result;
use crate::aio::run;
use crate::blobs::{Trim, WhenFull};

impl Files {
    /// Changes the set's ceiling for every handle, as
    /// [`crate::blobs::Files::set_max_size`] does; a lower one evicts at once.
    pub fn set_max_size(&self, bytes: u64) -> SetMaxSize {
        SetMaxSize { files: self.clone(), max_size: Some(bytes), if_current: None, when_full: None }
    }

    /// Removes the set's ceiling.
    pub fn remove_max_size(&self) -> SetMaxSize {
        SetMaxSize { files: self.clone(), max_size: None, if_current: None, when_full: None }
    }

    /// Removes files in the eviction's order until the set holds at most `to`
    /// bytes, as [`crate::blobs::Files::trim`] does, a batch a commit.
    pub fn trim(&self, to: u64) -> impl Future<Output = Result<Trim>> + Send + 'static {
        let files = self.blocking.clone();
        run::blocking(&self.pool, None, describe(&files, "", "a trim"), move || files.trim(to))
    }
}

/// A change of a set's ceiling, its condition before [`SetMaxSize::apply`].
#[derive(Debug)]
#[must_use = "a change of the ceiling does nothing until apply() is awaited"]
pub struct SetMaxSize {
    files: Files,
    max_size: Option<u64>,
    if_current: Option<u64>,
    when_full: Option<WhenFull>,
}

impl SetMaxSize {
    /// Changes the ceiling only while the set keeps `bytes`.
    pub fn if_current(mut self, bytes: u64) -> Self {
        self.if_current = Some(bytes);
        self
    }

    /// What a write past the new ceiling does; what the set does now when
    /// not given.
    pub fn when_full(mut self, when_full: WhenFull) -> Self {
        self.when_full = Some(when_full);
        self
    }

    /// Writes the ceiling on the store's pool, nothing when the set keeps it.
    pub fn apply(self) -> impl Future<Output = Result<()>> + Send + 'static {
        let SetMaxSize { files, max_size, if_current, when_full } = self;
        let (blocking, pool) = (files.blocking, files.pool);
        let what = describe(&blocking, "", "a change of its ceiling");
        run::blocking(&pool, None, what, move || {
            let mut change = match max_size {
                Some(bytes) => blocking.set_max_size(bytes),
                None => blocking.remove_max_size(),
            };
            if let Some(current) = if_current {
                change = change.if_current(current);
            }
            if let Some(when_full) = when_full {
                change = change.when_full(when_full);
            }
            change.apply()
        })
    }
}
