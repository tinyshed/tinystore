//! `tinystore kv`: a bucket's value by its key, and a bucket's keys, read
//! beside the program or server that holds the directory; a key set or
//! deleted through that server, or a sidecar the command starts.
//!
//! ```text
//! tinystore kv ./data get sessions 42          {"user": 42, "seen": "2026-10-18"}
//! tinystore kv ./data list sessions            every key of the bucket's root, with its value
//! tinystore kv ./data list sessions 42         the keys under the branch 42
//! tinystore kv ./data list drafts --database app   a bucket kept in sql/app.db
//! tinystore kv ./data set flags beta on --ttl 1h
//! tinystore kv ./data set counts hits 42 --as int
//! tinystore kv ./data delete sessions 42
//! ```
//!
//! A bucket keeps a value of any type its program chose: an integer as one,
//! text as its bytes, anything compound as JSON. Nothing says which, so a
//! value shows as what its bytes read as: JSON, then text, then hex.

use std::io::{IsTerminal as _, Read as _};
use std::process::ExitCode;

use tinystore::Store;
use tinystore::client::KvValue;
use tinystore::mcp::kv::{Reading, entries};

use crate::asked::{self, Asked, Done, Refused, Verb, failed, wrong};
use crate::{help, reach, window};

const VERBS: [Verb; 4] = [
    Verb { word: "get", command: &help::KV_GET, run: get },
    Verb { word: "list", command: &help::KV_LIST, run: list },
    Verb { word: "set", command: &help::KV_SET, run: set },
    Verb { word: "delete", command: &help::KV_DELETE, run: delete },
];

/// The keys `list` prints at most.
const MOST: usize = 1000;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run_verb("kv", &VERBS, args)
}

fn get(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("kv", "get", true)?;
    let [bucket, key] = &rest[..] else {
        return Err(wrong("get takes a bucket and a key: get sessions 42"));
    };
    let store = Store::open_beside(&dir).map_err(failed)?;
    let reading = Reading { bucket, database: asked.value("--database"), key: Some(key), branch: None };
    show_one(&store, &reading, asked.has("--json"))
}

fn list(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("kv", "list", true)?;
    let (Some(bucket), branch, None) = (rest.first(), rest.get(1), rest.get(2)) else {
        return Err(wrong("list takes a bucket and a branch at most: list sessions 42"));
    };
    let store = Store::open_beside(&dir).map_err(failed)?;
    let reading =
        Reading { bucket, database: asked.value("--database"), key: None, branch: branch.map(String::as_str) };
    show_all(&store, &reading, asked.has("--json"))
}

/// Sets a key through the server that holds the directory, to text unless
/// `--as` says an integer or JSON; `-` reads the value from stdin.
fn set(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("kv", "set", false)?;
    let [bucket, key, given] = &rest[..] else {
        return Err(wrong("set takes a bucket, a key and a value: set flags beta on"));
    };
    let ttl = asked.value("--ttl").map(asked::span).transpose().map_err(wrong)?;
    let database = asked.value("--database").map(str::to_owned);
    reach::store_at(&dir)?;
    if let Some(database) = &database {
        crate::sql::database_held(&dir, database)?;
    }
    let value = value_of(given, asked.value("--as"))?;
    let (named, keyed) = (bucket.clone(), key.clone());
    reach::write(&dir, "kv set", move |client| {
        let database = database.as_deref().map(|database| client.database(database)).transpose()?;
        let bucket = client.bucket(&named, database)?;
        client.set(bucket, &keyed, &value, ttl)
    })?;
    let expiring = ttl.map_or_else(String::new, |ttl| format!(", expiring in {}", window::span_text(ttl)));
    outln!("set {key} in bucket {bucket}{expiring}")
}

/// Deletes a key through the server that holds the directory.
fn delete(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("kv", "delete", false)?;
    let [bucket, key] = &rest[..] else {
        return Err(wrong("delete takes a bucket and a key: delete sessions 42"));
    };
    let database = asked.value("--database").map(str::to_owned);
    reach::store_at(&dir)?;
    if let Some(database) = &database {
        crate::sql::database_held(&dir, database)?;
    }
    let (named, keyed) = (bucket.clone(), key.clone());
    let found = reach::write(&dir, "kv delete", move |client| {
        let database = database.as_deref().map(|database| client.database(database)).transpose()?;
        let bucket = client.bucket(&named, database)?;
        client.delete(bucket, &keyed)
    })?;
    if found { outln!("deleted {key} from bucket {bucket}") } else { outln!("bucket {bucket} holds no key {key}") }
}

/// A value as `set` was given it: text unless `--as` says otherwise, and
/// stdin's when `-`.
fn value_of(given: &str, kind: Option<&str>) -> Result<KvValue, Refused> {
    let text = if given == "-" { stdin_value()? } else { given.to_owned() };
    match kind {
        None | Some("text") => Ok(KvValue::Text(text)),
        Some("int") => {
            text.trim().parse().map(KvValue::Int).map_err(|_| wrong(format!("{text:?}: --as int takes an integer")))
        }
        Some("json") => serde_json::from_str::<serde_json::Value>(&text)
            .map(|json| KvValue::Json(json.to_string()))
            .map_err(|error| wrong(format!("--as json: {error}"))),
        Some(other) => Err(wrong(format!("--as {other}: text, int or json"))),
    }
}

/// The value on stdin, its last line's end left out, as `echo` adds one.
fn stdin_value() -> Result<String, Refused> {
    if std::io::stdin().is_terminal() {
        return Err(wrong(
            "- reads the value on stdin, and stdin is a terminal\n  echo on | tinystore kv ./data set flags beta -",
        ));
    }
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text).map_err(|error| failed(format!("stdin: {error}")))?;
    let ended = text.strip_suffix('\n').map(|text| text.strip_suffix('\r').unwrap_or(text));
    Ok(ended.map(str::to_owned).unwrap_or(text))
}

fn show_one(store: &Store, reading: &Reading, json: bool) -> Done {
    let found = entries(store, reading, 1).map_err(failed)?;
    let Some(entry) = found.first() else {
        let (bucket, key) = (reading.bucket, reading.key.unwrap_or_default());
        return Err(failed(format!("kv bucket {bucket}: no key {key}")));
    };
    if json { outln!("{}", entry.json()) } else { outln!("{:#}", entry.value) }
}

fn show_all(store: &Store, reading: &Reading, json: bool) -> Done {
    let (bucket, branch) = (reading.bucket, reading.branch);
    let found = entries(store, reading, MOST + 1).map_err(failed)?;
    if found.is_empty() {
        let under = branch.map_or_else(String::new, |branch| format!(" under {branch}"));
        return outln!("kv bucket {bucket} holds no key{under}");
    }
    let width = found.iter().take(MOST).map(|entry| entry.key.chars().count()).max().unwrap_or(0).min(40);
    for entry in found.iter().take(MOST) {
        if json {
            outln!("{}", entry.json())?;
        } else {
            outln!("{:width$}  {}", entry.key, entry.value)?;
        }
    }
    if found.len() > MOST {
        eprintln!("the first {MOST} keys; a branch narrows them: list {bucket} <branch>");
    }
    Ok(())
}
