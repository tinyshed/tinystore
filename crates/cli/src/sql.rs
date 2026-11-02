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
//! statement that writes needs `--write`, and runs on the store this command
//! holds when no one else does, or through the server that holds it.

use std::path::Path;
use std::process::ExitCode;

use nu_ansi_term::Style;
use tinystore::mcp::sql::Row;
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
        asked::existing(&dir)?;
        let store = match Store::open(&dir, Options { background: false, ..Options::default() }) {
            Ok(store) => store,
            Err(error) if error.kind() == ErrorKind::InUse => return written_through(&dir, database, statement, json),
            Err(error) => return Err(error.into()),
        };
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
    if gives_rows(statement) {
        let rows: Vec<Row> = db.all(statement)?;
        return print_rows(&rows, json);
    }
    let done = db.exec(statement)?;
    print_done(done.changes, done.last_insert_rowid, json)
}

/// Runs a statement that writes through the server that holds the
/// directory, once, since a statement sent again may write twice. One that
/// gives rows back is refused: the server answers it with its rows, which
/// this command does not read.
fn written_through(dir: &Path, database: &str, statement: &str, json: bool) -> Done {
    if gives_rows(statement) {
        return Err(failed(format!(
            "a program or a server holds {}, and a statement that gives rows back writes through it from a program \
             alone; one without returning runs here",
            dir.display()
        )));
    }
    database_held(dir, database)?;
    let (named, text) = (database.to_owned(), statement.to_owned());
    let (changes, last) = crate::reach::write_once(dir, "sql", move |client| {
        let database = client.database(&named)?;
        client.exec(database, &text)
    })?;
    print_done(changes, last, json)
}

/// Whether a statement gives rows back: a read, or a write with `returning`.
fn gives_rows(statement: &str) -> bool {
    let lower = statement.trim_start().to_lowercase();
    ["select", "with", "values", "explain"].iter().any(|word| lower.starts_with(word)) || lower.contains("returning")
}

fn print_done(changes: u64, last_insert_rowid: i64, json: bool) -> Done {
    if json {
        outln!("{}", serde_json::json!({ "changes": changes, "last_insert_rowid": last_insert_rowid }))
    } else {
        outln!("{changes} rows changed")
    }
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
fn opened(store: &Store, database: &str) -> Result<Database, asked::Refused> {
    tinystore::mcp::sql::opened(store, database).map_err(failed)
}

/// Refuses a database the store does not hold, naming the nearest it does:
/// opening one makes it, so a typo would otherwise make an empty one.
pub(crate) fn database_held(dir: &Path, database: &str) -> Result<(), asked::Refused> {
    tinystore::mcp::given::database_held(dir, database).map_err(failed)
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

/// Prints rows as a table a person reads, or one JSON object a row.
fn print_rows(rows: &[Row], json: bool) -> Done {
    if json {
        for row in rows {
            outln!("{}", row.json())?;
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
