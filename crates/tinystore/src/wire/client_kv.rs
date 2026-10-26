//! A shell's writes to a bucket, `kv set` and `kv delete`, and the database
//! a bucket or a queue may be kept in.

use std::io::{Read, Write};
use std::time::Duration;

use super::{Client, millis_of};
use crate::Result;
use crate::wire::codec::Row;
use crate::wire::protocol::{self as wire, method};

/// The first byte of a row of bytes that text wrote, and JSON, as every SDK
/// writes them.
const TEXT: u8 = 1;
const JSON: u8 = 3;

/// A value as `kv set` writes it.
#[derive(Clone, Debug)]
pub enum KvValue {
    Text(String),
    Int(i64),
    /// JSON's text, as given.
    Json(String),
}

impl<S: Read + Write> Client<S> {
    /// The handle of the bucket `name`, in the database `database` when given;
    /// encrypted, when that is how the store keeps it.
    pub fn bucket(&mut self, name: &str, database: Option<u64>) -> Result<u64> {
        let open = |encrypted| wire::KvBucketOpen {
            name: name.to_owned(),
            database,
            encrypted,
            ..wire::KvBucketOpen::default()
        };
        match self.handle(method::KV_BUCKET_OPEN, &open(false)) {
            Err(error) if error.to_string().contains("encrypted") => self.handle(method::KV_BUCKET_OPEN, &open(true)),
            opened => opened,
        }
    }

    /// Sets `key` of the bucket to `value`, expiring `ttl` from now when given.
    pub fn set(&mut self, bucket: u64, key: &str, value: &KvValue, ttl: Option<Duration>) -> Result<()> {
        let row = match value {
            KvValue::Text(text) => Row::Bin([&[TEXT], text.as_bytes()].concat()),
            KvValue::Int(number) => Row::Int(*number),
            KvValue::Json(json) => Row::Bin([&[JSON], json.as_bytes()].concat()),
        };
        let call = wire::KvCall {
            handle: bucket,
            key: key.to_owned(),
            value: Some(row),
            ttl: ttl.map(millis_of),
            ..Default::default()
        };
        self.call(method::KV_SET, &call).map(drop)
    }

    /// Deletes `key` of the bucket: whether it held one.
    pub fn delete(&mut self, bucket: u64, key: &str) -> Result<bool> {
        let call = wire::KvCall { handle: bucket, key: key.to_owned(), ..wire::KvCall::default() };
        Ok(self.answer::<wire::KvFound>(method::KV_DELETE, &call)?.found)
    }

    /// The handle of the application's database `name`, opened with no
    /// migrations: as its program left it.
    #[cfg(feature = "sql")]
    pub fn database(&mut self, name: &str) -> Result<u64> {
        self.handle(method::SQL_OPEN, &wire::SqlOpen { name: name.to_owned(), ..wire::SqlOpen::default() })
    }

    /// Runs a statement that writes on the database: the rows it changed,
    /// and the last row it inserted.
    #[cfg(feature = "sql")]
    pub fn exec(&mut self, database: u64, statement: &str) -> Result<(u64, i64)> {
        let statement = wire::SqlStatement { handle: database, text: statement.to_owned(), values: Vec::new() };
        let done = self.answer::<wire::SqlDone>(method::SQL_EXEC, &statement)?;
        Ok((done.changes, done.last_insert_rowid))
    }
}
