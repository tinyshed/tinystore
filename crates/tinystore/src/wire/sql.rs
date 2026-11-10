//! sql over the protocol: databases by handle; a query's rows in parts, within
//! the client's credit; writes and batches through the shared commit's
//! completion, so an event loop fills commits; and a transaction whose calls
//! come as DATA on one stream, rolled back by the server's own timer when its
//! client stalls or leaves.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::codec::{Cell, Map, Message};
use super::protocol::{
    Empty, Failure, Handle, SqlBatch, SqlBatched, SqlColumn, SqlColumns, SqlDone, SqlOpen, SqlQuery, SqlRows,
    SqlStatement, SqlText, SqlTxAnswer, SqlTxCall, SqlTxOpen, method,
};
use super::{Route, codec, msgpack};
use crate::engine::SharedFile;
use crate::limits::{ANSWER_BYTES, Limit};
use crate::sql::{
    Answer, Column, Columns, Database, Done, Held, Keep, Kept, Migrations, Rows, Sql, TX_HELD, Tx, Value, Wanted, Write,
};
use crate::sync::lock;
use crate::{Error, ErrorKind, Store, Transaction};

use super::Answered;

/// How long a transaction holds the writer at most, its client's pauses
/// included: a database's own bound, past which the server rolls it back
/// without the client. The tests wait out a shorter one.
const TX_BOUND: Duration = if cfg!(test) { Duration::from_millis(300) } else { crate::sql::TX_BOUND };

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::SQL_EXEC | method::SQL_BATCH => Route::Submit,
        method::SQL_QUERY | method::SQL_COLUMNS => Route::Download,
        method::SQL_TX => Route::Transaction,
        _ => Route::Worker,
    }
}

/// What a stream that sends DATA needs of its session: a DATA, the last, and
/// a grant of credit for the DATA its client sent.
#[derive(Clone)]
pub(crate) struct Link {
    pub(crate) send: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
    pub(crate) finish: Arc<dyn Fn(Answered) + Send + Sync>,
    /// Gives the client back the credit of DATA bytes the stream let go of:
    /// a transaction's call, once it is answered.
    pub(crate) grant: Arc<dyn Fn(u32) + Send + Sync>,
    /// The DATA bytes the client takes before it grants more.
    pub(crate) credit: u64,
    pub(crate) max_body: usize,
}

/// What a connection opened, by handle, and its streams that send DATA.
#[derive(Default)]
pub(crate) struct Handles {
    open: super::ByHandle<Database>,
    downloads: Mutex<HashMap<u32, Arc<Downloading>>>,
    txs: Mutex<HashMap<u32, Arc<Txing>>>,
}

impl Handles {
    fn database(&self, handle: u64) -> Result<Database, Failure> {
        self.open.get(handle, || format!("handle {handle} names no database this connection opened"))
    }

    /// The file of the database `handle` names, for a bucket or a queue
    /// opened in it.
    pub(crate) fn shared(&self, handle: u64) -> Result<Arc<SharedFile>, Failure> {
        self.database(handle).map(|database| Arc::clone(database.shared()))
    }

    /// Credit the client granted a stream: a download's or a transaction's.
    pub(crate) fn grant(&self, stream: u32, credit: u32) {
        let download = lock(&self.downloads).get(&stream).cloned();
        if let Some(download) = download {
            return download.grant(credit, &self.downloads);
        }
        let tx = lock(&self.txs).get(&stream).cloned();
        if let Some(tx) = tx {
            tx.grant(credit);
        }
    }

    pub(crate) fn tx(&self, stream: u32) -> Option<Arc<Txing>> {
        lock(&self.txs).get(&stream).cloned()
    }

    /// Ends a download or a transaction the client cancelled; says whether
    /// the stream was one of them.
    pub(crate) fn cancel(&self, stream: u32) -> bool {
        if let Some(download) = lock(&self.downloads).remove(&stream) {
            download.end(Err(Failure::cancelled("the client cancelled its query")));
            return true;
        }
        if let Some(tx) = lock(&self.txs).remove(&stream) {
            // Its thread finds the calls gone, rolls back and ends the stream.
            tx.leave();
            return true;
        }
        false
    }

    /// Lets go of every stream: the connection ended. A transaction rolls
    /// back, and nothing more is sent.
    pub(crate) fn end(&self) {
        lock(&self.downloads).clear();
        for (_, tx) in lock(&self.txs).drain() {
            tx.leave();
        }
    }
}

/// Answers a call that runs to its end on this thread; a read connection's
/// is `read_only`.
pub(crate) fn call(store: &Store, handles: &Handles, called: u16, body: &[u8], read_only: bool) -> Answered {
    match called {
        method::SQL_OPEN => open(store, handles, SqlOpen::decode(body)?, read_only),
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

/// Opens a database by name, its migrations applied and checked; a read
/// connection's applies none, and opens a file the store holds as it is. It
/// chooses no durability either: the first open fixes a file's for every
/// writer.
fn open(store: &Store, handles: &Handles, asked: SqlOpen, read_only: bool) -> Answered {
    if asked.durability.is_some() && read_only {
        let refused = format!("sql {}: a read connection chooses no durability: a file's is its writers'", asked.name);
        return Err(Failure::permission(refused));
    }
    let mut builder = store.database(&asked.name);
    if read_only {
        let handle = handles.open.keep(builder.open_as_it_is()?);
        return Ok(Handle { handle }.encode());
    }
    if let Some(migrations) = asked.migrations {
        let files = migrations.into_iter().map(|migration| (migration.name, migration.sql)).collect();
        builder = builder.migrations(Migrations::Files(files));
    }
    if let Some(word) = asked.durability {
        let durability = word.parse().map_err(|error: Error| error.within(format!("sql {}", asked.name)))?;
        builder = builder.durability(durability);
    }
    let database = builder.open()?;
    let handle = handles.open.keep(database);
    Ok(Handle { handle }.encode())
}

/// A call of another engine that a database's batch carries, made into a
/// write of the batch: its method, and its request.
pub(crate) type Lend<'a> = &'a dyn Fn(u16, &[u8]) -> Result<Write, Failure>;

/// Queues a write; `done` answers it on the thread that commits it, or at
/// once when the call is refused before it is queued. A batch's call of
/// another engine goes to `lend`.
pub(crate) fn submit(
    handles: &Handles,
    (called, body): (u16, &[u8]),
    lend: Lend<'_>,
    done: impl FnOnce(Answered) + Send + 'static,
) {
    match called {
        method::SQL_EXEC => {
            let prepared = SqlStatement::decode(body)
                .and_then(|asked| Ok((handles.database(asked.handle)?, statement(asked.text, asked.values)?)));
            match prepared {
                Ok((database, statement)) => database.exec_then(statement, move |written| {
                    done(written.map(|written| done_of(&written).encode()).map_err(Failure::from));
                }),
                Err(failure) => done(Err(failure)),
            }
        }
        method::SQL_BATCH => {
            let prepared = SqlBatch::decode(body).and_then(|asked| {
                let database = handles.database(asked.handle)?;
                let writes = asked.statements.into_iter().map(|text| write_of(text, lend));
                Ok((database, writes.collect::<Result<Vec<_>, _>>()?))
            });
            match prepared {
                Ok((database, writes)) => database.batch_then(writes, move |written| {
                    let batched = written.map(|done| SqlBatched { done: done.iter().map(done_of).collect() }.encode());
                    done(batched.map_err(Failure::from));
                }),
                Err(failure) => done(Err(failure)),
            }
        }
        other => done(Err(Failure::unimplemented(format!("method {other:#06x}")))),
    }
}

/// Runs a query, `sql.query` or `sql.columns`, and says how its rows go: in
/// the RESPONSE when they fit one part, or a part a DATA within the client's
/// credit, the last ending the stream, through [`download`] once the RESPONSE
/// went out.
///
/// It does not wait for a commit: a read is given to `done` before this
/// returns, and a statement that writes once its commit ends, on the thread
/// that commits it, so that writes in flight hold no thread of the session.
pub(crate) fn query(
    handles: &Handles,
    (called, body): (u16, &[u8]),
    link: &Link,
    read_only: bool,
    done: impl FnOnce(Result<Queried, Failure>) + Send + 'static,
) {
    if called == method::SQL_COLUMNS {
        let asked = SqlStatement::decode(body)
            .and_then(|asked| Ok((handles.database(asked.handle)?, statement(asked.text, asked.values)?, Wanted::All)));
        return read_into::<Columns>(asked, link, read_only, done);
    }
    let asked = SqlQuery::decode(body).and_then(|asked| {
        let wanted = Wanted::named(&asked.want)
            .ok_or_else(|| Failure::invalid(format!("want {:?}: a query wants all, one or scalar", asked.want)))?;
        Ok((handles.database(asked.handle)?, statement(asked.text, asked.values)?, wanted))
    });
    read_into::<Kept<RowBytes>>(asked, link, read_only, done);
}

/// Reads what a query asked into an `A`, its rows or their columns, and
/// gives `done` how they go.
fn read_into<A: Parted>(
    asked: Result<(Database, Sql, Wanted), Failure>,
    link: &Link,
    read_only: bool,
    done: impl FnOnce(Result<Queried, Failure>) + Send + 'static,
) {
    let (database, statement, wanted) = match asked {
        Ok(asked) => asked,
        Err(failure) => return done(Err(failure)),
    };
    let bound = part_bound(link);
    if read_only {
        let rows = database
            .read::<A>(&statement, wanted)
            .map_err(Failure::from)
            .and_then(|rows| rows.ok_or_else(|| Failure::permission("a read connection's SQL writes nothing")));
        return done(rows.and_then(|rows| queried(rows, bound)));
    }
    database.rows_then(statement, wanted, move |rows: crate::Result<A>| {
        done(rows.map_err(Failure::from).and_then(|rows| queried(rows, bound)));
    });
}

/// A query's answer that goes in parts: its rows, or their columns.
trait Parted: Answer + Send + 'static {
    /// The store's memory it holds, for the download that sends it.
    fn held(&mut self) -> Held;

    /// Its parts, each of at most `bound` bytes; a row larger than a part
    /// alone is `limit`.
    fn parts(self, bound: usize) -> Result<VecDeque<Vec<u8>>, Failure>;
}

/// A query's rows as they are read: each already the bytes it takes of its
/// message, an array of its cells. No row is kept as values first, which
/// took four times a number's bytes and a second pass to write them.
#[derive(Default)]
struct RowBytes {
    /// Every row's cells, a row after another.
    rows: Vec<u8>,
    /// Where each row ends in `rows`.
    ends: Vec<usize>,
}

impl Keep for RowBytes {
    fn value(&mut self, column: usize, width: usize, value: Value) {
        if column == 0 {
            msgpack::write_array_head(&mut self.rows, width);
        }
        codec::write_cell(&mut self.rows, &cell_of(value));
        if column + 1 == width {
            self.ends.push(self.rows.len());
        }
    }
}

impl RowBytes {
    /// Where the row `row` begins in `rows`, and the last ends for the one
    /// after it.
    fn begins(&self, row: usize) -> usize {
        row.checked_sub(1).map_or(0, |before| self.ends[before])
    }
}

impl Parted for Kept<RowBytes> {
    fn held(&mut self) -> Held {
        Kept::held(self)
    }

    /// Its rows in parts of at most `bound` bytes, the first naming the
    /// columns, each a `sql.Rows` as [`row_parts`] writes one: the same
    /// fields around rows that are bytes already.
    fn parts(self, bound: usize) -> Result<VecDeque<Vec<u8>>, Failure> {
        let part = |rows: Range<usize>, first: bool| {
            let cells = &self.kept.rows[self.kept.begins(rows.start)..self.kept.begins(rows.end)];
            let mut out = Vec::with_capacity(cells.len() + 64);
            let mut map = Map::open(&mut out);
            if first && !self.columns.is_empty() {
                map.field(&mut out, 1);
                codec::write_list(&mut out, &self.columns, codec::write_str);
            }
            if !rows.is_empty() {
                map.field(&mut out, 2);
                msgpack::write_array_head(&mut out, rows.len());
                out.extend_from_slice(cells);
            }
            map.close(&mut out);
            out
        };
        let names = self.columns.iter().map(|name| name.len() + 5).sum::<usize>() + 16;
        let weight = |row: usize| self.kept.begins(row + 1) - self.kept.begins(row);
        let cuts = cuts(self.kept.ends.len(), weight, (names, 16), bound)?;
        Ok(cuts.into_iter().enumerate().map(|(index, rows)| part(rows, index == 0)).collect())
    }
}

impl Parted for Columns {
    fn held(&mut self) -> Held {
        Columns::held(self)
    }

    fn parts(self, bound: usize) -> Result<VecDeque<Vec<u8>>, Failure> {
        column_parts(self, bound)
    }
}

/// How a query's rows go: whole, or in parts with the memory they hold.
fn queried(mut rows: impl Parted, bound: usize) -> Result<Queried, Failure> {
    let held = rows.held();
    let mut parts = rows.parts(bound)?;
    if parts.len() == 1 {
        return Ok(Queried::Whole(parts.pop_front().unwrap_or_default()));
    }
    Ok(Queried::Parts(parts, held))
}

/// Sends a query's parts within the client's credit, its RESPONSE gone out;
/// a cancel that came before finds nothing, and the parts still go. The
/// store's memory the rows held is given back as their parts go.
pub(crate) fn download(handles: &Handles, stream: u32, (parts, held): (VecDeque<Vec<u8>>, Held), link: &Link) {
    let state = DownloadState { parts, held, credit: link.credit, ended: false };
    let download = Arc::new(Downloading { stream, state: Mutex::new(state), link: link.clone() });
    lock(&handles.downloads).insert(stream, Arc::clone(&download));
    download.flush(&handles.downloads);
}

/// A query's answer: its rows whole, or their parts and the store's memory
/// they hold.
pub(crate) enum Queried {
    Whole(Vec<u8>),
    Parts(VecDeque<Vec<u8>>, Held),
}

fn part_bound(link: &Link) -> usize {
    super::part_bound(link.max_body, link.credit, 0)
}

/// A query's rows in parts of at most `bound` bytes, the first naming the
/// columns; a row larger than a part alone is `limit`.
fn row_parts(rows: Rows, bound: usize) -> Result<VecDeque<Vec<u8>>, Failure> {
    let columns: Vec<String> = rows.columns().to_vec();
    let width = rows.width().max(1);
    let mut parts = VecDeque::new();
    let mut part = SqlRows { columns, rows: Vec::new() };
    let mut size = part.columns.iter().map(|name| name.len() + 5).sum::<usize>() + 16;
    let mut values = rows.into_values();
    for row in values.chunks_mut(width) {
        let weight = row.iter().map(weight_of).sum::<usize>() + 5;
        if weight + 16 > bound {
            return Err(past_a_message(weight, bound));
        }
        if size + weight > bound && !part.rows.is_empty() {
            parts.push_back(std::mem::take(&mut part).encode());
            size = 16;
        }
        size += weight;
        part.rows.push(row.iter_mut().map(|value| cell_of(std::mem::replace(value, Value::Null))).collect());
    }
    parts.push_back(part.encode());
    Ok(parts)
}

fn past_a_message(weight: usize, bound: usize) -> Failure {
    let refused = format!("a row of {weight} bytes, past the {bound} one message of this connection carries");
    Failure::from(Limit { name: "row bytes", bound: bound as u64 }.refuse(weight as u64, refused))
}

/// What a row of a packed column takes of a message: its eight bytes, and
/// its bit of nulls.
const PACKED: usize = 9;

/// A query's rows by their columns, in parts of at most `bound` bytes: every
/// part holds every column for the same rows, and a row larger than a part
/// alone is `limit`.
fn column_parts(mut columns: Columns, bound: usize) -> Result<VecDeque<Vec<u8>>, Failure> {
    let head = columns.columns.iter().map(|(name, _)| name.len() + 24).sum::<usize>() + 16;
    let row_weight = |row: usize| {
        let of = |(_, column): &(String, Column)| match column {
            Column::Values(values) => weight_of(&values[row]),
            Column::Integers { .. } | Column::Reals { .. } => PACKED,
        };
        columns.columns.iter().map(of).sum::<usize>()
    };
    let cuts = cuts(columns.rows, row_weight, (head, head), bound)?;
    Ok(cuts.into_iter().map(|cut| column_part(&mut columns, cut).encode()).collect())
}

/// The rows of each part of an answer of `rows` rows: as many as `bound`
/// bytes take beside the part's head, the first part's and any later one's,
/// each row weighing what `weight` says. A row no part takes is `limit`.
fn cuts(
    rows: usize,
    weight: impl Fn(usize) -> usize,
    (first_head, later_head): (usize, usize),
    bound: usize,
) -> Result<Vec<Range<usize>>, Failure> {
    let (mut cuts, mut from, mut size) = (Vec::new(), 0, first_head);
    for row in 0..rows {
        let weight = weight(row);
        if later_head + weight > bound {
            return Err(past_a_message(weight, bound));
        }
        if size + weight > bound && row > from {
            cuts.push(from..row);
            (from, size) = (row, later_head);
        }
        size += weight;
    }
    cuts.push(from..rows);
    Ok(cuts)
}

/// What a value takes of a message as a cell, its header with it.
fn weight_of(value: &Value) -> usize {
    match value {
        Value::Null => 1,
        Value::Integer(_) | Value::Real(_) => 9,
        Value::Text(text) => text.len() + 5,
        Value::Blob(bytes) => bytes.len() + 5,
    }
}

/// The rows `cut` of every column, as one part. It takes the values of a
/// column that is not packed, which go once.
fn column_part(columns: &mut Columns, cut: Range<usize>) -> SqlColumns {
    let part = |(name, column): &mut (String, Column)| {
        let mut part = SqlColumn { name: name.clone(), integers: None, reals: None, nulls: None, values: None };
        match column {
            Column::Integers { values, nulls } => {
                part.integers = Some(packed(&values[cut.clone()], |integer| integer.to_le_bytes()));
                part.nulls = null_bits(nulls, &cut);
            }
            Column::Reals { values, nulls } => {
                part.reals = Some(packed(&values[cut.clone()], |real| real.to_le_bytes()));
                part.nulls = null_bits(nulls, &cut);
            }
            Column::Values(values) => {
                let cells = values[cut.clone()].iter_mut().map(|value| cell_of(std::mem::replace(value, Value::Null)));
                part.values = Some(cells.collect());
            }
        }
        part
    };
    let (rows, total) = (cut.len() as u64, columns.rows as u64);
    SqlColumns { rows, total, columns: columns.columns.iter_mut().map(part).collect() }
}

/// A packed column's rows, eight bytes each, little end first.
fn packed<T: Copy>(values: &[T], bytes: impl Fn(T) -> [u8; 8]) -> Vec<u8> {
    let mut packed = Vec::with_capacity(values.len() * 8);
    for value in values {
        packed.extend_from_slice(&bytes(*value));
    }
    packed
}

/// A bit a row of `cut`, a byte's lowest first, set where the row holds
/// NULL; nothing when none of them does.
fn null_bits(nulls: &[bool], cut: &Range<usize>) -> Option<Vec<u8>> {
    let nulls = nulls.get(cut.clone()).filter(|nulls| nulls.contains(&true))?;
    let mut bits = vec![0u8; nulls.len().div_ceil(8)];
    for (row, _) in nulls.iter().enumerate().filter(|(_, null)| **null) {
        bits[row / 8] |= 1 << (row % 8);
    }
    Some(bits)
}

/// A query's parts going out within the client's credit.
pub(crate) struct Downloading {
    stream: u32,
    state: Mutex<DownloadState>,
    link: Link,
}

struct DownloadState {
    parts: VecDeque<Vec<u8>>,
    held: Held,
    credit: u64,
    ended: bool,
}

impl Downloading {
    fn grant(&self, credit: u32, downloads: &Mutex<HashMap<u32, Arc<Downloading>>>) {
        lock(&self.state).credit += u64::from(credit);
        self.flush(downloads);
    }

    /// Sends the parts the credit takes; the last ends the stream.
    fn flush(&self, downloads: &Mutex<HashMap<u32, Arc<Downloading>>>) {
        let mut state = lock(&self.state);
        while !state.ended {
            let Some(next) = state.parts.front() else {
                return;
            };
            if next.len() as u64 > state.credit {
                return;
            }
            let part = state.parts.pop_front().unwrap_or_default();
            state.credit -= part.len() as u64;
            state.held.give_back(part.len() as u64);
            if state.parts.is_empty() {
                state.ended = true;
                drop(state);
                lock(downloads).remove(&self.stream);
                return (self.link.finish)(Ok(part));
            }
            (self.link.send)(part);
        }
    }

    fn end(&self, last: Answered) {
        if std::mem::replace(&mut lock(&self.state).ended, true) {
            return;
        }
        (self.link.finish)(last);
    }
}

/// A client's transaction: the calls its stream brings, to the thread that
/// holds the writer, and the credit its answers go within.
pub(crate) struct Txing {
    calls: Mutex<Option<Sender<TxItem>>>,
    credit: Mutex<u64>,
    granted: Condvar,
}

/// Opens a transaction a `sql.tx` asks for; it runs once the RESPONSE went
/// out, through [`Txing::run`], in its turn on its database's line.
pub(crate) fn transaction(
    handles: &Handles,
    stream: u32,
    body: &[u8],
    link: &Link,
) -> Result<(Arc<Txing>, Running), Failure> {
    let asked = SqlTxOpen::decode(body)?;
    let database = handles.database(asked.handle)?;
    let (calls, received) = mpsc::channel();
    let txing =
        Arc::new(Txing { calls: Mutex::new(Some(calls)), credit: Mutex::new(link.credit), granted: Condvar::new() });
    lock(&handles.txs).insert(stream, Arc::clone(&txing));
    Ok((txing, Running { database, received, link: link.clone() }))
}

/// What a transaction's thread runs with.
pub(crate) struct Running {
    database: Database,
    received: Receiver<TxItem>,
    link: Link,
}

impl Running {
    /// The line the transactions of its database take the writer in, one
    /// at a time and in their order, across every connection to the store
    /// and the async API's transactions.
    pub(crate) fn line(&self) -> String {
        self.database.line()
    }
}

/// How a transaction ended other than by its commit.
enum Ended {
    RolledBack,
    Failed(Error),
    /// Its last DATA is not a call, and says neither commit nor roll back.
    Unread(Failure),
}

impl From<Error> for Ended {
    fn from(error: Error) -> Self {
        Ended::Failed(error)
    }
}

/// What a transaction's thread takes from the client: a call and the bytes
/// of its DATA, or END, which commits or rolls back; each, instead, may be
/// why its DATA is not a call.
pub(crate) enum TxItem {
    Call(Result<SqlTxCall, Failure>, usize),
    End(Result<bool, Failure>),
}

impl Txing {
    /// A DATA of the client's: a call, or, with END, the last, which commits
    /// or rolls back. One that is not a call is answered with why, naming
    /// its field, the transaction as it was; a last one rolls it back.
    ///
    /// Its credit goes back once the call is answered, not as it is queued:
    /// the calls that wait while the transaction waits for the writer are
    /// then within the stream's credit, past which a client is cut off.
    pub(crate) fn item(&self, body: &[u8], ended: bool) {
        let call = SqlTxCall::decode(body);
        // Only END ends the transaction, whatever a call's `want` holds.
        let item = if ended { TxItem::End(call.map(|call| call.commit)) } else { TxItem::Call(call, body.len()) };
        let calls = lock(&self.calls);
        if let Some(calls) = calls.as_ref() {
            let _ = calls.send(item);
        }
    }

    fn grant(&self, credit: u32) {
        *lock(&self.credit) += u64::from(credit);
        self.granted.notify_all();
    }

    /// Lets the transaction's thread find its calls gone: it rolls back.
    fn leave(&self) {
        lock(&self.calls).take();
    }

    /// Runs the transaction: each call as it comes, its answer as a DATA,
    /// until the last commits or rolls back, the client leaves, or its bound
    /// passes; then the stream's last DATA says how it ended. A call of kv or
    /// jobs goes to `nested`, which runs it in the transaction. One whose
    /// client left while it waited for its turn takes no writer.
    pub(crate) fn run(self: &Arc<Self>, running: Running, handles: &Handles, stream: u32, nested: Nested<'_>) {
        let Running { database, received, link } = running;
        let ended = if lock(&self.calls).is_none() {
            Err(Ended::Failed(left_its_transaction()))
        } else {
            database.tx(|tx| -> Result<(), Ended> { self.answer_calls(tx, &received, &link, nested) })
        };
        lock(&handles.txs).remove(&stream);
        let last = match ended {
            Ok(()) | Err(Ended::RolledBack) => Ok(Empty {}.encode()),
            Err(Ended::Failed(error)) => Err(Failure::from(error)),
            Err(Ended::Unread(failure)) => {
                Err(Failure { message: format!("{}, and the transaction rolled back", failure.message), ..failure })
            }
        };
        (link.finish)(last);
    }

    /// Answers the client's calls as they come, from when the transaction
    /// holds the writer, until its last or its bound.
    fn answer_calls(
        &self,
        tx: &Tx<'_>,
        received: &Receiver<TxItem>,
        link: &Link,
        nested: Nested<'_>,
    ) -> Result<(), Ended> {
        let deadline = Instant::now() + TX_BOUND;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let (call, read) = match received.recv_timeout(left) {
                Ok(TxItem::Call(call, read)) => (call, read),
                Ok(TxItem::End(Ok(commit))) => return if commit { Ok(()) } else { Err(Ended::RolledBack) },
                Ok(TxItem::End(Err(failure))) => return Err(Ended::Unread(failure)),
                Err(RecvTimeoutError::Timeout) => return Err(Ended::Failed(past_bound())),
                Err(RecvTimeoutError::Disconnected) => return Err(Ended::Failed(left_its_transaction())),
            };
            let answer = answer(tx, call, link.max_body, nested);
            // before the answer: by the time a client reads it, its call's credit is counted back
            if let Ok(read) = u32::try_from(read) {
                (link.grant)(read);
            }
            self.send(link, answer.encode(), deadline)?;
        }
    }

    /// Sends an answer once the client's credit takes it, waiting for a grant
    /// no longer than the transaction's bound.
    fn send(&self, link: &Link, body: Vec<u8>, deadline: Instant) -> Result<(), Ended> {
        let mut credit = lock(&self.credit);
        while *credit < body.len() as u64 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Ended::Failed(past_bound()));
            }
            credit = self.granted.wait_timeout(credit, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        *credit -= body.len() as u64;
        drop(credit);
        (link.send)(body);
        Ok(())
    }
}

fn past_bound() -> Error {
    let held = Limit { bound: TX_BOUND.as_millis() as u64, ..TX_HELD };
    held.run_out(format!("a transaction held the writer past its {TX_BOUND:?}, and rolled back"))
}

fn left_its_transaction() -> Error {
    Error::new(ErrorKind::Cancelled, "the client left its transaction, which rolled back")
}

/// A call of kv or jobs inside a transaction: its method, its request, and
/// the largest answer the connection agreed.
pub(crate) type Nested<'a> = &'a dyn Fn(&Transaction<'_>, u16, &[u8], usize) -> Answered;

/// A call's answer: its rows, what it changed, a method's answer, or why it
/// failed, which left the transaction as it was before the call. A DATA that
/// is not a call ran nothing, and its failure names the field that did not
/// read.
fn answer(tx: &Tx<'_>, call: Result<SqlTxCall, Failure>, max_body: usize, nested: Nested<'_>) -> SqlTxAnswer {
    let ran = || -> Result<SqlTxAnswer, Failure> {
        let call = call?;
        if call.want == "call" {
            let method = call.method.and_then(|method| u16::try_from(method).ok());
            let method = method.ok_or_else(|| Failure::invalid("a call inside a transaction names its method"))?;
            let body = nested(tx.transaction(), method, call.body.as_deref().unwrap_or_default(), max_body)?;
            return Ok(SqlTxAnswer { body: Some(body), ..SqlTxAnswer::default() });
        }
        let statement = statement(call.text, call.values)?;
        if call.want == "exec" {
            return Ok(SqlTxAnswer { done: Some(done_of(&tx.exec(statement)?)), ..SqlTxAnswer::default() });
        }
        let wanted = Wanted::named(&call.want).ok_or_else(|| {
            Failure::invalid(format!("want {:?}: a call wants all, one, scalar, exec or call", call.want))
        })?;
        let rows = tx.rows_of(&statement, wanted)?;
        let mut parts = row_parts(rows, max_body / 2)?;
        if parts.len() > 1 {
            let wanted = parts.iter().map(Vec::len).sum::<usize>() as u64;
            let refused = "rows past what one answer carries: read them outside the transaction, or fewer at a time";
            return Err(Failure::from(Limit { bound: max_body as u64 / 2, ..ANSWER_BYTES }.refuse(wanted, refused)));
        }
        let rows = SqlRows::decode(&parts.pop_front().unwrap_or_default())?;
        Ok(SqlTxAnswer { rows: Some(rows), ..SqlTxAnswer::default() })
    };
    ran().unwrap_or_else(|failure| SqlTxAnswer { failure: Some(failure), ..SqlTxAnswer::default() })
}

/// A batch's write: a statement, or a call of another engine, which has no
/// text and goes to `lend`.
fn write_of(text: SqlText, lend: Lend<'_>) -> Result<Write, Failure> {
    let Some(method) = text.method else {
        return Ok(statement(text.text, text.values)?.into());
    };
    if !text.text.is_empty() || !text.values.is_empty() {
        return Err(Failure::invalid("a batch's call of a queue has no text: its request is its body"));
    }
    let method = u16::try_from(method).map_err(|_| Failure::invalid(format!("method {method:#06x}")))?;
    lend(method, text.body.as_deref().unwrap_or_default())
}

/// A statement from its text and the cells of its `?`s.
fn statement(text: String, values: Vec<Cell>) -> Result<Sql, Failure> {
    let values = values.into_iter().map(value_of).collect::<Result<Vec<_>, _>>()?;
    Ok(Sql::with_values(text, values))
}

fn value_of(cell: Cell) -> Result<Value, Failure> {
    Ok(match cell {
        Cell::Nil => Value::Null,
        Cell::Int(n) => Value::Integer(n),
        Cell::Float(x) if x.is_nan() => return Err(Failure::invalid("a NaN, which SQLite keeps as NULL")),
        Cell::Float(x) => Value::Real(x),
        Cell::Str(text) => Value::Text(text),
        Cell::Bin(bytes) => Value::Blob(bytes),
    })
}

pub(super) fn cell_of(value: Value) -> Cell {
    match value {
        Value::Null => Cell::Nil,
        Value::Integer(n) => Cell::Int(n),
        Value::Real(x) => Cell::Float(x),
        Value::Text(text) => Cell::Str(text),
        Value::Blob(bytes) => Cell::Bin(bytes),
    }
}

fn done_of(done: &Done) -> SqlDone {
    SqlDone { changes: done.changes, last_insert_rowid: done.last_insert_rowid }
}

#[cfg(test)]
#[path = "sql_tests.rs"]
mod tests;
