use std::time::Duration;

use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::{Cell, Message};
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{
    Empty, Handle, Hello, SqlBatch, SqlBatched, SqlColumn, SqlColumns, SqlDone, SqlMigration, SqlOpen, SqlQuery,
    SqlRows, SqlStatement, SqlText, SqlTxAnswer, SqlTxCall, SqlTxOpen, method,
};

const NOTES: &str =
    "create table notes (id text primary key, title text not null, done integer not null default 0) strict";

fn open(client: &mut Client) -> u64 {
    let open = SqlOpen {
        name: "app".to_owned(),
        migrations: Some(vec![SqlMigration { name: "0001_notes.sql".to_owned(), sql: NOTES.to_owned() }]),
        durability: None,
    };
    client.call::<Handle>(method::SQL_OPEN, &open).unwrap().handle
}

fn text(value: &str) -> Cell {
    Cell::Str(value.to_owned())
}

fn exec(client: &mut Client, handle: u64, sql: &str, values: Vec<Cell>) -> SqlDone {
    let statement = SqlStatement { handle, text: sql.to_owned(), values };
    client.call(method::SQL_EXEC, &statement).unwrap()
}

fn query(client: &mut Client, handle: u64, sql: &str, want: &str) -> SqlRows {
    let query = SqlQuery { handle, text: sql.to_owned(), values: Vec::new(), want: want.to_owned() };
    client.call(method::SQL_QUERY, &query).unwrap()
}

fn insert(client: &mut Client, handle: u64, id: &str, title: &str) -> SqlDone {
    exec(client, handle, "insert into notes (id, title) values (?, ?)", vec![text(id), text(title)])
}

/// The rows a part's columns hold, as `sql.query` gives them: a packed
/// column's eight bytes a row, and nothing where its bit of nulls is set.
fn rows_of(part: &SqlColumns) -> Vec<Vec<Cell>> {
    let rows = usize::try_from(part.rows).unwrap();
    let cell = |column: &SqlColumn, row: usize| {
        let eight = |packed: &Vec<u8>| {
            assert_eq!(packed.len(), rows * 8, "{}: eight bytes a row", column.name);
            <[u8; 8]>::try_from(&packed[row * 8..row * 8 + 8]).unwrap()
        };
        let null = column.nulls.as_ref().is_some_and(|bits| bits[row / 8] >> (row % 8) & 1 == 1);
        match (&column.integers, &column.reals, &column.values) {
            (Some(integers), None, None) if null => Cell::from_null(i64::from_le_bytes(eight(integers)) == 0),
            (Some(integers), None, None) => Cell::Int(i64::from_le_bytes(eight(integers))),
            (None, Some(reals), None) if null => Cell::from_null(f64::from_le_bytes(eight(reals)).is_nan()),
            (None, Some(reals), None) => Cell::Float(f64::from_le_bytes(eight(reals))),
            (None, None, Some(values)) => values[row].clone(),
            _ => panic!("{}: a column is one of three", column.name),
        }
    };
    (0..rows).map(|row| part.columns.iter().map(|column| cell(column, row)).collect()).collect()
}

impl Cell {
    /// A NULL of a packed column, whose value is the one the schema says.
    fn from_null(as_the_schema_says: bool) -> Cell {
        assert!(as_the_schema_says, "a NULL is 0 among integers and a NaN among reals");
        Cell::Nil
    }
}

#[test]
fn a_client_opens_a_database_writes_it_and_reads_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client);

    assert_eq!(insert(&mut client, handle, "n1", "Buy milk"), SqlDone { changes: 1, last_insert_rowid: 1 });
    let batch = SqlBatch {
        handle,
        statements: vec![
            SqlText {
                text: "insert into notes (id, title) values (?, ?)".to_owned(),
                values: vec![text("n2"), text("Call mom")],
            },
            SqlText {
                text: "update notes set done = ? where id = ?".to_owned(),
                values: vec![Cell::Int(1), text("n1")],
            },
        ],
    };
    let batched: SqlBatched = client.call(method::SQL_BATCH, &batch).unwrap();
    assert_eq!(batched.done.iter().map(|done| done.changes).collect::<Vec<_>>(), [1, 1]);

    let rows = query(&mut client, handle, "select id, title, done from notes order by id", "all");
    assert_eq!(rows.columns, ["id", "title", "done"]);
    assert_eq!(
        rows.rows,
        [vec![text("n1"), text("Buy milk"), Cell::Int(1)], vec![text("n2"), text("Call mom"), Cell::Int(0)]]
    );
    let count = query(&mut client, handle, "select count(*) from notes", "scalar");
    assert_eq!(count.rows, [vec![Cell::Int(2)]]);
    let returned =
        query(&mut client, handle, "update notes set title = 'Buy oat milk' where id = 'n1' returning title", "one");
    assert_eq!(returned.rows, [vec![text("Buy oat milk")]], "a write with returning, through a read");

    let taken = client.call::<SqlDone>(
        method::SQL_EXEC,
        &SqlStatement {
            handle,
            text: "insert into notes (id, title) values ('n1', 'again')".to_owned(),
            values: Vec::new(),
        },
    );
    assert_eq!(taken.unwrap_err().code, "conflict");
    let refused = client.call::<SqlDone>(method::SQL_EXEC, &SqlStatement { handle: 99, ..SqlStatement::default() });
    assert_eq!(refused.unwrap_err().code, "invalid", "a write refused before it is queued is answered");
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
}

#[test]
fn rows_past_one_message_come_in_parts_within_the_clients_credit() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::connect(dir.path());
    let credit = 64 << 10;
    client.greet(Hello { protocol: 2, stream_credit: Some(credit), ..Hello::default() }).unwrap();
    let handle = open(&mut client);
    let title = "x".repeat(1000);
    for n in 0..200 {
        insert(&mut client, handle, &format!("n{n:03}"), &title);
    }

    let asked = SqlQuery {
        handle,
        text: "select id, title from notes order by id".to_owned(),
        values: Vec::new(),
        want: "all".to_owned(),
    };
    let stream = client.start(method::SQL_QUERY, &asked);
    let head = client.next_on(stream);
    assert_eq!((head.kind, head.flags), (Kind::Response, 0), "the rows do not fit one message");
    let (mut rows, mut columns, mut taken) = (0, Vec::new(), 0u64);
    loop {
        while client.silent_on(stream, Duration::from_millis(50)) {
            assert!(taken > 0, "the first part goes without a grant");
            client.write(Frame::new(Kind::Credit, stream, u32::try_from(taken).unwrap().to_le_bytes().to_vec()));
            taken = 0;
        }
        let part = client.next_on(stream);
        assert!(part.body.len() as u64 <= credit, "a part within the credit");
        taken += part.body.len() as u64;
        let decoded = SqlRows::decode(&part.body).unwrap();
        if columns.is_empty() {
            columns = decoded.columns;
        }
        rows += decoded.rows.len();
        if part.flags & frame::END != 0 {
            break;
        }
    }
    assert_eq!((rows, columns), (200, vec!["id".to_owned(), "title".to_owned()]));
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn a_download_holds_the_stores_memory_until_its_last_part_is_sent() {
    let dir = tempfile::tempdir().unwrap();
    let options = crate::Options { memory: Some(4 << 20), background: false, ..crate::Options::default() };
    let store = crate::Store::open(dir.path(), options).unwrap();
    let mut client = Client::over(crate::pipe::Pipe::connect(&store, crate::pipe::Connect::default()).unwrap());
    client.greet(Hello { protocol: 2, stream_credit: Some(64 << 10), ..Hello::default() }).unwrap();
    let handle = open(&mut client);
    let title = "x".repeat(1000);
    for n in 0..200 {
        insert(&mut client, handle, &format!("n{n:03}"), &title);
    }

    let asked = SqlQuery {
        handle,
        text: "select id, title from notes order by id".to_owned(),
        values: Vec::new(),
        want: "all".to_owned(),
    };
    let stream = client.start(method::SQL_QUERY, &asked);
    assert_eq!(client.next_on(stream).kind, Kind::Response);
    let mut taken = 0u64;
    loop {
        while client.silent_on(stream, Duration::from_millis(50)) {
            assert!(store.memory().used() > 0, "the parts not sent yet hold the store's memory");
            client.write(Frame::new(Kind::Credit, stream, u32::try_from(taken).unwrap().to_le_bytes().to_vec()));
            taken = 0;
        }
        let part = client.next_on(stream);
        taken += part.body.len() as u64;
        if part.flags & frame::END != 0 {
            break;
        }
    }
    assert_eq!(store.memory().used(), 0, "the last part gave the rest back");
    drop(client);
    store.close().unwrap();
}

/// Opens a transaction on `handle`, its stream open both ways.
fn begin(client: &mut Client, handle: u64) -> u32 {
    let stream = client.open_stream(method::SQL_TX, &SqlTxOpen { handle });
    let started = client.next_on(stream);
    assert_eq!((started.kind, started.flags), (Kind::Response, 0), "the transaction stays open");
    stream
}

fn call(client: &mut Client, stream: u32, sql: &str, values: Vec<Cell>, want: &str) -> SqlTxAnswer {
    let call =
        SqlTxCall { text: sql.to_owned(), values, want: want.to_owned(), commit: false, method: None, body: None };
    client.write(Frame::new(Kind::Data, stream, call.encode()));
    let answer = client.next_on(stream);
    assert_eq!((answer.kind, answer.flags), (Kind::Data, 0), "an answer, the transaction open");
    SqlTxAnswer::decode(&answer.body).unwrap()
}

fn end(client: &mut Client, stream: u32, commit: bool) -> Frame {
    let last = SqlTxCall { commit, ..SqlTxCall::default() };
    client.write(Frame::new(Kind::Data, stream, last.encode()).ending());
    client.next_on(stream)
}

#[test]
fn a_transaction_holds_its_calls_until_its_last_data_commits_or_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client);
    insert(&mut client, handle, "n1", "Buy milk");

    let stream = begin(&mut client, handle);
    let read = call(&mut client, stream, "select count(*) from notes", Vec::new(), "scalar");
    assert_eq!(read.rows.unwrap().rows, [vec![Cell::Int(1)]]);
    let wrote =
        call(&mut client, stream, "insert into notes (id, title) values (?, ?)", vec![text("n2"), text("b")], "exec");
    assert_eq!(wrote.done.unwrap().changes, 1);
    let taken = call(&mut client, stream, "insert into notes (id, title) values ('n1', 'again')", Vec::new(), "exec");
    assert_eq!(taken.failure.unwrap().code, "conflict", "a call that fails leaves the transaction open");
    let seen = call(&mut client, stream, "select count(*) from notes", Vec::new(), "scalar");
    assert_eq!(seen.rows.unwrap().rows, [vec![Cell::Int(2)]], "a read sees the transaction's own write");
    let last = end(&mut client, stream, true);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "committed");
    assert_eq!(query(&mut client, handle, "select count(*) from notes", "scalar").rows, [vec![Cell::Int(2)]]);

    let stream = begin(&mut client, handle);
    call(&mut client, stream, "delete from notes", Vec::new(), "exec");
    assert_eq!(end(&mut client, stream, false).flags, frame::END, "rolled back as asked");
    assert_eq!(query(&mut client, handle, "select count(*) from notes", "scalar").rows, [vec![Cell::Int(2)]]);
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn a_transaction_whose_client_stalls_or_cancels_is_rolled_back_by_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client);

    let stalled = begin(&mut client, handle);
    call(&mut client, stalled, "insert into notes (id, title) values ('n1', 'a')", Vec::new(), "exec");
    let last = client.next_on(stalled);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END | frame::ERROR), "the server's timer ended it");
    assert_eq!(answered::<Empty>(&last).unwrap_err().code, "limit");

    let cancelled = begin(&mut client, handle);
    call(&mut client, cancelled, "insert into notes (id, title) values ('n2', 'b')", Vec::new(), "exec");
    client.write(Frame::new(Kind::Cancel, cancelled, Vec::new()));
    let last = client.next_on(cancelled);
    assert_eq!(answered::<Empty>(&last).unwrap_err().code, "cancelled");

    assert_eq!(
        query(&mut client, handle, "select count(*) from notes", "scalar").rows,
        [vec![Cell::Int(0)]],
        "both rolled back"
    );
    assert_eq!(insert(&mut client, handle, "n3", "c").changes, 1, "the writer is free again");
    assert_eq!(client.pipe.streams(), 0);
}

/// A call of kv or jobs inside a transaction: its method and its request.
#[cfg(feature = "jobs")]
fn nested(client: &mut Client, stream: u32, called: u16, request: &impl Message) -> SqlTxAnswer {
    let call = SqlTxCall {
        want: "call".to_owned(),
        method: Some(u64::from(called)),
        body: Some(request.encode()),
        ..SqlTxCall::default()
    };
    client.write(Frame::new(Kind::Data, stream, call.encode()));
    let answer = client.next_on(stream);
    assert_eq!((answer.kind, answer.flags), (Kind::Data, 0), "an answer, the transaction open");
    SqlTxAnswer::decode(&answer.body).unwrap()
}

#[cfg(feature = "jobs")]
#[test]
fn a_bucket_and_a_queue_opened_from_a_database_write_inside_its_transactions() {
    use crate::wire::codec::Row;
    use crate::wire::protocol::{
        JobsCall, JobsChanged, JobsId, JobsJob, JobsQueueOpen, KvBucketOpen, KvCall, KvEntry, KvWritten,
    };

    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let database = open(&mut client);
    let bucket_open = KvBucketOpen { name: "sessions".to_owned(), database: Some(database), ..KvBucketOpen::default() };
    let sessions = client.call::<Handle>(method::KV_BUCKET_OPEN, &bucket_open).unwrap().handle;
    let queue_open = JobsQueueOpen { name: "emails".to_owned(), database: Some(database), ..JobsQueueOpen::default() };
    let emails = client.call::<Handle>(method::JOBS_QUEUE_OPEN, &queue_open).unwrap().handle;
    let in_kv = client.call::<Handle>(
        method::KV_BUCKET_OPEN,
        &KvBucketOpen { name: "sessions".to_owned(), ..KvBucketOpen::default() },
    );
    let in_kv = in_kv.unwrap().handle;
    let set =
        KvCall { handle: sessions, key: "t-1".to_owned(), value: Some(Row::Bin(b"ann".to_vec())), ..KvCall::default() };
    let add = JobsCall { handle: emails, id: Some("n1".to_owned()), value: "{}".to_owned(), ..JobsCall::default() };
    let session = KvCall { handle: sessions, key: "t-1".to_owned(), ..KvCall::default() };
    let job = JobsId { handle: emails, id: "n1".to_owned() };

    for commit in [false, true] {
        let stream = begin(&mut client, database);
        call(&mut client, stream, "insert into notes (id, title) values ('n1', 'a')", Vec::new(), "exec");
        assert!(KvWritten::decode(&nested(&mut client, stream, method::KV_SET, &set).body.unwrap()).unwrap().written);
        assert!(
            JobsChanged::decode(&nested(&mut client, stream, method::JOBS_ADD, &add).body.unwrap()).unwrap().changed
        );
        let seen = nested(&mut client, stream, method::KV_GET, &session);
        assert!(KvEntry::decode(&seen.body.unwrap()).unwrap().found, "a read sees the transaction's own write");
        let elsewhere = KvCall { handle: in_kv, ..set.clone() };
        let refused = nested(&mut client, stream, method::KV_SET, &elsewhere).failure.unwrap();
        assert!(refused.message.contains("kept in kv.db, outside this transaction of sql app"), "{}", refused.message);
        assert_eq!(end(&mut client, stream, commit).flags, frame::END);

        let found = client.call::<KvEntry>(method::KV_GET, &session).unwrap().found;
        let queued = client.call::<JobsJob>(method::JOBS_GET, &job).unwrap().found;
        let rows = query(&mut client, database, "select count(*) from notes", "scalar").rows;
        assert_eq!(
            (found, queued, rows),
            (commit, commit, vec![vec![Cell::Int(i64::from(commit))]]),
            "commit {commit}"
        );
    }
    let in_kv_db = client.call::<KvEntry>(method::KV_GET, &KvCall { handle: in_kv, ..session.clone() }).unwrap();
    assert!(!in_kv_db.found, "kv.db is another file");
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn writes_through_a_query_in_flight_hold_no_thread_of_the_session() {
    use crate::pipe::{Connect, Pipe};
    use crate::{Options, Store, sql};

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let mut client = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
    client.hello(2).unwrap();
    let handle = open(&mut client);
    let held = store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    let (began, begun) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();

    std::thread::scope(|scope| {
        // a transaction of the database holds its writer, so that no write can commit
        let held = &held;
        let holding = scope.spawn(move || {
            held.tx(|tx| -> crate::Result<()> {
                tx.exec(sql!("insert into notes (id, title) values ('held', 'a')"))?;
                began.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
        });
        begun.recv().unwrap();
        let inserts: Vec<u32> = (0..40)
            .map(|n| {
                let text = "insert into notes (id, title) values (?, 'b') returning id".to_owned();
                let values = vec![Cell::Str(format!("n{n}"))];
                client.start(method::SQL_QUERY, &SqlQuery { handle, text, values, want: "one".to_owned() })
            })
            .collect();
        // more writes wait than the session has threads, and a read is still answered
        assert_eq!(query(&mut client, handle, "select count(*) from notes", "scalar").rows.len(), 1);
        release.send(()).unwrap();
        holding.join().unwrap().unwrap();
        for stream in inserts {
            assert_eq!(answered::<SqlRows>(&client.next_on(stream)).unwrap().rows.len(), 1);
        }
    });
    assert_eq!(query(&mut client, handle, "select id from notes", "all").rows.len(), 41);
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
    drop(client);
    store.close().unwrap();
}

#[test]
fn a_client_says_how_far_a_databases_commits_go_and_one_open_keeps_what_it_has() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let asking = |word: &str| SqlOpen { name: "fast".to_owned(), migrations: None, durability: Some(word.to_owned()) };
    client.call::<Handle>(method::SQL_OPEN, &asking("os")).unwrap();
    client.call::<Handle>(method::SQL_OPEN, &asking("os")).unwrap();

    let other = client.call::<Handle>(method::SQL_OPEN, &asking("full")).unwrap_err();
    assert_eq!(other.code, "invalid");
    assert_eq!(other.message, "sql fast: open with durability os, and asked for full: invalid");
    let unknown = SqlOpen { name: "other".to_owned(), migrations: None, durability: Some("fast".to_owned()) };
    let refused = client.call::<Handle>(method::SQL_OPEN, &unknown).unwrap_err();
    assert_eq!(refused.message, "sql other: durability \"fast\": it is full or os: invalid");
}

#[test]
fn a_read_connection_queries_on_a_reader_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path(), crate::Options::default()).unwrap();
    let mut owner = Client::admitted(&store, crate::pipe::Capability::Data);
    let handle = open(&mut owner);
    insert(&mut owner, handle, "n1", "kept");

    let mut reader = Client::admitted(&store, crate::pipe::Capability::Read);
    let read = open(&mut reader);
    let rows = query(&mut reader, read, "select title from notes", "all");
    assert_eq!(rows.rows.len(), 1);
    let by_columns = SqlStatement { handle: read, text: "select title from notes".to_owned(), values: Vec::new() };
    assert_eq!(reader.call::<SqlColumns>(method::SQL_COLUMNS, &by_columns).unwrap().total, 1);
    let copy = dir.path().join("copy.db");
    let other = dir.path().join("other.db");
    let writes = [
        "delete from notes returning id".to_owned(),
        format!("vacuum into '{}'", copy.display()),
        format!("attach database '{}' as other", other.display()),
        "pragma query_only = 0".to_owned(),
    ];
    for text in writes {
        let asked = SqlQuery { handle: read, text: text.clone(), values: Vec::new(), want: "all".to_owned() };
        let refused = reader.call::<SqlRows>(method::SQL_QUERY, &asked).unwrap_err();
        assert_eq!(refused.code, "permission", "{text}: {}", refused.message);
        let by_columns = SqlStatement { handle: read, text: text.clone(), values: Vec::new() };
        let refused = reader.call::<SqlColumns>(method::SQL_COLUMNS, &by_columns).unwrap_err();
        assert_eq!(refused.code, "permission", "by columns, {text}: {}", refused.message);
    }
    let statement = SqlStatement { handle: read, text: "delete from notes".to_owned(), values: Vec::new() };
    assert_eq!(reader.call::<SqlDone>(method::SQL_EXEC, &statement).unwrap_err().code, "permission");
    assert_eq!(query(&mut owner, handle, "select title from notes", "all").rows.len(), 1);
    assert!(!copy.exists() && !other.exists(), "no file was made");
}

#[test]
fn a_connection_that_writes_attaches_no_file_and_ends_no_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path(), crate::Options::default()).unwrap();
    let mut client = Client::admitted(&store, crate::pipe::Capability::Data);
    let handle = open(&mut client);
    insert(&mut client, handle, "n1", "kept");
    let kv = dir.path().join("kv.db").display().to_string().replace(char::from(b'\\'), "/");
    for text in [format!("attach database '{kv}' as kv"), "pragma query_only = off".to_owned(), "commit".to_owned()] {
        let asked = SqlQuery { handle, text: text.clone(), values: Vec::new(), want: "all".to_owned() };
        let refused = client.call::<SqlRows>(method::SQL_QUERY, &asked).unwrap_err();
        assert_eq!(refused.code, "invalid", "a query of {text}: {}", refused.message);
        let statement = SqlStatement { handle, text: text.clone(), values: Vec::new() };
        let refused = client.call::<SqlDone>(method::SQL_EXEC, &statement).unwrap_err();
        assert_eq!(refused.code, "invalid", "a write of {text}: {}", refused.message);
    }
    assert_eq!(query(&mut client, handle, "select title from notes", "all").rows.len(), 1);
}

#[test]
fn a_statements_columns_hold_what_its_rows_hold() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client);
    for (id, title) in [("n1", "Buy milk"), ("n2", "Call mom"), ("n3", "Write")] {
        insert(&mut client, handle, id, title);
    }
    exec(&mut client, handle, "update notes set done = 1 where id = ?", vec![text("n2")]);

    let read = "select id, done, done * 1.5 as load, nullif(done, 0) as only_done, nullif(done * 1.5, 0) as only_load, \
                iif(done, 1, 0.5) as either, null as empty, rowid * 9007199254740993 as past_a_float \
                from notes order by id";
    let asked = SqlStatement { handle, text: read.to_owned(), values: Vec::new() };
    let columns: SqlColumns = client.call(method::SQL_COLUMNS, &asked).unwrap();
    assert_eq!((columns.rows, columns.total), (3, 3));
    let packed: Vec<(&str, &str)> = columns
        .columns
        .iter()
        .map(|column| match (&column.integers, &column.reals, &column.values) {
            (Some(_), None, None) => (column.name.as_str(), "integers"),
            (None, Some(_), None) => (column.name.as_str(), "reals"),
            (None, None, Some(_)) => (column.name.as_str(), "values"),
            _ => panic!("{}: a column is one of three", column.name),
        })
        .collect();
    assert_eq!(
        packed,
        [
            ("id", "values"),
            ("done", "integers"),
            ("load", "reals"),
            ("only_done", "integers"),
            ("only_load", "reals"),
            ("either", "values"),
            ("empty", "values"),
            ("past_a_float", "integers"),
        ]
    );
    let nulls: Vec<Option<&[u8]>> = columns.columns.iter().map(|column| column.nulls.as_deref()).collect();
    let none: Option<&[u8]> = None;
    // n1 and n3 are not done: the lowest bit is the first row's
    assert_eq!(nulls, [none, none, none, Some(&[0b101][..]), Some(&[0b101][..]), none, none, none]);

    assert_eq!(rows_of(&columns), query(&mut client, handle, read, "all").rows, "an INTEGER goes whole, past 2^53 too");
}

#[test]
fn columns_past_one_message_come_in_parts_of_the_same_rows_within_the_clients_credit() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::connect(dir.path());
    let credit = 64 << 10;
    client.greet(Hello { protocol: 2, stream_credit: Some(credit), ..Hello::default() }).unwrap();
    let handle = open(&mut client);
    let title = "x".repeat(1000);
    for n in 0..200 {
        insert(&mut client, handle, &format!("n{n:03}"), &title);
    }

    let read = "select id, title, nullif(rowid % 3, 0) as third, rowid / 4.0 as quarter from notes order by id";
    let asked = SqlStatement { handle, text: read.to_owned(), values: Vec::new() };
    let stream = client.start(method::SQL_COLUMNS, &asked);
    let head = client.next_on(stream);
    assert_eq!((head.kind, head.flags), (Kind::Response, 0), "the columns do not fit one message");
    let (mut rows, mut parts, mut taken) = (Vec::new(), 0, 0u64);
    loop {
        while client.silent_on(stream, Duration::from_millis(50)) {
            assert!(taken > 0, "the first part goes without a grant");
            client.write(Frame::new(Kind::Credit, stream, u32::try_from(taken).unwrap().to_le_bytes().to_vec()));
            taken = 0;
        }
        let part = client.next_on(stream);
        assert!(part.body.len() as u64 <= credit, "a part within the credit");
        taken += part.body.len() as u64;
        let decoded = SqlColumns::decode(&part.body).unwrap();
        assert_eq!(decoded.total, 200, "every part says the rows of them all");
        let names: Vec<&str> = decoded.columns.iter().map(|column| column.name.as_str()).collect();
        assert_eq!(names, ["id", "title", "third", "quarter"], "every part holds every column");
        rows.extend(rows_of(&decoded));
        parts += 1;
        if part.flags & frame::END != 0 {
            break;
        }
    }
    assert!(parts > 3, "{parts} parts");
    let asked = SqlQuery { handle, text: read.to_owned(), values: Vec::new(), want: "all".to_owned() };
    let stream = client.start(method::SQL_QUERY, &asked);
    client.next_on(stream);
    let mut whole = Vec::new();
    loop {
        let part = client.next_on(stream);
        client.write(Frame::new(Kind::Credit, stream, u32::try_from(part.body.len()).unwrap().to_le_bytes().to_vec()));
        whole.extend(SqlRows::decode(&part.body).unwrap().rows);
        if part.flags & frame::END != 0 {
            break;
        }
    }
    assert_eq!(rows.len(), 200);
    assert_eq!(rows, whole, "the parts' columns hold what the rows hold");
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn a_row_larger_than_a_message_is_limit_by_columns_as_by_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::connect(dir.path());
    client.greet(Hello { protocol: 2, stream_credit: Some(64 << 10), ..Hello::default() }).unwrap();
    let handle = open(&mut client);
    exec(&mut client, handle, "insert into notes (id, title) values ('n1', printf('%.*c', 100000, 'x'))", Vec::new());

    let asked = SqlStatement { handle, text: "select id, title from notes".to_owned(), values: Vec::new() };
    let refused = client.call::<SqlColumns>(method::SQL_COLUMNS, &asked).unwrap_err();
    assert_eq!(refused.code, "limit", "{}", refused.message);
    assert!(refused.message.contains("past the 32768 one message of this connection carries"), "{}", refused.message);
    let asked = SqlQuery { handle, text: asked.text, values: Vec::new(), want: "all".to_owned() };
    assert_eq!(client.call::<SqlRows>(method::SQL_QUERY, &asked).unwrap_err().code, "limit");
}
