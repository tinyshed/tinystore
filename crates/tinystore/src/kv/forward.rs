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

use super::bucket::{Bucket, Expires, Expiry, Put, Stamp, Version, WriteOptions};
use super::cells::Cell;
use super::counters::Counters;
use super::engine::Kv;
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

/// What kv's file is called where its home passes writes.
pub(crate) const FILE: &str = "kv.db";

/// The handle a call is made on.
pub(crate) enum Handle {
    Bucket {
        name: String,
        expiry: Expiry,
    },
    Counters {
        name: String,
        ttl: Option<Duration>,
    },
    /// A name alone, made of a kind, as a handle opening makes it.
    Name {
        name: String,
        role: String,
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
}

/// What a call answered.
pub(crate) enum Answer {
    Stamp(Stamp),
    Found(bool),
    Cell(Option<Cell>),
    Sum(i64),
    Done,
}

/// The bytes of a call on `key` of `handle` under `owners`.
pub(crate) fn encode(handle: &Handle, owners: &[String], key: &str, call: &Call) -> Vec<u8> {
    let mut out = Out(vec![crate::replication::KV]);
    match handle {
        Handle::Bucket { name, expiry } => {
            out.byte(0);
            out.text(name);
            let (kind, span) = match expiry {
                Expiry::Never => (0, Duration::ZERO),
                Expiry::Ttl(span) => (1, *span),
                Expiry::Idle(span) => (2, *span),
            };
            out.byte(kind);
            out.long(millis(span));
        }
        Handle::Counters { name, ttl } => {
            out.byte(1);
            out.text(name);
            out.byte(u8::from(ttl.is_some()));
            out.long(ttl.map_or(0, millis));
        }
        Handle::Name { name, role } => {
            out.byte(2);
            out.text(name);
            out.text(role);
        }
    }
    out.count(owners.len());
    for owner in owners {
        out.text(owner);
    }
    out.text(key);
    match call {
        Call::Put { raw, put, options } => {
            out.byte(0);
            raw_out(&mut out, raw);
            out.byte(u8::from(*put == Put::OnlyNew));
            options_out(&mut out, options);
        }
        Call::Remove(options) => {
            out.byte(1);
            options_out(&mut out, options);
        }
        Call::Take(options) => {
            out.byte(2);
            options_out(&mut out, options);
        }
        Call::Expire(options) => {
            out.byte(3);
            options_out(&mut out, options);
        }
        Call::Clear => out.byte(4),
        Call::Add(n) => {
            out.byte(5);
            out.long(*n as u64);
        }
        Call::DeleteCounter => out.byte(6),
        Call::ClearCounters => out.byte(7),
        Call::Open => out.byte(8),
    }
    out.0
}

/// Runs at the home a call another node passed on, and gives its answer's
/// bytes.
pub(crate) fn run(store: &Store, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut at = In { bytes, at: 0 };
    let handle = match at.byte()? {
        0 => {
            let name = at.text()?;
            let (kind, span) = (at.byte()?, Duration::from_millis(at.long()?));
            let expiry = match kind {
                0 => Expiry::Never,
                1 => Expiry::Ttl(span),
                2 => Expiry::Idle(span),
                _ => return Err(corrupt("a bucket's expiry")),
            };
            Handle::Bucket { name, expiry }
        }
        1 => {
            let name = at.text()?;
            let ttl = (at.byte()? == 1).then_some(Duration::from_millis(at.long()?));
            Handle::Counters { name, ttl }
        }
        2 => Handle::Name { name: at.text()?, role: at.text()? },
        _ => return Err(corrupt("a handle")),
    };
    let owners: Vec<String> = (0..at.count()?).map(|_| at.text()).collect::<Result<_>>()?;
    let key = at.text()?;
    let call = match at.byte()? {
        0 => {
            let raw = raw_in(&mut at)?;
            let put = if at.byte()? == 1 { Put::OnlyNew } else { Put::Always };
            Call::Put { raw, put, options: options_in(&mut at)? }
        }
        1 => Call::Remove(options_in(&mut at)?),
        2 => Call::Take(options_in(&mut at)?),
        3 => Call::Expire(options_in(&mut at)?),
        4 => Call::Clear,
        5 => Call::Add(at.long()? as i64),
        6 => Call::DeleteCounter,
        7 => Call::ClearCounters,
        8 => Call::Open,
        _ => return Err(corrupt("a call")),
    };
    if !at.done() {
        return Err(corrupt("a call that holds more than its fields"));
    }
    Ok(answer_bytes(&call_here(store, handle, &owners, &key, call)?))
}

fn call_here(store: &Store, handle: Handle, owners: &[String], key: &str, call: Call) -> Result<Answer> {
    match handle {
        Handle::Name { name, role } => {
            let kind = Kind::of_role(&role).ok_or_else(|| corrupt("a name's kind"))?;
            super::scope::check_name(&name)?;
            Kv::of(store)?.name(&name, kind)?;
            Ok(Answer::Done)
        }
        Handle::Bucket { name, expiry } => {
            let builder = store.bucket::<()>(&name);
            let builder = match expiry {
                Expiry::Never => builder,
                Expiry::Ttl(span) => builder.ttl(span),
                Expiry::Idle(span) => builder.idle(span),
            };
            let bucket: Bucket<()> = owners.iter().fold(builder.open()?, |bucket, owner| bucket.under(owner));
            Ok(match call {
                Call::Put { raw, put, options } => Answer::Stamp(bucket.put_raw(key, raw, options, put)?),
                Call::Remove(options) => Answer::Found(bucket.remove(key, options)?),
                Call::Take(options) => Answer::Cell(bucket.take_cell(key, options)?),
                Call::Expire(options) => Answer::Found(bucket.expire_as(key, options)?),
                Call::Clear => {
                    bucket.clear()?;
                    Answer::Done
                }
                _ => return Err(Error::invalid("a counter's call on a bucket")),
            })
        }
        Handle::Counters { name, ttl } => {
            let builder = store.counters(&name);
            let builder = match ttl {
                Some(ttl) => builder.ttl(ttl),
                None => builder,
            };
            let counters: Counters = owners.iter().fold(builder.open()?, |counters, owner| counters.under(owner));
            Ok(match call {
                Call::Add(n) => Answer::Sum(counters.add(key, n)?),
                Call::DeleteCounter => Answer::Found(counters.delete(key)?),
                Call::ClearCounters => {
                    counters.clear()?;
                    Answer::Done
                }
                _ => return Err(Error::invalid("a bucket's call on counters")),
            })
        }
    }
}

fn answer_bytes(answer: &Answer) -> Vec<u8> {
    let mut out = Out(Vec::new());
    match answer {
        Answer::Stamp(stamp) => {
            out.byte(u8::from(stamp.written));
            out.long(stamp.version.revision() as u64);
            expires_out(&mut out, stamp.expires);
        }
        Answer::Found(found) => out.byte(u8::from(*found)),
        Answer::Cell(cell) => match cell {
            Some(cell) => {
                out.byte(1);
                raw_out(&mut out, &cell.raw);
                out.long(cell.version as u64);
                expires_out(&mut out, cell.expires);
            }
            None => out.byte(0),
        },
        Answer::Sum(sum) => out.long(*sum as u64),
        Answer::Done => {}
    }
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

pub(crate) fn sum(bytes: &[u8]) -> Result<i64> {
    In { bytes, at: 0 }.long().map(|sum| sum as i64)
}

/// Passes a call to the home of the copy `store`'s kv.db is.
pub(crate) fn pass_on(store: &Store, handle: &Handle, owners: &[String], key: &str, call: &Call) -> Result<Vec<u8>> {
    crate::replication::pass_on(store, FILE, encode(handle, owners, key, call))
}

/// Why a handle of a copy cannot pass a write on, if it cannot: an
/// encrypted bucket's values would cross the link unsealed, and a bucket in
/// a database's file is passed on with the database's writes, not yet built.
pub(crate) fn refuse(encrypted: bool, lent_by: Option<&str>) -> Result<()> {
    if encrypted {
        return Err(Error::new(
            ErrorKind::Unavailable,
            "a copy passes no write of an encrypted bucket to its home: its values would cross the link unsealed",
        ));
    }
    if let Some(owner) = lent_by {
        return Err(Error::new(
            ErrorKind::Unavailable,
            format!("a copy passes no write of a bucket in {owner}'s file to its home yet"),
        ));
    }
    Ok(())
}

fn raw_out(out: &mut Out, raw: &Raw) {
    match raw {
        Raw::None => out.byte(0),
        Raw::Int(n) => {
            out.byte(1);
            out.long(*n as u64);
        }
        Raw::Bytes(bytes) => {
            out.byte(2);
            out.bytes(bytes);
        }
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

fn options_out(out: &mut Out, options: &WriteOptions) {
    match options.expires {
        None => out.byte(0),
        Some(Expires::In(span)) => {
            out.byte(1);
            out.long(millis(span));
        }
        Some(Expires::At(time)) => {
            out.byte(2);
            out.long(unix_millis(time) as u64);
        }
    }
    match options.if_version {
        None => out.byte(0),
        Some(version) => {
            out.byte(1);
            out.long(version.revision() as u64);
        }
    }
}

fn options_in(at: &mut In<'_>) -> Result<WriteOptions> {
    let expires = match at.byte()? {
        0 => None,
        1 => Some(Expires::In(Duration::from_millis(at.long()?))),
        2 => Some(Expires::At(from_unix_millis(at.long()? as i64))),
        _ => return Err(corrupt("an expiry")),
    };
    let if_version = match at.byte()? {
        0 => None,
        1 => Some(Version::new(at.long()? as i64)),
        _ => return Err(corrupt("a version")),
    };
    Ok(WriteOptions { expires, if_version })
}

fn expires_out(out: &mut Out, expires: Option<i64>) {
    out.byte(u8::from(expires.is_some()));
    out.long(expires.unwrap_or(0) as u64);
}

fn expires_in(at: &mut In<'_>) -> Result<Option<i64>> {
    let some = at.byte()? == 1;
    let at_time = at.long()? as i64;
    Ok(some.then_some(at_time))
}

fn millis(span: Duration) -> u64 {
    u64::try_from(span.as_millis()).unwrap_or(u64::MAX)
}

fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a kv write passed on: {what}"))
}
