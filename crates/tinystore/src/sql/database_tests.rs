//! An application's database: its migrations, reads, writes, batches and
//! transactions, and the values between the program and SQLite.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use super::{Database, Done, Migrations};
use crate::{CancellationToken, ConstraintKind, ErrorKind, Options, Store, TestClock, sql};

const NOTES: &str = "create table notes (
    id         text primary key,
    author_id  integer not null,
    title      text not null,
    done       integer not null default 0 check (done in (0, 1)),
    tags       text not null default '[]' check (json_valid(tags)),
    created_at integer not null default 0
) strict";

const TAGS: &str = "alter table notes add column due text";

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    clock: Arc<TestClock>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let store = open_store(&dir, &clock);
    Fixture { dir, store, clock }
}

fn open_store(dir: &tempfile::TempDir, clock: &Arc<TestClock>) -> Store {
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    Store::open(dir.path(), options).unwrap()
}

impl Fixture {
    fn app(&self) -> Database {
        self.store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap()
    }

    fn reopen(&mut self) {
        self.store.close().unwrap();
        self.store = open_store(&self.dir, &self.clock);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Note {
    id: String,
    author_id: i64,
    title: String,
    done: bool,
    tags: Vec<String>,
    created_at: SystemTime,
}

fn note(id: &str, title: &str) -> Note {
    Note {
        id: id.to_owned(),
        author_id: 42,
        title: title.to_owned(),
        done: false,
        tags: vec!["home".to_owned()],
        created_at: UNIX_EPOCH + Duration::from_millis(1_791_547_200_123),
    }
}

fn insert(db: &Database, note: &Note) -> Done {
    db.exec(sql!(
        "insert into notes (id, author_id, title, done, tags, created_at) values (?, ?, ?, ?, ?, ?)",
        note.id,
        note.author_id,
        note.title,
        note.done,
        note.tags,
        note.created_at
    ))
    .unwrap()
}

// contract: SQL-03
#[test]
fn a_database_applies_each_migration_once_and_checks_them_at_every_open() {
    let mut f = fixture();
    let db = f.app();
    insert(&db, &note("n1", "Buy milk"));
    let grown = Migrations::from([("0001_notes.sql", NOTES), ("0002_due.sql", TAGS)]);
    let db = f.store.database("app").migrations(grown.clone()).open().unwrap();
    db.exec(sql!("update notes set due = ? where id = ?", "2026-10-11", "n1")).unwrap();

    f.reopen();
    let db = f.store.database("app").migrations(grown).open().unwrap();
    let due: Option<String> = db.scalar(sql!("select due from notes where id = ?", "n1")).unwrap();
    assert_eq!(due.as_deref(), Some("2026-10-11"), "applied once, kept across a reopen");
    let applied: i64 = db.scalar("select count(*) from _tinystore_migrations").unwrap();
    assert_eq!(applied, 2);
    assert!(f.dir.path().join("sql").join("app.db").is_file());
}

#[test]
fn a_database_opens_without_migrations_as_it_is() {
    let f = fixture();
    let scratch = f.store.database("scratch").open().unwrap();
    assert_eq!(scratch.scalar::<i64>("select 1").unwrap(), 1, "a new file, created empty");
    scratch.exec("create table t (x integer)").unwrap();
    scratch.exec(sql!("insert into t (x) values (?)", 7)).unwrap();

    let migrated =
        f.store.database("scratch").migrations([("0001_u.sql", "create table u (y integer)")]).open().unwrap();
    migrated.exec(sql!("insert into u (y) values (?)", 8)).unwrap();
    assert_eq!(scratch.scalar::<i64>("select x from t").unwrap(), 7, "both handles are one database");
}

#[test]
fn a_statement_sqlite_cannot_run_as_written_is_invalid() {
    let f = fixture();
    let db = f.app();
    db.exec("create virtual table notes_fts using fts5 (title)").unwrap();
    let wrong = [
        db.all::<IgnoredAny>("selec 1").unwrap_err(),
        db.all::<IgnoredAny>("select * from nope").unwrap_err(),
        db.exec("insert into notes (nope) values (1)").unwrap_err(),
        db.all::<IgnoredAny>(sql!("select * from notes_fts where notes_fts match ?", "wine AND (")).unwrap_err(),
    ];
    for error in wrong {
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    }
    let broken = f.store.database("broken").migrations([("0001_x.sql", "create table x (")]).open().unwrap_err();
    assert_eq!(broken.kind(), ErrorKind::Invalid, "{broken}");
}

// contract: SQL-03
#[test]
fn a_migration_changed_renamed_missing_or_out_of_order_refuses_to_open() {
    let f = fixture();
    f.store.database("app").migrations([("0001_notes.sql", NOTES), ("0002_due.sql", TAGS)]).open().unwrap();

    let refused = |migrations: Migrations| f.store.database("app").migrations(migrations).open().unwrap_err();
    let edited = refused(Migrations::from([
        ("0001_notes.sql", NOTES),
        ("0002_due.sql", "alter table notes add column due integer"),
    ]));
    assert!(edited.to_string().contains("0002_due.sql changed after it was applied"), "{edited}");
    let renamed = refused(Migrations::from([("0001_notes.sql", NOTES), ("0002_deadline.sql", TAGS)]));
    assert!(renamed.to_string().contains("was renamed"), "{renamed}");
    let older = refused(Migrations::from([("0001_notes.sql", NOTES)]));
    assert!(older.to_string().contains("a newer program"), "{older}");
    let gap = refused(Migrations::from([("0001_notes.sql", NOTES), ("0003_due.sql", TAGS)]));
    assert!(gap.to_string().contains("without a gap"), "{gap}");
    for error in [edited, renamed, older, gap] {
        assert_eq!(error.kind(), ErrorKind::Invalid);
        assert!(error.to_string().starts_with("sql app: "), "{error}");
    }
}

// contract: SQL-04
#[test]
fn migrations_run_without_foreign_keys_and_leave_none_broken() {
    let f = fixture();
    let parents = "create table authors (id integer primary key, name text not null) strict;
        create table books (id integer primary key, author_id integer not null references authors (id) on delete cascade) strict";
    let db = f.store.database("library").migrations([("0001_books.sql", parents)]).open().unwrap();
    db.exec("insert into authors (id, name) values (1, 'Le Guin')").unwrap();
    db.exec("insert into books (id, author_id) values (1, 1), (2, 1)").unwrap();

    // SQLite's procedure for changing a table: a new one, the rows, the old dropped.
    let rebuild = "create table authors_new (id integer primary key, name text not null, born integer) strict;
        insert into authors_new (id, name) select id, name from authors;
        drop table authors;
        alter table authors_new rename to authors";
    let steps = [("0001_books.sql", parents), ("0002_born.sql", rebuild)];
    let db = f.store.database("library").migrations(steps).open().unwrap();
    assert_eq!(db.scalar::<i64>("select count(*) from books").unwrap(), 2, "a rebuilt parent keeps its children");

    let orphan = "insert into books (id, author_id) values (3, 99)";
    let broken = [("0001_books.sql", parents), ("0002_born.sql", rebuild), ("0003_orphan.sql", orphan)];
    let error = f.store.database("library").migrations(broken).open().unwrap_err();
    assert!(error.to_string().contains("a row of books whose authors is missing"), "{error}");
    assert_eq!(db.scalar::<i64>("select count(*) from books").unwrap(), 2, "the failed migration left nothing");
}

// contract: SQL-05
#[test]
fn a_row_reads_as_the_programs_type_and_each_value_as_it_went_in() {
    let f = fixture();
    let db = f.app();
    let written = Note { done: true, tags: vec!["home".to_owned(), "urgent".to_owned()], ..note("n1", "Buy milk") };
    insert(&db, &written);

    let read: Note = db.one(sql!("select * from notes where id = ?", "n1")).unwrap().unwrap();
    assert_eq!(read, written, "a bool, a JSON list and a time to the millisecond");
    let stored: (i64, String, i64) = db.one("select done, tags, created_at from notes").unwrap().unwrap();
    assert_eq!(stored, (1, r#"["home","urgent"]"#.to_owned(), 1_791_547_200_123), "as SQLite keeps them");
    let titles: Vec<String> = db.all("select title from notes").unwrap();
    assert_eq!(titles, ["Buy milk"], "a row of one column is its value");
    assert_eq!(db.one::<Note>(sql!("select * from notes where id = ?", "none")).unwrap(), None);

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum Status {
        Open,
        Closed,
    }
    let status: Status = db.scalar(sql!("select ?", Status::Closed)).unwrap();
    assert_eq!(status, Status::Closed, "an enum is the name of its variant");
    let bytes: Vec<u8> = db.scalar(sql!("select ?", vec![0u8, 255, 7])).unwrap();
    assert_eq!(bytes, [0, 255, 7], "a Vec<u8> is a BLOB");
    let kind: String = db.scalar(sql!("select typeof(?)", Some(vec![0u8, 255, 7]))).unwrap();
    assert_eq!(kind, "blob", "and so is the Vec<u8> an Option has");
    let nothing: Option<i64> = db.scalar("select max(author_id) from notes where author_id > 100").unwrap();
    assert_eq!(nothing, None, "max over no rows is NULL");
}

// contract: ERR-02
#[test]
fn a_value_that_does_not_read_as_its_field_names_its_column() {
    let f = fixture();
    let db = f.app();
    db.exec("insert into notes (id, author_id, title) values ('n1', 42, 'Buy milk')").unwrap();
    let error = db.one::<(String, bool)>("select id, title from notes").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(error.to_string().contains(r#"column title: TEXT "Buy milk" does not read as a bool"#), "{error}");
}

// contract: SQL-05
#[test]
fn a_value_sqlite_would_change_is_refused_and_every_placeholder_takes_one() {
    let f = fixture();
    let db = f.app();
    let nan =
        db.exec(sql!("insert into notes (id, author_id, title) values (?, ?, ?)", "n1", f64::NAN, "x")).unwrap_err();
    assert!(nan.to_string().contains("NaN, which SQLite keeps as NULL"), "{nan}");
    let past = db.scalar::<i64>(sql!("select ?", u64::MAX)).unwrap_err();
    assert!(past.to_string().contains("past the 64-bit integers"), "{past}");
    let short = db.exec(sql!("insert into notes (id, author_id, title) values (?, ?, ?)", "n1", 42)).unwrap_err();
    assert!(short.to_string().contains("3 placeholders and 2 values"), "{short}");
    for error in [nan, past, short] {
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    }
    assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 0);
}

#[test]
fn exec_says_what_it_changed_and_the_rowid_of_its_insert() {
    let f = fixture();
    let db = f
        .store
        .database("events")
        .migrations([("0001_events.sql", "create table events (id integer primary key, kind text not null) strict")])
        .open()
        .unwrap();
    let first = db.exec(sql!("insert into events (kind) values (?)", "signup")).unwrap();
    assert_eq!(first, Done { changes: 1, last_insert_rowid: 1 });
    db.exec(sql!("insert into events (kind) values (?)", "login")).unwrap();
    let updated = db.exec("update events set kind = upper(kind)").unwrap();
    assert_eq!(updated, Done { changes: 2, last_insert_rowid: 0 }, "an update inserted nothing");
    assert_eq!(db.exec("select * from events").unwrap(), Done::default(), "a read changes nothing");
    let made = db.exec("create table seen (id integer primary key)").unwrap();
    assert_eq!(made, Done::default(), "a schema change counts no rows, whatever wrote before it");
    let seen = db.exec("insert into seen (id) values (2)").unwrap();
    assert_eq!(seen, Done { changes: 1, last_insert_rowid: 2 }, "row 2 of another table, as the last insert left");
}

// contract: SQL-01
#[test]
fn a_write_with_returning_runs_on_the_writer_and_gives_its_rows_once_durable() {
    let mut f = fixture();
    let db = f.app();
    insert(&db, &note("n1", "Buy milk"));
    insert(&db, &note("n2", "Call mom"));

    let done: Note = db.one(sql!("update notes set done = 1 where id = ? returning *", "n1")).unwrap().unwrap();
    assert!(done.done);
    let added: Vec<(String,)> = db
        .all(sql!("insert into notes (id, author_id, title) values (?, 1, 'a'), (?, 1, 'b') returning id", "n3", "n4"))
        .unwrap();
    assert_eq!(added, [("n3".to_owned(),), ("n4".to_owned(),)], "an insert returning several rows");
    let title: String =
        db.scalar(sql!("update notes set title = title || '!' where id = ? returning title", "n2")).unwrap();
    assert_eq!(title, "Call mom!");

    f.reopen();
    let db = f.app();
    assert!(db.one::<Note>(sql!("select * from notes where id = ?", "n1")).unwrap().unwrap().done, "it was durable");
    assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 4);
}

// contract: SQL-01
#[test]
fn one_given_two_rows_of_a_write_rolls_the_write_back() {
    let f = fixture();
    let db = f.app();
    insert(&db, &note("n1", "Buy milk"));
    insert(&db, &note("n2", "Call mom"));
    let error = db.one::<Note>("update notes set done = 1 returning *").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(error.to_string().contains("gave several"), "{error}");
    assert_eq!(db.scalar::<i64>("select count(*) from notes where done = 1").unwrap(), 0, "rolled back");
}

// contract: SQL-01
#[test]
fn a_constraint_failing_after_rows_came_rolls_the_whole_write_back() {
    let f = fixture();
    let db = f.app();
    insert(&db, &note("n2", "Call mom"));
    let error = db
        .all::<(String,)>("insert into notes (id, author_id, title) values ('n1', 1, 'a'), ('n2', 1, 'b') returning id")
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 1, "n1 is not kept either");
}

// contract: ERR-03, SQL-08
#[test]
fn a_key_already_held_is_a_conflict_and_another_constraint_is_invalid() {
    let f = fixture();
    let db = f.app();
    insert(&db, &note("n1", "Buy milk"));
    let taken = db.exec("insert into notes (id, author_id, title) values ('n1', 1, 'again')").unwrap_err();
    assert_eq!(taken.kind(), ErrorKind::Conflict, "{taken}");
    let checked = db.exec("insert into notes (id, author_id, title, done) values ('n9', 1, 'x', 7)").unwrap_err();
    assert_eq!(checked.kind(), ErrorKind::Invalid, "{checked}");
    let null = db.exec("insert into notes (id, author_id, title) values ('n9', 1, null)").unwrap_err();
    assert_eq!(null.kind(), ErrorKind::Invalid, "{null}");
    assert!(taken.to_string().starts_with(r#"sql app: statement "insert into notes"#), "{taken}");
}

// contract: ERR-03, SQL-08
#[test]
fn a_broken_constraint_says_which_as_sqlite_names_it() {
    let f = fixture();
    let schema = "create table orgs (id integer primary key);
        create table users (
            id     text primary key,
            org    integer not null references orgs (id),
            email  text not null,
            seats  integer not null constraint seats_positive check (seats > 0),
            age    integer check (age >= 18),
            unique (org, email)
        ) strict;
        create unique index users_lower_email on users (lower(email)) where org = 2;";
    let db = f.store.database("app").migrations([("0001_users.sql", schema)]).open().unwrap();
    db.exec("insert into orgs (id) values (1), (2)").unwrap();
    db.exec("insert into users (id, org, email, seats) values ('u1', 1, 'ann@example.com', 1)").unwrap();
    db.exec("insert into users (id, org, email, seats) values ('u3', 2, 'bob@example.com', 1)").unwrap();

    let broken = |text: &str| {
        let error = db.exec(text).unwrap_err();
        let constraint = error.constraint().unwrap_or_else(|| panic!("{error}"));
        (error.kind(), constraint.kind, constraint.table, constraint.columns, constraint.name)
    };
    let held = |columns: &[&str]| columns.iter().map(|column| (*column).to_owned()).collect::<Vec<_>>();
    let insert = |values: &str| format!("insert into users (id, org, email, seats, age) values {values}");
    assert_eq!(
        broken(&insert("('u1', 1, 'bob@example.com', 1, null)")),
        (ErrorKind::Conflict, ConstraintKind::PrimaryKey, Some("users".into()), held(&["id"]), None)
    );
    assert_eq!(
        broken(&insert("('u2', 1, 'ann@example.com', 1, null)")),
        (ErrorKind::Conflict, ConstraintKind::Unique, Some("users".into()), held(&["org", "email"]), None)
    );
    assert_eq!(
        broken(&insert("('u2', 2, 'BOB@example.com', 1, null)")),
        (ErrorKind::Conflict, ConstraintKind::Unique, None, held(&[]), Some("users_lower_email".into())),
        "a unique index on an expression names itself"
    );
    assert_eq!(
        broken(&insert("('u2', 1, null, 1, null)")),
        (ErrorKind::Invalid, ConstraintKind::NotNull, Some("users".into()), held(&["email"]), None)
    );
    assert_eq!(
        broken(&insert("('u2', 1, 'bob@example.com', 0, null)")),
        (ErrorKind::Invalid, ConstraintKind::Check, None, held(&[]), Some("seats_positive".into()))
    );
    assert_eq!(
        broken(&insert("('u2', 1, 'bob@example.com', 1, 9)")),
        (ErrorKind::Invalid, ConstraintKind::Check, None, held(&[]), Some("age >= 18".into())),
        "a check without a name is its text"
    );
    assert_eq!(
        broken(&insert("('u2', 7, 'bob@example.com', 1, null)")),
        (ErrorKind::Invalid, ConstraintKind::ForeignKey, None, held(&[]), None)
    );
    let typed = db.exec(&*insert("('u2', 1, 'bob@example.com', 'many', null)")).unwrap_err();
    assert_eq!(
        (typed.kind(), typed.constraint()),
        (ErrorKind::Invalid, None),
        "a strict column's type is no constraint"
    );
}

// contract: DUR-03
#[test]
fn a_batch_writes_all_of_its_statements_or_none() {
    let f = fixture();
    let db = f.app();
    let written = db
        .batch([
            sql!("insert into notes (id, author_id, title) values (?, ?, ?)", "n1", 1, "a"),
            sql!("insert into notes (id, author_id, title) values (?, ?, ?)", "n2", 1, "b"),
        ])
        .unwrap();
    assert_eq!(written.iter().map(|done| done.changes).collect::<Vec<_>>(), [1, 1]);

    let error = db
        .batch([
            sql!("insert into notes (id, author_id, title) values (?, ?, ?)", "n3", 1, "c"),
            sql!("insert into notes (id, author_id, title) values (?, ?, ?)", "n1", 1, "taken"),
        ])
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 2, "n3 went back with the batch");
}

// contract: DUR-02, SQL-07
#[test]
fn a_transaction_sees_its_writes_and_rolls_back_on_an_error() {
    let f = fixture();
    let db = f.app();
    insert(&db, &note("n1", "Buy milk"));

    let titles = db
        .tx(|tx| -> crate::Result<Vec<String>> {
            tx.exec(sql!("update notes set title = ? where id = ?", "Buy oat milk", "n1"))?;
            let taken = tx.exec("insert into notes (id, author_id, title) values ('n1', 1, 'again')");
            assert_eq!(taken.unwrap_err().kind(), ErrorKind::Conflict, "a call that fails leaves the rest");
            tx.all("select title from notes")
        })
        .unwrap();
    assert_eq!(titles, ["Buy oat milk"], "a read sees the transaction's own write");

    #[derive(Debug)]
    struct SoldOut;
    impl From<crate::Error> for SoldOut {
        fn from(_: crate::Error) -> Self {
            SoldOut
        }
    }
    let refused = db.tx(|tx| -> Result<(), SoldOut> {
        tx.exec("delete from notes")?;
        Err(SoldOut)
    });
    assert!(refused.is_err());
    assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 1, "the delete rolled back");
}

// contract: SQL-07
#[test]
fn a_transaction_past_its_bound_rolls_back() {
    let f = fixture();
    let db = f.app();
    let error = db
        .tx(|tx| {
            tx.exec("insert into notes (id, author_id, title) values ('n1', 1, 'a')")?;
            f.clock.advance(Duration::from_secs(6));
            tx.exec("insert into notes (id, author_id, title) values ('n2', 1, 'b')")
        })
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 0);

    let late = db.tx(|tx| {
        tx.exec("insert into notes (id, author_id, title) values ('n3', 1, 'c')")?;
        f.clock.advance(Duration::from_secs(6));
        Ok::<_, crate::Error>(())
    });
    assert_eq!(late.unwrap_err().kind(), ErrorKind::Limit, "a bound passed at the end rolls back too");
    assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 0);
}

// contract: DUR-07
#[test]
fn a_call_around_a_transaction_from_inside_it_is_invalid() {
    let f = fixture();
    let db = f.app();
    let error = db.tx(|_| db.exec("insert into notes (id, author_id, title) values ('n1', 1, 'a')")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    let read = db.tx(|_| db.scalar::<i64>("select count(*) from notes")).unwrap_err();
    assert_eq!(read.kind(), ErrorKind::Invalid, "{read}");
}

// contract: SQL-11
#[test]
fn a_name_that_could_leave_sql_or_meet_another_is_refused() {
    let f = fixture();
    for name in ["../kv", "App", "", "a/b", "-x", &"a".repeat(65)] {
        let error = f.store.database(name).open().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{name}: {error}");
    }
}

// contract: STORE-03
#[test]
fn a_database_closes_with_its_store() {
    let f = fixture();
    let db = f.app();
    f.store.close().unwrap();
    let error = db.scalar::<i64>("select 1").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Closed, "{error}");
}

// contract: ERR-05
#[test]
fn reading_past_the_bound_is_a_limit() {
    // as the adapter does before a connection of its own, so that this one
    // does not start SQLite with other mutexes than the adapter's
    crate::sqlite::give_mutexes();
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let mut statement = connection.prepare("select zeroblob(600) from (select 1 union all select 2)").unwrap();
    let read = <super::rows::Rows as super::rows::Answer>::read(
        &mut statement,
        &[],
        (usize::MAX, 1000),
        super::rows::Held::default(),
    );
    let error = read.map(drop).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert_eq!((error.fact("limit"), error.fact("bound")), (Some("bytes of a call's rows"), Some("1000")));
}

#[test]
fn rows_hold_the_stores_memory_while_a_call_holds_them() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options { memory: Some(1 << 20), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    let schema = "create table blobs (id integer primary key, body blob not null) strict";
    let db = store.database("app").migrations([("0001_blobs.sql", schema)]).open().unwrap();
    for _ in 0..12 {
        db.exec(sql!("insert into blobs (body) values (zeroblob(100000))")).unwrap();
    }

    let whole: Vec<IgnoredAny> = db.all(sql!("select body from blobs where id <= 8")).unwrap();
    assert_eq!(whole.len(), 8);
    assert_eq!(store.memory().used(), 0, "the rows' memory is given back with the call");

    let past = db.all::<IgnoredAny>(sql!("select body from blobs")).unwrap_err();
    let limit = (past.kind(), past.fact("limit"), past.fact("bound"));
    assert_eq!(limit, (ErrorKind::Limit, Some("store memory"), Some("1048576")), "{past}");

    let held = store.memory().try_reserve(600 << 10, "another call").unwrap();
    let now = db.all::<IgnoredAny>(sql!("select body from blobs where id <= 8")).unwrap_err();
    assert_eq!((now.kind(), now.fact("limit")), (ErrorKind::Limit, Some("store memory, now")), "{now}");
    drop(held);
    assert_eq!(db.all::<IgnoredAny>(sql!("select body from blobs where id <= 8")).unwrap().len(), 8);
    assert_eq!(store.memory().used(), 0);
}

/// SQLite's `synchronous` on a database's writer, which a transaction runs
/// on: 2 syncs each commit, 1 leaves the sync to checkpoints.
fn synchronous(db: &Database) -> i64 {
    db.tx(|tx| tx.scalar(sql!("pragma synchronous"))).unwrap()
}

// contract: DUR-01
#[test]
fn a_database_commits_as_far_as_it_says_or_as_its_store_does() {
    use crate::Durability::{Full, Os};

    let dir = tempfile::tempdir().unwrap();
    let options = Options { durability: Some(Os), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    let cache = store.database("cache").open().unwrap();
    let audit = store.database("audit").durability(Full).open().unwrap();
    assert_eq!((synchronous(&cache), synchronous(&audit)), (1, 2), "the store's, and the database's own");

    // open already, a database keeps what it was opened with: the same again is itself, another is refused
    assert_eq!(synchronous(&store.database("audit").durability(Full).open().unwrap()), 2);
    assert_eq!(synchronous(&store.database("audit").open().unwrap()), 2, "nothing asked is nothing changed");
    let refused = store.database("audit").durability(Os).open().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert_eq!(refused.to_string(), "sql audit: open with durability full, and asked for os: invalid");
    store.close().unwrap();

    // a store that says nothing syncs each commit, and a database may still say its own
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    assert_eq!(synchronous(&store.database("app").open().unwrap()), 2);
    assert_eq!(synchronous(&store.database("cache").durability(Os).open().unwrap()), 1);
    store.close().unwrap();
}

// contract: SEC-01
#[test]
fn a_query_is_known_by_its_first_word_past_spaces_and_comments() {
    for query in
        ["select 1", "  WITH t AS (select 1) select * from t", "-- why\n/* how */ values (1)", "explain select 1"]
    {
        assert!(super::database::is_query(query), "{query}");
    }
    for other in ["attach database 'x.db' as x", "pragma query_only = 0", "/* select */ delete from t", "-- select"] {
        assert!(!super::database::is_query(other), "{other}");
    }
}

/// What would change a connection for whoever uses it next.
const CHANGES_ITS_CONNECTION: [&str; 17] = [
    "attach database 'other.db' as other",
    "attach database ('other' || '.db') as other",
    "detach database other",
    "begin",
    "begin immediate",
    "COMMIT",
    "end",
    "rollback",
    "savepoint mine",
    "release mine",
    "rollback to mine",
    "pragma query_only = off",
    "pragma cache_size = 1000000",
    "pragma foreign_keys(off)",
    "pragma main.journal_mode = delete",
    "explain pragma query_only = off",
    // the text the store's own commit has, which is compiled and kept already
    "/* tinystore */ commit",
];

// contract: SQL-02
#[test]
fn a_statement_changes_nothing_of_the_connection_it_runs_on() {
    let fixture = fixture();
    let db = fixture.app();
    let settings = |db: &Database| -> (i64, i64, i64) {
        let query_only = db.scalar("pragma query_only").unwrap();
        let cache_size = db.scalar("pragma cache_size").unwrap();
        (query_only, cache_size, db.scalar("select count(*) from pragma_database_list").unwrap())
    };
    let before = settings(&db);
    for text in CHANGES_ITS_CONNECTION {
        let read = db.all::<IgnoredAny>(text).unwrap_err();
        assert_eq!(read.kind(), ErrorKind::Invalid, "a read of {text}: {read}");
        let written = db.exec(text).unwrap_err();
        assert_eq!(written.kind(), ErrorKind::Invalid, "a write of {text}: {written}");
        let in_a_transaction = db.tx(|tx| tx.exec(text)).unwrap_err();
        assert_eq!(in_a_transaction.kind(), ErrorKind::Invalid, "{text} in a transaction: {in_a_transaction}");
    }
    assert_eq!(settings(&db), before);
    insert(&db, &note("n1", "the writer still writes"));
}

// contract: SQL-02
#[test]
fn a_statement_reads_no_other_database_through_an_attach() {
    let fixture = fixture();
    let db = fixture.app();
    let secrets = fixture.store.database("secrets").migrations([("0001_keys.sql", "create table keys (key text)")]);
    secrets.open().unwrap().exec("insert into keys values ('hunter2')").unwrap();
    let file = fixture.dir.path().join("sql/secrets.db").display().to_string().replace(char::from(b'\\'), "/");

    let attached = db.all::<IgnoredAny>(format!("attach database '{file}' as other")).unwrap_err();
    assert_eq!(attached.kind(), ErrorKind::Invalid, "{attached}");
    assert!(attached.to_string().contains("attaches no file"), "{attached}");
    assert!(db.all::<String>("select key from other.keys").is_err());
}

#[test]
fn a_batch_that_would_commit_writes_none_of_its_statements() {
    let fixture = fixture();
    let db = fixture.app();
    for commit in ["commit", "/* tinystore */ commit", "rollback"] {
        let kept = "insert into notes (id, author_id, title) values ('early', 1, 'before the commit')";
        let failed = db.batch([kept, commit, "insert into nowhere values (1)"]).unwrap_err();
        assert_eq!(failed.kind(), ErrorKind::Invalid, "{commit}: {failed}");
        assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 0, "{commit}: all of a batch or none");
    }
}

#[test]
fn a_pragma_that_only_reads_runs() {
    let fixture = fixture();
    let db = fixture.app();
    assert_eq!(db.all::<IgnoredAny>("pragma table_info(notes)").unwrap().len(), 6);
    assert_eq!(db.all::<String>("select name from pragma_table_info('notes')").unwrap().len(), 6);
    assert_eq!(db.scalar::<i64>("pragma user_version").unwrap(), 0);
    assert_eq!(db.scalar::<String>("pragma integrity_check").unwrap(), "ok");
    assert!(db.all::<IgnoredAny>("pragma foreign_key_check(notes)").unwrap().is_empty());
    assert!(!db.all::<IgnoredAny>("explain query plan select * from notes").unwrap().is_empty());
}

// contract: SQL-02
#[test]
fn a_migration_changes_tables_and_nothing_of_its_connection() {
    let fixture = fixture();
    let steps = ["pragma journal_mode = delete", "commit", "attach database 'other.db' as other"];
    for step in steps {
        let sql = format!("create table early (x); {step}; create table late (x)");
        let refused = fixture.store.database("app").migrations([("0001_tables.sql", sql.as_str())]).open().unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{step}: {refused}");
        assert!(refused.to_string().contains("0001_tables.sql"), "{refused}");
    }
    // a migration refused left nothing: `early` would be there had one committed
    let db = fixture.store.database("app").migrations([("0001_tables.sql", "create table early (x)")]).open().unwrap();
    assert_eq!(db.scalar::<i64>("select count(*) from early").unwrap(), 0);
}

#[test]
fn a_kept_statement_names_the_columns_a_table_has_after_a_change_of_it() {
    let f = fixture();
    let db =
        f.store.database("scratch").migrations([("0001_t.sql", "create table t (a integer, b integer, c integer)")]);
    let db = db.open().unwrap();
    db.exec(sql!("insert into t values (1, 2, 3)")).unwrap();
    let before: Vec<serde_json::Value> = db.all(sql!("select * from t")).unwrap();
    assert_eq!(before, [serde_json::json!({ "a": 1, "b": 2, "c": 3 })]);
    db.exec(sql!("alter table t drop column b")).unwrap();
    let after: Vec<serde_json::Value> = db.all(sql!("select * from t")).unwrap();
    assert_eq!(after, [serde_json::json!({ "a": 1, "c": 3 })]);
    db.exec(sql!("alter table t add column d integer default 4")).unwrap();
    let added: Vec<serde_json::Value> = db.all(sql!("select * from t")).unwrap();
    assert_eq!(added, [serde_json::json!({ "a": 1, "c": 3, "d": 4 })]);
    let none: Vec<serde_json::Value> = db.all(sql!("select * from t where a = 0")).unwrap();
    assert!(none.is_empty());
}

/// Counts to a billion, minutes of work: a read only its cancel ends.
const COUNT_FOR_MINUTES: &str =
    "with recursive c(x) as (select 1 union all select x + 1 from c where x < 1000000000) select count(*) from c";

#[test]
fn reads_their_callers_cancelled_give_their_readers_back_at_once() {
    // contract: STORE-09
    // issue #1: a page's twelve panels asked three times, two of the pages
    // gone; the next visitor's read waited for every reader and failed
    let f = fixture();
    let db = f.app();
    let tokens: Vec<CancellationToken> = (0..36).map(|_| CancellationToken::new()).collect();
    let reads: Vec<_> = tokens
        .iter()
        .map(|token| {
            let (db, token) = (db.clone(), token.clone());
            std::thread::spawn(move || token.run(|| db.scalar::<i64>(COUNT_FOR_MINUTES)))
        })
        .collect();
    std::thread::sleep(Duration::from_millis(100));
    tokens.iter().for_each(CancellationToken::cancel);
    let next = Instant::now();
    assert_eq!(db.scalar::<i64>("select 1").unwrap(), 1);
    assert!(next.elapsed() < Duration::from_secs(2), "the next read waited {:?}", next.elapsed());
    for read in reads {
        let error = read.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Cancelled, "{error}");
        assert!(error.to_string().starts_with(r#"sql app: statement "with recursive"#), "{error}");
        assert!(error.to_string().ends_with("app.db: a read: cancelled"), "{error}");
    }
}

#[test]
fn a_read_past_its_timeout_fails_cancelled_and_a_write_under_the_token_runs() {
    // contract: STORE-09
    let f = fixture();
    let db = f.app();
    let token = CancellationToken::timeout(Duration::from_millis(50));
    let error = token.run(|| db.scalar::<i64>(COUNT_FOR_MINUTES)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled, "{error}");
    assert!(error.to_string().ends_with("a read past its timeout of 50ms: cancelled"), "{error}");
    let written = token.run(|| db.exec(sql!("insert into notes (id, author_id, title) values ('n1', 7, 'kept')")));
    assert_eq!(written.unwrap().changes, 1, "a write is not a read a token stops");
    assert_eq!(db.scalar::<i64>("select count(*) from notes").unwrap(), 1);
}
