//! `tinystore kv`: a bucket's value by its key, and a bucket's keys, read
//! beside the program or server that holds the directory.
//!
//! ```text
//! tinystore kv ./data get sessions 42          {"user": 42, "seen": "2026-10-18"}
//! tinystore kv ./data list sessions            every key of the bucket's root, with its value
//! tinystore kv ./data list sessions 42         the keys under the branch 42
//! tinystore kv ./data list drafts --database app   a bucket kept in sql/app.db
//! ```
//!
//! A bucket keeps a value of any type its program chose: an integer as one,
//! text as its bytes, anything compound as JSON. Nothing says which, so a
//! value shows as what its bytes read as: JSON, then text, then hex.

use std::fmt;
use std::process::ExitCode;
use std::time::SystemTime;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use tinystore::Store;
use tinystore::kv::{Bucket, BucketBuilder};
use tinystore::sql::Database;

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;

/// The keys `list` prints at most.
const MOST: usize = 1000;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::KV, args, kv)
}

fn kv(asked: Asked) -> Done {
    let at = asked.words.iter().position(|word| word == "get" || word == "list");
    let (dir, verb, rest) = match (at, &asked.dir) {
        (Some(1), Some(_)) => return Err(wrong("a directory given twice: --dir, and a word before the verb")),
        (Some(at @ (0 | 1)), dir) => {
            let dir = dir.clone().or_else(|| asked.words.get(..at).and_then(|words| words.first()).map(Into::into));
            (dir.unwrap_or_else(|| ".".into()), asked.words[at].as_str(), &asked.words[at + 1..])
        }
        _ => return Err(wrong("get <bucket> <key> or list <bucket> [branch]")),
    };
    let store = Store::open_beside(&dir).map_err(failed)?;
    let at = Kept::at(&store, asked.value("--database"))?;
    let json = asked.has("--json");
    match (verb, rest) {
        ("get", [bucket, key]) => get(&at, bucket, key, json),
        ("get", _) => Err(wrong("get takes a bucket and a key: get sessions 42")),
        ("list", [bucket, branch @ ..]) if branch.len() <= 1 => list(&at, bucket, branch.first(), json),
        _ => Err(wrong("list takes a bucket and a branch at most: list sessions 42")),
    }
}

fn get(at: &Kept, bucket: &str, key: &str, json: bool) -> Done {
    let found = read(at, bucket, |bucket| {
        let Some(entry) = bucket.entry(key)? else { return Ok(Vec::new()) };
        Ok(vec![(entry.key, entry.value, entry.expires_at)])
    })?;
    let Some((_, value, expires)) = found.into_iter().next() else {
        return Err(failed(format!("kv bucket {bucket}: no key {key}")));
    };
    if json {
        println!("{}", shown(key, &value, expires));
    } else {
        println!("{value:#}");
    }
    Ok(())
}

fn list(at: &Kept, bucket: &str, branch: Option<&String>, json: bool) -> Done {
    let entries = read(at, bucket, |opened| {
        let opened = under(opened, branch.map(String::as_str));
        let mut entries = Vec::new();
        for entry in opened.all().take(MOST + 1) {
            let entry = entry?;
            entries.push((entry.key, entry.value, entry.expires_at));
        }
        Ok(entries)
    })?;
    if entries.is_empty() {
        let under = branch.map_or_else(String::new, |branch| format!(" under {branch}"));
        println!("kv bucket {bucket} holds no key{under}");
        return Ok(());
    }
    let width = entries.iter().take(MOST).map(|(key, ..)| key.chars().count()).max().unwrap_or(0).min(40);
    for (key, value, expires) in entries.iter().take(MOST) {
        if json {
            println!("{}", shown(key, value, *expires));
        } else {
            println!("{key:width$}  {value}");
        }
    }
    if entries.len() > MOST {
        eprintln!("the first {MOST} keys; a branch narrows them: list {bucket} <branch>");
    }
    Ok(())
}

/// The bucket and the key or branch an MCP tool reads.
pub(crate) struct Reading<'a> {
    pub(crate) bucket: &'a str,
    pub(crate) database: Option<&'a str>,
    pub(crate) key: Option<&'a str>,
    pub(crate) branch: Option<&'a str>,
}

/// A bucket's entries as JSON, for the MCP's tools: the one of `key`, or
/// those of `branch`, `most` at most.
pub(crate) fn entries(store: &Store, reading: &Reading, most: usize) -> Result<Vec<serde_json::Value>, String> {
    let Reading { bucket, database, key, branch } = *reading;
    let at = Kept::at(store, database).map_err(|refused| refused.to_string())?;
    let found = read(&at, bucket, |opened| match key {
        Some(key) => {
            Ok(opened.entry(key)?.map(|entry| (entry.key, entry.value, entry.expires_at)).into_iter().collect())
        }
        None => {
            let branch = under(opened, branch);
            branch.all().take(most).map(|entry| entry.map(|entry| (entry.key, entry.value, entry.expires_at))).collect()
        }
    })
    .map_err(|refused| refused.to_string())?;
    Ok(found.iter().map(|(key, value, expires)| shown(key, value, *expires)).collect())
}

fn shown(key: &str, value: &Stored, expires: Option<SystemTime>) -> serde_json::Value {
    serde_json::json!({ "key": key, "value": value.json(), "expires": millis(expires) })
}

/// A key, its value and when it expires.
type Entry = (String, Stored, Option<SystemTime>);

/// Where a bucket is kept: kv.db, or the database a program opened it from.
enum Kept<'a> {
    Store(&'a Store),
    Database(Database),
}

impl<'a> Kept<'a> {
    fn at(store: &'a Store, database: Option<&str>) -> Result<Kept<'a>, asked::Refused> {
        match database {
            Some(database) => Ok(Kept::Database(crate::sql::opened(store, database)?)),
            None if !store.dir().join("kv.db").exists() => {
                Err(failed(crate::files::none_kept(store.dir(), "kv.db", "bucket")))
            }
            None => Ok(Kept::Store(store)),
        }
    }

    fn bucket(&self, name: &str) -> BucketBuilder<Stored> {
        match self {
            Kept::Store(store) => store.bucket(name),
            Kept::Database(database) => database.bucket(name),
        }
    }
}

/// The bucket under `branch`, its owners written as `42/devices`.
fn under(opened: &Bucket<Stored>, branch: Option<&str>) -> Bucket<Stored> {
    branch.map_or_else(|| opened.clone(), |branch| branch.split('/').fold(opened.clone(), |at, owner| at.under(owner)))
}

/// Reads the bucket's entries as `Stored` values; an encrypted bucket with
/// the store's key.
fn read(
    at: &Kept,
    name: &str,
    read: impl Fn(&Bucket<Stored>) -> tinystore::Result<Vec<Entry>>,
) -> Result<Vec<Entry>, asked::Refused> {
    let encrypted = match at.bucket(name).open() {
        Err(error) if error.to_string().contains("encrypted") => true,
        Err(error) => return Err(failed(error)),
        Ok(_) => false,
    };
    let builder = at.bucket(name);
    let stored = if encrypted { builder.encrypted().open() } else { builder.open() };
    Ok(read(&stored.map_err(failed)?)?)
}

fn millis(at: Option<SystemTime>) -> Option<u64> {
    at.and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|since| since.as_millis() as u64)
}

/// A value as a bucket's row keeps it, whatever type its program chose: the
/// row says by its first byte which kind wrote it, so each reads back as what
/// it is.
#[derive(Debug, PartialEq)]
enum Stored {
    Nothing,
    Integer(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Json(serde_json::Value),
    /// A row this command cannot read.
    Unread,
}

impl Stored {
    /// The value as JSON: JSON as it is, text as a string, bytes as their hex.
    fn json(&self) -> serde_json::Value {
        match self {
            Stored::Nothing | Stored::Unread => serde_json::Value::Null,
            Stored::Integer(integer) => (*integer).into(),
            Stored::Float(float) => serde_json::Number::from_f64(*float).map_or(serde_json::Value::Null, Into::into),
            Stored::Text(text) => text.as_str().into(),
            Stored::Bytes(bytes) => hex(bytes).into(),
            Stored::Json(json) => json.clone(),
        }
    }
}

impl fmt::Display for Stored {
    /// JSON on one line, or spread over lines with `{:#}`; text as it is;
    /// bytes as hex with their count.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stored::Nothing => f.write_str("(no value)"),
            Stored::Unread => f.write_str("(a value this command cannot read)"),
            Stored::Integer(integer) => write!(f, "{integer}"),
            Stored::Float(float) => write!(f, "{float}"),
            Stored::Text(text) => f.write_str(text),
            Stored::Bytes(bytes) => write!(f, "{} bytes: {}", bytes.len(), hex(bytes)),
            Stored::Json(json) => {
                let written =
                    if f.alternate() { serde_json::to_string_pretty(json) } else { serde_json::to_string(json) };
                f.write_str(&written.map_err(|_| fmt::Error)?)
            }
        }
    }
}

use crate::local::hex;

impl Serialize for Stored {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Stored::Nothing | Stored::Unread => serializer.serialize_unit(),
            Stored::Integer(integer) => serializer.serialize_i64(*integer),
            Stored::Float(float) => serializer.serialize_f64(*float),
            Stored::Text(text) => serializer.serialize_str(text),
            Stored::Bytes(bytes) => serializer.serialize_bytes(bytes),
            Stored::Json(json) => json.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Stored {
    /// A row as its kind wrote it; one this command cannot read is `Unread`.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(deserializer.deserialize_any(Rows).unwrap_or(Stored::Unread))
    }
}

/// Reads a row as the kind its first byte names.
struct Rows;

impl<'de> Visitor<'de> for Rows {
    type Value = Stored;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a bucket's row")
    }

    fn visit_none<E: de::Error>(self) -> Result<Stored, E> {
        Ok(Stored::Nothing)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Stored, E> {
        Ok(Stored::Nothing)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Stored, E> {
        Ok(Stored::Json(value.into()))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Stored, E> {
        Ok(Stored::Integer(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Stored, E> {
        Ok(Stored::Json(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Stored, E> {
        Ok(Stored::Float(value))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Stored, E> {
        Ok(Stored::Text(value.to_owned()))
    }

    fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Stored, E> {
        Ok(Stored::Bytes(bytes.to_vec()))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Stored, A::Error> {
        serde_json::Value::deserialize(de::value::SeqAccessDeserializer::new(seq)).map(Stored::Json)
    }

    fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<Stored, A::Error> {
        serde_json::Value::deserialize(de::value::MapAccessDeserializer::new(map)).map(Stored::Json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_a_program_kept_reads_back_as_what_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), tinystore::Options { background: false, ..Default::default() }).unwrap();
        store.bucket::<String>("names").open().unwrap().set("a", &"ann".to_owned()).unwrap();
        store.bucket::<serde_json::Value>("docs").open().unwrap().set("a", &serde_json::json!({"n": 1})).unwrap();
        let raw = tinystore::kv::Bytes(vec![0xff, 0x00]);
        store.bucket::<tinystore::kv::Bytes>("raw").open().unwrap().set("a", &raw).unwrap();
        store.bucket::<f64>("floats").open().unwrap().set("a", &1.5).unwrap();
        store.bucket::<i64>("counts").open().unwrap().set("a", &7).unwrap();
        let one = |name: &str| {
            let read = read(&Kept::Store(&store), name, |bucket| {
                let entry = bucket.entry("a")?.unwrap();
                Ok(vec![(entry.key, entry.value, entry.expires_at)])
            });
            read.unwrap().remove(0).1
        };
        assert_eq!(one("names"), Stored::Text("ann".to_owned()));
        assert_eq!(one("docs"), Stored::Json(serde_json::json!({"n": 1})));
        assert_eq!(one("raw"), Stored::Bytes(vec![0xff, 0x00]));
        assert_eq!(one("floats"), Stored::Float(1.5));
        assert_eq!(one("counts"), Stored::Integer(7));
        assert_eq!(one("raw").to_string(), "2 bytes: ff00");
    }

    #[test]
    fn a_bucket_kept_in_a_database_is_read_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), tinystore::Options { background: false, ..Default::default() }).unwrap();
        let db = store.database("app").open().unwrap();
        db.bucket::<String>("drafts").open().unwrap().set("a", &"kept in app.db".to_owned()).unwrap();
        let beside = Store::open_beside(dir.path()).unwrap();
        let why = Kept::at(&beside, None).err().unwrap().to_string();
        assert!(why.contains("holds no kv.db") && why.ends_with("one of app"), "{why}");
        let at = Kept::at(&beside, Some("app")).unwrap();
        let found = read(&at, "drafts", |bucket| {
            Ok(bucket
                .all()
                .collect::<tinystore::Result<Vec<_>>>()?
                .into_iter()
                .map(|entry| (entry.key, entry.value, entry.expires_at))
                .collect())
        });
        assert_eq!(found.unwrap()[0].1, Stored::Text("kept in app.db".to_owned()));
    }
}
