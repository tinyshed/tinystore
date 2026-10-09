//! kv's methods of protocol 2, protocol/kv.wire: a handle on what a client
//! opened, and the calls on it. A handle holds rows, nil, an integer or bin,
//! so that the server reads what any language wrote.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::codec::{Message, Row};
use super::protocol::{
    Empty, Failure, Handle, KvAllowance, KvAnswer, KvBranch, KvBucketOpen, KvCall, KvCheck, KvCount, KvCountersOpen,
    KvEntry, KvFound, KvList, KvOnceOpen, KvOutcome, KvPage, KvQuotaOpen, KvRateLimitOpen, KvTx, KvTxResults,
    KvWindowUse, KvWritten, method,
};
use crate::clock::from_unix_millis;
use crate::kv::{
    Allowance, Batch, Bucket, Cell, Counters, Hand, Handed, MAX_KEY, Outcome, PAGE_KEYS, Place, Put, Quota, RateLimit,
    Raw, Stamp, Version, WriteOptions, hand, once_rows,
};
use crate::{Error, Store, unix_millis};

/// What a connection opened, by handle.
#[derive(Debug, Default)]
pub(crate) struct Handles {
    open: Mutex<HashMap<u64, Opened>>,
    last: AtomicU64,
}

#[derive(Clone, Debug)]
enum Opened {
    Bucket(Bucket<()>),
    Counters(Counters),
    RateLimit(RateLimit),
    Quota(Quota),
    Once(Bucket<()>),
}

pub(crate) type Answered = Result<Vec<u8>, Failure>;

/// How a session runs a method: a point read at once on the caller's thread,
/// a write queued for its group commit with no thread waiting on it, a run of
/// once handed over, anything else on a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Inline,
    Submit,
    Handover,
    Worker,
}

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::KV_GET | method::KV_HAS | method::KV_ONCE_GET => Route::Inline,
        method::KV_SET | method::KV_CREATE | method::KV_TAKE | method::KV_DELETE | method::KV_EXPIRE => Route::Submit,
        method::KV_ONCE_RUN => Route::Handover,
        _ => Route::Worker,
    }
}

/// Answers a call that runs to its end on this thread.
pub(crate) fn call(store: &Store, handles: &Handles, called: u16, body: &[u8], max_body: usize) -> Answered {
    match called {
        method::KV_BUCKET_OPEN => handles.open(open_bucket(store, KvBucketOpen::decode(body)?)?),
        method::KV_COUNTERS_OPEN => handles.open(open_counters(store, KvCountersOpen::decode(body)?)?),
        method::KV_RATE_LIMIT_OPEN => handles.open(open_rate_limit(store, KvRateLimitOpen::decode(body)?)?),
        method::KV_QUOTA_OPEN => handles.open(open_quota(store, KvQuotaOpen::decode(body)?)?),
        method::KV_ONCE_OPEN => {
            let open = KvOnceOpen::decode(body)?;
            handles.open(Opened::Once(once_rows(store, &open.name, open.keep.map(Duration::from_millis))?))
        }
        method::KV_GET => get(handles, KvCall::decode(body)?),
        method::KV_HAS => has(handles, KvCall::decode(body)?),
        method::KV_CLEAR => clear(handles, KvBranch::decode(body)?),
        method::KV_LIST => list(handles, KvList::decode(body)?, max_body),
        method::KV_COUNTERS_ADD | method::KV_COUNTERS_GET | method::KV_COUNTERS_DELETE => {
            counting(handles, called, KvCall::decode(body)?)
        }
        method::KV_COUNTERS_CLEAR => {
            let branch = KvBranch::decode(body)?;
            handles.counters(branch.handle, &branch.under)?.clear()?;
            Ok(Empty {}.encode())
        }
        method::KV_ALLOW | method::KV_PEEK | method::KV_RESET | method::KV_REFUND => {
            limit(handles, called, KvCall::decode(body)?)
        }
        method::KV_ONCE_GET => {
            let call = KvCall::decode(body)?;
            let cell = handles.once(call.handle, &call.under)?.read_cell(&call.key)?;
            Ok(answer(cell.map(|cell| cell.raw)).encode())
        }
        method::KV_ONCE_DELETE => {
            let call = KvCall::decode(body)?;
            let removed = handles.once(call.handle, &call.under)?.remove(&call.key, WriteOptions::default())?;
            Ok(KvFound { found: removed.is_some() }.encode())
        }
        method::KV_TX => tx(handles, KvTx::decode(body)?),
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

/// Queues a write and returns; `done` gets its answer on the thread that
/// commits it.
pub(crate) fn submit(handles: &Handles, called: u16, body: &[u8], done: impl FnOnce(Answered) + Send + 'static) {
    let prepared = || -> Result<_, Failure> {
        let call = KvCall::decode(body)?;
        let bucket = handles.bucket(call.handle, &call.under)?;
        let options = options(&call)?;
        Ok((call, bucket, options))
    };
    let (call, bucket, options) = match prepared() {
        Ok(prepared) => prepared,
        Err(failure) => return done(Err(failure)),
    };
    let key = call.key.clone();
    match called {
        method::KV_SET | method::KV_CREATE => {
            let put = if called == method::KV_SET { Put::Always } else { Put::OnlyNew };
            let raw = raw_of(call.value.unwrap_or_default());
            bucket
                .put_raw_then(&key, (raw, options, put), move |stamp| done(stamp.map(written).map_err(Failure::from)));
        }
        method::KV_TAKE => bucket.remove_then(&key, options, move |cell| done(cell.map(taken).map_err(Failure::from))),
        method::KV_DELETE => bucket.remove_then(&key, options, move |cell| {
            done(cell.map(|cell| KvFound { found: cell.is_some() }.encode()).map_err(Failure::from));
        }),
        method::KV_EXPIRE => match bucket.expire_work(&key, options) {
            Ok(work) => bucket.scope.submit(&key, 0, work, move |found| {
                done(found.map(|found| KvFound { found }.encode()).map_err(Failure::from));
            }),
            Err(error) => done(Err(Failure::from(error))),
        },
        other => done(Err(Failure::unimplemented(format!("method {other:#06x}")))),
    }
}

/// How a client's run of a once key starts: the answer kept, which ends the
/// stream; the run handed over, kept until the client's answer; or a wait for
/// the run that holds the key, `again` asked once it ends.
pub(crate) enum Run {
    Answered(Vec<u8>),
    Handed(Handed, Vec<u8>),
    Waiting,
}

pub(crate) fn run(handles: &Handles, body: &[u8], again: Box<dyn FnOnce() + Send>) -> Result<Run, Failure> {
    let call = KvCall::decode(body)?;
    let answers = handles.once(call.handle, &call.under)?;
    Ok(match hand(&answers, &call.key, again)? {
        Hand::Kept(raw) => Run::Answered(answer(Some(raw)).encode()),
        Hand::Yours(handed) => Run::Handed(handed, answer(None).encode()),
        Hand::Wait => Run::Waiting,
    })
}

/// Keeps what a client's handed run answered, and lets its key go.
pub(crate) fn kept(handed: Handed, body: &[u8]) -> Answered {
    let answered = KvAnswer::decode(body)?;
    let raw = answered.found.then(|| raw_of(answered.value.unwrap_or_default()));
    handed.keep(raw)?;
    Ok(Empty {}.encode())
}

impl Handles {
    fn open(&self, opened: Opened) -> Answered {
        let handle = self.last.fetch_add(1, Ordering::SeqCst) + 1;
        self.lock().insert(handle, opened);
        Ok(Handle { handle }.encode())
    }

    fn opened(&self, handle: u64) -> Result<Opened, Failure> {
        self.lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Failure::invalid(format!("handle {handle} names nothing this connection opened")))
    }

    fn bucket(&self, handle: u64, under: &[String]) -> Result<Bucket<()>, Failure> {
        match self.opened(handle)? {
            Opened::Bucket(bucket) => Ok(under.iter().fold(bucket, |bucket, owner| bucket.under(owner))),
            other => Err(wrong(handle, &other, "a bucket")),
        }
    }

    fn counters(&self, handle: u64, under: &[String]) -> Result<Counters, Failure> {
        match self.opened(handle)? {
            Opened::Counters(counters) => Ok(under.iter().fold(counters, |counters, owner| counters.under(owner))),
            other => Err(wrong(handle, &other, "counters")),
        }
    }

    fn once(&self, handle: u64, under: &[String]) -> Result<Bucket<()>, Failure> {
        match self.opened(handle)? {
            Opened::Once(answers) => Ok(under.iter().fold(answers, |answers, owner| answers.under(owner))),
            other => Err(wrong(handle, &other, "once's answers")),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Opened>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn open_bucket(store: &Store, open: KvBucketOpen) -> Result<Opened, Failure> {
    let mut builder = store.bucket::<()>(&open.name);
    if let Some(ttl) = open.ttl {
        builder = builder.ttl(Duration::from_millis(ttl));
    }
    if let Some(idle) = open.idle {
        builder = builder.idle(Duration::from_millis(idle));
    }
    Ok(Opened::Bucket(builder.open()?))
}

fn open_counters(store: &Store, open: KvCountersOpen) -> Result<Opened, Failure> {
    let mut builder = store.counters(&open.name);
    if let Some(ttl) = open.ttl {
        builder = builder.ttl(Duration::from_millis(ttl));
    }
    if let Some(span) = open.flush_every {
        builder = builder.flush_every(Duration::from_millis(span));
    }
    Ok(Opened::Counters(builder.open()?))
}

fn open_rate_limit(store: &Store, open: KvRateLimitOpen) -> Result<Opened, Failure> {
    let mut builder = store.rate_limit(&open.name).rate(open.rate, Duration::from_millis(open.per));
    if let Some(burst) = open.burst {
        builder = builder.burst(burst);
    }
    Ok(Opened::RateLimit(builder.open()?))
}

fn open_quota(store: &Store, open: KvQuotaOpen) -> Result<Opened, Failure> {
    let builder = open.windows.iter().fold(store.quota(&open.name), |quota, window| {
        quota.window(&window.name, window.limit, Duration::from_millis(window.per))
    });
    Ok(Opened::Quota(builder.open()?))
}

fn get(handles: &Handles, call: KvCall) -> Answered {
    let cell = handles.bucket(call.handle, &call.under)?.read_cell(&call.key)?;
    Ok(entry(cell, None).encode())
}

fn has(handles: &Handles, call: KvCall) -> Answered {
    let found = handles.bucket(call.handle, &call.under)?.has(&*call.key)?;
    Ok(KvFound { found }.encode())
}

fn clear(handles: &Handles, branch: KvBranch) -> Answered {
    handles.bucket(branch.handle, &branch.under)?.clear()?;
    Ok(Empty {}.encode())
}

/// A page of the branch's own keys, as many as `limit` asks and the agreed
/// body holds: a page cut by the body starts the next after its last key.
fn list(handles: &Handles, list: KvList, max_body: usize) -> Answered {
    let bucket = handles.bucket(list.handle, &list.under)?;
    let limit = list.limit.map_or(100, |limit| usize::try_from(limit).unwrap_or(usize::MAX));
    let rows = bucket.list_rows(limit, list.after.as_deref())?;
    let full = rows.len() == limit.clamp(1, PAGE_KEYS);
    let mut page = KvPage::default();
    // a page's own fields and the next key's room, kept apart from what entries may take
    let mut room = max_body.saturating_sub(64 + list.after.as_ref().map_or(0, String::len) + MAX_KEY);
    for (key, cell) in rows {
        let entry = entry(Some(cell), Some(key));
        let bytes = entry.encode().len() + 1;
        if bytes > room {
            if page.entries.is_empty() {
                return Err(Failure::from(Error::limit("an entry past the largest body this connection agreed")));
            }
            page.next = page.entries.last().and_then(|entry| entry.key.clone());
            return Ok(page.encode());
        }
        room -= bytes;
        page.entries.push(entry);
    }
    if full {
        page.next = page.entries.last().and_then(|entry| entry.key.clone());
    }
    Ok(page.encode())
}

fn counting(handles: &Handles, called: u16, call: KvCall) -> Answered {
    let counters = handles.counters(call.handle, &call.under)?;
    Ok(match called {
        method::KV_COUNTERS_ADD => KvCount { value: counters.add(&*call.key, call.n.unwrap_or(1))? }.encode(),
        method::KV_COUNTERS_GET => KvCount { value: counters.get(&*call.key)? }.encode(),
        _ => KvFound { found: counters.delete(&*call.key)? }.encode(),
    })
}

/// A rate limit's or a quota's call: n is the requests or uses asked, 1 when
/// absent.
fn limit(handles: &Handles, called: u16, call: KvCall) -> Answered {
    let n = u64::try_from(call.n.unwrap_or(1)).map_err(|_| Failure::invalid("a negative count of requests"))?;
    let key = &*call.key;
    let answered = match handles.opened(call.handle)? {
        Opened::RateLimit(limit) => {
            let limit = call.under.iter().fold(limit, |limit, owner| limit.under(owner));
            match called {
                method::KV_ALLOW => Some(limit.allow_n(key, n)?),
                method::KV_PEEK => Some(limit.peek(key)?),
                method::KV_RESET => limit.reset(key).map(|()| None)?,
                _ => return Err(Failure::invalid("a rate limit gives no refund: its requests are spent")),
            }
        }
        Opened::Quota(quota) => {
            let quota = call.under.iter().fold(quota, |quota, owner| quota.under(owner));
            match called {
                method::KV_ALLOW => Some(quota.allow_n(key, n)?),
                method::KV_PEEK => Some(quota.peek(key)?),
                method::KV_RESET => quota.reset(key).map(|()| None)?,
                _ => quota.refund_n(key, n).map(|()| None)?,
            }
        }
        other => return Err(wrong(call.handle, &other, "a rate limit or a quota")),
    };
    Ok(answered.map_or_else(|| Empty {}.encode(), |allowance| allowance_of(&allowance).encode()))
}

/// A transaction's checks and writes in one batch, all or none; a failed one
/// names its place in `what`.
fn tx(handles: &Handles, tx: KvTx) -> Answered {
    let mut batch = Batch::default();
    for (index, check) in tx.checks.iter().enumerate() {
        add_check(handles, &mut batch, check).map_err(|failure| failure.at(Place::Check(index)))?;
    }
    for (index, op) in tx.writes.iter().enumerate() {
        add_write(handles, &mut batch, op.method, &op.call).map_err(|failure| failure.at(Place::Write(index)))?;
    }
    match batch.commit() {
        Ok(outcomes) => Ok(KvTxResults { outcomes: outcomes.into_iter().map(outcome_of).collect() }.encode()),
        Err((Some(place), error)) => Err(Failure::from(error).at(place)),
        Err((None, error)) => Err(Failure::from(error)),
    }
}

fn add_check(handles: &Handles, batch: &mut Batch, check: &KvCheck) -> Result<(), Failure> {
    let bucket = handles.bucket(check.handle, &check.under)?;
    let version = check.version.as_deref().map(revision_of).transpose()?;
    Ok(batch.check(&bucket, &check.key, version)?)
}

fn add_write(handles: &Handles, batch: &mut Batch, called: u64, call: &KvCall) -> Result<(), Failure> {
    let called = u16::try_from(called).unwrap_or(0);
    if called == method::KV_COUNTERS_ADD {
        let counters = handles.counters(call.handle, &call.under)?;
        return Ok(batch.add(&counters, &call.key, call.n.unwrap_or(1))?);
    }
    let bucket = handles.bucket(call.handle, &call.under)?;
    let options = options(call)?;
    match called {
        method::KV_SET | method::KV_CREATE => {
            let put = if called == method::KV_SET { Put::Always } else { Put::OnlyNew };
            let raw = raw_of(call.value.clone().unwrap_or_default());
            Ok(batch.put(&bucket, &call.key, (raw, options, put))?)
        }
        method::KV_TAKE | method::KV_DELETE => Ok(batch.remove(&bucket, &call.key, options)?),
        method::KV_EXPIRE => Ok(batch.expire(&bucket, &call.key, options)?),
        method::KV_CLEAR => Ok(batch.clear(&bucket.scope)?),
        other => Err(Failure::invalid(format!("method {other:#06x} in a transaction, which writes"))),
    }
}

/// What a call asks of a write: its expiry and the version the key must be at.
fn options(call: &KvCall) -> Result<WriteOptions, Failure> {
    let mut options = WriteOptions::default();
    if let Some(ttl) = call.ttl {
        options = options.ttl(Duration::from_millis(ttl));
    }
    if let Some(at) = call.expires_at {
        options = options.expires_at(from_unix_millis(at));
    }
    if let Some(version) = &call.if_version {
        options = options.if_version(Version::new(revision_of(version)?));
    }
    Ok(options)
}

/// A version travels as the decimal text of its revision, in a bin the client
/// compares only for equality.
fn revision_of(bytes: &[u8]) -> Result<i64, Failure> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| Failure::invalid("a version this server never gave"))
}

fn version_bytes(revision: i64) -> Vec<u8> {
    revision.to_string().into_bytes()
}

fn entry(cell: Option<Cell>, key: Option<String>) -> KvEntry {
    let Some(cell) = cell else {
        return KvEntry::default();
    };
    KvEntry {
        found: true,
        value: Some(row_of(cell.raw)),
        version: Some(version_bytes(cell.version)),
        expires_at: cell.expires,
        key,
    }
}

fn answer(raw: Option<Raw>) -> KvAnswer {
    KvAnswer { found: raw.is_some(), value: raw.map(row_of) }
}

fn written(stamp: Stamp) -> Vec<u8> {
    let written = KvWritten {
        written: stamp.written,
        version: version_bytes(stamp.version.revision()),
        expires_at: stamp.expires,
    };
    written.encode()
}

fn taken(cell: Option<Cell>) -> Vec<u8> {
    KvEntry { found: cell.is_some(), value: cell.map(|cell| row_of(cell.raw)), ..KvEntry::default() }.encode()
}

fn outcome_of(outcome: Outcome) -> KvOutcome {
    KvOutcome {
        found: outcome.found,
        written: outcome.written,
        value: outcome.value.map(row_of),
        version: outcome.version.map(version_bytes),
        expires_at: outcome.expires,
        count: outcome.count,
    }
}

fn allowance_of(allowance: &Allowance) -> KvAllowance {
    let windows = allowance
        .windows
        .iter()
        .map(|window| KvWindowUse {
            name: window.name.clone(),
            used: window.used,
            limit: window.limit,
            left: window.left,
            resets_at: window.resets_at.map(unix_millis),
        })
        .collect();
    KvAllowance { ok: allowance.ok, left: allowance.left, retry_at: allowance.retry_at.map(unix_millis), windows }
}

fn row_of(raw: Raw) -> Row {
    match raw {
        Raw::None => Row::Nil,
        Raw::Int(number) => Row::Int(number),
        Raw::Bytes(bytes) => Row::Bin(bytes),
    }
}

fn raw_of(row: Row) -> Raw {
    match row {
        Row::Nil => Raw::None,
        Row::Int(number) => Raw::Int(number),
        Row::Bin(bytes) => Raw::Bytes(bytes),
    }
}

impl Failure {
    /// The failure of a transaction's check or write, its place named in
    /// `what`, so that a client knows which read changed.
    fn at(self, place: Place) -> Failure {
        match place {
            Place::Check(index) => self.naming("check", index.to_string()),
            Place::Write(index) => self.naming("write", index.to_string()),
        }
    }
}

fn wrong(handle: u64, opened: &Opened, wanted: &str) -> Failure {
    let opened = match opened {
        Opened::Bucket(_) => "a bucket",
        Opened::Counters(_) => "counters",
        Opened::RateLimit(_) => "a rate limit",
        Opened::Quota(_) => "a quota",
        Opened::Once(_) => "once's answers",
    };
    Failure::invalid(format!("handle {handle} is {opened}, not {wanted}"))
}
