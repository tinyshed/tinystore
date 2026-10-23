use std::sync::Arc;

use super::*;
use crate::ErrorKind;
use crate::sqlite::{Config, File, given_sql_error, sql_error};

fn open() -> (tempfile::TempDir, Arc<File>) {
    let dir = tempfile::tempdir().unwrap();
    let file = File::open(dir.path().join("test.db"), Config::default()).unwrap();
    file.transaction(|tx| {
        tx.execute_batch("create table orders (id integer primary key, total integer not null)")
            .map_err(|error| sql_error("create orders", error))
    })
    .unwrap();
    (dir, Arc::new(file))
}

/// Runs `text` on the writer as a statement the store was given, as a
/// database's `exec` does.
fn write_given(file: &File, text: &str) -> Result<()> {
    let text = text.to_owned();
    file.write(0, move |tx| given(&text, || tx.execute_batch(&text).map_err(|error| given_sql_error("given", error))))
}

/// The one number `text` reads on a reader, as a statement the store was given.
fn read_given(file: &File, text: &str) -> Result<i64> {
    file.read(|connection| {
        given(text, || {
            connection.query_row(text, [], |row| row.get(0)).map_err(|error| given_sql_error("given", error))
        })
    })
}

// contract: SQL-02
#[test]
fn a_given_statement_makes_no_temporary_object_on_the_connection_it_shares() {
    let (_dir, file) = open();
    let temporary = [
        "create temp table orders (id integer primary key, total integer)",
        "create temporary table staging (line text)",
        "create table temp.staging (line text)",
        "create temp view totals as select total from orders",
        "create temp trigger audit after insert on main.orders begin select 1; end",
        "create trigger temp.audit after insert on main.orders begin select 1; end",
        "create view temp.totals as select total from orders",
        "create virtual table temp.words using fts5(body)",
    ];
    for text in temporary {
        let Err(refused) = write_given(&file, text) else { panic!("{text}: it ran") };
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{text}: {refused}");
        assert!(refused.to_string().contains("temporary"), "{text}: {refused}");
    }
    write_given(&file, "insert into orders (total) values (70)").unwrap();
    assert_eq!(read_given(&file, "select count(*) from orders").unwrap(), 1, "the row is in the file");
}

#[test]
fn a_given_statement_writes_no_page_of_its_file() {
    let (_dir, file) = open();
    write_given(&file, "insert into orders (total) values (70)").unwrap();
    let writes = [
        "update sqlite_dbpage set data = zeroblob(4096) where pgno = 2",
        "insert into sqlite_dbpage (pgno, data) values (2, zeroblob(4096))",
        "create virtual table pages using sqlite_dbpage",
    ];
    for text in writes {
        let Err(refused) = write_given(&file, text) else { panic!("{text}: it ran") };
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{text}: {refused}");
        assert!(refused.to_string().contains("sqlite_dbpage"), "{text}: {refused}");
    }
    assert!(read_given(&file, "select count(*) from sqlite_dbpage").unwrap() >= 2, "its pages read");
    assert_eq!(read_given(&file, "select count(*) from orders").unwrap(), 1);
    assert_eq!(
        read_given(&file, "select count(*) from pragma_integrity_check where integrity_check = 'ok'").unwrap(),
        1
    );
}
