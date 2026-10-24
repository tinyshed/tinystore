//! The application's own SQLite databases: its tables, joins, reports and
//! transactions, in `sql/<name>.db`. SQL stays SQL; TinyStore does the work
//! around it: the file and its connections, migrations checked at every open,
//! writes from many callers sharing a commit, and values that come back as
//! they went in.
//!
//! ```no_run
//! # use tinystore::sql;
//! # #[derive(serde::Deserialize)]
//! # struct Note { id: String, title: String, done: bool }
//! # fn main() -> tinystore::Result<()> {
//! # let store = tinystore::Store::open("data", Default::default())?;
//! # let user_id = 42;
//! let db = store.database("app").migrations("migrations").open()?;
//! db.exec(sql!("insert into notes (id, author_id, title) values (?, ?, ?)", "0192f2a4", user_id, "Buy milk"))?;
//! let mine: Vec<Note> = db.all(sql!("select id, title, done from notes where author_id = ?", user_id))?;
//! # Ok(())
//! # }
//! ```
//!
//! docs/guides/sql/README.md is the guide: the TypeScript, Python and Go spellings, and
//! why each call is as it is.

mod batch;
#[cfg(feature = "wire")]
mod columns;
mod database;
mod engine;
#[cfg(feature = "replication")]
pub(crate) mod forward;
mod included;
mod migrations;
mod pieces;
mod query;
mod rows;
mod run;
mod statement;
mod tx;
mod values;

pub use batch::Write;
pub use database::{Database, DatabaseBuilder, Done};
pub use migrations::Migrations;
pub use pieces::{and, eq, has, ident, is_in, json, list, or, value};
pub use query::{Direction, Page, Query, Table, Upsert};
pub use statement::Sql;
pub use tx::Tx;

// What the wire takes of sql: its answers by rows and by columns, and their values.
#[cfg(feature = "wire")]
pub(crate) use {
    columns::{Column, Columns},
    rows::{Answer, Held, Keep, Kept},
    tx::BOUND as TX_BOUND,
    values::Value,
};

// What the wire and the async API both take: a statement's rows, and what a call wants of them.
#[cfg(any(feature = "wire", feature = "async"))]
pub(crate) use {rows::Rows, run::Wanted};

// What the async API's queries are made of: the blocking builder's parts and statements.
#[cfg(feature = "async")]
pub(crate) use query::{Paging, Parts, insert_sql, plan_of, upsert_sql};

// What a table's search index reads of its hits' rows.
#[cfg(all(feature = "search", feature = "wire"))]
pub(crate) use run::cells_on;
#[cfg(feature = "search")]
pub(crate) use run::rows_on;

#[cfg(test)]
#[path = "database_tests.rs"]
mod database_tests;
#[cfg(test)]
#[path = "query_tests.rs"]
mod query_tests;
