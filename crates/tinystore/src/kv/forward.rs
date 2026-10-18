//! A write on a copy of kv.db, passed to the file's home: the handle and the
//! call as bytes, and the same call run there on the same bucket or counters.
//!
//! ```text
//! u8 handle    0 a bucket, 1 counters
//! name, then its expiry: u8 0 none, 1 ttl, 2 idle, and u64 milliseconds
//! owners       the branch, outermost first
//! key
//! u8 call, then its fields
//! ```

use std::time::Duration;

use super::allowance::{Allowance, Window as WindowUse};
use super::bucket::{Bucket, Expires, Expiry, Put, Stamp, Version, WriteOptions};
use super::cells::Cell;
use super::counters::Counters;
use super::engine::Kv;
use super::quota::Quota;
use super::rate_limit::RateLimit;
use super::scope::Kind;
use super::value::Raw;
use crate::clock::from_unix_millis;
use crate::replication::{In, Out};
use crate::{Error, ErrorKind, Result, Store, unix_millis};

/// Opens the engine at the home, which makes its file for a copy waiting
/// for it.
pub(crate) fn make(store: &Store) -> Result<()> {
    super::engine::Kv::of(store).map(drop)
}

/// Opens kv in the file `home` lends, which makes its tables there.
pub(crate) fn make_in(home: &crate::engine::Home) -> Result<()> {
    Kv::at(home).map(drop)
}

/// What kv's file is called where its home passes writes.
pub(crate) use super::engine::FILE;

/// The handle a call is made on.
pub(crate) enum Handle {
    Bucket {
        name: String,
        expiry: Expiry,
        encrypted: bool,
        /// The database whose file keeps the bucket; kv.db when none.
        database: Option<String>,
    },
    Counters {
        name: String,
        ttl: Option<Duration>,
    },
    /// Once's answers, each kept for `keep` milliseconds.
    Once {
        name: String,
        keep: u64,
    },
    /// A rate limit, its rate as the nanoseconds one request takes.
    RateLimit {
        name: String,
        interval: i64,
        burst: i64,
    },
    /// A quota and its windows: their names, limits and spans in milliseconds.
    Quota {
        name: String,
        windows: Vec<(String, u64, i64)>,
    },
    /// A name alone, made of a kind, as a handle opening makes it.
    Name {
        name: String,
        role: String,
        database: Option<String>,
    },
}

pub(crate) enum Call {
    Put {
        raw: Raw,
        put: Put,
        options: WriteOptions,
    },
    Remove(WriteOptions),
    Take(WriteOptions),
    Expire(WriteOptions),
    Clear,
    Add(i64),
    DeleteCounter,
    ClearCounters,
    /// Makes the handle's name, which a handle opening on a copy found missing.
    Open,
    /// An add a copy queued and answered at once, and, as its home reads it,
    /// the node that queued it and the op's number there.
    AddQueued {
        n: i64,
        from: Option<(String, u64)>,
    },
    /// A take of an encrypted bucket for a copy that shares the key `key_id`,
    /// whose value comes back sealed with it.
    TakeSealed {
        options: WriteOptions,
        key_id: String,
    },
    /// Claims a once's key for the node that asked, or answers what is kept.
    OnceClaim,
    /// Keeps what the run a claim handed over answered, none when it failed.
    OnceKeep {
        token: u64,
        answer: Option<Raw>,
    },
    /// Asks a rate limit or a quota for `n` of a key.
    Allow(u64),
    /// Gives `n` uses of a quota's key back.
    Refund(u64),
    /// Forgets a rate limit's or a quota's key.
    Reset,
    /// A put of an encrypted bucket's value, sealed where it was written
    /// with the key `key_id`, which its home shares: no value crosses the
    /// link unsealed.
    PutSealed {
        raw: Raw,
        key_id: String,
        put: Put,
        options: WriteOptions,
    },
}

/// What a call answered.
pub(crate) enum Answer {
    Stamp(Stamp),
    Found(bool),
    Cell(Option<Cell>),
    Sum(i64),
    Allowance(Allowance),
    Claimed(Claimed),
    Done,
}

/// What a claim of a once's key answers a copy.
pub(crate) enum Claimed {
    Kept(Raw),
    /// The run is the copy's, by this token, which its keep names.
    Yours(u64),
}

/// The bytes of a call on `key` of `handle` under `owners`.
pub(crate) fn encode(handle: &Handle, owners: &[String], key: &str, call: &Call) -> Vec<u8> {
    let mut out = Out(vec![crate::replication::KV]);
    handle_out(&mut out, handle);
    out.texts(owners).text(key);
    call_out(&mut out, call);
    out.0
}

fn handle_out(out: &mut Out, handle: &Handle) {
    match handle {
        Handle::Bucket { name, expiry, encrypted, database } => {
            let (kind, span) = match expiry {
                Expiry::Never => (0, Duration::ZERO),
                Expiry::Ttl(span) => (1, *span),
                Expiry::Idle(span) => (2, *span),
            };
            out.byte(0).text(name).byte(kind).span(span).byte(u8::from(*encrypted)).maybe_text(database.as_deref())
        }
        Handle::Counters { name, ttl } => {
            out.byte(1).text(name).maybe_long(ttl.map(|ttl| crate::clock::millis(ttl) as u64))
        }
        Handle::Name { name, role, database } => out.byte(2).text(name).text(role).maybe_text(database.as_deref()),
        Handle::RateLimit { name, interval, burst } => {
            out.byte(3).text(name).long(*interval as u64).long(*burst as u64)
        }
        Handle::Quota { name, windows } => out.byte(4).text(name).list(windows, |out, (window, limit, span)| {
            out.text(window).long(*limit).long(*span as u64);
        }),
        Handle::Once { name, keep } => out.byte(5).text(name).long(*keep),
    };
}

fn call_out(out: &mut Out, call: &Call) {
    match call {
        Call::Put { raw, put, options } => {
            raw_out(out.byte(0), raw).byte(u8::from(*put == Put::OnlyNew));
            options_out(out, options)
        }
        Call::Remove(options) => options_out(out.byte(1), options),
        Call::Take(options) => options_out(out.byte(2), options),
        Call::Expire(options) => options_out(out.byte(3), options),
        Call::Clear => out.byte(4),
        Call::Add(n) => out.byte(5).long(*n as u64),
        Call::DeleteCounter => out.byte(6),
        Call::ClearCounters => out.byte(7),
        Call::Open => out.byte(8),
        Call::PutSealed { raw, key_id, put, options } => {
            raw_out(out.byte(9), raw).text(key_id).byte(u8::from(*put == Put::OnlyNew));
            options_out(out, options)
        }
        Call::Allow(n) => out.byte(10).long(*n),
        Call::Refund(n) => out.byte(11).long(*n),
        Call::Reset => out.byte(12),
        Call::OnceClaim => out.byte(13),
        Call::OnceKeep { token, answer } => match answer {
            Some(raw) => raw_out(out.byte(14).long(*token).byte(1), raw),
            None => out.byte(14).long(*token).byte(0),
        },
        Call::AddQueued { n, from } => match from {
            Some((node, op)) => out.byte(15).long(*n as u64).text(node).long(*op),
            None => out.byte(15).long(*n as u64),
        },
        Call::TakeSealed { options, key_id } => options_out(out.byte(16), options).text(key_id),
    };
}

/// Runs at the home a call another node passed on, and gives its answer's
/// bytes and the file it wrote: kv.db, or the database's that keeps it.
pub(crate) fn run(store: &Store, lenders: crate::replication::Lenders<'_>, bytes: &[u8]) -> Result<(Vec<u8>, String)> {
    let mut at = In { bytes, at: 0 };
    let handle = handle_in(&mut at)?;
    let (owners, key) = (at.texts()?, at.text()?);
    let call = call_in(&mut at)?;
    if !at.done() {
        return Err(corrupt("a call that holds more than its fields"));
    }
    let file = match &handle {
        Handle::Bucket { database: Some(database), .. } | Handle::Name { database: Some(database), .. } => {
            crate::replication::database_file(database)
        }
        _ => FILE.to_owned(),
    };
    Ok((answer_bytes(&call_here(store, lenders, handle, &owners, &key, call)?), file))
}

fn handle_in(at: &mut In<'_>) -> Result<Handle> {
    Ok(match at.byte()? {
        0 => {
            let name = at.text()?;
            let expiry = match (at.byte()?, at.span()?) {
                (0, _) => Expiry::Never,
                (1, span) => Expiry::Ttl(span),
                (2, span) => Expiry::Idle(span),
                _ => return Err(corrupt("a bucket's expiry")),
            };
            Handle::Bucket { name, expiry, encrypted: at.byte()? == 1, database: at.maybe_text()? }
        }
        1 => Handle::Counters { name: at.text()?, ttl: at.maybe_long()?.map(Duration::from_millis) },
        2 => Handle::Name { name: at.text()?, role: at.text()?, database: at.maybe_text()? },
        3 => Handle::RateLimit { name: at.text()?, interval: at.long()? as i64, burst: at.long()? as i64 },
        4 => Handle::Quota {
            name: at.text()?,
            windows: at.list(1 << 10, |at| Ok((at.text()?, at.long()?, at.long()? as i64)))?,
        },
        5 => Handle::Once { name: at.text()?, keep: at.long()? },
        _ => return Err(corrupt("a handle")),
    })
}

fn call_in(at: &mut In<'_>) -> Result<Call> {
    Ok(match at.byte()? {
        0 => Call::Put { raw: raw_in(at)?, put: put_in(at)?, options: options_in(at)? },
        1 => Call::Remove(options_in(at)?),
        2 => Call::Take(options_in(at)?),
        3 => Call::Expire(options_in(at)?),
        4 => Call::Clear,
        5 => Call::Add(at.long()? as i64),
        6 => Call::DeleteCounter,
        7 => Call::ClearCounters,
        8 => Call::Open,
        9 => Call::PutSealed { raw: raw_in(at)?, key_id: at.text()?, put: put_in(at)?, options: options_in(at)? },
        10 => Call::Allow(at.long()?),
        11 => Call::Refund(at.long()?),
        12 => Call::Reset,
        13 => Call::OnceClaim,
        14 => Call::OnceKeep { token: at.long()?, answer: if at.byte()? == 1 { Some(raw_in(at)?) } else { None } },
        15 => Call::AddQueued { n: at.long()? as i64, from: Some((at.text()?, at.long()?)) },
        16 => Call::TakeSealed { options: options_in(at)?, key_id: at.text()? },
        _ => return Err(corrupt("a call")),
    })
}

/// Opens at the home the handle a call was made on, under its owners, and
/// makes the call on it.
fn call_here(
    store: &Store,
    lenders: crate::replication::Lenders<'_>,
    handle: Handle,
    owners: &[String],
    key: &str,
    call: Call,
) -> Result<Answer> {
    match handle {
        Handle::Name { name, role, database } => {
            let kind = Kind::of_role(&role).ok_or_else(|| corrupt("a name's kind"))?;
            super::scope::check_name(&name)?;
            let kv = match database {
                Some(database) => Kv::at(&lenders(&database)?)?,
                None => Kv::of(store)?,
            };
            kv.name(&name, kind)?;
            Ok(Answer::Done)
        }
        Handle::Bucket { name, expiry, encrypted, database } => {
            let builder = match database {
                Some(database) => super::bucket::BucketBuilder::<()>::lent(lenders(&database)?, &name),
                None => store.bucket::<()>(&name),
            };
            let builder = match expiry {
                Expiry::Never => builder,
                Expiry::Ttl(span) => builder.ttl(span),
                Expiry::Idle(span) => builder.idle(span),
            };
            let builder = if encrypted { builder.encrypted() } else { builder };
            on_bucket(&owners.iter().fold(builder.open()?, |bucket, owner| bucket.under(owner)), key, call)
        }
        Handle::Once { name, keep } => {
            let answers = super::once::rows(store, &name, Some(Duration::from_millis(keep)))?;
            on_once(&owners.iter().fold(answers, |answers, owner| answers.under(owner)), key, call)
        }
        Handle::RateLimit { name, interval, burst } => {
            let per = Duration::from_nanos(u64::try_from(interval).map_err(|_| corrupt("a rate"))?);
            let limit = store.rate_limit(&name).rate(1, per).burst(u64::try_from(burst).unwrap_or(1)).open()?;
            on_rate_limit(&owners.iter().fold(limit, |limit, owner| limit.under(owner)), key, call)
        }
        Handle::Quota { name, windows } => {
            let builder = windows.iter().fold(store.quota(&name), |quota, (window, limit, span)| {
                quota.window(window, *limit, Duration::from_millis(u64::try_from(*span).unwrap_or(0)))
            });
            on_quota(&owners.iter().fold(builder.open()?, |quota, owner| quota.under(owner)), key, call)
        }
        Handle::Counters { name, ttl } => {
            let builder = store.counters(&name);
            let builder = match ttl {
                Some(ttl) => builder.ttl(ttl),
                None => builder,
            };
            on_counters(&owners.iter().fold(builder.open()?, |counters, owner| counters.under(owner)), key, call)
        }
    }
}

fn on_bucket(bucket: &Bucket<()>, key: &str, call: Call) -> Result<Answer> {
    Ok(match call {
        Call::Put { raw, put, options } => Answer::Stamp(bucket.put_raw(key, raw, options, put)?),
        Call::PutSealed { raw, key_id, put, options } => {
            Answer::Stamp(bucket.put_sealed(key, raw, &key_id, options, put)?)
        }
        Call::Remove(options) => Answer::Found(bucket.remove(key, options)?),
        Call::Take(options) => Answer::Cell(bucket.take_cell(key, options)?),
        Call::TakeSealed { options, key_id } => Answer::Cell(bucket.take_sealed(key, options, &key_id)?),
        Call::Expire(options) => Answer::Found(bucket.expire_as(key, options)?),
        Call::Clear => {
            bucket.clear()?;
            Answer::Done
        }
        _ => return Err(Error::invalid("a counter's call on a bucket")),
    })
}

fn on_once(answers: &Bucket<()>, key: &str, call: Call) -> Result<Answer> {
    match call {
        Call::OnceClaim => Ok(Answer::Claimed(claim_here(answers, key)?)),
        Call::OnceKeep { token, answer } => {
            let handed = answers.scope.kv.take_passed(token).ok_or_else(|| {
                Error::new(ErrorKind::Lost, "the run's claim went before it answered: another run may have run")
            })?;
            handed.keep(answer)?;
            Ok(Answer::Done)
        }
        _ => Err(Error::invalid("a call once's answers do not take")),
    }
}

fn on_rate_limit(limit: &RateLimit, key: &str, call: Call) -> Result<Answer> {
    Ok(match call {
        Call::Allow(n) => Answer::Allowance(limit.allow_n(key, n)?),
        Call::Reset => {
            limit.reset(key)?;
            Answer::Done
        }
        _ => return Err(Error::invalid("a call a rate limit does not take")),
    })
}

fn on_quota(quota: &Quota, key: &str, call: Call) -> Result<Answer> {
    Ok(match call {
        Call::Allow(n) => Answer::Allowance(quota.allow_n(key, n)?),
        Call::Refund(n) => {
            quota.refund_n(key, n)?;
            Answer::Done
        }
        Call::Reset => {
            quota.reset(key)?;
            Answer::Done
        }
        _ => return Err(Error::invalid("a call a quota does not take")),
    })
}

fn on_counters(counters: &Counters, key: &str, call: Call) -> Result<Answer> {
    Ok(match call {
        Call::Add(n) => Answer::Sum(counters.add(key, n)?),
        // the node that queued it answered its caller already
        Call::AddQueued { n, from: Some((node, op)) } => {
            Answer::Sum(crate::sqlite::own_work(|| counters.add_queued(key, n, &node, op))?)
        }
        Call::DeleteCounter => Answer::Found(counters.delete(key)?),
        Call::ClearCounters => {
            counters.clear()?;
            Answer::Done
        }
        _ => return Err(Error::invalid("a bucket's call on counters")),
    })
}

fn answer_bytes(answer: &Answer) -> Vec<u8> {
    let mut out = Out(Vec::new());
    match answer {
        Answer::Stamp(stamp) => {
            out.byte(u8::from(stamp.written)).long(stamp.version.revision() as u64).maybe_long(expiry(stamp.expires))
        }
        Answer::Found(found) => out.byte(u8::from(*found)),
        Answer::Cell(Some(cell)) => {
            raw_out(out.byte(1), &cell.raw).long(cell.version as u64).maybe_long(expiry(cell.expires))
        }
        Answer::Cell(None) => out.byte(0),
        Answer::Sum(sum) => out.long(*sum as u64),
        Answer::Claimed(Claimed::Kept(raw)) => raw_out(out.byte(0), raw),
        Answer::Claimed(Claimed::Yours(token)) => out.byte(1).long(*token),
        Answer::Allowance(allowance) => {
            out.byte(u8::from(allowance.ok)).long(allowance.left);
            out.maybe_long(expiry(allowance.retry_at.map(unix_millis))).list(&allowance.windows, |out, window| {
                out.text(&window.name).long(window.used).long(window.limit).long(window.left);
                out.maybe_long(expiry(window.resets_at.map(unix_millis)));
            })
        }
        Answer::Done => &mut out,
    };
    out.0
}

/// Reads what the home answered a call, as the call's own answer.
pub(crate) fn stamp(bytes: &[u8]) -> Result<Stamp> {
    let mut at = In { bytes, at: 0 };
    let written = at.byte()? == 1;
    let version = Version::new(at.long()? as i64);
    Ok(Stamp { written, version, expires: expires_in(&mut at)? })
}

pub(crate) fn found(bytes: &[u8]) -> Result<bool> {
    In { bytes, at: 0 }.byte().map(|found| found == 1)
}

pub(crate) fn cell(bytes: &[u8]) -> Result<Option<Cell>> {
    let mut at = In { bytes, at: 0 };
    if at.byte()? == 0 {
        return Ok(None);
    }
    let raw = raw_in(&mut at)?;
    let version = at.long()? as i64;
    Ok(Some(Cell { version, expires: expires_in(&mut at)?, raw }))
}

pub(crate) fn allowance(bytes: &[u8]) -> Result<Allowance> {
    let mut at = In { bytes, at: 0 };
    let ok = at.byte()? == 1;
    let left = at.long()?;
    let retry_at = expires_in(&mut at)?.map(from_unix_millis);
    let windows = at.list(1 << 10, |at| {
        let name = at.text()?;
        let (used, limit, left) = (at.long()?, at.long()?, at.long()?);
        Ok(WindowUse { name, used, limit, left, resets_at: expires_in(at)?.map(from_unix_millis) })
    })?;
    Ok(Allowance { ok, left, retry_at, windows })
}

pub(crate) fn claimed(bytes: &[u8]) -> Result<Claimed> {
    let mut at = In { bytes, at: 0 };
    match at.byte()? {
        0 => Ok(Claimed::Kept(raw_in(&mut at)?)),
        1 => Ok(Claimed::Yours(at.long()?)),
        _ => Err(corrupt("a claim's answer")),
    }
}

/// Claims `key` at the home for the node that asked: the answer kept, or the
/// run handed over and kept by a token, waiting meanwhile for a run under way.
fn claim_here(answers: &Bucket<()>, key: &str) -> Result<Claimed> {
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    loop {
        let (ended, waiting) = std::sync::mpsc::channel::<()>();
        let waiter = Box::new(move || {
            let _ = ended.send(());
        });
        match super::once::hand(answers, key, waiter)? {
            super::once::Hand::Kept(raw) => return Ok(Claimed::Kept(raw)),
            super::once::Hand::Yours(handed) => return Ok(Claimed::Yours(answers.scope.kv.keep_passed(handed))),
            super::once::Hand::Wait => {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() || waiting.recv_timeout(left).is_err() && std::time::Instant::now() >= deadline {
                    return Err(Error::new(ErrorKind::Unavailable, "another run of the key held it too long"));
                }
            }
        }
    }
}

/// What a counter's queued add changes, by which a node's queue finds the
/// adds a copy's read counts: the counters' name, the branch's owners, the key.
pub(crate) fn subject(name: &str, owners: &[String], key: &str) -> Vec<u8> {
    let mut out = Out(Vec::new());
    out.text(name).texts(owners).text(key);
    out.0
}

/// What a counter's queued add adds; none for another request.
pub(crate) fn added(request: &[u8]) -> Option<i64> {
    let mut at = In { bytes: request.get(1..)?, at: 0 };
    if at.byte().ok()? != 1 {
        return None;
    }
    let _name = at.text().ok()?;
    let _ttl = (at.byte().ok()?, at.long().ok()?);
    for _ in 0..at.count().ok()? {
        at.text().ok()?;
    }
    let _key = at.text().ok()?;
    if at.byte().ok()? != 15 {
        return None;
    }
    Some(at.long().ok()? as i64)
}

pub(crate) fn sum(bytes: &[u8]) -> Result<i64> {
    In { bytes, at: 0 }.long().map(|sum| sum as i64)
}

/// Passes a call to the home of the copy `store`'s kv.db is.
pub(crate) fn pass_on(store: &Store, handle: &Handle, owners: &[String], key: &str, call: &Call) -> Result<Vec<u8>> {
    crate::replication::pass_on(store, FILE, encode(handle, owners, key, call))
}

fn raw_out<'a>(out: &'a mut Out, raw: &Raw) -> &'a mut Out {
    match raw {
        Raw::None => out.byte(0),
        Raw::Int(n) => out.byte(1).long(*n as u64),
        Raw::Bytes(bytes) => out.byte(2).bytes(bytes),
    }
}

fn raw_in(at: &mut In<'_>) -> Result<Raw> {
    Ok(match at.byte()? {
        0 => Raw::None,
        1 => Raw::Int(at.long()? as i64),
        2 => Raw::Bytes(at.bytes()?.to_vec()),
        _ => return Err(corrupt("a value")),
    })
}

fn put_in(at: &mut In<'_>) -> Result<Put> {
    Ok(if at.byte()? == 1 { Put::OnlyNew } else { Put::Always })
}

fn options_out<'a>(out: &'a mut Out, options: &WriteOptions) -> &'a mut Out {
    match options.expires {
        None => out.byte(0),
        Some(Expires::In(span)) => out.byte(1).span(span),
        Some(Expires::At(time)) => out.byte(2).long(unix_millis(time) as u64),
    };
    out.maybe_long(options.if_version.map(|version| version.revision() as u64))
}

fn options_in(at: &mut In<'_>) -> Result<WriteOptions> {
    let expires = match at.byte()? {
        0 => None,
        1 => Some(Expires::In(at.span()?)),
        2 => Some(Expires::At(from_unix_millis(at.long()? as i64))),
        _ => return Err(corrupt("an expiry")),
    };
    let if_version = at.maybe_long()?.map(|revision| Version::new(revision as i64));
    Ok(WriteOptions { expires, if_version })
}

/// An expiry in Unix milliseconds as the link carries it.
fn expiry(expires: Option<i64>) -> Option<u64> {
    expires.map(|at| at as u64)
}

fn expires_in(at: &mut In<'_>) -> Result<Option<i64>> {
    Ok(at.maybe_long()?.map(|at| at as i64))
}

fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a kv write passed on: {what}"))
}
