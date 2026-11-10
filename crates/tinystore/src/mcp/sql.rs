//! The tool `sql_query`, and a row as `tinystore sql` reads it too: its
//! columns in their order, each value of the kind SQLite keeps.

use std::fmt;
use std::sync::Arc;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde_json::{Value, json};

use super::given::database_held;
use super::store::{Arguments, MOST, Opener, opened as store_opened, schema, text};
use super::tool::Tool;
use crate::sql::Database;
use crate::text::hex;
use crate::{ErrorKind, Store};

pub(super) fn tools(open: &Opener) -> Vec<Tool> {
    let open = Arc::clone(open);
    let takes = json!({
        "database": text("the database, as app"),
        "query": text("one statement that reads, as select count(*) from users"),
    });
    let description = "Rows of a read-only SQL query on one of the application's databases, sql/<database>.db, 200 \
                       rows at most: a query that gives more is refused, so page it with limit and offset.";
    vec![Tool::reads("sql_query", description, schema(takes, &["database", "query"]), move |given, _| {
        let given = Arguments { tool: "sql_query", given };
        Ok(rows(&store_opened(&open)?, given.needed("database")?, given.needed("query")?, MOST)?)
    })]
}

/// A database as it is, without migrations: one the store does not hold is
/// named, rather than made.
pub fn opened(store: &Store, database: &str) -> Result<Database, String> {
    database_held(store.dir(), database)?;
    store.database(database).open().map_err(|error| error.to_string())
}

/// A query's rows as JSON, each an object of its columns, `most` at most: a
/// query that gives more is refused rather than cut, and its rows are read
/// no further than the one past `most`.
pub fn rows(store: &Store, database: &str, statement: &str, most: usize) -> Result<Value, String> {
    let db = opened(store, database)?;
    let whole = || db.all::<Row>(statement);
    let read = match bounded(statement, most) {
        // A query that fails is read again as it was given, so that its
        // error names the query rather than the one it was read within.
        Some(within) => db.all::<Row>(within).or_else(|error| match error.kind() {
            ErrorKind::Cancelled => Err(error),
            _ => whole(),
        }),
        None => whole(),
    };
    let rows = read.map_err(|error| error.to_string())?;
    if rows.len() > most {
        return Err(format!("the query gives more than the {most} rows a call answers: add limit and offset"));
    }
    Ok(Value::Array(rows.iter().map(Row::json).collect()))
}

/// A query read as a subquery whose rows stop one past `most`, so that more
/// rows are known without reading them: a `select`, a `with` or a `values`,
/// its last `;` taken off. None for another statement, as an `explain`,
/// which is read whole.
fn bounded(statement: &str, most: usize) -> Option<String> {
    let query = statement.trim_end().trim_end_matches(';');
    let first: String = query.trim_start().chars().take_while(char::is_ascii_alphabetic).collect();
    let reads = ["select", "with", "values"].iter().any(|word| first.eq_ignore_ascii_case(word));
    // on a line of its own, so that a comment the query ends with ends before the `)`
    reads.then(|| format!("select * from (\n{query}\n) limit {}", most.saturating_add(1)))
}

/// A row by its columns in their order.
#[derive(Debug)]
pub struct Row(pub Vec<(String, Cell)>);

impl Row {
    /// The row as one JSON object.
    pub fn json(&self) -> Value {
        Value::Object(self.0.iter().map(|(column, cell)| (column.clone(), cell.json())).collect())
    }
}

impl<'de> Deserialize<'de> for Row {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Row, D::Error> {
        deserializer.deserialize_map(Columns)
    }
}

struct Columns;

impl<'de> Visitor<'de> for Columns {
    type Value = Row;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a row")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut columns: A) -> Result<Row, A::Error> {
        let mut row = Vec::new();
        while let Some((column, cell)) = columns.next_entry::<String, Cell>()? {
            row.push((column, cell));
        }
        Ok(Row(row))
    }
}

/// A value as SQLite keeps it.
#[derive(Debug, PartialEq)]
pub enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Cell {
    /// The value as JSON: a blob as its hex.
    pub fn json(&self) -> Value {
        match self {
            Cell::Null => Value::Null,
            Cell::Integer(integer) => (*integer).into(),
            Cell::Real(real) => serde_json::Number::from_f64(*real).map_or(Value::Null, Into::into),
            Cell::Text(text) => text.clone().into(),
            Cell::Blob(bytes) => hex(bytes).into(),
        }
    }
}

impl fmt::Display for Cell {
    /// The value in a table a person reads: text on one line, a blob as its
    /// first sixteen bytes and its size.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cell::Null => f.write_str("null"),
            Cell::Integer(integer) => write!(f, "{integer}"),
            Cell::Real(real) => write!(f, "{real}"),
            Cell::Text(text) => f.write_str(&text.replace('\n', "⏎")),
            Cell::Blob(bytes) => {
                let hex = hex(&bytes[..bytes.len().min(16)]);
                let more = if bytes.len() > 16 { "…" } else { "" };
                write!(f, "x'{hex}{more}' ({} bytes)", bytes.len())
            }
        }
    }
}

impl<'de> Deserialize<'de> for Cell {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Cell, D::Error> {
        deserializer.deserialize_any(Cells)
    }
}

struct Cells;

impl Visitor<'_> for Cells {
    type Value = Cell;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Cell, E> {
        Ok(Cell::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Cell, E> {
        Ok(Cell::Null)
    }

    fn visit_i64<E: de::Error>(self, integer: i64) -> Result<Cell, E> {
        Ok(Cell::Integer(integer))
    }

    fn visit_u64<E: de::Error>(self, integer: u64) -> Result<Cell, E> {
        Ok(i64::try_from(integer).map_or(Cell::Real(integer as f64), Cell::Integer))
    }

    fn visit_f64<E: de::Error>(self, real: f64) -> Result<Cell, E> {
        Ok(Cell::Real(real))
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Cell, E> {
        Ok(Cell::Text(text.to_owned()))
    }

    fn visit_string<E: de::Error>(self, text: String) -> Result<Cell, E> {
        Ok(Cell::Text(text))
    }

    fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Cell, E> {
        Ok(Cell::Blob(bytes.to_vec()))
    }

    fn visit_byte_buf<E: de::Error>(self, bytes: Vec<u8>) -> Result<Cell, E> {
        Ok(Cell::Blob(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;

    fn store_with_users() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let db = store
            .database("app")
            .migrations([(
                "0001_users.sql",
                "create table users (id integer primary key, email text, avatar blob, score real)",
            )])
            .open()
            .unwrap();
        db.exec(
            "insert into users (email, avatar, score) values ('ada@example.com', x'0102', 1.5), (null, null, null)",
        )
        .unwrap();
        (dir, store)
    }

    #[test]
    fn a_rows_columns_keep_their_order_and_their_values_their_kinds() {
        let (dir, _holder) = store_with_users();
        let beside = Store::open_beside(dir.path()).unwrap();
        let rows: Vec<Row> =
            opened(&beside, "app").unwrap().all("select id, email, avatar, score from users order by id").unwrap();
        let columns: Vec<&str> = rows[0].0.iter().map(|(column, _)| column.as_str()).collect();
        assert_eq!(columns, ["id", "email", "avatar", "score"]);
        let cells: Vec<&Cell> = rows[0].0.iter().map(|(_, cell)| cell).collect();
        assert_eq!(
            cells,
            [&Cell::Integer(1), &Cell::Text("ada@example.com".to_owned()), &Cell::Blob(vec![1, 2]), &Cell::Real(1.5)]
        );
        assert_eq!(rows[1].0[1].1, Cell::Null);
        assert_eq!(Cell::Blob(vec![1, 2]).to_string(), "x'0102' (2 bytes)");
    }

    #[test]
    fn a_query_is_read_within_a_bound_one_past_its_most_and_another_statement_as_it_is() {
        assert_eq!(bounded("select 1;", 200).as_deref(), Some("select * from (\nselect 1\n) limit 201"));
        let with = "  WITH x AS (select 1) select * from x -- the last\n";
        assert_eq!(
            bounded(with, 2).as_deref(),
            Some("select * from (\n  WITH x AS (select 1) select * from x -- the last\n) limit 3")
        );
        assert!(bounded("values (1), (2)", 200).is_some());
        assert_eq!(bounded("explain query plan select 1", 200), None);
        assert_eq!(bounded("selection", 200), None, "a word that starts like select is not one");
    }

    #[test]
    fn a_database_the_store_does_not_hold_is_named_with_those_it_does() {
        let (dir, _holder) = store_with_users();
        let beside = Store::open_beside(dir.path()).unwrap();
        let why = opened(&beside, "ap").unwrap_err();
        assert!(why.contains("holds no database ap") && why.contains("it holds app"), "{why}");
    }
}
