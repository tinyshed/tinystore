//! Documents set by id in a named index and found again by their words, by
//! vectors the program's own model gave them, or by both, in `search.db`:
//! words in any language and any script, a typo forgiven, the exact best by
//! BM25 over the fields and their weights; vectors the exact best by cosine.
//!
//! ```no_run
//! # fn main() -> tinystore::Result<()> {
//! #[derive(serde::Serialize, serde::Deserialize)]
//! struct Article { title: String, body: String }
//!
//! let store = tinystore::Store::open("data", Default::default())?;
//! let articles = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open()?;
//! articles.set("a-17", &Article { title: "Хлеб на закваске".into(), body: "…".into() })?;
//! let found = articles.search("как испечь хлеб").run()?;
//! for hit in found.hits {
//!     println!("{} {}", hit.id, hit.value.title);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! A search answers what scoring every document would, never an estimate,
//! and the next search sees a set. Under the surface, postings are blocks
//! of 128 documents that a search skips while their bound cannot reach the
//! best ten; a set writes to a head that searches read beside them, and the
//! store's background work packs it into the blocks. A search by a vector
//! reads a code of every vector, a byte a dimension, and scores again only
//! those whose bound reaches the best.

mod analyzer;
mod correct;
mod document;
mod engine;
mod execute;
#[cfg(test)]
mod fixture;
mod hybrid;
mod index;
mod kept;
mod pack;
mod postings;
mod purge;
mod read;
mod score;
mod settings;
mod vectors;
mod write;

pub use engine::Maintenance;
pub use index::{Document, DocumentCall, Found, Hit, Index, IndexBuilder, SearchCall};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Result, Store};

impl Store {
    /// The index `name`, made the first time it opens: documents of type
    /// `T` set by id and found again by their words, their vectors or both.
    pub fn index<T: Serialize + DeserializeOwned>(&self, name: &str) -> IndexBuilder<'_, T> {
        IndexBuilder::new(self, name)
    }
}

/// Runs the store's search maintenance now, as a store opened without
/// background work has to: every head packed and the documents gone purged.
pub fn maintain(store: &Store) -> Result<Maintenance> {
    engine::Search::of(store)?.maintain(true)
}
