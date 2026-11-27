//! One question asked of several indexes, awaited: the blocking call's
//! groups and its merge by rank, run on the store's pool.

use std::future::Future;
use std::sync::Arc;

#[cfg(feature = "sql")]
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::super::Store;
use super::super::run;
#[cfg(feature = "sql")]
use super::TableIndex;
use super::{Index, Searched};
use crate::Result;
use crate::search::{Among, ManyFound};

impl Store {
    /// One question asked of several indexes in one call, as
    /// [`crate::Store::search`] asks it: each named with
    /// [`among`](ManySearch::among), the call awaited.
    pub fn search(&self, text: &str) -> ManySearch<'_> {
        ManySearch { call: self.blocking().search(text), store: self }
    }
}

/// One question asked of several indexes, run by its awaited
/// [`ManySearch::run`].
#[must_use = "a search runs at `run`"]
pub struct ManySearch<'s> {
    call: crate::search::ManySearch<'s>,
    store: &'s Store,
}

impl std::fmt::Debug for ManySearch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.call, f)
    }
}

impl ManySearch<'_> {
    /// As [`crate::search::ManySearch::among`].
    pub fn among(self, index: &impl Searched, options: impl FnOnce(Among) -> Among) -> Self {
        ManySearch { call: self.call.among(index, options), store: self.store }
    }

    /// Runs the search on the store's pool, as
    /// [`crate::search::ManySearch::run`] does. How many indexes it asks and
    /// their limits and boosts are checked here, so that a call refused for
    /// them answers in its first poll and never reaches the pool.
    pub fn run(self) -> impl Future<Output = Result<ManyFound>> + Send + 'static {
        let (store, pool) = (self.store.blocking().clone(), Arc::clone(self.store.pool()));
        let (text, among) = self.call.asked();
        let read = crate::search::check_among(&among).map(|()| {
            let what = format!("a search among {} indexes", among.len());
            run::read(&pool, what, move || crate::search::search_among(&store, &text, &among))
        });
        async move { read?.await }
    }
}

impl<T> crate::search::SearchedSealed for Index<T> {}

#[cfg(feature = "sql")]
impl<T> crate::search::SearchedSealed for TableIndex<T> {}

impl<T: DeserializeOwned + Send + 'static> Searched for Index<T> {
    fn erased(&self) -> crate::search::Index<Value> {
        self.blocking().erased()
    }
}

#[cfg(feature = "sql")]
impl<T: Serialize + DeserializeOwned + Send + 'static> Searched for TableIndex<T> {
    fn erased(&self) -> crate::search::Index<Value> {
        self.blocking().erased()
    }
}
