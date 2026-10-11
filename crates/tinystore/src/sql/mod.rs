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
//! plan/api/sqldb.md is the book: the TypeScript, Python and Go spellings, and
//! why each call is as it is.

mod columns;
mod database;
mod engine;
mod included;
mod migrations;
mod pieces;
mod query;
mod rows;
mod run;
mod statement;
mod tx;
mod values;

pub use database::{Database, DatabaseBuilder, Done};
pub use migrations::Migrations;
pub use pieces::{and, eq, has, ident, is_in, json, list, or, value};
pub use query::{Direction, Page, Query, Table, Upsert};
pub use statement::Sql;
pub use tx::Tx;

pub(crate) use columns::{Column, Columns};
pub(crate) use rows::{Answer, Held, Rows};
pub(crate) use run::Wanted;
pub(crate) use values::Value;

#[cfg(test)]
#[path = "database_tests.rs"]
mod database_tests;
#[cfg(test)]
#[path = "query_tests.rs"]
mod query_tests;
