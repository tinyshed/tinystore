//! An application's database: its migrations, reads, writes, batches and
//! transactions, and the values between the program and SQLite.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use super::{Database, Done, Migrations};
use crate::{ConstraintKind, ErrorKind, Options, Store, TestClock, sql};

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

#[test]
fn a_value_that_does_not_read_as_its_field_names_its_column() {
    let f = fixture();
    let db = f.app();
    db.exec("insert into notes (id, author_id, title) values ('n1', 42, 'Buy milk')").unwrap();
    let error = db.one::<(String, bool)>("select id, title from notes").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(error.to_string().contains(r#"column title: TEXT "Buy milk" does not read as a bool"#), "{error}");
}

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
}

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

#[test]
fn a_call_around_a_transaction_from_inside_it_is_invalid() {
    let f = fixture();
    let db = f.app();
    let error = db.tx(|_| db.exec("insert into notes (id, author_id, title) values ('n1', 1, 'a')")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    let read = db.tx(|_| db.scalar::<i64>("select count(*) from notes")).unwrap_err();
    assert_eq!(read.kind(), ErrorKind::Invalid, "{read}");
}

#[test]
fn a_name_that_could_leave_sql_or_meet_another_is_refused() {
    let f = fixture();
    for name in ["../kv", "App", "", "a/b", "-x", &"a".repeat(65)] {
        let error = f.store.database(name).open().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{name}: {error}");
    }
}

#[test]
fn a_database_closes_with_its_store() {
    let f = fixture();
    let db = f.app();
    f.store.close().unwrap();
    let error = db.scalar::<i64>("select 1").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Closed, "{error}");
}

#[test]
fn reading_past_the_bound_is_a_limit() {
    // as the adapter does before a connection of its own, so that this one
    // does not start SQLite with other mutexes than the adapter's
    crate::sqlite::give_mutexes();
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let mut statement = connection.prepare("select zeroblob(600) from (select 1 union all select 2)").unwrap();
    let read = super::rows::Rows::read(&mut statement, &[], (usize::MAX, 1000), super::rows::Held::default());
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
