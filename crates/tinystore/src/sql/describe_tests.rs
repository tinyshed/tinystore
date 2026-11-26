//! A statement's columns and their declared types, without its rows.

use serde::de::IgnoredAny;

use super::{Column, Database};
use crate::{ErrorKind, Options, Store, sql};

const NOTES: &str = "create table notes (
    id        integer primary key,
    title     varchar(80) not null,
    body,
    done      integer not null default 0
)";

fn database() -> (Store, Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let db = store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    (store, db, dir)
}

fn column(name: &str, declared: Option<&str>) -> Column {
    Column { name: name.to_owned(), declared: declared.map(str::to_owned) }
}

// contract: SQL-16
#[test]
fn a_statement_that_answers_no_row_still_names_its_columns_in_its_order_with_their_declared_types() {
    // SQLite spells the five types a strict table takes in capitals, and keeps any other as written
    let (_store, db, _dir) = database();
    let statement = "select done, title as heading, id, body, length(title) as size from notes where id = ?";
    let described = db.describe(statement).unwrap();
    assert_eq!(
        described,
        [
            column("done", Some("INTEGER")),
            column("heading", Some("varchar(80)")),
            column("id", Some("INTEGER")),
            column("body", None),
            column("size", None),
        ]
    );
    assert!(db.all::<IgnoredAny>(sql!(statement, 1)).unwrap().is_empty(), "the table holds no row");
}

// contract: SQL-16
#[test]
fn describing_runs_nothing_of_the_statement() {
    let (_store, db, _dir) = database();
    // SQLite fails this only as it runs: the absolute value of the least i64
    let overflows = "select abs(-9223372036854775807 - 1) as overflow";
    assert_eq!(db.all::<IgnoredAny>(overflows).unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(db.describe(overflows).unwrap(), [column("overflow", None)]);
}

#[test]
fn a_statement_that_writes_is_refused_returning_or_not_and_nothing_is_written() {
    let (_store, db, _dir) = database();
    for writes in ["insert into notes (title) values ('a') returning id", "delete from notes"] {
        let refused = db.describe(writes).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{writes}");
        assert!(refused.to_string().contains("this one writes"), "{refused}");
    }
    assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 0);
}

#[test]
fn a_mistyped_column_fails_as_all_fails_it_and_a_statement_that_changes_its_connection_is_refused() {
    let (_store, db, _dir) = database();
    let mistyped = "select titel from notes";
    let (read, described) = (db.all::<IgnoredAny>(mistyped).unwrap_err(), db.describe(mistyped).unwrap_err());
    assert_eq!((described.kind(), described.to_string()), (read.kind(), read.to_string()));
    assert!(described.to_string().starts_with("sql app: statement \"select titel from notes\""), "{described}");
    let attach = db.describe("attach database 'other.db' as other").unwrap_err();
    assert_eq!(attach.kind(), ErrorKind::Invalid);
}

#[test]
fn a_reader_describes_a_table_as_the_writer_changed_it_since() {
    let (_store, db, _dir) = database();
    assert_eq!(db.describe("select * from notes").unwrap().len(), 4);
    db.exec("alter table notes add column due text").unwrap();
    let described = db.describe("select * from notes").unwrap();
    assert_eq!(described.last(), Some(&column("due", Some("TEXT"))));
}
