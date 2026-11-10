//! Where a job is: one job by its id, or a page of a queue's jobs.

use std::sync::Arc;
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::de::DeserializeOwned;

use super::claim::Lease;
use super::rows::due_of;
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
            j.ran, j.took, j.grp"
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
macro_rules! failed_columns {
    () => {
        "f.key, f.failed, f.id, f.at, f.attempt, null, 1, null, f.error, f.value, f.spill,
            coalesce(length(f.value), (select length(s.value) from _tinystore_jobs_spilled s where s.id = f.spill), 0),
            f.ran, f.took, f.grp
        from _tinystore_jobs_failed f"
    };
}

const GET_JOB: &str = concat!("select ", keyed_columns!(), " where k.queue = ?1 and k.key = ?2");
const GET_FAILED: &str = concat!("select ", failed_columns!(), " where f.queue = ?1 and f.key = ?2");
const GET_DONE: &str = "select d.key, d.until, 0, d.at, d.attempt, null, 2, null, null, d.value, d.spill,
        coalesce(length(d.value), (select length(s.value) from _tinystore_jobs_spilled s where s.id = d.spill), 0),
        d.ran, d.took, d.grp
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
    " where k.queue = ?1 and k.key > ?2 and k.key >= ?3 and (?4 is null or k.key < ?4) and ?5 in (0, 1)
        union all
        select ",
    failed_columns!(),
    " where f.queue = ?1 and f.key > ?2 and f.key >= ?3 and (?4 is null or f.key < ?4) and ?5 in (0, 2)
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
    where j.queue = ?1 and j.key is null and (j.next, j.id) > (?7, ?8)
    order by j.next, j.id limit cast(?6 as integer)"
);
/// A page of the failed jobs, the last failed first, before the one that
/// failed at ?7 with the number ?8; ?5 is 1 for those without ids alone.
const PAGE_FAILED: &str = concat!(
    "select ",
    failed_columns!(),
    " where f.queue = ?1 and (?5 = 0 or f.key is null) and (f.failed, f.id) < (?7, ?8)
    order by f.failed desc, f.id desc limit cast(?6 as integer)"
);

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
        found = one(c, GET_FAILED, params![queue.id, key])?;
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
                all(c, PAGE_FAILED, params![queue.id, no, no, no, 0, left, before, id])?
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
                all(c, PAGE_FAILED, params![queue.id, no, no, no, 1, left, before, id])?
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

/// A job as its error names it: by its id, or, for one without, by its number
/// and its time, unix milliseconds, which say where it lies in the queue.
fn named(found: &Found) -> String {
    match (&found.key, found.table) {
        (Some(key), _) => format!("id {key:?}"),
        (None, Table::Failed) => format!("job {} without an id, failed at unix ms {}", found.id, found.time),
        (None, _) => format!("job {} without an id, due at unix ms {}", found.id, due_of(found.time)),
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

fn found(row: &Row<'_>) -> rusqlite::Result<Found> {
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
