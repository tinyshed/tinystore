//! Where a job is: one job by its id, or a page of a queue's jobs.

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::de::DeserializeOwned;

use super::rows::due_of;
use super::state::QueueState;
use super::values;
use crate::clock::from_unix_millis;
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// `ahead` counts this far: a job further back reads it.
const MAX_AHEAD: usize = 10_000;

/// Jobs a page holds unless a filter says, and at most; and the value bytes it
/// holds at most.
pub const PAGE_JOBS: usize = 1000;
const DEFAULT_PAGE: usize = 100;
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

/// Which jobs a page lists: those whose ids start with a prefix, in the byte
/// order of their ids, or with no prefix and `Failed` the failed jobs, the
/// last failed first. A prefix is text: `chat:4` meets chat 42 too, so end it
/// with a separator.
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

/// One page of jobs, from one snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct Page<V> {
    pub jobs: Vec<Job<V>>,
    /// Where the next page starts, when there is more.
    pub next: Option<String>,
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
macro_rules! keyed_columns {
    () => {
        "j.key, j.next, j.id, j.at, j.attempt, l.attempt, 0, j.repeat, j.error, j.value, j.spill,
            coalesce(length(j.value), (select length(s.value) from _tinystore_jobs_spilled s where s.id = j.spill), 0),
            j.ran, j.took, j.grp
        from _tinystore_jobs_keys k join _tinystore_jobs j on j.queue = k.queue and j.next = k.next and j.id = k.id
        left join _tinystore_jobs_leases l on l.id = j.id and l.until > ?9"
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
const PAGE_FAILED: &str = concat!(
    "select ",
    failed_columns!(),
    " where f.queue = ?1 and (f.failed, f.id) < (?7, ?8) order by f.failed desc, f.id desc limit cast(?6 as integer)"
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
    let found = one(c, GET_JOB, params![queue.id, key, no, no, no, no, no, no, now])?
        .or(one(c, GET_FAILED, params![queue.id, key])?)
        .or(one(c, GET_DONE, params![queue.id, key, now])?);
    let Some(found) = found else {
        return Ok(None);
    };
    let mut job = job_of::<V>(c, queue, &found, now)?;
    if job.state == State::Waiting {
        job.ahead = ahead(c, queue, &found, now)?;
    }
    Ok(Some(job))
}

/// Reads a page of a queue's jobs from one snapshot.
pub(crate) fn page<V: DeserializeOwned>(
    c: &Connection,
    queue: &QueueState,
    filter: &Filter,
    now: i64,
) -> Result<Page<V>> {
    let limit = filter.limit.unwrap_or(DEFAULT_PAGE);
    if limit == 0 || limit > PAGE_JOBS {
        return Err(Error::invalid(format!("a page of {limit} jobs, not 1 to {PAGE_JOBS}")));
    }
    if filter.state.is_some_and(|state| matches!(state, State::Done | State::Cancelled)) {
        return Err(Error::invalid("a page lists scheduled, waiting, running or failed jobs"));
    }
    let found = rows(c, queue, filter, limit + 1, now)?;
    let mut page = Page { jobs: Vec::new(), next: None };
    let (mut bytes, mut read, mut last) = (0, 0, None);
    for (at, found) in found.iter().enumerate() {
        if at == limit || at > 0 && bytes + found.size > PAGE_BYTES {
            break;
        }
        read += 1;
        bytes += found.size;
        last = Some(cursor_of(filter, found));
        // whether a held job runs is memory's to say, so a page of one state
        // leaves out the rows it read that turn out another, and may hold fewer
        let job = job_of::<V>(c, queue, found, now)?;
        if filter.state.is_none_or(|state| state == job.state) {
            page.jobs.push(job);
        }
    }
    if read < found.len() {
        page.next = last;
    }
    Ok(page)
}

/// The rows a page reads, up to `limit`.
fn rows(c: &Connection, queue: &QueueState, filter: &Filter, limit: usize, now: i64) -> Result<Vec<Found>> {
    let limit = limit as i64;
    if filter.prefix.is_empty() && filter.state == Some(State::Failed) {
        let (before, id) = failed_cursor(filter.after.as_deref())?;
        let no = Option::<i64>::None;
        return all(c, PAGE_FAILED, params![queue.id, no, no, no, no, limit, before, id]);
    }
    let tables = match filter.state {
        None => 0,
        Some(State::Failed) => 2,
        Some(_) => 1,
    };
    let after = filter.after.as_deref().unwrap_or("");
    let end = prefix_end(&filter.prefix);
    let end = end.as_deref().map(TextBytes);
    all(c, PAGE_BY_KEY, params![queue.id, after, filter.prefix, end, tables, limit, None::<i64>, None::<i64>, now])
}

/// Bytes bound as text, which SQLite compares with ids byte by byte. A blob
/// would not do: every text is less than every blob.
struct TextBytes<'a>(&'a [u8]);

impl rusqlite::ToSql for TextBytes<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(rusqlite::types::ValueRef::Text(self.0)))
    }
}

/// Where the next page starts after a job: its id, or for failed jobs listed
/// by time, when it failed and its job's number.
fn cursor_of(filter: &Filter, found: &Found) -> String {
    if filter.prefix.is_empty() && filter.state == Some(State::Failed) {
        return format!("{}.{}", found.time, found.id);
    }
    found.key.clone().unwrap_or_default()
}

fn failed_cursor(after: Option<&str>) -> Result<(i64, i64)> {
    let Some(after) = after else {
        return Ok((i64::MAX, i64::MAX));
    };
    let parsed = after.split_once('.').and_then(|(failed, id)| Some((failed.parse().ok()?, id.parse().ok()?)));
    parsed.ok_or_else(|| Error::invalid(format!("a cursor of {after:?}")))
}

/// The first text after every text starting with `prefix`, in the byte order
/// SQLite compares ids by; none when the prefix is empty or only bytes that
/// cannot grow.
fn prefix_end(prefix: &str) -> Option<Vec<u8>> {
    let mut end = prefix.as_bytes().to_vec();
    while let Some(last) = end.pop() {
        if last < 0xff {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

fn one(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Option<Found>> {
    c.prepare_cached(sql)
        .and_then(|mut select| select.query_row(params, found).optional())
        .map_err(|error| sql_error("a job", error))
}

fn all(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Found>> {
    let read = || -> rusqlite::Result<Vec<Found>> {
        let mut select = c.prepare_cached(sql)?;
        let rows = select.query_map(params, found)?;
        rows.collect()
    };
    read().map_err(|error| sql_error("a page of jobs", error))
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

/// Makes a row a job, its state as memory and the file say together: a row
/// a lease holds runs once its handler has it, and until then waits.
fn job_of<V: DeserializeOwned>(c: &Connection, queue: &QueueState, found: &Found, now: i64) -> Result<Job<V>> {
    let held = found.leased.and_then(|_| queue.held(found.id)).filter(|lease| lease.started() && !lease.settled());
    let (state, at) = match found.table {
        Table::Failed => (State::Failed, found.at),
        Table::Done => (State::Done, found.at),
        Table::Jobs if held.is_some() => (State::Running, found.at),
        Table::Jobs if due_of(found.time) > now => (State::Scheduled, due_of(found.time)),
        Table::Jobs => (State::Waiting, due_of(found.time)),
    };
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
    let value = values::decode::<V>(&encoded).map_err(|error| Error::corrupt(error.to_string()))?;
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

/// The jobs that run before a waiting one: the rows before it, at most
/// `MAX_AHEAD` past those a handler runs, which wait before nothing.
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

#[cfg(test)]
mod tests {
    use super::prefix_end;

    #[test]
    fn a_prefixs_end_is_the_first_text_past_every_text_it_starts() {
        assert_eq!(prefix_end("chat:42:"), Some(b"chat:42;".to_vec()));
        assert_eq!(prefix_end("aé"), Some(vec![b'a', 0xc3, 0xaa]));
        assert_eq!(prefix_end(""), None);
    }
}
