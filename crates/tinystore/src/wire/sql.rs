//! sql over the protocol: databases by handle; a query's rows in parts, within
//! the client's credit; writes and batches through the shared commit's
//! completion, so an event loop fills commits; and a transaction whose calls
//! come as DATA on one stream, rolled back by the server's own timer when its
//! client stalls or leaves.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use super::codec::{Cell, Map, Message};
use super::protocol::{
    Empty, Failure, Handle, SqlBatch, SqlBatched, SqlBegin, SqlColumn, SqlColumns, SqlDescribed, SqlDescribedColumn,
    SqlDiff, SqlDone, SqlMirror, SqlOpen, SqlOrder, SqlQuery, SqlRows, SqlRun, SqlStatement, SqlText, SqlTxAnswer,
    SqlTxCall, SqlTxOpen, method,
};
use super::{Route, codec, msgpack};
use crate::engine::SharedFile;
use crate::limits::{ANSWER_BYTES, Limit};
use crate::sql::{
    Answer, Columns, Database, Direction, Done, End, First, Found, Given, Held, Keep, Kept, Key, Listed, Migrations,
    Mode, Packed, Part, Rows, Shape, Sql, TX_HELD, Taken, Tx, Unit, Value, Walk, Walking, Wanted, Write, is_query,
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
        method::SQL_EXEC | method::SQL_BATCH | method::SQL_MIRROR | method::SQL_ADD => Route::Submit,
        method::SQL_QUERY | method::SQL_COLUMNS | method::SQL_COMMIT => Route::Download,
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
    downloads: Arc<Downloads>,
    txs: Mutex<HashMap<u32, Arc<Txing>>>,
    runs: Mutex<run::Runs>,
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
        lock(&self.runs).clear();
    }
}

/// Answers a call that runs to its end on this thread; a read connection's
/// is `read_only`.
pub(crate) fn call(store: &Store, handles: &Handles, called: u16, body: &[u8], read_only: bool) -> Answered {
    match called {
        method::SQL_OPEN => open(store, handles, SqlOpen::decode(body)?, read_only),
        method::SQL_DESCRIBE => describe(handles, SqlStatement::decode(body)?),
        method::SQL_BEGIN => run::begin(handles, SqlBegin::decode(body)?),
        method::SQL_DROP => run::drop(handles, SqlRun::decode(body)?),
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
    let mut builder = store.database(&asked.name).backup(asked.backup.unwrap_or(true));
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

/// A statement's columns, compiled on a reader and run not at all: a read,
/// which a read connection may ask too.
fn describe(handles: &Handles, asked: SqlStatement) -> Answered {
    let database = handles.database(asked.handle)?;
    let values = stamped(asked.values, asked.now.as_deref(), || database.now())?;
    let columns = database.describe(statement(asked.text, values)?)?;
    let columns = columns.into_iter().map(|column| SqlDescribedColumn { name: column.name, declared: column.declared });
    Ok(SqlDescribed { columns: columns.collect() }.encode())
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
            let prepared = SqlStatement::decode(body).and_then(|asked| {
                let database = handles.database(asked.handle)?;
                let values = stamped(asked.values, asked.now.as_deref(), || database.now())?;
                Ok((database, statement(asked.text, values)?))
            });
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
        method::SQL_MIRROR => match SqlMirror::decode(body).and_then(|asked| mirrored(handles, asked)) {
            Ok((database, asked)) => database.mirror_then(asked, ANSWER_BYTES, move |found| {
                done(found.map(|found| diff_of(found).encode()).map_err(Failure::from));
            }),
            Err(failure) => done(Err(failure)),
        },
        method::SQL_ADD => run::add(handles, body, done),
        other => done(Err(Failure::unimplemented(format!("method {other:#06x}")))),
    }
}

/// A mirror's call as the core takes it: its database, its table's shape,
/// its list as checked, and what it compares the list with.
fn mirrored(handles: &Handles, asked: SqlMirror) -> Result<(Database, (Shape, Given, Mode)), Failure> {
    let database = handles.database(asked.handle)?;
    let describe = format!("sql {}: mirror {}", database.name(), asked.table);
    let unit = unit_named(asked.gone_unit.as_deref())
        .map_err(|failure| Failure::invalid(format!("{describe}: gone {}", failure.message)))?;
    let shape = Shape { table: asked.table, key: asked.key, gone: asked.gone, unit, change_on: asked.change_on };
    let mode = match asked.mode.as_str() {
        "replace" => replace_mode(&describe, (asked.scope, asked.scope_values), asked.min_kept)?,
        "upsert" if asked.scope.is_none() && asked.min_kept.is_none() => Mode::Upsert,
        "upsert" => return Err(Failure::invalid(format!("{describe}: an upsert takes no scope and no minKept"))),
        other => {
            return Err(Failure::invalid(format!("{describe}: a mirror's mode {other:?}: it is replace or upsert")));
        }
    };
    let rows = asked.rows.rows.into_iter().map(|row| row.into_iter().map(value_of).collect::<Result<Vec<_>, _>>());
    let listed = Listed { columns: asked.rows.columns, rows: rows.collect::<Result<_, _>>()? };
    let given = Given::checked(&shape, listed, &mode).map_err(|error| error.within(describe))?;
    Ok((database, (shape, given, mode)))
}

/// A replace of the scope its columns and values name, with its guard.
fn replace_mode(
    describe: &str,
    (columns, values): (Option<Vec<String>>, Option<Vec<Cell>>),
    min_kept: Option<f64>,
) -> Result<Mode, Failure> {
    let (columns, values) = (columns.unwrap_or_default(), values.unwrap_or_default());
    if columns.len() != values.len() {
        let why = format!("{describe}: a scope of {} columns and {} values", columns.len(), values.len());
        return Err(Failure::invalid(why));
    }
    let values = values.into_iter().map(value_of).collect::<Result<Vec<_>, _>>()?;
    Ok(Mode::Replace { scope: columns.into_iter().zip(values).collect(), min_kept })
}

/// A mirror's answer as cells.
fn diff_of(found: Found) -> SqlDiff {
    let cells = |rows: Vec<Vec<Value>>| -> Vec<Vec<Cell>> {
        rows.into_iter().map(|row| row.into_iter().map(cell_of).collect()).collect()
    };
    let (before, after): (Vec<_>, Vec<_>) = found.changed.into_iter().unzip();
    SqlDiff {
        columns: found.columns,
        added: cells(found.added),
        before: cells(before),
        after: cells(after),
        gone: cells(found.gone),
        same: found.same,
        suspect: found.suspect,
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
    if called == method::SQL_COMMIT {
        return run::commit(handles, body, link, done);
    }
    if called == method::SQL_COLUMNS {
        let asked = SqlStatement::decode(body).and_then(|asked| {
            let database = handles.database(asked.handle)?;
            let values = stamped(asked.values, asked.now.as_deref(), || database.now())?;
            Ok((database, statement(asked.text, values)?, Wanted::All))
        });
        return read_into::<Columns>(asked, link, read_only, done);
    }
    let asked = SqlQuery::decode(body);
    if let Ok(each) = asked.as_ref().map(|asked| asked.want == "each")
        && each
    {
        let walked = asked.and_then(|asked| {
            let key = asked.order.map(|order| order.into_iter().map(key_of).collect());
            Ok((handles.database(asked.handle)?, statement(asked.text, asked.values)?, key))
        });
        return walk(walked, link, read_only, done);
    }
    let asked = asked.and_then(|asked| {
        let wanted = Wanted::named(&asked.want).ok_or_else(|| {
            Failure::invalid(format!("want {:?}: a query wants all, one, scalar or each", asked.want))
        })?;
        Ok((handles.database(asked.handle)?, statement(asked.text, asked.values)?, wanted))
    });
    read_into::<Kept<RowBytes>>(asked, link, read_only, done);
}

/// Walks a query's rows, `want: each`, on this thread: its parts are sent as
/// they are read, within the client's credit, rather than once every row is.
/// Its first answer goes as [`query`] sends one, whole when one part holds
/// every row; a statement that writes is read as `all` reads it.
fn walk(
    asked: Result<(Database, Sql, Option<Vec<Key>>), Failure>,
    link: &Link,
    read_only: bool,
    done: impl FnOnce(Result<Queried, Failure>) + Send + 'static,
) {
    let (database, statement, key) = match asked {
        Ok(asked) => asked,
        Err(failure) => return done(Err(failure)),
    };
    if database.known_to_write(&statement) || (read_only && !is_query(statement.text())) {
        return read_into::<Kept<RowBytes>>(Ok((database, statement, Wanted::All)), link, read_only, done);
    }
    let walking = Walking { database: database.clone(), statement: statement.clone(), key, bound: part_bound(link) };
    let (walk, reader) = match Walk::<RowPart>::new(walking) {
        Ok(walk) => walk,
        Err(error) => return done(Err(Failure::from(database.failed_by(&statement)(error)))),
    };
    let waiting = Arc::new(Mutex::new(Some(walk)));
    let first = FirstAnswer {
        walk: Arc::downgrade(&waiting),
        done: Mutex::new(Some(Box::new(done))),
        asked: (database, statement),
        link: link.clone(),
        read_only,
    };
    if let Some(walk) = lock(&waiting).as_ref() {
        walk.on_change(Arc::new(move || first.answer()));
    }
    // held here while the reader runs, so that the first answer finds the walk
    let _waiting = waiting;
    reader.run();
}

/// What a walk over the wire answers its first part with.
type Answering = Box<dyn FnOnce(Result<Queried, Failure>) + Send>;

/// A walk's first answer, which waits for the walk to tell it: its rows
/// whole, how it failed, or that its parts follow the RESPONSE. The walk
/// holds it, and it holds the walk weakly until it gives it away.
struct FirstAnswer {
    walk: Weak<Mutex<Option<Walk<RowPart>>>>,
    done: Mutex<Option<Answering>>,
    asked: (Database, Sql),
    link: Link,
    read_only: bool,
}

impl FirstAnswer {
    fn answer(&self) {
        let Some(waiting) = self.walk.upgrade() else { return };
        let mut waiting = lock(&waiting);
        let Some(found) = waiting.as_ref().and_then(Walk::first) else { return };
        let (Some(done), Some(walk)) = (lock(&self.done).take(), waiting.take()) else { return };
        drop(waiting);
        match found {
            First::Whole(part) => done(Ok(Queried::Whole(part.map(|part| part.message).unwrap_or_default()))),
            First::End(End::Failed(error)) => done(Err(Failure::from(error))),
            First::End(End::Writes) => {
                let (database, statement) = &self.asked;
                database.remember_write(statement);
                let asked = Ok((database.clone(), statement.clone(), Wanted::All));
                read_into::<Kept<RowBytes>>(asked, &self.link, self.read_only, done);
            }
            First::End(End::Done) | First::Parts => done(Ok(Queried::Walk(walk))),
        }
    }
}

fn key_of(order: SqlOrder) -> Key {
    Key { column: order.column, direction: if order.descending { Direction::Desc } else { Direction::Asc } }
}

/// A walk's part as the client reads it, a `sql.Rows`: its cells written as
/// its rows are read, its message once its last is in. The walk's first
/// part names the columns.
pub(crate) struct RowPart {
    columns: Option<Arc<[String]>>,
    cells: Vec<u8>,
    rows: usize,
    /// What its message takes, its head among it, within `bound`.
    bytes: usize,
    bound: usize,
    message: Vec<u8>,
    held: Held,
}

impl Part for RowPart {
    fn begin(columns: &Arc<[String]>, first: bool, bound: usize, held: Held) -> RowPart {
        let head = if first { columns.iter().map(|name| name.len() + 5).sum::<usize>() + 16 } else { 16 };
        let columns = first.then(|| Arc::clone(columns));
        RowPart { columns, cells: Vec::new(), rows: 0, bytes: head, bound, message: Vec::new(), held }
    }

    fn weight(row: &[Value]) -> usize {
        row.iter().map(weight_of).sum::<usize>() + 5
    }

    fn push(&mut self, row: Vec<Value>, weight: usize) -> crate::Result<()> {
        if weight + 16 > self.bound {
            return Err(row_past_a_message(weight, self.bound));
        }
        msgpack::write_array_head(&mut self.cells, row.len());
        for value in row {
            codec::write_cell(&mut self.cells, &cell_of(value));
        }
        self.rows += 1;
        self.bytes += weight;
        self.held.reach(self.bytes as u64)
    }

    fn bytes(&self) -> usize {
        self.bytes
    }

    fn seal(&mut self) {
        let mut out = Vec::with_capacity(self.cells.len() + 64);
        let mut map = Map::open(&mut out);
        if let Some(columns) = self.columns.as_ref().filter(|columns| !columns.is_empty()) {
            map.field(&mut out, 1);
            codec::write_list(&mut out, columns, codec::write_str);
        }
        if self.rows > 0 {
            map.field(&mut out, 2);
            msgpack::write_array_head(&mut out, self.rows);
            out.extend_from_slice(&self.cells);
        }
        map.close(&mut out);
        self.message = out;
        self.cells = Vec::new();
        self.held.settle();
    }
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
    let download = Downloading::begun(handles, stream, Source::Read(parts, held), link);
    download.flush(&handles.downloads);
}

/// Sends a walk's parts as it reads them, within the client's credit, its
/// RESPONSE gone out: each part once the next or the walk's end is known, so
/// that the last ends the stream.
pub(crate) fn download_walk(handles: &Handles, stream: u32, walk: Walk<RowPart>, link: &Link) {
    let download = Downloading::begun(handles, stream, Source::Walk(walk), link);
    let (downloading, downloads) = (Arc::downgrade(&download), Arc::downgrade(&handles.downloads));
    let flush: Wake = Arc::new(move || {
        if let (Some(download), Some(downloads)) = (downloading.upgrade(), downloads.upgrade()) {
            download.flush(&downloads);
        }
    });
    let _ = download.wake.set(flush);
    download.flush(&handles.downloads);
}

/// A query's answer: its rows whole, their parts and the store's memory they
/// hold, or a walk of them that sends its parts as it reads them.
pub(crate) enum Queried {
    Whole(Vec<u8>),
    Parts(VecDeque<Vec<u8>>, Held),
    Walk(Walk<RowPart>),
}

fn part_bound(link: &Link) -> usize {
    super::part_bound(link.max_body, link.credit, 0)
}

/// Rows in parts of at most `bound` bytes as they are written, the first
/// naming the columns, for a transaction, which sends each inside an answer
/// of its own; a row larger than a part alone is `limit`.
fn row_parts(rows: Rows, bound: usize) -> Result<VecDeque<SqlRows>, Failure> {
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
            parts.push_back(std::mem::take(&mut part));
            size = 16;
        }
        size += weight;
        part.rows.push(row.iter_mut().map(|value| cell_of(std::mem::replace(value, Value::Null))).collect());
    }
    parts.push_back(part);
    Ok(parts)
}

fn past_a_message(weight: usize, bound: usize) -> Failure {
    Failure::from(row_past_a_message(weight, bound))
}

fn row_past_a_message(weight: usize, bound: usize) -> Error {
    let refused = format!("a row of {weight} bytes, past the {bound} one message of this connection carries");
    Limit { name: "row bytes", bound: bound as u64 }.refuse(weight as u64, refused)
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
        let of = |(_, column): &(String, Packed)| match column {
            Packed::Values(values) => weight_of(&values[row]),
            Packed::Integers { .. } | Packed::Reals { .. } => PACKED,
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
    let part = |(name, column): &mut (String, Packed)| {
        let mut part = SqlColumn { name: name.clone(), integers: None, reals: None, nulls: None, values: None };
        match column {
            Packed::Integers { values, nulls } => {
                part.integers = Some(packed(&values[cut.clone()], |integer| integer.to_le_bytes()));
                part.nulls = null_bits(nulls, &cut);
            }
            Packed::Reals { values, nulls } => {
                part.reals = Some(packed(&values[cut.clone()], |real| real.to_le_bytes()));
                part.nulls = null_bits(nulls, &cut);
            }
            Packed::Values(values) => {
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
    /// Held by the one thread that sends: one that finds it held leaves
    /// `again` set, which the sender sees before it lets go. A walk wakes its
    /// download from the thread that reads it, which may be sending already.
    sending: Mutex<()>,
    again: AtomicBool,
    /// What a walk calls once it has a part or its end.
    wake: OnceLock<Wake>,
}

type Downloads = Mutex<HashMap<u32, Arc<Downloading>>>;
type Wake = Arc<dyn Fn() + Send + Sync>;

struct DownloadState {
    source: Source,
    credit: u64,
    ended: bool,
}

/// Where a download's parts come from: rows read whole, and the store's
/// memory they hold, or a walk that reads them as they go.
enum Source {
    Read(VecDeque<Vec<u8>>, Held),
    Walk(Walk<RowPart>),
}

impl Downloading {
    fn begun(handles: &Handles, stream: u32, source: Source, link: &Link) -> Arc<Downloading> {
        let state = Mutex::new(DownloadState { source, credit: link.credit, ended: false });
        let (sending, again, wake) = (Mutex::new(()), AtomicBool::new(false), OnceLock::new());
        let download = Arc::new(Downloading { stream, state, link: link.clone(), sending, again, wake });
        lock(&handles.downloads).insert(stream, Arc::clone(&download));
        download
    }

    fn grant(&self, credit: u32, downloads: &Downloads) {
        lock(&self.state).credit += u64::from(credit);
        self.flush(downloads);
    }

    /// Sends the parts the credit takes, on one thread at a time.
    fn flush(&self, downloads: &Downloads) {
        self.again.store(true, Ordering::SeqCst);
        loop {
            let Ok(sending) = self.sending.try_lock() else {
                return;
            };
            while self.again.swap(false, Ordering::SeqCst) {
                self.send(downloads);
            }
            drop(sending);
            if !self.again.load(Ordering::SeqCst) {
                return;
            }
        }
    }

    /// Sends the parts the credit takes; the last ends the stream.
    fn send(&self, downloads: &Downloads) {
        let mut state = lock(&self.state);
        while !state.ended {
            let credit = state.credit;
            let (part, last) = match &mut state.source {
                Source::Read(parts, held) => {
                    let Some(next) = parts.front() else { return };
                    if next.len() as u64 > credit {
                        return;
                    }
                    let part = parts.pop_front().unwrap_or_default();
                    held.give_back(part.len() as u64);
                    let last = parts.is_empty();
                    (part, last)
                }
                Source::Walk(walk) => {
                    let wake = self.wake.get().cloned().unwrap_or_else(|| Arc::new(|| {}));
                    let fits = |part: &RowPart, followed: bool| followed && part.message.len() as u64 <= credit;
                    match walk.take(fits, wake) {
                        (Taken::Part(part), last) => (part.message, last),
                        (Taken::Pending, _) => return,
                        (Taken::End(ended), _) => {
                            state.ended = true;
                            drop(state);
                            lock(downloads).remove(&self.stream);
                            let last = match ended {
                                End::Failed(error) => Err(Failure::from(error)),
                                End::Done | End::Writes => Ok(SqlRows::default().encode()),
                            };
                            return (self.link.finish)(last);
                        }
                    }
                }
            };
            state.credit = credit.saturating_sub(part.len() as u64);
            if last {
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
            let answers = answer(tx, call, link, nested);
            // before the answer: by the time a client reads it, its call's credit is counted back
            if let Ok(read) = u32::try_from(read) {
                (link.grant)(read);
            }
            for answer in answers {
                self.send(link, answer.encode(), deadline)?;
            }
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

/// A call's answers: its rows, what it changed, a method's answer, or why it
/// failed, which left the transaction as it was before the call. A DATA that
/// is not a call ran nothing, and its failure names the field that did not
/// read.
///
/// Rows past one answer go in several, each within the client's credit and
/// every one but the last saying `more`, to a client that asked for parts;
/// a client of 0.10 or before asks for none and is refused them.
fn answer(tx: &Tx<'_>, call: Result<SqlTxCall, Failure>, link: &Link, nested: Nested<'_>) -> Vec<SqlTxAnswer> {
    let ran = || -> Result<Vec<SqlTxAnswer>, Failure> {
        let call = call?;
        if call.want == "call" {
            let method = call.method.and_then(|method| u16::try_from(method).ok());
            let method = method.ok_or_else(|| Failure::invalid("a call inside a transaction names its method"))?;
            let body = nested(tx.transaction(), method, call.body.as_deref().unwrap_or_default(), link.max_body)?;
            return Ok(vec![SqlTxAnswer { body: Some(body), ..SqlTxAnswer::default() }]);
        }
        let statement = statement(call.text, stamped(call.values, call.now.as_deref(), || tx.now())?)?;
        if call.want == "exec" {
            return Ok(vec![SqlTxAnswer { done: Some(done_of(&tx.exec(statement)?)), ..SqlTxAnswer::default() }]);
        }
        let wanted = Wanted::named(&call.want).ok_or_else(|| {
            Failure::invalid(format!("want {:?}: a call wants all, one, scalar, exec or call", call.want))
        })?;
        let bound = part_bound(link);
        let parts = row_parts(tx.rows_of(&statement, wanted)?, bound)?;
        if parts.len() > 1 && !call.parts {
            return Err(rows_past_one_answer(&parts, bound));
        }
        let last = parts.len().saturating_sub(1);
        let answer = |(at, rows)| SqlTxAnswer { rows: Some(rows), more: at < last, ..SqlTxAnswer::default() };
        Ok(parts.into_iter().enumerate().map(answer).collect())
    };
    ran().unwrap_or_else(|failure| vec![SqlTxAnswer { failure: Some(failure), ..SqlTxAnswer::default() }])
}

/// Why a client that reads one answer a call, of 0.10 or before, is refused
/// rows past it: never "read them outside the transaction", which loses the
/// write a transaction is for.
fn rows_past_one_answer(parts: &VecDeque<SqlRows>, bound: usize) -> Failure {
    let wanted = parts.iter().map(|part| part.encode().len()).sum::<usize>() as u64;
    let carried = if bound.is_multiple_of(1 << 20) { format!("{} MiB", bound >> 20) } else { format!("{bound} bytes") };
    let refused =
        format!("rows past what one answer of this client carries, {carried}: a client of 0.11 reads them whole");
    Failure::from(Limit { bound: bound as u64, ..ANSWER_BYTES }.refuse(wanted, refused))
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

/// A statement's values with the store's time first, in the unit `now`
/// names, when it names one: what a table's delete marks a row with, read
/// from the store's clock, which a test's clock moves, and not the client's.
fn stamped(values: Vec<Cell>, now: Option<&str>, clock: impl FnOnce() -> SystemTime) -> Result<Vec<Cell>, Failure> {
    let Some(now) = now else { return Ok(values) };
    let unit = unit_named(Some(now))?;
    Ok(std::iter::once(Cell::Int(unit.of(clock()))).chain(values).collect())
}

/// The unit of a mark the wire names, `s` or `ms`; milliseconds when absent.
fn unit_named(name: Option<&str>) -> Result<Unit, Failure> {
    let Some(name) = name else { return Ok(Unit::default()) };
    Unit::named(name).ok_or_else(|| Failure::invalid(format!("a unit {name:?}: a mark's time is s or ms")))
}

/// A statement from its text and the cells of its `?`s.
pub(super) fn statement(text: String, values: Vec<Cell>) -> Result<Sql, Failure> {
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

#[path = "sql_run.rs"]
mod run;

#[cfg(test)]
#[path = "sql_tests.rs"]
mod tests;
