//! Documents set by id in a named index and found again by their words, by
//! vectors the program's own model gave them, or by both, in `search.db`:
//! words in any language and any script, a typo forgiven, the exact best by
//! BM25 over the fields and their weights; vectors the exact best by cosine;
//! either kept to the documents a filter passes, made of [`eq`], [`is_in`],
//! [`range`], [`missing`], [`or`] and [`and`] as a query's conditions are of
//! `sql::eq` and the rest.
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
//! those whose bound reaches the best; on an index opened with `recall`, it
//! reads only the clusters nearest its question.

mod analyzer;
mod clusters;
mod correct;
mod document;
mod engine;
mod execute;
mod filter;
#[cfg(test)]
mod fixture;
#[cfg(feature = "replication")]
pub(crate) mod forward;
mod hybrid;
mod index;
mod kept;
mod open;
mod pack;
mod postings;
mod purge;
mod rabitq;
mod read;
mod rebuild;
mod score;
mod settings;
mod table;
#[cfg(feature = "sql")]
mod table_index;
mod upkeep;
mod vectors;
mod write;

pub use engine::Maintenance;
pub use filter::{Condition, and, eq, is_in, missing, or, range};
pub use index::{Document, DocumentCall, Found, Hit, Index, IndexBuilder, Rebuild, SearchCall};
pub use open::Status;
pub use rebuild::Listed;
#[cfg(feature = "sql")]
pub(crate) use table_index::{ReadRow, drop_in_file};
#[cfg(feature = "sql")]
pub use table_index::{TableIndex, TableIndexBuilder, TxTableIndex};
#[cfg(all(feature = "sql", feature = "wire"))]
pub(crate) use table_index::{by_cells, found_by, table_as_it_is};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Result, Store};

impl Store {
    /// The index `name`, made the first time it opens: documents of type
    /// `T` set by id and found again by their words, their vectors or both.
    pub fn index<T: Serialize + DeserializeOwned>(&self, name: &str) -> IndexBuilder<'_, T> {
        IndexBuilder::new(self, name)
    }

    /// Drops the index `name` and every document it holds, in the background,
    /// and says whether there was one; a handle of it is refused from then
    /// on, and opening the name again makes a new index. An index no program
    /// opens any more is kept until it is dropped, since another version of
    /// the program may open it again.
    pub fn drop_index(&self, name: &str) -> Result<bool> {
        let within = |error: crate::Error| error.within(format!("index {name}"));
        engine::Search::of(self).and_then(|search| search.drop_index(name)).map_err(within)
    }
}

/// The conditions a search's `where` writes over the wire.
#[cfg(feature = "wire")]
pub(crate) use filter::from_json as conditions;
#[cfg(feature = "wire")]
pub(crate) use index::open_as_it_is;

/// Every index the store's `search.db` holds, by name: its documents and
/// vectors, when a program last opened it, and whether a rebuild is under
/// way.
pub fn list(store: &Store) -> Result<Vec<Listed>> {
    engine::Search::of(store)?.list()
}

/// Runs the store's search maintenance now, as a store opened without
/// background work has to: every head packed and the documents gone purged,
/// in search.db and in each database a table's index was opened in, and a
/// table's rows read the first time.
pub fn maintain(store: &Store) -> Result<Maintenance> {
    let mut done = engine::Search::of(store)?.maintain(true)?;
    for search in engine::InFiles::of(store)?.engines() {
        let more = search.maintain(true)?;
        done.packed += more.packed;
        done.purged += more.purged;
    }
    Ok(done)
}
