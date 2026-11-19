//! Where a job is: one job by its id, or a page of a queue's jobs.

use std::sync::Arc;
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;

use super::claim::Lease;
use super::rows::{PARKED_BY, PARKED_FROM, due_of};
use super::state::QueueState;
use super::values;
use crate::clock::from_unix_millis;
use crate::sqlite::{File, TextBytes, prefix_end, query_cached, sql_error};
use crate::{Error, Result};

/// `ahead` counts this far: a job further back reads it.
const MAX_AHEAD: usize = 10_000;

/// The most jobs a page holds.
pub const PAGE_JOBS: usize = 1000;
/// The jobs a page holds unless a call says.
const DEFAULT_PAGE: usize = 100;
/// The most value bytes a page holds.
const PAGE_BYTES: usize = 4 << 20;

/// Where a job is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    /// Its time has not come.
    Scheduled,
    /// Due, and no handler has it yet.
    Waiting,
    Running,
    /// Done, while the queue's dedupe keeps its id.
    Done,
    /// Failed for good, kept for the queue's `keep`.
    Failed,
    /// Taken by a cancel; only a watcher sees it, as its last.
    Cancelled,
}

/// A job as its queue holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Job<V> {
    pub id: Option<String>,
    pub value: V,
    pub state: State,
    /// When it runs next; for a job done or failed, when its last run was for.
    pub at: SystemTime,
    /// The attempts its run has had, a running one included.
    pub attempt: u32,
    /// The jobs that run before a waiting one, up to 10,000.
    pub ahead: u32,
    /// What its handler last reported of a running job.
    pub progress: Option<serde_json::Value>,
    /// Why its last run failed.
    pub error: Option<String>,
    pub group: Option<String>,
    /// A repeating job's repeat, as it keeps it.
    pub repeat: Option<String>,
    pub last_run: Option<LastRun>,
    /// When it leaves its queue unless written again: its `ttl` from its last
    /// write, or its `expires_at`.
    pub expires_at: Option<SystemTime>,
    /// Where a job without an id lies, which a cancel of it names.
    pub(crate) row: Option<Row>,
}

/// Where a job without an id lies in its queue's file: its queue's rows by
/// its time and number, or the failed ones by when it failed and its number.
/// A number is taken once a file and never again, so a row names that job or
/// none: one retried or run again for its repeat has moved to another row,
/// and a parked one is found at the time it is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Queued { next: i64, number: i64 },
    Failed { failed: i64, number: i64 },
}

impl Row {
    /// A row from its text, which is a page's cursor after that row.
    #[cfg(any(feature = "wire", feature = "replication"))]
    pub(crate) fn parse(text: &str) -> Result<Row> {
        match Cursor::parse(text) {
            Ok(Cursor::Queued(next, number)) => Ok(Row::Queued { next, number }),
            Ok(Cursor::Failed(failed, number)) => Ok(Row::Failed { failed, number }),
            _ => Err(Error::invalid(format!("a job's row of {text:?}, which no page gave"))),
        }
    }

    /// The job as an error names it: by its number and its time, unix
    /// milliseconds, which say where it lies in the queue.
    pub(crate) fn describe(self) -> String {
        match self {
            Row::Queued { next, number } => format!("job {number} without an id, due at unix ms {}", due_of(next)),
            Row::Failed { failed, number } => format!("job {number} without an id, failed at unix ms {failed}"),
        }
    }
}

impl std::fmt::Display for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cursor = match *self {
            Row::Queued { next, number } => Cursor::Queued(next, number),
            Row::Failed { failed, number } => Cursor::Failed(failed, number),
        };
        cursor.fmt(f)
    }
}

/// The job a cancel takes: its id, `"welcome-42"`, or a job a page gave,
/// `&job`, taken by its id, or by the row it was listed from when it has none.
pub trait JobRef: named::Named {}

impl<T: named::Named> JobRef for T {}

pub(crate) mod named {
    use super::{Job, Row};

    /// How a cancel finds the job it takes.
    #[derive(Clone, Debug)]
    pub enum Which {
        Id(String),
        Row(Row),
    }

    pub trait Named {
        /// None for a job that carries neither an id nor a row.
        fn which(&self) -> Option<Which>;
    }

    impl<T: AsRef<str>> Named for T {
        fn which(&self) -> Option<Which> {
            Some(Which::Id(self.as_ref().to_owned()))
        }
    }

    impl<V> Named for &Job<V> {
        fn which(&self) -> Option<Which> {
            match (&self.id, self.row) {
                (Some(id), _) => Some(Which::Id(id.clone())),
                (None, row) => row.map(Which::Row),
            }
        }
    }

    impl Which {
        /// The job as an error names it.
        pub(crate) fn describe(&self) -> String {
            match self {
                Which::Id(id) => format!("id {id:?}"),
                Which::Row(row) => row.describe(),
            }
        }
    }
}

/// How many jobs a queue holds in each state, from one snapshot. A job done
/// is not counted: it left the queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Counts {
    /// Due later.
    pub scheduled: u64,
    /// Due, and no handler runs it yet.
    pub waiting: u64,
    pub running: u64,
    /// Failed for good, kept for the queue's `keep`.
    pub failed: u64,
}

/// When the last run a handler finished began and ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastRun {
    pub started_at: SystemTime,
    pub ended_at: SystemTime,
}

/// Which jobs a page lists. With a prefix, those whose ids start with it, in
/// the byte order of their ids; a prefix is text: `chat:4` meets chat 42 too,
/// so end it with a separator. With none, every job the queue holds: those
/// with ids in that order, then those without in the order they run, then
/// those without that failed, the last failed first. With no prefix and
/// `Failed`, the failed jobs alone, the last failed first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    prefix: String,
    state: Option<State>,
    limit: Option<usize>,
    after: Option<String>,
}

impl Filter {
    pub fn prefix(prefix: impl Into<String>) -> Filter {
        Filter { prefix: prefix.into(), ..Filter::default() }
    }

    /// The jobs in `state` alone: scheduled, waiting, running or failed.
    pub fn state(mut self, state: State) -> Filter {
        self.state = Some(state);
        self
    }

    /// Jobs a page holds at most: 100 unless said, 1000 at most.
    pub fn limit(mut self, jobs: usize) -> Filter {
        self.limit = Some(jobs);
        self
    }

    /// Where the page before ended: its `next`.
    pub fn after(mut self, next: impl Into<String>) -> Filter {
        self.after = Some(next.into());
        self
    }
}

/// One page of jobs, from one snapshot. A page of one state may read
/// several, one for each window of rows it reads past the other states'.
#[derive(Clone, Debug, PartialEq)]
pub struct Page<V> {
    pub jobs: Vec<Job<V>>,
    /// Where the next page starts, when there is more.
    pub next: Option<String>,
}

/// A page as a walk reads it: each job, or the error of one whose value no
/// longer reads, which names the job, so that a walk goes on past it.
pub(crate) struct Listed<V> {
    pub(crate) jobs: Vec<Result<Job<V>>>,
    pub(crate) next: Option<String>,
}

/// A job's row as a read finds it, in the queue's rows or kept failed or done.
#[derive(Debug)]
struct Found {
    key: Option<String>,
    /// Its next, or when it failed, or until when dedupe keeps it.
    time: i64,
    id: i64,
    at: i64,
    attempt: i64,
    /// The running attempt, when a live lease holds the job.
    leased: Option<i64>,
    table: Table,
    repeat: Option<String>,
    error: Option<String>,
    value: Option<Vec<u8>>,
    spill: Option<i64>,
    size: usize,
    ran: Option<i64>,
    took: Option<i64>,
    group: Option<String>,
    expires: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Table {
    Jobs,
    Failed,
    Done,
}

/// The columns every read takes, in the order `found` reads them. A value's
/// size is read without its bytes: SQLite takes a blob's length from the row's
/// header and leaves its overflow pages alone.
macro_rules! job_columns {
    () => {
        "j.key, j.next, j.id, j.at, j.attempt, l.attempt, 0, j.repeat, j.error, j.value, j.spill,
            coalesce(length(j.value), (select length(s.value) from _tinystore_jobs_spilled s where s.id = j.spill), 0),
            j.ran, j.took, j.grp, j.expires"
    };
}
macro_rules! keyed_columns {
    () => {
        concat!(
            job_columns!(),
            " from _tinystore_jobs_keys k join _tinystore_jobs j on j.queue = k.queue and j.next = k.next and j.id = k.id
            left join _tinystore_jobs_leases l on l.id = j.id and l.until > ?9"
        )
    };
}
/// A job past its term that no run holds is gone, though no claim or
/// maintenance deleted it yet.
macro_rules! within_term {
    () => {
        " and (j.expires is null or j.expires > ?9 or l.id is not null)"
    };
}
macro_rules! failed_columns {
    () => {
        "f.key, f.failed, f.id, f.at, f.attempt, null, 1, null, f.error, f.value, f.spill,
            coalesce(length(f.value), (select length(s.value) from _tinystore_jobs_spilled s where s.id = f.spill), 0),
            f.ran, f.took, f.grp, null
        from _tinystore_jobs_failed f"
    };
}

const GET_JOB: &str = concat!("select ", keyed_columns!(), " where k.queue = ?1 and k.key = ?2", within_term!());
/// A failed job past a keep of its own is gone, though maintenance has not
/// deleted it yet.
const GET_FAILED: &str =
    concat!("select ", failed_columns!(), " where f.queue = ?1 and f.key = ?2 and (f.until is null or f.until > ?3)");
const GET_DONE: &str = "select d.key, d.until, 0, d.at, d.attempt, null, 2, null, null, d.value, d.spill,
        coalesce(length(d.value), (select length(s.value) from _tinystore_jobs_spilled s where s.id = d.spill), 0),
        d.ran, d.took, d.grp, null
    from _tinystore_jobs_done d where d.queue = ?1 and d.key = ?2 and d.until > ?3";
const SPILLED: &str = "select value from _tinystore_jobs_spilled where id = ?1";
/// The rows before a job in the order the queue runs them, at most ?4.
const ROWS_BEFORE: &str = "select count(*) from (select 1 from _tinystore_jobs
    where queue = ?1 and (next, id) < (?2, ?3) limit cast(?4 as integer))";
/// A page by ids: both tables merged in key order; ?5 is the tables a state
/// asks for, 0 both, 1 the queue's, 2 the failed.
const PAGE_BY_KEY: &str = concat!(
    "select * from (
        select ",
    keyed_columns!(),
    " where k.queue = ?1 and k.key > ?2 and k.key >= ?3 and (?4 is null or k.key < ?4) and ?5 in (0, 1)",
    within_term!(),
    "
        union all
        select ",
    failed_columns!(),
    " where f.queue = ?1 and f.key > ?2 and f.key >= ?3 and (?4 is null or f.key < ?4) and ?5 in (0, 2)
        and (f.until is null or f.until > ?9)
    ) order by 1 limit cast(?6 as integer)"
);
/// A page of the jobs without ids in the order the queue runs them, after the
/// job whose next and number are ?7 and ?8. The queue's rows are read in that
/// order, those with ids passed over, so the first page after the last id
/// may read through every row the queue holds to find none.
const PAGE_UNNAMED: &str = concat!(
    "select ",
    job_columns!(),
    " from _tinystore_jobs j left join _tinystore_jobs_leases l on l.id = j.id and l.until > ?9
    where j.queue = ?1 and j.key is null and (j.next, j.id) > (?7, ?8)",
    within_term!(),
    "
    order by j.next, j.id limit cast(?6 as integer)"
);
/// A page of the failed jobs, the last failed first, before the one that
/// failed at ?7 with the number ?8; ?5 is 1 for those without ids alone.
const PAGE_FAILED: &str = concat!(
    "select ",
    failed_columns!(),
    " where f.queue = ?1 and (?5 = 0 or f.key is null) and (f.failed, f.id) < (?7, ?8)
        and (f.until is null or f.until > ?9)
    order by f.failed desc, f.id desc limit cast(?6 as integer)"
);

/// The rows a queue holds, which its triggers keep, so that a count walks
/// the rows due later alone and not the due ones, which may be millions.
const COUNT_ROWS: &str = "select waiting from _tinystore_jobs_queues where id = ?1";
/// The rows due after ?2: those below the parked bound ?3, and a parked one
/// due later still, past ?4, which only a clock set back makes.
const COUNT_LATER: &str = "select (select count(*) from _tinystore_jobs where queue = ?1 and next > ?2 and next < ?3)
    + (select count(*) from _tinystore_jobs where queue = ?1 and next > ?4)";
/// The jobs live leases hold, which run once memory says their handler began.
const LIVE_LEASES: &str = "select id from _tinystore_jobs_leases where queue = ?1 and until > ?2";
const COUNT_FAILED: &str =
    "select count(*) from _tinystore_jobs_failed where queue = ?1 and (until is null or until > ?2)";

/// How many jobs the queue holds in each state at `now`, as a page would
/// list them: a job a lease holds runs once its handler has it, and until
/// then waits.
pub(crate) fn counts(c: &Connection, queue: &QueueState, now: i64) -> Result<Counts> {
    let count = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> Result<u64> {
        let counted: i64 = c
            .prepare_cached(sql)
            .and_then(|mut select| select.query_row(params, |row| row.get(0)))
            .map_err(|error| sql_error("a count of jobs", error))?;
        Ok(u64::try_from(counted).unwrap_or(0))
    };
    let rows = count(COUNT_ROWS, &[&queue.id])?;
    let later = count(COUNT_LATER, &[&queue.id, &now, &PARKED_FROM, &now.saturating_add(PARKED_BY)])?;
    let leased: Vec<i64> =
        query_cached(c, (LIVE_LEASES, "the jobs that run"), params![queue.id, now], |row| row.get(0))?;
    let running =
        leased.into_iter().filter_map(|id| queue.held(id)).filter(|lease| lease.started() && !lease.settled()).count()
            as u64;
    let failed = count(COUNT_FAILED, &[&queue.id, &now])?;
    let scheduled = later.min(rows);
    let running = running.min(rows - scheduled);
    Ok(Counts { scheduled, waiting: rows - scheduled - running, running, failed })
}

/// Reads the job under `key` from one snapshot: in the queue's rows, failed,
/// or done while the queue's dedupe keeps its id.
pub(crate) fn get<V: DeserializeOwned>(
    c: &Connection,
    queue: &QueueState,
    key: &str,
    now: i64,
) -> Result<Option<Job<V>>> {
    let no = Option::<i64>::None;
    // Each lookup only when the one before found nothing.
    let mut found = one(c, GET_JOB, params![queue.id, key, no, no, no, no, no, no, now])?;
    if found.is_none() {
        found = one(c, GET_FAILED, params![queue.id, key, now])?;
    }
    if found.is_none() {
        found = one(c, GET_DONE, params![queue.id, key, now])?;
    }
    let Some(found) = found else {
        return Ok(None);
    };
    let mut job = job_of::<V>(c, queue, &found, now)?;
    if job.state == State::Waiting {
        job.ahead = ahead(c, queue, &found, now)?;
    }
    Ok(Some(job))
}

/// Reads a page of a queue's jobs a window of rows at a time, each window from
/// a snapshot of its own and as many rows as the page holds and one more, so
/// that no read is long. Whether a held job runs is memory's to say, so a
/// page of one state reads on past the rows of the others until it holds its
/// limit or the queue's rows end (JOB-14). A job whose value no longer reads
/// takes its place on the page as its error.
pub(crate) fn page<V: DeserializeOwned>(
    file: &File,
    queue: &QueueState,
    filter: &Filter,
    now: i64,
) -> Result<Listed<V>> {
    let limit = filter.limit.unwrap_or(DEFAULT_PAGE);
    if limit == 0 || limit > PAGE_JOBS {
        return Err(Error::invalid(format!("a page of {limit} jobs, not 1 to {PAGE_JOBS}")));
    }
    if filter.state.is_some_and(|state| matches!(state, State::Done | State::Cancelled)) {
        return Err(Error::invalid("a page lists scheduled, waiting, running or failed jobs"));
    }
    let mut page = Listed { jobs: Vec::new(), next: None };
    let (mut from, mut bytes) = (filter.clone(), 0);
    loop {
        let read = file.read(|c| window(c, queue, (&from, limit, now), (&mut page.jobs, &mut bytes)))?;
        page.next = match read {
            Window::ReadOn(after) if page.jobs.len() < limit => {
                from.after = Some(after.to_string());
                continue;
            }
            Window::ReadOn(after) | Window::Full(Some(after)) => Some(after.to_string()),
            Window::Full(None) => from.after,
            Window::Ended => None,
        };
        return Ok(page);
    }
}

/// How one window of a page ended.
enum Window {
    /// Every row it fetched read, after the last of which the page reads on.
    ReadOn(Cursor),
    /// The page full: the next starts after this row, or where the window
    /// began when it read none.
    Full(Option<Cursor>),
    /// The queue's rows ended.
    Ended,
}

/// Reads one window of rows from where `filter` stands at `now`, and adds to
/// `jobs` those in its state, until they are `limit` or their values
/// `PAGE_BYTES`.
fn window<V: DeserializeOwned>(
    c: &Connection,
    queue: &QueueState,
    (filter, limit, now): (&Filter, usize, i64),
    (jobs, bytes): (&mut Vec<Result<Job<V>>>, &mut usize),
) -> Result<Window> {
    let found = rows(c, queue, filter, limit + 1, now)?;
    let mut last = None;
    for found in &found {
        let kept = filter.state.is_none_or(|state| state == state_of(queue, found, now).0);
        if jobs.len() == limit || kept && !jobs.is_empty() && *bytes + found.size > PAGE_BYTES {
            return Ok(Window::Full(last.map(|last| cursor_of(filter, last))));
        }
        last = Some(found);
        if kept {
            *bytes += found.size;
            jobs.push(job_of::<V>(c, queue, found, now).map_err(|error| error.within(named(found))));
        }
    }
    Ok(match last {
        Some(last) if found.len() > limit => Window::ReadOn(cursor_of(filter, last)),
        _ => Window::Ended,
    })
}

/// The rows a page reads, up to `limit`: the walk its cursor names from where
/// it stopped, and each walk after it from its start, until `limit` are read.
fn rows(c: &Connection, queue: &QueueState, filter: &Filter, limit: usize, now: i64) -> Result<Vec<Found>> {
    let given = filter.after.as_deref().map(Cursor::parse).transpose()?;
    let no = Option::<i64>::None;
    let by_failure = filter.prefix.is_empty() && filter.state == Some(State::Failed);
    let mut walk = match given {
        None if by_failure => Cursor::Failed(i64::MAX, i64::MAX),
        None => Cursor::Id(String::new()),
        Some(cursor) => cursor,
    };
    let mut found = Vec::new();
    loop {
        let left = (limit - found.len()) as i64;
        let read = match &walk {
            Cursor::Failed(before, id) if by_failure => {
                all(c, PAGE_FAILED, params![queue.id, no, no, no, 0, left, before, id, now])?
            }
            Cursor::Id(after) if !by_failure => {
                let tables = match filter.state {
                    None => 0,
                    Some(State::Failed) => 2,
                    Some(_) => 1,
                };
                let end = prefix_end(&filter.prefix);
                let end = end.as_deref().map(TextBytes);
                all(c, PAGE_BY_KEY, params![queue.id, after, filter.prefix, end, tables, left, no, no, now])?
            }
            Cursor::Queued(next, id) if filter.prefix.is_empty() && !by_failure => {
                all(c, PAGE_UNNAMED, params![queue.id, no, no, no, no, left, next, id, now])?
            }
            Cursor::Failed(before, id) if filter.prefix.is_empty() && filter.state.is_none() => {
                all(c, PAGE_FAILED, params![queue.id, no, no, no, 1, left, before, id, now])?
            }
            _ => {
                let given = filter.after.as_deref().unwrap_or_default();
                return Err(Error::invalid(format!("a cursor of {given:?}, which no page of this filter gave")));
            }
        };
        found.extend(read);
        if found.len() == limit {
            break;
        }
        walk = match walk {
            Cursor::Id(_) if filter.prefix.is_empty() => Cursor::Queued(i64::MIN, i64::MIN),
            Cursor::Queued(..) if filter.state.is_none() => Cursor::Failed(i64::MAX, i64::MAX),
            _ => break,
        };
    }
    Ok(found)
}

/// Where a page ended, which its `next` carries: after an id, after a job
/// without one in the order the queue runs them, its next and number, or
/// after a failed job, the last failed first, when it failed and its number.
#[derive(Debug, PartialEq, Eq)]
enum Cursor {
    Id(String),
    Queued(i64, i64),
    Failed(i64, i64),
}

impl Cursor {
    fn parse(text: &str) -> Result<Cursor> {
        if let Some(id) = text.strip_prefix("id:") {
            return Ok(Cursor::Id(id.to_owned()));
        }
        let place = |kind: &str| {
            let (time, number) = text.strip_prefix(kind)?.split_once('.')?;
            Some((time.parse().ok()?, number.parse().ok()?))
        };
        if let Some((next, number)) = place("queued:") {
            return Ok(Cursor::Queued(next, number));
        }
        if let Some((failed, number)) = place("failed:") {
            return Ok(Cursor::Failed(failed, number));
        }
        Err(Error::invalid(format!("a cursor of {text:?}, which no page gave")))
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cursor::Id(id) => write!(f, "id:{id}"),
            Cursor::Queued(next, number) => write!(f, "queued:{next}.{number}"),
            Cursor::Failed(failed, number) => write!(f, "failed:{failed}.{number}"),
        }
    }
}

/// Where the next page starts after a job: its id, or for one without, where
/// it lies in its walk; for failed jobs listed by time, when it failed.
fn cursor_of(filter: &Filter, found: &Found) -> Cursor {
    let by_failure = filter.prefix.is_empty() && filter.state == Some(State::Failed);
    match (&found.key, found.table) {
        (Some(key), _) if !by_failure => Cursor::Id(key.clone()),
        (_, Table::Failed) => Cursor::Failed(found.time, found.id),
        (_, _) => Cursor::Queued(found.time, found.id),
    }
}

/// A job as its error names it: by its id, or, for one without, by its row.
fn named(found: &Found) -> String {
    match (&found.key, row_of(found)) {
        (Some(key), _) => format!("id {key:?}"),
        (None, Some(row)) => row.describe(),
        (None, None) => format!("job {} without an id", found.id),
    }
}

/// Where a job lies; one kept done is in no row a cancel takes.
fn row_of(found: &Found) -> Option<Row> {
    match found.table {
        Table::Jobs => Some(Row::Queued { next: found.time, number: found.id }),
        Table::Failed => Some(Row::Failed { failed: found.time, number: found.id }),
        Table::Done => None,
    }
}

fn one(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Option<Found>> {
    c.prepare_cached(sql)
        .and_then(|mut select| select.query_row(params, found).optional())
        .map_err(|error| sql_error("a job", error))
}

fn all(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Found>> {
    query_cached(c, (sql, "a page of jobs"), params, found)
}

fn found(row: &rusqlite::Row<'_>) -> rusqlite::Result<Found> {
    let table = match row.get::<_, i64>(6)? {
        1 => Table::Failed,
        2 => Table::Done,
        _ => Table::Jobs,
    };
    Ok(Found {
        key: row.get(0)?,
        time: row.get(1)?,
        id: row.get(2)?,
        at: row.get(3)?,
        attempt: row.get(4)?,
        leased: row.get(5)?,
        table,
        repeat: row.get(7)?,
        error: row.get(8)?,
        value: row.get(9)?,
        spill: row.get(10)?,
        size: usize::try_from(row.get::<_, i64>(11)?).unwrap_or(0),
        ran: row.get(12)?,
        took: row.get(13)?,
        group: row.get(14)?,
        expires: row.get(15)?,
    })
}

/// A row's state as memory and the file say together, the time it shows and
/// the lease that runs it: a row a lease holds runs once its handler has it,
/// and until then waits.
fn state_of(queue: &QueueState, found: &Found, now: i64) -> (State, i64, Option<Arc<Lease>>) {
    let held = found.leased.and_then(|_| queue.held(found.id)).filter(|lease| lease.started() && !lease.settled());
    let (state, at) = match found.table {
        Table::Failed => (State::Failed, found.at),
        Table::Done => (State::Done, found.at),
        Table::Jobs if held.is_some() => (State::Running, found.at),
        Table::Jobs if due_of(found.time) > now => (State::Scheduled, due_of(found.time)),
        Table::Jobs => (State::Waiting, due_of(found.time)),
    };
    (state, at, held)
}

fn job_of<V: DeserializeOwned>(c: &Connection, queue: &QueueState, found: &Found, now: i64) -> Result<Job<V>> {
    let (state, at, held) = state_of(queue, found, now);
    let attempt = match (&held, found.leased) {
        (Some(_), Some(leased)) => leased,
        _ => found.attempt,
    };
    let encoded = match found.spill {
        Some(spill) => c
            .prepare_cached(SPILLED)
            .and_then(|mut select| select.query_row([spill], |row| row.get::<_, Vec<u8>>(0)))
            .map_err(|error| sql_error("the value a job spilled", error))?,
        None => found.value.clone().unwrap_or_default(),
    };
    let value = values::decode::<V>(&encoded).map_err(|error| Error::corrupt("a job's value").with_source(error))?;
    let last_run = match (found.ran, found.took) {
        (Some(ran), Some(took)) => {
            Some(LastRun { started_at: from_unix_millis(ran), ended_at: from_unix_millis(ran.saturating_add(took)) })
        }
        _ => None,
    };
    Ok(Job {
        id: found.key.clone(),
        value,
        state,
        at: from_unix_millis(at),
        attempt: u32::try_from(attempt).unwrap_or(u32::MAX),
        ahead: 0,
        progress: held.and_then(|lease| lease.progress()),
        error: found.error.clone(),
        group: found.group.clone(),
        repeat: found.repeat.clone(),
        last_run,
        expires_at: found.expires.map(from_unix_millis),
        row: if found.key.is_none() { row_of(found) } else { None },
    })
}

/// How many jobs run before a waiting one: the waiting rows due before it,
/// counted up to `MAX_AHEAD`; those a handler runs now wait for nothing and
/// are not counted.
fn ahead(c: &Connection, queue: &QueueState, found: &Found, now: i64) -> Result<u32> {
    if found.leased.is_some() {
        return Ok(0); // held for a busy handler, which runs it next
    }
    let running = queue.running_before(found.time, found.id, now);
    let rows: i64 = c
        .prepare_cached(ROWS_BEFORE)
        .and_then(|mut select| {
            select.query_row(params![queue.id, found.time, found.id, (MAX_AHEAD + running) as i64], |row| row.get(0))
        })
        .map_err(|error| sql_error("the jobs ahead of a job", error))?;
    let ahead = usize::try_from(rows).unwrap_or(0).saturating_sub(running).min(MAX_AHEAD);
    Ok(u32::try_from(ahead).unwrap_or(u32::MAX))
}
