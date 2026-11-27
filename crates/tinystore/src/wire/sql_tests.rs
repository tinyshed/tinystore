use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use crate::TestClock;
use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::{Cell, Message};
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{
    Empty, Handle, Hello, SqlBatch, SqlBatched, SqlColumn, SqlColumns, SqlDescribed, SqlDescribedColumn, SqlDiff,
    SqlDone, SqlMigration, SqlMirror, SqlOpen, SqlOrder, SqlQuery, SqlRows, SqlStatement, SqlText, SqlTxAnswer,
    SqlTxCall, SqlTxOpen, method,
};

const NOTES: &str =
    "create table notes (id text primary key, title text not null, done integer not null default 0) strict";

fn open(client: &mut Client) -> u64 {
    let open = SqlOpen {
        name: "app".to_owned(),
        migrations: Some(vec![SqlMigration { name: "0001_notes.sql".to_owned(), sql: NOTES.to_owned() }]),
        durability: None,
        backup: None,
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
    let query = SqlQuery { handle, text: sql.to_owned(), values: Vec::new(), want: want.to_owned(), order: None };
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
                ..SqlText::default()
            },
            SqlText {
                text: "update notes set done = ? where id = ?".to_owned(),
                values: vec![Cell::Int(1), text("n1")],
                ..SqlText::default()
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
    client.greet(Hello { protocol: 1, stream_credit: Some(credit), ..Hello::default() }).unwrap();
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
        order: None,
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
    client.greet(Hello { protocol: 1, stream_credit: Some(64 << 10), ..Hello::default() }).unwrap();
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
        order: None,
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

/// A client of a store of its own, on `clock`, within `memory`, which takes
/// `credit` bytes of a stream before it grants more.
fn walking_client(
    dir: &std::path::Path,
    memory: Option<u64>,
    clock: &Arc<TestClock>,
    credit: u64,
) -> (crate::Store, Client) {
    let options = crate::Options { memory, clock: Some(clock.clone()), background: false, ..crate::Options::default() };
    let store = crate::Store::open(dir, options).unwrap();
    let mut client = Client::over(crate::pipe::Pipe::connect(&store, crate::pipe::Connect::default()).unwrap());
    client.greet(Hello { protocol: 1, stream_credit: Some(credit), ..Hello::default() }).unwrap();
    (store, client)
}

fn each(handle: u64, text: &str, order: Option<Vec<SqlOrder>>) -> SqlQuery {
    SqlQuery { handle, text: text.to_owned(), values: Vec::new(), want: "each".to_owned(), order }
}

/// Reads a walk's parts to its last, granting back what each took once the
/// stream goes silent, and calling `each_part` with each: the rows it gave,
/// and how the stream ended.
fn walked(client: &mut Client, stream: u32, mut each_part: impl FnMut(&SqlRows)) -> Result<(), String> {
    let mut taken = 0u64;
    loop {
        while client.silent_on(stream, Duration::from_millis(20)) {
            client.write(Frame::new(Kind::Credit, stream, u32::try_from(taken).unwrap().to_le_bytes().to_vec()));
            taken = 0;
        }
        let part = client.next_on(stream);
        taken += part.body.len() as u64;
        match answered::<SqlRows>(&part) {
            Ok(rows) => each_part(&rows),
            Err(failure) => return Err(format!("{}: {}", failure.code, failure.message)),
        }
        if part.flags & frame::END != 0 {
            return Ok(());
        }
    }
}

#[test]
fn each_sends_rows_past_what_all_holds_as_its_reader_reads_them() {
    // contract: SQL-13
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let (store, mut client) = walking_client(dir.path(), Some(16 << 20), &clock, 1 << 20);
    let handle = open(&mut client);
    exec(&mut client, handle, "create table big (id integer primary key, data blob not null)", Vec::new());
    let rows = "with recursive n(i) as (select 1 union all select i + 1 from n where i < 280) \
                insert into big (id, data) select i, zeroblob(262144) from n";
    exec(&mut client, handle, rows, Vec::new());

    let all = SqlQuery { want: "all".to_owned(), ..each(handle, "select id, data from big order by id", None) };
    assert_eq!(client.call::<SqlRows>(method::SQL_QUERY, &all).unwrap_err().code, "limit", "all holds every row");

    let stream = client.start(method::SQL_QUERY, &each(handle, "select id, data from big order by id", None));
    let head = client.next_on(stream);
    assert_eq!((head.kind, head.flags), (Kind::Response, 0), "the rows go in parts");
    let (mut ids, mut bytes, mut most) = (Vec::new(), 0, 0);
    let ended = walked(&mut client, stream, |part| {
        for row in &part.rows {
            let (Cell::Int(id), Cell::Bin(data)) = (&row[0], &row[1]) else { panic!("{row:?}") };
            ids.push(*id);
            bytes += data.len();
        }
        most = most.max(store.memory().used());
    });
    assert_eq!(ended, Ok(()));
    assert_eq!(ids, (1..=280).collect::<Vec<_>>());
    assert_eq!(bytes, 280 << 18, "70 MiB of rows");
    assert!(most <= 8 << 20, "the store held a few parts at once, not the rows: {most} bytes");
    assert_eq!(client.pipe.streams(), 0);
    drop(client);
    store.close().unwrap();
}

#[test]
fn each_answers_a_few_rows_whole_and_a_statement_that_fails_as_all_does() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client);
    insert(&mut client, handle, "n1", "Buy milk");

    let few: SqlRows = client.call(method::SQL_QUERY, &each(handle, "select id, title from notes", None)).unwrap();
    assert_eq!(
        (few.columns, few.rows),
        (vec!["id".to_owned(), "title".to_owned()], vec![vec![text("n1"), text("Buy milk")]])
    );
    let none: SqlRows = client.call(method::SQL_QUERY, &each(handle, "select id from notes where 0", None)).unwrap();
    assert_eq!((none.columns, none.rows.len()), (vec!["id".to_owned()], 0), "no rows, its columns named");
    let wrong = client.call::<SqlRows>(method::SQL_QUERY, &each(handle, "select nothing from nowhere", None));
    assert_eq!(wrong.unwrap_err().code, "invalid");
    let written = each(handle, "update notes set done = 1 returning id", None);
    assert_eq!(client.call::<SqlRows>(method::SQL_QUERY, &written).unwrap().rows, [vec![text("n1")]], "a write's rows");
    assert_eq!(client.pipe.streams(), 0);
}

/// A database of 60 notes of 10 KiB, three to a part of a walk within
/// `walking_client`'s 64 KiB of credit.
fn sixty_notes(client: &mut Client) -> u64 {
    let handle = open(client);
    let rows = "with recursive n(i) as (select 1 union all select i + 1 from n where i < 60) \
                insert into notes (id, title) select printf('n%03d', i), replace(hex(zeroblob(5120)), '0', 'x') from n";
    exec(client, handle, rows, Vec::new());
    handle
}

/// Takes the parts the first credit carries, waits while the walk's reader
/// fills the parts ahead of a client that grants nothing, then moves the
/// clock past one snapshot's five seconds and lets the reader find it. Gives
/// the bytes taken and their rows.
fn outlast_the_snapshot(client: &mut Client, stream: u32, clock: &TestClock) -> (u64, Vec<Vec<Cell>>) {
    let (mut taken, mut rows) = (0, Vec::new());
    while !client.silent_on(stream, Duration::from_millis(300)) {
        let part = client.next_on(stream);
        taken += part.body.len() as u64;
        rows.extend(SqlRows::decode(&part.body).unwrap().rows);
    }
    clock.advance(Duration::from_secs(6));
    std::thread::sleep(Duration::from_millis(300));
    (taken, rows)
}

#[test]
fn each_ordered_by_its_key_reads_on_past_its_snapshot_and_without_one_fails_limit_naming_list() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let (store, mut client) = walking_client(dir.path(), None, &clock, 64 << 10);
    let handle = sixty_notes(&mut client);
    let notes = "select id, title from notes order by id";

    let order = vec![SqlOrder { column: "id".to_owned(), descending: false }];
    let stream = client.start(method::SQL_QUERY, &each(handle, notes, Some(order)));
    assert_eq!(client.next_on(stream).kind, Kind::Response);
    let (taken, mut rows) = outlast_the_snapshot(&mut client, stream, &clock);
    exec(&mut client, handle, "update notes set title = 'changed' where id = 'n055'", Vec::new());
    insert(&mut client, handle, "n061", "new");
    client.write(Frame::new(Kind::Credit, stream, u32::try_from(taken).unwrap().to_le_bytes().to_vec()));
    assert_eq!(walked(&mut client, stream, |part| rows.extend(part.rows.iter().cloned())), Ok(()));
    let ids: Vec<Cell> = rows.iter().map(|row| row[0].clone()).collect();
    let every: Vec<Cell> = (1..=61).map(|n| text(&format!("n{n:03}"))).collect();
    assert_eq!(ids, every, "each row once, in order, the new one among them");
    assert!(rows.iter().any(|row| row == &vec![text("n055"), text("changed")]), "read as it is after the snapshot");

    let stream = client.start(method::SQL_QUERY, &each(handle, notes, None));
    assert_eq!(client.next_on(stream).kind, Kind::Response);
    let (taken, _) = outlast_the_snapshot(&mut client, stream, &clock);
    client.write(Frame::new(Kind::Credit, stream, u32::try_from(taken).unwrap().to_le_bytes().to_vec()));
    let failed = walked(&mut client, stream, |_| {}).unwrap_err();
    assert!(failed.starts_with("limit") && failed.contains("list"), "{failed}");
    assert_eq!(client.pipe.streams(), 0);
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
    let call = SqlTxCall { text: sql.to_owned(), values, want: want.to_owned(), ..SqlTxCall::default() };
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

// contract: WIRE-06
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

// contract: WIRE-03
#[test]
fn a_transactions_call_gives_its_credit_back_once_it_is_answered() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client);
    // one transaction holds the writer, so that the next waits its turn, its call queued
    let holding = begin(&mut client, handle);
    call(&mut client, holding, "insert into notes (id, title) values ('held', 'a')", Vec::new(), "exec");
    let waiting = client.open_stream(method::SQL_TX, &SqlTxOpen { handle });
    let insert = SqlTxCall {
        text: "insert into notes (id, title) values ('waited', 'b')".to_owned(),
        want: "exec".to_owned(),
        ..SqlTxCall::default()
    };
    let sent = insert.encode();
    client.write(Frame::new(Kind::Data, waiting, sent.clone()));
    // Ending the one that holds the writer reads past every frame sent so far:
    // a grant for the queued call, had it gone back at once, among them.
    let last = end(&mut client, holding, true);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "{:?}", answered::<Empty>(&last));
    let opened = client.next_on(waiting);
    assert_eq!((opened.kind, opened.flags), (Kind::Response, 0), "the waiting transaction opened");
    assert_eq!(client.credit_on(waiting), sent.len() as u64, "the call's credit, once the call ran");
    let answer = SqlTxAnswer::decode(&client.next_on(waiting).body).unwrap();
    assert_eq!(answer.done.map(|done| done.changes), Some(1), "{:?}", answer.failure);
    assert_eq!(end(&mut client, waiting, true).flags, frame::END, "committed");
    assert_eq!(query(&mut client, handle, "select count(*) from notes", "scalar").rows, [vec![Cell::Int(2)]]);
    assert_eq!(client.pipe.streams(), 0);
}

// contract: WIRE-02, DUR-02
#[test]
fn a_transactions_data_that_is_not_a_call_is_invalid_naming_its_field_and_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client);
    let stream = begin(&mut client, handle);
    call(&mut client, stream, "insert into notes (id, title) values ('n1', 'a')", Vec::new(), "exec");
    // a map of one field, 3, `want`, whose value is a number rather than text
    client.write(Frame::new(Kind::Data, stream, vec![0x81, 3, 7]));
    let refused = client.next_on(stream);
    assert_eq!((refused.kind, refused.flags), (Kind::Data, 0), "an answer, the transaction open");
    let failure = SqlTxAnswer::decode(&refused.body).unwrap().failure.unwrap();
    assert_eq!((failure.code.as_str(), failure.message.as_str()), ("invalid", "want is not a str"));
    let seen = call(&mut client, stream, "select count(*) from notes", Vec::new(), "scalar");
    assert_eq!(seen.rows.unwrap().rows, [vec![Cell::Int(1)]], "the write before it stands");

    // a last one whose field 4, `commit`, is a number says neither commit nor roll back
    client.write(Frame::new(Kind::Data, stream, vec![0x81, 4, 7]).ending());
    let last = client.next_on(stream);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END | frame::ERROR));
    let failure = answered::<Empty>(&last).unwrap_err();
    assert_eq!(failure.code, "invalid", "{failure:?}");
    assert_eq!(failure.message, "commit is not a bool, and the transaction rolled back");
    let rows = query(&mut client, handle, "select count(*) from notes", "scalar").rows;
    assert_eq!(rows, [vec![Cell::Int(0)]], "rolled back");
    assert_eq!(client.pipe.streams(), 0);
}

/// A client whose stream credit is 64 KiB, so that a read of 400 KB in a
/// transaction comes in parts of 32 KiB, two of them within the credit.
fn of_little_credit(dir: &std::path::Path) -> Client {
    let mut client = Client::connect(dir);
    let hello = Hello { protocol: 1, client: "test/0".to_owned(), stream_credit: Some(64 << 10), ..Hello::default() };
    client.greet(hello).unwrap();
    client
}

const TWO_THOUSAND_NOTES: &str = "with recursive n(i) as (select 1 union all select i + 1 from n where i < 2000)     insert into notes (id, title) select printf('n%05d', i), printf('%0200d', i) from n";

// contract: SQL-14
#[test]
fn a_read_in_a_transaction_past_one_answer_comes_in_parts_to_a_client_that_asks_for_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = of_little_credit(dir.path());
    let handle = open(&mut client);
    let stream = begin(&mut client, handle);
    assert_eq!(call(&mut client, stream, TWO_THOUSAND_NOTES, Vec::new(), "exec").done.unwrap().changes, 2000);

    let read = SqlTxCall {
        text: "select * from notes order by id".to_owned(),
        want: "all".to_owned(),
        parts: true,
        ..SqlTxCall::default()
    };
    client.write(Frame::new(Kind::Data, stream, read.encode()));
    let (mut rows, mut parts) = (Vec::new(), 0);
    loop {
        let frame = client.next_on(stream);
        assert_eq!((frame.kind, frame.flags), (Kind::Data, 0), "an answer, the transaction open");
        assert!(frame.body.len() <= 32 << 10, "a part of {} bytes, within half the credit", frame.body.len());
        let grant = u32::try_from(frame.body.len()).unwrap().to_le_bytes().to_vec();
        client.write(Frame::new(Kind::Credit, stream, grant));
        let answer = SqlTxAnswer::decode(&frame.body).unwrap();
        assert_eq!(answer.failure, None);
        let part = answer.rows.unwrap();
        if parts == 0 {
            assert_eq!(part.columns, ["id", "title", "done"], "the first part names the columns");
        }
        rows.extend(part.rows);
        parts += 1;
        if !answer.more {
            break;
        }
    }
    assert!(parts > 10, "{parts} parts");
    let ids: Vec<Cell> = rows.iter().map(|row| row[0].clone()).collect();
    assert_eq!(ids, (1..=2000).map(|n| text(&format!("n{n:05}"))).collect::<Vec<_>>(), "every row once, in order");

    let after = call(&mut client, stream, "select count(*) from notes", Vec::new(), "scalar");
    assert_eq!(after.rows.unwrap().rows, [vec![Cell::Int(2000)]], "the transaction goes on after the parts");
    assert_eq!(end(&mut client, stream, true).flags, frame::END, "committed");
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn a_read_in_a_transaction_past_one_answer_is_refused_to_a_client_that_reads_one_naming_the_release_that_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = of_little_credit(dir.path());
    let handle = open(&mut client);
    let stream = begin(&mut client, handle);
    call(&mut client, stream, TWO_THOUSAND_NOTES, Vec::new(), "exec");
    let refused = call(&mut client, stream, "select * from notes", Vec::new(), "all").failure.unwrap();
    assert_eq!(refused.code, "limit");
    assert!(refused.message.contains("one answer of this client carries, 32768 bytes"), "{}", refused.message);
    assert!(refused.message.contains("a client of 0.11 reads them whole"), "{}", refused.message);
    assert!(!refused.message.contains("outside the transaction"), "{}", refused.message);
    let seen = call(&mut client, stream, "select count(*) from notes", Vec::new(), "scalar");
    assert_eq!(seen.rows.unwrap().rows, [vec![Cell::Int(2000)]], "the refusal leaves the transaction open");
    assert_eq!(end(&mut client, stream, true).flags, frame::END, "committed");
}

/// Opens a transaction on `handle` that inserts the note `id` and commits,
/// every call sent before any answer comes, as a client with many in flight
/// sends them.
fn insert_in_tx(client: &mut Client, handle: u64, id: &str) -> u32 {
    let stream = client.open_stream(method::SQL_TX, &SqlTxOpen { handle });
    let insert = SqlTxCall {
        text: "insert into notes (id, title) values (?, 'waited')".to_owned(),
        values: vec![text(id)],
        want: "exec".to_owned(),
        ..SqlTxCall::default()
    };
    client.write(Frame::new(Kind::Data, stream, insert.encode()));
    client.write(Frame::new(Kind::Data, stream, SqlTxCall { commit: true, ..SqlTxCall::default() }.encode()).ending());
    stream
}

/// Reads a transaction of [`insert_in_tx`] to its end: it inserted its note
/// and committed.
fn committed(client: &mut Client, stream: u32) {
    let opened = client.next_on(stream);
    assert_eq!((opened.kind, opened.flags), (Kind::Response, 0), "stream {stream} opened");
    let inserted = client.next_on(stream);
    let answer = SqlTxAnswer::decode(&inserted.body).unwrap();
    assert_eq!(answer.done.map(|done| done.changes), Some(1), "stream {stream}: {:?}", answer.failure);
    let last = client.next_on(stream);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "stream {stream}: {:?}", answered::<Empty>(&last));
}

/// A call a worker answers, while transactions wait for the writer: the
/// database opened again, which writes nothing.
fn open_again(client: &mut Client) {
    let again = SqlOpen { name: "app".to_owned(), migrations: None, durability: None, backup: None };
    client.call::<Handle>(method::SQL_OPEN, &again).expect("a call on a worker is answered");
}

#[test]
fn transactions_past_the_workers_wait_their_turn_on_no_thread_and_each_commits() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client);

    // one transaction holds the writer, its client still to say how it ends
    let holding = begin(&mut client, handle);
    call(&mut client, holding, "insert into notes (id, title) values ('held', 'a')", Vec::new(), "exec");
    let waiting: Vec<u32> =
        (0..3 * crate::pool::MOST).map(|n| insert_in_tx(&mut client, handle, &format!("n{n}"))).collect();
    // The client's next call runs on a worker, and the one holding the
    // writer goes on only once it is answered: none of the threads may be
    // waiting for the writer.
    open_again(&mut client);
    let last = end(&mut client, holding, true);
    assert_eq!((last.kind, last.flags), (Kind::Data, frame::END), "{:?}", answered::<Empty>(&last));

    for stream in waiting {
        committed(&mut client, stream);
    }
    let rows = query(&mut client, handle, "select count(*) from notes", "scalar").rows;
    assert_eq!(rows, [vec![Cell::Int(1 + 3 * crate::pool::MOST as i64)]]);
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn transactions_of_every_connection_wait_for_a_writer_held_elsewhere_on_one_worker() {
    use crate::pipe::{Connect, Pipe};
    use crate::{Options, Store, sql};

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let connect = || {
        let mut client = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
        client.hello(1).unwrap();
        client
    };
    let mut clients: Vec<Client> = (0..4).map(|_| connect()).collect();
    let handles: Vec<u64> = clients.iter_mut().map(open).collect();
    let held = store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();

    std::thread::scope(|scope| {
        // made here, so that a failing assertion lets the holder go rather than wait for it
        let (began, begun) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let held = &held;
        let holding = scope.spawn(move || {
            held.tx(|tx| -> crate::Result<()> {
                tx.exec(sql!("insert into notes (id, title) values ('held', 'a')"))?;
                began.send(()).unwrap();
                let _ = released.recv();
                Ok(())
            })
        });
        begun.recv().unwrap();
        let waiting: Vec<(usize, u32)> = (0..2 * crate::pool::MOST)
            .map(|n| {
                let at = n % clients.len();
                (at, insert_in_tx(&mut clients[at], handles[at], &format!("n{n}")))
            })
            .collect();
        open_again(&mut connect());
        release.send(()).unwrap();
        holding.join().unwrap().unwrap();
        for (at, stream) in waiting {
            committed(&mut clients[at], stream);
        }
    });
    let rows = query(&mut clients[0], handles[0], "select count(*) from notes", "scalar").rows;
    assert_eq!(rows, [vec![Cell::Int(1 + 2 * crate::pool::MOST as i64)]]);
    drop(clients);
    store.close().unwrap();
}

#[test]
fn a_query_its_client_cancels_stops_and_frees_its_worker_and_reader() {
    // contract: WIRE-10
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client);
    // more queries of minutes than the session has workers and the file readers
    let text = "with recursive c(x) as (select 1 union all select x + 1 from c where x < 1000000000) \
        select count(*) from c";
    let slow: Vec<u32> = (0..20)
        .map(|_| {
            let asked =
                SqlQuery { handle, text: text.to_owned(), values: Vec::new(), want: "scalar".to_owned(), order: None };
            client.start(method::SQL_QUERY, &asked)
        })
        .collect();
    std::thread::sleep(Duration::from_millis(100));
    for stream in &slow {
        client.write(Frame::new(Kind::Cancel, *stream, Vec::new()));
    }
    for stream in slow {
        assert_eq!(answered::<SqlRows>(&client.next_on(stream)).unwrap_err().code, "cancelled");
    }
    let next = std::time::Instant::now();
    assert_eq!(query(&mut client, handle, "select 1", "scalar").rows, [vec![Cell::Int(1)]]);
    assert!(next.elapsed() < Duration::from_secs(2), "the next query waited {:?}", next.elapsed());
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
}

#[test]
fn the_queries_of_a_client_that_leaves_stop() {
    // contract: WIRE-10
    use crate::pipe::{Connect, Pipe};
    use crate::{Options, Store};

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let mut leaving = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
    leaving.hello(1).unwrap();
    let handle = open(&mut leaving);
    let text = "with recursive c(x) as (select 1 union all select x + 1 from c where x < 1000000000) \
        select count(*) from c";
    for _ in 0..20 {
        let asked =
            SqlQuery { handle, text: text.to_owned(), values: Vec::new(), want: "scalar".to_owned(), order: None };
        leaving.start(method::SQL_QUERY, &asked);
    }
    std::thread::sleep(Duration::from_millis(100));
    drop(leaving);
    let mut next = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
    next.hello(1).unwrap();
    let handle = open(&mut next);
    let started = std::time::Instant::now();
    assert_eq!(query(&mut next, handle, "select 1", "scalar").rows, [vec![Cell::Int(1)]]);
    assert!(started.elapsed() < Duration::from_secs(2), "the next client waited {:?}", started.elapsed());
    drop(next);
    store.close().unwrap();
}

/// A call of kv, jobs or records inside a transaction: its method and its
/// request.
#[cfg(any(feature = "jobs", feature = "records"))]
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
    let job = JobsId { handle: emails, id: "n1".to_owned(), row: None };

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

// contract: ERR-03
// contract: WIRE-01
#[cfg(any(feature = "jobs", feature = "records"))]
#[test]
fn a_take_or_a_delete_given_a_ttl_inside_a_transaction_is_invalid_as_in_the_core() {
    use crate::wire::protocol::{KvBucketOpen, KvCall};

    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let database = open(&mut client);
    let bucket_open = KvBucketOpen { name: "sessions".to_owned(), database: Some(database), ..KvBucketOpen::default() };
    let sessions = client.call::<Handle>(method::KV_BUCKET_OPEN, &bucket_open).unwrap().handle;
    let timed = KvCall { handle: sessions, key: "t-1".to_owned(), ttl: Some(60_000), ..KvCall::default() };
    let stream = begin(&mut client, database);
    for (called, verb) in [(method::KV_TAKE, "take"), (method::KV_DELETE, "delete")] {
        let refused = nested(&mut client, stream, called, &timed).failure.unwrap();
        assert_eq!(refused.code, "invalid", "{verb}: {}", refused.message);
        assert!(refused.message.contains(&format!("a ttl or a time on a {verb}")), "{}", refused.message);
    }
    assert_eq!(end(&mut client, stream, false).flags, frame::END);
}

// contract: JOB-02
#[cfg(feature = "jobs")]
#[test]
fn a_batch_carries_a_queues_add_which_commits_with_its_rows() {
    use crate::wire::protocol::{JobsCall, JobsId, JobsJob, JobsQueueOpen};

    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let database = open(&mut client);
    let mut queue = |database| {
        let open = JobsQueueOpen { name: "indexing".to_owned(), database, ..JobsQueueOpen::default() };
        client.call::<Handle>(method::JOBS_QUEUE_OPEN, &open).unwrap().handle
    };
    let (indexing, in_jobs) = (queue(Some(database)), queue(None));
    let insert = |id: &str| SqlText {
        text: "insert into notes (id, title) values (?, 'a')".to_owned(),
        values: vec![text(id)],
        ..SqlText::default()
    };
    let add = |handle, id: &str| SqlText {
        method: Some(u64::from(method::JOBS_ADD)),
        body: Some(
            JobsCall { handle, id: Some(id.to_owned()), value: "{}".to_owned(), ..JobsCall::default() }.encode(),
        ),
        ..SqlText::default()
    };
    let batch = |statements| SqlBatch { handle: database, statements };
    let found = |client: &mut Client, id: &str| {
        let job = JobsId { handle: indexing, id: id.to_owned(), row: None };
        client.call::<JobsJob>(method::JOBS_GET, &job).unwrap().found
    };

    let batched: SqlBatched =
        client.call(method::SQL_BATCH, &batch(vec![insert("n1"), add(indexing, "n1"), add(indexing, "n1")])).unwrap();
    assert_eq!(batched.done.iter().map(|done| done.changes).collect::<Vec<_>>(), [1, 1, 0], "the id was taken");
    assert!(found(&mut client, "n1"));

    let taken = client.call::<SqlBatched>(method::SQL_BATCH, &batch(vec![add(indexing, "n2"), insert("n1")]));
    assert_eq!(taken.unwrap_err().code, "conflict");
    assert!(!found(&mut client, "n2"), "the job rolled back with the row");

    let elsewhere = client.call::<SqlBatched>(method::SQL_BATCH, &batch(vec![insert("n3"), add(in_jobs, "n3")]));
    let elsewhere = elsewhere.unwrap_err();
    assert!(elsewhere.message.contains("kept in jobs.db, outside this batch of sql app"), "{}", elsewhere.message);
    let cancel = SqlText {
        method: Some(u64::from(method::JOBS_CANCEL)),
        body: Some(JobsId { handle: indexing, id: "n1".to_owned(), row: None }.encode()),
        ..SqlText::default()
    };
    let cancelled = client.call::<SqlBatched>(method::SQL_BATCH, &batch(vec![insert("n4"), cancel]));
    assert_eq!(cancelled.unwrap_err().code, "invalid", "a batch takes a queue's add alone");
    let with_text = SqlText { text: "delete from notes".to_owned(), ..add(indexing, "n5") };
    assert_eq!(client.call::<SqlBatched>(method::SQL_BATCH, &batch(vec![with_text])).unwrap_err().code, "invalid");

    let rows = query(&mut client, database, "select count(*) from notes", "scalar").rows;
    assert_eq!(rows, [vec![Cell::Int(1)]], "a refused batch wrote nothing");
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn writes_through_a_query_in_flight_hold_no_thread_of_the_session() {
    use crate::pipe::{Connect, Pipe};
    use crate::{Options, Store, sql};

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let mut client = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
    client.hello(1).unwrap();
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
                client.start(method::SQL_QUERY, &SqlQuery { handle, text, values, want: "one".to_owned(), order: None })
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
    let asking = |word: &str| SqlOpen {
        name: "fast".to_owned(),
        migrations: None,
        durability: Some(word.to_owned()),
        backup: None,
    };
    client.call::<Handle>(method::SQL_OPEN, &asking("os")).unwrap();
    client.call::<Handle>(method::SQL_OPEN, &asking("os")).unwrap();

    let other = client.call::<Handle>(method::SQL_OPEN, &asking("full")).unwrap_err();
    assert_eq!(other.code, "invalid");
    assert_eq!(other.message, "sql fast: open with durability os, and asked for full: invalid");
    let unknown =
        SqlOpen { name: "other".to_owned(), migrations: None, durability: Some("fast".to_owned()), backup: None };
    let refused = client.call::<Handle>(method::SQL_OPEN, &unknown).unwrap_err();
    assert_eq!(refused.message, "sql other: durability \"fast\": it is full or os: invalid");
}

// contract: SQL-16
#[test]
fn describe_names_a_statements_columns_without_its_rows_and_a_read_connection_asks_it_too() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path(), crate::Options::default()).unwrap();
    let mut owner = Client::admitted(&store, crate::pipe::Capability::Data);
    open(&mut owner);
    let mut reader = Client::admitted(&store, crate::pipe::Capability::Read);
    let read = open(&mut reader);
    let column = |name: &str, declared: Option<&str>| SqlDescribedColumn {
        name: name.to_owned(),
        declared: declared.map(str::to_owned),
    };
    let asked = |text: &str| SqlStatement { handle: read, text: text.to_owned(), values: Vec::new() };
    let described: SqlDescribed = reader
        .call(method::SQL_DESCRIBE, &asked("select done, upper(title) as shout, id from notes where id = ?"))
        .unwrap();
    assert_eq!(described.columns, [column("done", Some("INTEGER")), column("shout", None), column("id", Some("TEXT"))]);

    let copy = dir.path().join("copy.db");
    for text in ["delete from notes returning id".to_owned(), format!("vacuum into '{}'", copy.display())] {
        let refused = reader.call::<SqlDescribed>(method::SQL_DESCRIBE, &asked(&text)).unwrap_err();
        assert_eq!(refused.code, "invalid", "{text}: {}", refused.message);
    }
    let mistyped = reader.call::<SqlDescribed>(method::SQL_DESCRIBE, &asked("select titel from notes")).unwrap_err();
    assert_eq!((mistyped.code.as_str(), mistyped.message.contains("no such column: titel")), ("invalid", true));
    assert!(!copy.exists(), "nothing ran");
}

// contract: SQL-17
#[test]
fn a_mirror_answers_what_it_added_changed_and_dropped_and_refuses_what_cannot_be_right() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path(), crate::Options::default()).unwrap();
    let mut client = Client::admitted(&store, crate::pipe::Capability::Data);
    let handle = open(&mut client);
    exec(
        &mut client,
        handle,
        "create table works (course_id integer, id text, grade real, primary key (course_id, id))",
        vec![],
    );
    let row = |id: &str, grade: f64| vec![Cell::Int(1), text(id), Cell::Float(grade)];
    let asked = |rows: Vec<Vec<Cell>>| SqlMirror {
        handle,
        table: "works".to_owned(),
        key: vec!["course_id".to_owned(), "id".to_owned()],
        gone: None,
        change_on: None,
        mode: "replace".to_owned(),
        scope: Some(vec!["course_id".to_owned()]),
        scope_values: Some(vec![Cell::Int(1)]),
        rows: SqlRows { columns: vec!["course_id".to_owned(), "id".to_owned(), "grade".to_owned()], rows },
        min_kept: None,
    };
    let first: SqlDiff = client.call(method::SQL_MIRROR, &asked(vec![row("a", 4.0), row("b", 3.0)])).unwrap();
    assert_eq!((first.added.len(), first.same), (2, 0));
    let diff: SqlDiff = client.call(method::SQL_MIRROR, &asked(vec![row("a", 4.0), row("c", 1.0)])).unwrap();
    assert_eq!(diff.columns, ["course_id", "id", "grade"]);
    assert_eq!((diff.added, diff.gone, diff.same, diff.suspect), (vec![row("c", 1.0)], vec![row("b", 3.0)], 1, false));
    assert!(diff.before.is_empty() && diff.after.is_empty());
    let guarded = SqlMirror { min_kept: Some(0.9), ..asked(vec![]) };
    let held: SqlDiff = client.call(method::SQL_MIRROR, &guarded).unwrap();
    assert!(held.suspect);
    assert_eq!(
        query(&mut client, handle, "select id from works order by id", "all").rows.len(),
        2,
        "nothing was written"
    );

    let unknown = SqlMirror { mode: "patch".to_owned(), ..asked(vec![]) };
    let refused = client.call::<SqlDiff>(method::SQL_MIRROR, &unknown).unwrap_err();
    assert_eq!(refused.message, "sql app: mirror works: a mirror's mode \"patch\": it is replace or upsert");
    let several = SqlMirror { key: vec!["id".to_owned()], ..asked(vec![]) };
    let refused = client.call::<SqlDiff>(method::SQL_MIRROR, &several).unwrap_err();
    assert_eq!(refused.code, "invalid");
    assert!(refused.message.starts_with("sql app: mirror works: the key (id) names no one row"), "{}", refused.message);
    let mut reader = Client::admitted(&store, crate::pipe::Capability::Read);
    let read = open(&mut reader);
    let refused = reader.call::<SqlDiff>(method::SQL_MIRROR, &SqlMirror { handle: read, ..asked(vec![]) }).unwrap_err();
    assert_eq!(refused.code, "permission");
}

// contract: SEC-01
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
        let asked =
            SqlQuery { handle: read, text: text.clone(), values: Vec::new(), want: "all".to_owned(), order: None };
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

    // a file's durability is its writers', which the first open fixes
    let durable =
        SqlOpen { name: "later".to_owned(), migrations: None, durability: Some("os".to_owned()), backup: None };
    let refused = reader.call::<Handle>(method::SQL_OPEN, &durable).unwrap_err();
    assert_eq!(refused.code, "permission", "{}", refused.message);
}

// contract: SQL-02
#[test]
fn a_connection_that_writes_attaches_no_file_and_ends_no_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path(), crate::Options::default()).unwrap();
    let mut client = Client::admitted(&store, crate::pipe::Capability::Data);
    let handle = open(&mut client);
    insert(&mut client, handle, "n1", "kept");
    let kv = dir.path().join("kv.db").display().to_string().replace(char::from(b'\\'), "/");
    for text in [format!("attach database '{kv}' as kv"), "pragma query_only = off".to_owned(), "commit".to_owned()] {
        let asked = SqlQuery { handle, text: text.clone(), values: Vec::new(), want: "all".to_owned(), order: None };
        let refused = client.call::<SqlRows>(method::SQL_QUERY, &asked).unwrap_err();
        assert_eq!(refused.code, "invalid", "a query of {text}: {}", refused.message);
        let statement = SqlStatement { handle, text: text.clone(), values: Vec::new() };
        let refused = client.call::<SqlDone>(method::SQL_EXEC, &statement).unwrap_err();
        assert_eq!(refused.code, "invalid", "a write of {text}: {}", refused.message);
    }
    assert_eq!(query(&mut client, handle, "select title from notes", "all").rows.len(), 1);
}

// contract: SQL-06
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
    client.greet(Hello { protocol: 1, stream_credit: Some(credit), ..Hello::default() }).unwrap();
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
    let asked = SqlQuery { handle, text: read.to_owned(), values: Vec::new(), want: "all".to_owned(), order: None };
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
    client.greet(Hello { protocol: 1, stream_credit: Some(64 << 10), ..Hello::default() }).unwrap();
    let handle = open(&mut client);
    exec(&mut client, handle, "insert into notes (id, title) values ('n1', printf('%.*c', 100000, 'x'))", Vec::new());

    let asked = SqlStatement { handle, text: "select id, title from notes".to_owned(), values: Vec::new() };
    let refused = client.call::<SqlColumns>(method::SQL_COLUMNS, &asked).unwrap_err();
    assert_eq!(refused.code, "limit", "{}", refused.message);
    assert!(refused.message.contains("past the 32768 one message of this connection carries"), "{}", refused.message);
    let asked = SqlQuery { handle, text: asked.text, values: Vec::new(), want: "all".to_owned(), order: None };
    assert_eq!(client.call::<SqlRows>(method::SQL_QUERY, &asked).unwrap_err().code, "limit");
}

#[test]
fn rows_written_as_they_are_read_are_the_message_its_codec_writes() {
    use super::{Parted, RowBytes, row_parts};
    use crate::sql::{Kept, Rows, Sql, Wanted};

    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path(), crate::Options::default()).unwrap();
    let db = store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    db.exec(Sql::new("insert into notes (id, title, done) values ('n1', 'Buy milk', 0), ('n2', 'Call mom', 1)"))
        .unwrap();
    let statements = [
        "select id, title, done, done * 1.5 as load, null as empty, x'00ff' as raw, -9007199254740993 as far from notes",
        "select id from notes where 0",
        "select 1 where 0",
    ];
    for text in statements {
        let read = Sql::new(text);
        let written: Kept<RowBytes> = db.read(&read, Wanted::All).unwrap().expect("a query");
        let kept: Rows = db.read(&read, Wanted::All).unwrap().expect("a query");
        assert_eq!(
            written.parts(1 << 20).unwrap(),
            row_parts(kept, 1 << 20)
                .unwrap()
                .into_iter()
                .map(|part| part.encode())
                .collect::<std::collections::VecDeque<_>>(),
            "{text}"
        );
    }
}

// contract: DUR-06
#[cfg(feature = "blobs")]
#[test]
fn files_opened_from_a_database_write_inside_its_transactions() {
    use crate::wire::protocol::{BlobsAt, BlobsGot, BlobsHead, BlobsOpen, BlobsWrite, BlobsWritten};

    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let database = open(&mut client);
    let files_open = BlobsOpen { name: "packages".to_owned(), database: Some(database), ..BlobsOpen::default() };
    let packages = client.call::<Handle>(method::BLOBS_OPEN, &files_open).unwrap().handle;
    let package = vec![7_u8; 40 << 10];
    let put =
        BlobsWrite { handle: packages, path: "stripe.js".to_owned(), bytes: package.clone(), ..BlobsWrite::default() };
    let at = BlobsAt { handle: packages, path: "stripe.js".to_owned(), ..BlobsAt::default() };

    for commit in [false, true] {
        let stream = begin(&mut client, database);
        call(&mut client, stream, "insert into notes (id, title) values ('n1', 'a')", Vec::new(), "exec");
        let written =
            BlobsWritten::decode(&nested(&mut client, stream, method::BLOBS_PUT, &put).body.unwrap()).unwrap();
        assert_eq!(written.info.unwrap().size, package.len() as u64);
        let seen = BlobsHead::decode(&nested(&mut client, stream, method::BLOBS_HEAD, &at).body.unwrap()).unwrap();
        assert!(seen.info.is_some(), "a head sees the transaction's own file");
        let refused = nested(&mut client, stream, method::BLOBS_GET, &at).failure.unwrap();
        assert!(refused.message.contains("does not run inside a transaction"), "{}", refused.message);
        assert!(!refused.message.contains("  "), "one sentence: {}", refused.message);
        assert_eq!(end(&mut client, stream, commit).flags, frame::END);

        let got = client.call::<BlobsGot>(method::BLOBS_GET, &at).unwrap();
        let rows = query(&mut client, database, "select count(*) from notes", "scalar").rows;
        assert_eq!((got.info.is_some(), rows), (commit, vec![vec![Cell::Int(i64::from(commit))]]), "commit {commit}");
        if commit {
            assert_eq!(got.bytes, package);
        }
    }
    assert_eq!(client.pipe.streams(), 0);
}

// contract: REC-13
#[cfg(feature = "records")]
#[test]
fn a_stream_opened_through_a_database_appends_with_its_transactions_and_batches() {
    use crate::wire::protocol::{
        RecordsAppend, RecordsAppended, RecordsNewEvent, RecordsOpen, RecordsPage, RecordsQuery,
    };

    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let database = open(&mut client);
    let mut stream_of = |database| {
        let open = RecordsOpen { stream: "audit".to_owned(), database, ..RecordsOpen::default() };
        client.call::<Handle>(method::RECORDS_OPEN, &open).unwrap().handle
    };
    let (audit, in_records) = (stream_of(Some(database)), stream_of(None));
    let append = |handle, name: &str, id: Option<&str>| RecordsAppend {
        handle,
        events: vec![RecordsNewEvent {
            name: name.to_owned(),
            id: id.map(str::to_owned),
            fields: Some(r#"{"note":"n1"}"#.to_owned()),
            ..RecordsNewEvent::default()
        }],
    };
    let names = |client: &mut Client| {
        let page: RecordsPage = client.call(method::RECORDS_LIST, &RecordsQuery::default()).unwrap();
        page.events.into_iter().map(|event| event.name).collect::<Vec<_>>()
    };

    for commit in [false, true] {
        let stream = begin(&mut client, database);
        call(&mut client, stream, "insert into notes (id, title) values ('n1', 'a')", Vec::new(), "exec");
        let kept = nested(&mut client, stream, method::RECORDS_APPEND, &append(audit, "note.added", Some("n1")));
        assert_eq!(RecordsAppended::decode(&kept.body.unwrap()).unwrap().held, 0);
        let refused = nested(&mut client, stream, method::RECORDS_APPEND, &append(in_records, "never", None));
        let refused = refused.failure.unwrap();
        assert!(refused.message.contains("kept in records.db, outside this transaction of sql app"), "{refused:?}");
        assert_eq!(end(&mut client, stream, commit).flags, frame::END);
        let expected: &[&str] = if commit { &["note.added"] } else { &[] };
        assert_eq!(names(&mut client), expected, "commit {commit}: in the stream once the last DATA answered");
    }

    let insert = SqlText { text: "insert into notes (id, title) values ('n2', 'b')".to_owned(), ..SqlText::default() };
    let appending = |request: RecordsAppend| SqlText {
        method: Some(u64::from(method::RECORDS_APPEND)),
        body: Some(request.encode()),
        ..SqlText::default()
    };
    let batch = SqlBatch {
        handle: database,
        statements: vec![
            insert,
            appending(append(audit, "note.added", Some("n2"))),
            appending(append(audit, "x", Some("n1"))),
        ],
    };
    let batched: SqlBatched = client.call(method::SQL_BATCH, &batch).unwrap();
    assert_eq!(batched.done.iter().map(|done| done.changes).collect::<Vec<_>>(), [1, 1, 0], "the stream holds n1");
    assert_eq!(names(&mut client), ["note.added", "note.added"], "in the stream once the batch answered");
    let elsewhere = SqlBatch { handle: database, statements: vec![appending(append(in_records, "never", None))] };
    let refused = client.call::<SqlBatched>(method::SQL_BATCH, &elsewhere).unwrap_err();
    assert_eq!(refused.code, "invalid", "{refused:?}");
    assert_eq!(client.pipe.streams(), 0);
}
