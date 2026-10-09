//! kv's methods of docs/wire.md: a handle on a bucket, and the calls on it.
//! A handle holds rows' values, nil, an integer or bin, so the server reads
//! what any bucket wrote.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use super::message::{Answer, Failure, Fields};
use super::msgpack::Value;
use super::session::int;
use crate::Store;
use crate::kv::{Bucket, Cell, Put, Raw, Stamp, Version, WriteOptions};

const OPEN: u16 = 0x0101;
const GET: u16 = 0x0102;
const HAS: u16 = 0x0103;
const SET: u16 = 0x0104;
const DELETE: u16 = 0x0105;
const TAKE: u16 = 0x0106;
const TOUCH: u16 = 0x0107;
const CLEAR: u16 = 0x010a;

/// A bucket's fields, and those of kinds the Rust core does not serve yet.
const BUCKET_FIELDS: &[u64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
const NOT_YET: &[(u64, &str)] =
    &[(2, "counters"), (5, "lose at most"), (6, "config"), (7, "rate"), (10, "once"), (11, "windows"), (12, "in")];
const CALL_FIELDS: &[u64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// The buckets one connection opened, by handle.
#[derive(Debug, Default)]
pub(crate) struct Handles {
    open: Mutex<HashMap<u64, Bucket<()>>>,
    last: AtomicU64,
}

type Answered = Result<Vec<u8>, Failure>;

/// How a session runs a method: a point read at once on the caller's thread,
/// a write queued for its group commit with no thread waiting on it, anything
/// else on a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Inline,
    Submit,
    Worker,
}

pub(crate) fn route(method: u16) -> Route {
    match method {
        GET | HAS => Route::Inline,
        SET | DELETE | TAKE => Route::Submit,
        _ => Route::Worker,
    }
}

/// Queues a write and returns; `done` gets its answer on the thread that
/// commits it.
pub(crate) fn submit(handles: &Handles, method: u16, body: &[u8], done: impl FnOnce(Answered) + Send + 'static) {
    let prepared = || -> Result<_, Failure> {
        let fields = Fields::decode(body, CALL_FIELDS)?;
        let bucket = handles.bucket(&fields)?;
        let (key, write) = (key(&fields)?, write(&fields)?);
        Ok((fields, bucket, key, write))
    };
    let (fields, bucket, key, write) = match prepared() {
        Ok(prepared) => prepared,
        Err(failure) => return done(Err(failure)),
    };
    match method {
        SET => match set_args(&fields, write) {
            Ok(args) => bucket.put_raw_then(&key, args, move |stamp| done(stamp.map(stamped).map_err(Failure::from))),
            Err(failure) => done(Err(failure)),
        },
        DELETE => bucket.remove_then(&key, write, move |cell| {
            done(cell.map(|_| Answer::default().encode()).map_err(Failure::from));
        }),
        TAKE => bucket.remove_then(&key, write, move |cell| done(cell.map(taken).map_err(Failure::from))),
        other => done(Err(Failure::unimplemented(format!("method {other:#06x}")))),
    }
}

pub(crate) fn call(store: &Store, handles: &Handles, method: u16, body: &[u8]) -> Answered {
    if method == OPEN {
        return open(store, handles, body);
    }
    let fields = Fields::decode(body, CALL_FIELDS)?;
    let bucket = handles.bucket(&fields)?;
    match method {
        GET | HAS => read(&bucket, &fields, method == HAS),
        SET => set(&bucket, &fields),
        DELETE => delete(&bucket, &fields),
        TAKE => take(&bucket, &fields),
        TOUCH => touch(&bucket, &fields),
        CLEAR => bucket.clear().map(|()| Answer::default().encode()).map_err(Failure::from),
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

fn open(store: &Store, handles: &Handles, body: &[u8]) -> Answered {
    let fields = Fields::decode(body, BUCKET_FIELDS)?;
    if let Some((_, name)) = NOT_YET.iter().find(|(number, _)| fields.value(*number).is_some()) {
        return Err(Failure::unimplemented(format!("a bucket's {name}")));
    }
    let name = fields.str(1, "name")?.ok_or_else(|| Failure::invalid("a bucket without a name"))?;
    let mut builder = store.bucket::<()>(name);
    if let Some(ttl) = fields.uint(3, "default ttl")? {
        builder = builder.ttl(Duration::from_millis(ttl));
    }
    if let Some(idle) = fields.uint(4, "sliding")? {
        builder = builder.idle(Duration::from_millis(idle));
    }
    let bucket = builder.open()?;
    let handle = handles.last.fetch_add(1, Ordering::SeqCst) + 1;
    handles.lock().insert(handle, bucket);
    Ok(Answer::default().put(1, Value::Uint(handle)).encode())
}

/// A get answers the entry, a has whether it is there; either fails
/// `conflict` when the call's version or `if absent` no longer holds.
fn read(bucket: &Bucket<()>, fields: &Fields, only_found: bool) -> Answered {
    let key = key(fields)?;
    let cell = bucket.read_cell(&key)?;
    let found = cell.as_ref().map(|cell| cell.version);
    if let Some(version) = version(fields)?
        && found != Some(version.revision())
    {
        return Err(conflict(&key, "is not at the version given"));
    }
    if fields.bool(8, "if absent")? && cell.is_some() {
        return Err(conflict(&key, "holds a value"));
    }
    if only_found {
        return Ok(Answer::default().put(1, Value::Bool(cell.is_some())).encode());
    }
    Ok(entry(cell.as_ref()).encode())
}

fn set(bucket: &Bucket<()>, fields: &Fields) -> Answered {
    let key = key(fields)?;
    let (raw, write, put) = set_args(fields, write(fields)?)?;
    Ok(stamped(bucket.put_raw(&key, raw, write, put)?))
}

/// What a set writes, and whether only a key that is not there.
fn set_args(fields: &Fields, write: WriteOptions) -> Result<(Raw, WriteOptions, Put), Failure> {
    let raw = raw_of(fields.value(4))?;
    let put = if fields.bool(8, "if absent")? { Put::OnlyNew } else { Put::Always };
    Ok((raw, write, put))
}

fn stamped(stamp: Stamp) -> Vec<u8> {
    let answer = Answer::default()
        .put(1, Value::Bool(stamp.written))
        .put(3, version_value(stamp.version))
        .put(4, stamp.expires.map_or(Value::Nil, int));
    answer.encode()
}

fn taken(cell: Option<Cell>) -> Vec<u8> {
    let found = Value::Bool(cell.is_some());
    Answer::default().put(1, found).put(2, cell.map_or(Value::Nil, |cell| value_of(cell.raw))).encode()
}

fn delete(bucket: &Bucket<()>, fields: &Fields) -> Answered {
    bucket.remove(&key(fields)?, write(fields)?)?;
    Ok(Answer::default().encode())
}

fn take(bucket: &Bucket<()>, fields: &Fields) -> Answered {
    Ok(taken(bucket.remove(&key(fields)?, write(fields)?)?))
}

fn touch(bucket: &Bucket<()>, fields: &Fields) -> Answered {
    let found = bucket.expire_as(&key(fields)?, write(fields)?)?;
    Ok(Answer::default().put(1, Value::Bool(found)).encode())
}

impl Handles {
    /// The bucket a call's handle names, seen from the branch its owners name.
    fn bucket(&self, fields: &Fields) -> Result<Bucket<()>, Failure> {
        let handle = fields.uint(1, "handle")?.ok_or_else(|| Failure::invalid("a call without a handle"))?;
        let mut bucket = self
            .lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Failure::invalid(format!("handle {handle} names no bucket this connection opened")))?;
        for owner in fields.array(2, "owners")? {
            bucket = bucket.under(text_of(owner, "an owner")?);
        }
        Ok(bucket)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Bucket<()>>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn key(fields: &Fields) -> Result<String, Failure> {
    let key = fields.value(3).ok_or_else(|| Failure::invalid("a call without a key"))?;
    text_of(key, "a key")
}

/// A key's text: a str, a bin that is UTF-8, or an integer's decimal spelling.
fn text_of(value: &Value, what: &str) -> Result<String, Failure> {
    match value {
        Value::Str(text) => Ok(text.clone()),
        Value::Uint(number) => Ok(number.to_string()),
        Value::Int(number) => Ok(number.to_string()),
        Value::Bin(bytes) => String::from_utf8(bytes.clone())
            .map_err(|_| Failure::unimplemented(format!("{what} of bytes that are not UTF-8"))),
        _ => Err(Failure::invalid(format!("{what} is a str, a bin or an integer"))),
    }
}

/// What a call asks of a write: its expiry and the version the key must be at.
fn write(fields: &Fields) -> Result<WriteOptions, Failure> {
    let mut write = WriteOptions::default();
    if let Some(ttl) = fields.uint(5, "ttl")? {
        write = write.ttl(Duration::from_millis(ttl));
    }
    if let Some(at) = fields.int(6, "expire at")? {
        write = write.expires_at(time_of(at));
    }
    if let Some(version) = version(fields)? {
        write = write.if_version(version);
    }
    Ok(write)
}

/// A version travels as the decimal text of its revision, in a bin the client
/// compares only for equality.
fn version(fields: &Fields) -> Result<Option<Version>, Failure> {
    let Some(bytes) = fields.bin(7, "if version")? else {
        return Ok(None);
    };
    let revision = std::str::from_utf8(bytes).ok().and_then(|text| text.parse().ok());
    revision
        .map(|revision| Some(Version::new(revision)))
        .ok_or_else(|| Failure::invalid("a version this server never gave"))
}

fn version_value(version: Version) -> Value {
    Value::Bin(version.revision().to_string().into_bytes())
}

fn entry(cell: Option<&Cell>) -> Answer {
    let Some(cell) = cell else {
        return Answer::default();
    };
    Answer::default()
        .put(1, Value::Bool(true))
        .put(2, value_of(cell.raw.clone()))
        .put(3, version_value(Version::new(cell.version)))
        .put(4, cell.expires.map_or(Value::Nil, int))
}

fn value_of(raw: Raw) -> Value {
    match raw {
        Raw::None => Value::Nil,
        Raw::Int(number) => int(number),
        Raw::Bytes(bytes) => Value::Bin(bytes),
    }
}

/// A kv value as its row keeps it: nil, an integer or bin.
fn raw_of(value: Option<&Value>) -> Result<Raw, Failure> {
    match value {
        None | Some(Value::Nil) => Ok(Raw::None),
        Some(Value::Uint(number)) => {
            i64::try_from(*number).map(Raw::Int).map_err(|_| Failure::invalid("a value past an int 64"))
        }
        Some(Value::Int(number)) => Ok(Raw::Int(*number)),
        Some(Value::Bin(bytes)) => Ok(Raw::Bytes(bytes.clone())),
        Some(_) => Err(Failure::invalid("a kv value is nil, an integer or bin")),
    }
}

fn conflict(key: &str, why: &str) -> Failure {
    let what = vec![("key".to_owned(), Value::Str(key.to_owned()))];
    Failure { code: "conflict", message: format!("key {key:?} {why}"), what }
}

fn time_of(millis: i64) -> std::time::SystemTime {
    match u64::try_from(millis) {
        Ok(after) => UNIX_EPOCH + Duration::from_millis(after),
        Err(_) => UNIX_EPOCH - Duration::from_millis(millis.unsigned_abs()),
    }
}
