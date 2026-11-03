//! The tools `kv_get` and `kv_list`, and a bucket's entries as `tinystore kv
//! get` and `kv list` read them too: a value of whatever type its program
//! kept, read back as what it is.
//!
//! A bucket keeps a value of any type its program chose: an integer as one,
//! text as its bytes, anything compound as JSON. The row says by its first
//! byte which kind wrote it, so each reads back as that kind.

use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

use super::given::none_kept;
use super::store::{Arguments, MOST, Opener, count, opened, schema, text, with_database};
use super::tool::Tool;
use crate::Store;
use crate::kv::{Bucket, BucketBuilder};
use crate::text::hex;

pub(super) fn tools(open: &Opener) -> Vec<Tool> {
    let (one, all) = (Arc::clone(open), Arc::clone(open));
    let get = with_database(json!({ "bucket": text("the bucket"), "key": text("the key") }));
    let list = with_database(json!({
        "bucket": text("the bucket"),
        "branch": text("the keys under this branch, as 42 or 42/devices"),
        "limit": count("200 at most"),
    }));
    vec![
        Tool::reads(
            "kv_get",
            "One value of a kv bucket by its key.",
            schema(get, &["bucket", "key"]),
            move |given, _| Ok(value(&one, &Arguments { tool: "kv_get", given })?),
        ),
        Tool::reads(
            "kv_list",
            "A kv bucket's keys and values, in key order.",
            schema(list, &["bucket"]),
            move |given, _| Ok(values(&all, &Arguments { tool: "kv_list", given })?),
        ),
    ]
}

fn value(open: &Opener, given: &Arguments) -> Result<Value, String> {
    let (bucket, key) = (given.needed("bucket")?, given.needed("key")?);
    let reading = Reading { bucket, database: given.text("database"), key: Some(key), branch: None };
    let found = entries(&opened(open)?, &reading, 1)?;
    found.first().map(Entry::json).ok_or_else(|| format!("kv bucket {bucket}: no key {key}"))
}

fn values(open: &Opener, given: &Arguments) -> Result<Value, String> {
    let reading = Reading {
        bucket: given.needed("bucket")?,
        database: given.text("database"),
        key: None,
        branch: given.text("branch"),
    };
    let found = entries(&opened(open)?, &reading, given.limit(MOST))?;
    Ok(Value::Array(found.iter().map(Entry::json).collect()))
}

/// The bucket read, and the key or the branch of it.
#[derive(Debug)]
pub struct Reading<'a> {
    pub bucket: &'a str,
    /// The database a program opened the bucket from; kv.db when none.
    pub database: Option<&'a str>,
    pub key: Option<&'a str>,
    /// Its owners written as `42/devices`.
    pub branch: Option<&'a str>,
}

/// A key, its value and when it expires.
#[derive(Debug)]
pub struct Entry {
    pub key: String,
    pub value: Stored,
    pub expires: Option<SystemTime>,
}

impl Entry {
    fn of(entry: crate::kv::Entry<Stored>) -> Entry {
        Entry { key: entry.key, value: entry.value, expires: entry.expires_at }
    }

    /// The entry as JSON: its expiry in milliseconds since 1970.
    pub fn json(&self) -> Value {
        let expires = self.expires.and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok());
        json!({ "key": self.key, "value": self.value.json(), "expires": expires.map(|since| since.as_millis() as u64) })
    }
}

/// A bucket's entries: the one of `key`, or those under `branch`, `most` at
/// most, in key order. An encrypted bucket opens with the store's key.
pub fn entries(store: &Store, reading: &Reading, most: usize) -> Result<Vec<Entry>, String> {
    let at = Kept::at(store, reading.database)?;
    let encrypted = match at.bucket(reading.bucket).open() {
        Err(error) if error.to_string().contains("encrypted") => true,
        Err(error) => return Err(error.to_string()),
        Ok(_) => false,
    };
    let builder = at.bucket(reading.bucket);
    let opened = if encrypted { builder.encrypted().open() } else { builder.open() };
    let opened = opened.map_err(|error| error.to_string())?;
    let found: crate::Result<Vec<Entry>> = match reading.key {
        Some(key) => opened.entry(key).map(|entry| entry.into_iter().map(Entry::of).collect()),
        None => under(&opened, reading.branch).all().take(most).map(|entry| entry.map(Entry::of)).collect(),
    };
    found.map_err(|error| error.to_string())
}

/// Where a bucket is kept: kv.db, or the database a program opened it from.
enum Kept<'a> {
    Store(&'a Store),
    #[cfg(feature = "sql")]
    Database(crate::sql::Database),
}

impl<'a> Kept<'a> {
    fn at(store: &'a Store, database: Option<&str>) -> Result<Kept<'a>, String> {
        match database {
            #[cfg(feature = "sql")]
            Some(database) => Ok(Kept::Database(super::sql::opened(store, database)?)),
            #[cfg(not(feature = "sql"))]
            Some(database) => Err(format!("database {database}: this program was built without sql's databases")),
            None if !store.dir().join("kv.db").exists() => Err(none_kept(store.dir(), "kv.db", "bucket")),
            None => Ok(Kept::Store(store)),
        }
    }

    fn bucket(&self, name: &str) -> BucketBuilder<Stored> {
        match self {
            Kept::Store(store) => store.bucket(name),
            #[cfg(feature = "sql")]
            Kept::Database(database) => database.bucket(name),
        }
    }
}

/// The bucket under `branch`, its owners written as `42/devices`.
fn under(opened: &Bucket<Stored>, branch: Option<&str>) -> Bucket<Stored> {
    branch.map_or_else(|| opened.clone(), |branch| branch.split('/').fold(opened.clone(), |at, owner| at.under(owner)))
}

/// A value as a bucket's row keeps it, whatever type its program chose: the
/// row says by its first byte which kind wrote it, so each reads back as what
/// it is.
#[derive(Debug, PartialEq)]
pub enum Stored {
    Nothing,
    Integer(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Json(Value),
    /// A row of a kind this reader does not know.
    Unread,
}

impl Stored {
    /// The value as JSON: JSON as it is, text as a string, bytes as their hex.
    pub fn json(&self) -> Value {
        match self {
            Stored::Nothing | Stored::Unread => Value::Null,
            Stored::Integer(integer) => (*integer).into(),
            Stored::Float(float) => serde_json::Number::from_f64(*float).map_or(Value::Null, Into::into),
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
    /// A row as its kind wrote it; one this reader does not know is `Unread`.
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
        Value::deserialize(de::value::SeqAccessDeserializer::new(seq)).map(Stored::Json)
    }

    fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<Stored, A::Error> {
        Value::deserialize(de::value::MapAccessDeserializer::new(map)).map(Stored::Json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;

    fn kept(dir: &std::path::Path) -> Store {
        Store::open(dir, Options { background: false, ..Options::default() }).unwrap()
    }

    #[test]
    fn each_kind_a_program_kept_reads_back_as_what_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let store = kept(dir.path());
        store.bucket::<String>("names").open().unwrap().set("a", &"ann".to_owned()).unwrap();
        store.bucket::<Value>("docs").open().unwrap().set("a", &json!({"n": 1})).unwrap();
        store.bucket::<crate::kv::Bytes>("raw").open().unwrap().set("a", &crate::kv::Bytes(vec![0xff, 0x00])).unwrap();
        store.bucket::<f64>("floats").open().unwrap().set("a", &1.5).unwrap();
        store.bucket::<i64>("counts").open().unwrap().set("a", &7).unwrap();
        let one = |bucket: &str| {
            let reading = Reading { bucket, database: None, key: Some("a"), branch: None };
            entries(&store, &reading, 1).unwrap().remove(0).value
        };
        assert_eq!(one("names"), Stored::Text("ann".to_owned()));
        assert_eq!(one("docs"), Stored::Json(json!({"n": 1})));
        assert_eq!(one("raw"), Stored::Bytes(vec![0xff, 0x00]));
        assert_eq!(one("floats"), Stored::Float(1.5));
        assert_eq!(one("counts"), Stored::Integer(7));
        assert_eq!(one("raw").to_string(), "2 bytes: ff00");
    }

    #[cfg(feature = "sql")]
    #[test]
    fn a_bucket_kept_in_a_database_is_read_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = kept(dir.path());
        let db = store.database("app").open().unwrap();
        db.bucket::<String>("drafts").open().unwrap().set("a", &"kept in app.db".to_owned()).unwrap();
        let beside = Store::open_beside(dir.path()).unwrap();
        let why = Kept::at(&beside, None).err().unwrap();
        assert!(why.contains("holds no kv.db") && why.ends_with("one of app"), "{why}");
        let reading = Reading { bucket: "drafts", database: Some("app"), key: None, branch: None };
        let found = entries(&beside, &reading, 10).unwrap();
        assert_eq!(found[0].value, Stored::Text("kept in app.db".to_owned()));
    }
}
