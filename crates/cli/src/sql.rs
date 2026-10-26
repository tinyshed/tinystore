//! `tinystore sql`, `migrate` and `schema`: a query on one of the
//! application's databases, its migration files applied, and its tables as
//! the statements that made them.
//!
//! ```text
//! tinystore sql ./data app "select id, email from users limit 2"
//! id  email
//! 1   ada@example.com
//! 2   alan@example.com
//! ```
//!
//! A query reads beside the program or server that holds the directory; a
//! statement that writes needs `--write`, and the directory no one else holds.

use std::fmt;
use std::path::Path;
use std::process::ExitCode;

use nu_ansi_term::Style;
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use tinystore::sql::Database;
use tinystore::{ErrorKind, Options, Store};

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;
use crate::lines::{Terminal, paint};
use crate::on_disk;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::SQL, args, query)
}

pub(crate) fn migrate(args: &[String]) -> ExitCode {
    asked::run(&help::MIGRATE, args, apply)
}

pub(crate) fn schema(args: &[String]) -> ExitCode {
    asked::run(&help::SCHEMA, args, tables)
}

fn query(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_or_here(2)?;
    let [database, statement] = &rest[..] else {
        return Err(wrong("a database and a query: sql ./data app \"select count(*) from users\""));
    };
    let json = asked.has("--json");
    if asked.has("--write") {
        let store = held(&dir)?;
        let written = write(&store, database, statement, json);
        // The write's own failure is the answer; a close that fails after it
        // succeeded is.
        let closed = store.close();
        return written.and_then(|()| Ok(closed?));
    }
    let store = Store::open_beside(&dir).map_err(failed)?;
    let db = opened(&store, database)?;
    let rows: Vec<Row> = db.all(statement.as_str()).map_err(|error| {
        if error.to_string().contains("readonly") || store.is_beside() && error.kind() == ErrorKind::Invalid {
            failed(format!(
                "{error}\nthe statement writes: add --write, with no program or server holding {}",
                dir.display()
            ))
        } else {
            failed(error)
        }
    })?;
    print_rows(&rows, json)
}

/// Runs a statement that may write, on the store this process holds.
fn write(store: &Store, database: &str, statement: &str, json: bool) -> Done {
    let db = opened(store, database)?;
    let reads = ["select", "with", "values", "explain"]
        .iter()
        .any(|word| statement.trim_start().to_lowercase().starts_with(word));
    if reads || statement.to_lowercase().contains("returning") {
        let rows: Vec<Row> = db.all(statement)?;
        print_rows(&rows, json)?;
    } else {
        let done = db.exec(statement)?;
        if json {
            outln!("{}", serde_json::json!({ "changes": done.changes, "last_insert_rowid": done.last_insert_rowid }))?;
        } else {
            outln!("{} rows changed", done.changes)?;
        }
    }
    Ok(())
}

/// The store, held by this process alone: a write and a migration need the
/// directory no program or server holds.
fn held(dir: &Path) -> Result<Store, asked::Refused> {
    asked::existing(dir)?;
    match Store::open(dir, Options { background: false, ..Options::default() }) {
        Ok(store) => Ok(store),
        Err(error) if error.kind() == ErrorKind::InUse => {
            Err(failed(format!("{error}\na program or a server holds {}: stop it, or write through it", dir.display())))
        }
        Err(error) => Err(error.into()),
    }
}

/// A database as it is, without migrations: one the store does not hold is
/// named, rather than made.
pub(crate) fn opened(store: &Store, database: &str) -> Result<Database, asked::Refused> {
    database_held(store.dir(), database)?;
    Ok(store.database(database).open()?)
}

/// Refuses a database the store does not hold, naming the nearest it does:
/// opening one makes it, so a typo would otherwise make an empty one.
pub(crate) fn database_held(dir: &Path, database: &str) -> Result<(), asked::Refused> {
    if dir.join("sql").join(format!("{database}.db")).exists() {
        return Ok(());
    }
    let known = on_disk::databases(dir);
    let names: Vec<&str> = known.iter().map(String::as_str).collect();
    let held = if known.is_empty() { "none".to_owned() } else { known.join(", ") };
    Err(failed(format!(
        "{} holds no database {database}{}; it holds {held}",
        dir.display(),
        crate::meant(database, &names)
    )))
}

fn apply(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_needed("migrate")?;
    let [database, migrations] = &rest[..] else {
        return Err(wrong("a database and its migrations: migrate ./data app ./migrations"));
    };
    if !Path::new(migrations).is_dir() {
        return Err(failed(format!("{migrations}: no such directory of migrations")));
    }
    let store = held(&dir)?;
    let before = applied(&store, database)?;
    let opened = store.database(database).migrations(Path::new(migrations)).open();
    let after = opened.map_err(asked::Refused::from).and_then(|_| applied(&store, database));
    let closed = store.close();
    let after = after?;
    closed?;
    let new: Vec<&String> = after.iter().filter(|name| !before.contains(name)).collect();
    match new.len() {
        0 => outln!("● {database}  up to date: {} migrations applied", after.len()),
        _ => outln!("● {database}  applied {}", new.iter().map(|name| name.as_str()).collect::<Vec<_>>().join(", ")),
    }
}

/// The migrations a database applied, by their names: its own, not those of
/// the buckets and queues it keeps; none for a database the store does not
/// hold yet.
fn applied(store: &Store, database: &str) -> Result<Vec<String>, asked::Refused> {
    if !store.dir().join("sql").join(format!("{database}.db")).exists() {
        return Ok(Vec::new());
    }
    let db = store.database(database).open()?;
    Ok(db.all(APPLIED)?)
}

const APPLIED: &str = "select name from _tinystore_migrations where history = 'migrations' order by version";
/// A database's own tables, views, indexes and triggers. `glob`, not `like`,
/// since `like` reads `_` as any character.
const SCHEMA: &str = "select type, name, sql from sqlite_schema where sql is not null
    and name not glob 'sqlite_*' and name not glob '_tinystore_*'
    order by case type when 'table' then 0 when 'view' then 1 else 2 end, name";

fn tables(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_needed("schema")?;
    let store = Store::open_beside(&dir).map_err(failed)?;
    let json = asked.has("--json");
    match &rest[..] {
        [] => {
            let databases = on_disk::databases(&dir);
            if json {
                outln!("{}", serde_json::json!(databases))?;
            } else if databases.is_empty() {
                outln!("{} holds no database: a program opens one with store.database(\"app\")", dir.display())?;
            } else {
                for database in databases {
                    outln!("{database}")?;
                }
            }
            Ok(())
        }
        [database] => {
            let db = opened(&store, database)?;
            let made: Vec<(String, String, String)> = db.all(SCHEMA)?;
            if json {
                let shown: Vec<serde_json::Value> = made
                    .iter()
                    .map(|(kind, name, sql)| serde_json::json!({ "type": kind, "name": name, "sql": sql }))
                    .collect();
                outln!("{}", serde_json::Value::Array(shown))?;
            } else if made.is_empty() {
                outln!("{database} holds no table yet")?;
            } else {
                let statements: Vec<String> = made.iter().map(|(_, _, sql)| format!("{sql};")).collect();
                outln!("{}", statements.join("\n\n"))?;
            }
            Ok(())
        }
        _ => Err(wrong("a directory and a database at most: schema ./data app")),
    }
}

/// A read-only query's rows as JSON, `most` at most, for the MCP's tool.
pub(crate) fn rows_json(
    store: &Store,
    database: &str,
    statement: &str,
    most: usize,
) -> Result<serde_json::Value, String> {
    let db = opened(store, database).map_err(|refused| refused.to_string())?;
    let rows: Vec<Row> = db.all(statement).map_err(|error| error.to_string())?;
    let shown: Vec<serde_json::Value> = rows
        .iter()
        .take(most)
        .map(|row| {
            serde_json::Value::Object(row.0.iter().map(|(column, cell)| (column.clone(), cell.json())).collect())
        })
        .collect();
    Ok(serde_json::Value::Array(shown))
}

/// Prints rows as a table a person reads, or one JSON object a row.
fn print_rows(rows: &[Row], json: bool) -> Done {
    if json {
        for row in rows {
            let object: serde_json::Map<String, serde_json::Value> =
                row.0.iter().map(|(column, cell)| (column.clone(), cell.json())).collect();
            outln!("{}", serde_json::Value::Object(object))?;
        }
        return Ok(());
    }
    let Some(first) = rows.first() else {
        return outln!("no rows");
    };
    let colours = Terminal::of_stdout().colours();
    let columns: Vec<&String> = first.0.iter().map(|(column, _)| column).collect();
    let shown: Vec<Vec<String>> =
        rows.iter().map(|row| row.0.iter().map(|(_, cell)| cell.to_string()).collect()).collect();
    let widths: Vec<usize> = (0..columns.len())
        .map(|at| {
            shown
                .iter()
                .map(|cells| cells.get(at).map_or(0, |cell| cell.chars().count()))
                .chain([columns[at].chars().count()])
                .max()
                .unwrap_or(0)
                .min(60)
        })
        .collect();
    let line = |cells: Vec<String>| {
        let padded: Vec<String> = cells.iter().zip(&widths).map(|(cell, width)| format!("{cell:width$}")).collect();
        padded.join("  ").trim_end().to_owned()
    };
    outln!("{}", paint(colours, Style::new().bold(), line(columns.iter().map(|column| (*column).clone()).collect())))?;
    for cells in shown {
        outln!("{}", line(cells))?;
    }
    if rows.len() > 20 {
        outln!("{}", paint(colours, Style::new().dimmed(), format!("{} rows", rows.len())))?;
    }
    Ok(())
}

/// A row by its columns in their order.
struct Row(Vec<(String, Cell)>);

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
enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Cell {
    fn json(&self) -> serde_json::Value {
        match self {
            Cell::Null => serde_json::Value::Null,
            Cell::Integer(integer) => (*integer).into(),
            Cell::Real(real) => serde_json::Number::from_f64(*real).map_or(serde_json::Value::Null, Into::into),
            Cell::Text(text) => text.clone().into(),
            Cell::Blob(bytes) => crate::local::hex(bytes).into(),
        }
    }
}

impl fmt::Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cell::Null => f.write_str("null"),
            Cell::Integer(integer) => write!(f, "{integer}"),
            Cell::Real(real) => write!(f, "{real}"),
            Cell::Text(text) => f.write_str(&text.replace('\n', "⏎")),
            Cell::Blob(bytes) => {
                let hex = crate::local::hex(&bytes[..bytes.len().min(16)]);
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
    fn a_database_the_store_does_not_hold_is_named_with_those_it_does() {
        let (dir, _holder) = store_with_users();
        let beside = Store::open_beside(dir.path()).unwrap();
        let Err(asked::Refused::Failed(why)) = opened(&beside, "ap") else { panic!("opened") };
        assert!(why.contains("holds no database ap") && why.contains("it holds app"), "{why}");
    }
}
