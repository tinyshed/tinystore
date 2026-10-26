//! The tools of `tinystore mcp` that write, offered only as `--write` names
//! them, a tool an operation, each marked as writing and, where it removes
//! something, destructive: an agent's host asks its person for each tool as
//! it would for any other, and a flag alone is no boundary for an agent that
//! also has a shell. Each writes through the server that holds the
//! directory, or a sidecar it starts, as the command line's writes do.
//!
//! ```text
//! claude mcp add tinystore -- tinystore mcp ./data --write kv_set,jobs_add
//! ```

use std::path::Path;

use serde_json::{Value, json};
use tinystore::client::{Event, KvValue, NewJob};

use crate::asked::{self, Refused, wrong};
use crate::reach;

/// Each tool that writes, what it does, and whether it removes something.
const TOOLS: [(&str, &str, bool); 11] = [
    ("kv_set", "Sets a kv bucket's key to a value: text unless `as` says int or json.", false),
    ("kv_delete", "Deletes a kv bucket's key.", true),
    ("jobs_add", "Adds a job, JSON, to a queue; with an id, a queue that holds it adds nothing.", false),
    ("jobs_cancel", "Cancels a queue's job by its id.", true),
    ("files_put", "Writes a text file at a path of a set of files, replacing what the path held.", true),
    ("files_delete", "Deletes the file at a path of a set of files.", true),
    ("events_append", "Appends an event to a stream; with an id, a stream that holds it writes nothing.", false),
    ("channels_publish", "Hands a JSON message to whoever listens on a channel now.", false),
    ("config_update", "Changes a config's live settings, which its programs take at once.", false),
    ("config_reset", "Drops the changes of a config's setting, a group, or every one.", true),
    ("sql_exec", "Runs one SQL statement that writes on one of the application's databases.", true),
];

/// The tools `--write` names: `all`, or their names with commas between.
pub(crate) fn allowed(given: Option<&str>) -> Result<Vec<&'static str>, Refused> {
    let names: Vec<&'static str> = TOOLS.iter().map(|(name, ..)| *name).collect();
    match given {
        None => Ok(Vec::new()),
        Some("all") => Ok(names),
        Some(list) => list
            .split(',')
            .map(|word| {
                let word = word.trim();
                names.iter().find(|name| **name == word).copied().ok_or_else(|| {
                    wrong(format!(
                        "--write {word}: no tool that writes{}; all, or {}",
                        crate::meant(word, &names),
                        names.join(", ")
                    ))
                })
            })
            .collect(),
    }
}

/// The tools of `allowed`, each with what it takes and its marks.
pub(crate) fn tools(allowed: &[&str]) -> Vec<Value> {
    TOOLS
        .iter()
        .filter(|(name, ..)| allowed.contains(name))
        .map(|(name, does, destructive)| {
            let (properties, required) = inputs(name);
            json!({
                "name": name,
                "description": does,
                "inputSchema": { "type": "object", "properties": properties, "required": required },
                "annotations": { "readOnlyHint": false, "destructiveHint": destructive, "openWorldHint": false },
            })
        })
        .collect()
}

fn text(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

/// What a tool takes, and what of it is required.
fn inputs(name: &str) -> (Value, Vec<&'static str>) {
    let database = text("the database the program opened it from, as app, when it is kept there");
    let any = |description: &str| json!({ "description": description });
    let object = |description: &str| json!({ "type": "object", "description": description });
    let changes = "each live setting's path and its value, as {\"model.max_tokens\": \"2048\"}: text is read by \
                   the setting's kind, other JSON taken as it is";
    match name {
        "kv_set" => (
            json!({
                "bucket": text("the bucket"),
                "key": text("the key"),
                "value": text("the value, as text"),
                "as": text("text, int or json: what the value is; text when absent"),
                "ttl": text("expire it this long from now, as 1h"),
                "database": database,
            }),
            vec!["bucket", "key", "value"],
        ),
        "kv_delete" => (
            json!({ "bucket": text("the bucket"), "key": text("the key"), "database": database }),
            vec!["bucket", "key"],
        ),
        "jobs_add" => (
            json!({
                "queue": text("the queue"),
                "job": any("the job, any JSON"),
                "id": text("what names the job; cancel needs it"),
                "delay": text("run it this long from now, as 1m"),
                "database": database,
            }),
            vec!["queue", "job"],
        ),
        "jobs_cancel" => (
            json!({ "queue": text("the queue"), "id": text("the job's id"), "database": database }),
            vec!["queue", "id"],
        ),
        "files_put" => (
            json!({
                "set": text("the set of files"),
                "path": text("the file's path in it"),
                "text": text("its text"),
                "type": text("its content type, as text/markdown"),
            }),
            vec!["set", "path", "text"],
        ),
        "files_delete" => {
            (json!({ "set": text("the set of files"), "path": text("the file's path in it") }), vec!["set", "path"])
        }
        "events_append" => (
            json!({
                "stream": text("the stream"),
                "name": text("the event's name, as role.changed"),
                "fields": object("what it carries"),
                "level": text("trace, debug, info, warn or error"),
                "id": text("what names it in its stream"),
            }),
            vec!["stream", "name"],
        ),
        "channels_publish" => (
            json!({
                "channel": text("the channel"),
                "message": any("the message, any JSON"),
                "under": text("the branch, its owners as 42/devices"),
            }),
            vec!["channel", "message"],
        ),
        "config_update" => {
            (json!({ "config": text("the config"), "changes": object(changes) }), vec!["config", "changes"])
        }
        "config_reset" => {
            let path = text("a setting or a group; every change when absent");
            (json!({ "config": text("the config"), "path": path }), vec!["config"])
        }
        _ => {
            let statement = text("one statement that writes");
            (json!({ "database": text("the database, as app"), "statement": statement }), vec!["database", "statement"])
        }
    }
}

/// A tool's answer when it is one that writes and `allowed` names it; none
/// for any other.
pub(crate) fn call(dir: &Path, allowed: &[&str], name: &str, arguments: &Value) -> Option<Result<Value, String>> {
    if !TOOLS.iter().any(|(tool, ..)| *tool == name) {
        return None;
    }
    if !allowed.contains(&name) {
        return Some(Err(format!("{name} writes, and tinystore mcp was not started with --write {name}")));
    }
    Some(written(dir, name, &Arguments(arguments)).map_err(|refused| refused.to_string()))
}

/// A tool's arguments, each read as what it should be.
struct Arguments<'a>(&'a Value);

impl Arguments<'_> {
    fn text(&self, key: &str) -> Option<String> {
        self.0.get(key).and_then(Value::as_str).map(str::to_owned)
    }

    fn needed(&self, key: &str) -> Result<String, Refused> {
        self.text(key).ok_or_else(|| wrong(format!("it takes {key}, as text")))
    }

    fn json(&self, key: &str) -> Result<Value, Refused> {
        self.0.get(key).cloned().ok_or_else(|| wrong(format!("it takes {key}")))
    }

    fn span(&self, key: &str) -> Result<Option<std::time::Duration>, Refused> {
        self.text(key).map(|span| asked::span(&span)).transpose().map_err(wrong)
    }
}

fn written(dir: &Path, name: &str, given: &Arguments) -> Result<Value, Refused> {
    reach::store_at(dir)?;
    match name {
        "kv_set" | "kv_delete" => kv(dir, name, given),
        "jobs_add" | "jobs_cancel" => jobs(dir, name, given),
        "files_put" | "files_delete" => files(dir, name, given),
        "events_append" => events(dir, given),
        "channels_publish" => {
            let (channel, message) = (given.needed("channel")?, given.json("message")?.to_string());
            let under: Vec<String> =
                given.text("under").map_or_else(Vec::new, |under| under.split('/').map(str::to_owned).collect());
            reach::write_once(dir, "mcp", move |client| client.publish(&channel, &under, &message))?;
            Ok(json!({ "published": true }))
        }
        "config_update" | "config_reset" => config(dir, name, given),
        _ => {
            let (database, statement) = (given.needed("database")?, given.needed("statement")?);
            crate::sql::database_held(dir, &database)?;
            let (changes, last) = reach::write_once(dir, "mcp", move |client| {
                let database = client.database(&database)?;
                client.exec(database, &statement)
            })?;
            Ok(json!({ "changes": changes, "last_insert_rowid": last }))
        }
    }
}

fn kv(dir: &Path, name: &str, given: &Arguments) -> Result<Value, Refused> {
    let (bucket, key, database) = (given.needed("bucket")?, given.needed("key")?, given.text("database"));
    if let Some(database) = &database {
        crate::sql::database_held(dir, database)?;
    }
    if name == "kv_delete" {
        let deleted = reach::write(dir, "mcp", move |client| {
            let database = database.as_deref().map(|database| client.database(database)).transpose()?;
            let bucket = client.bucket(&bucket, database)?;
            client.delete(bucket, &key)
        })?;
        return Ok(json!({ "deleted": deleted }));
    }
    let value = given.needed("value")?;
    let value = match given.text("as").as_deref() {
        None | Some("text") => KvValue::Text(value),
        Some("int") => {
            KvValue::Int(value.trim().parse().map_err(|_| wrong(format!("{value:?}: as int takes an integer")))?)
        }
        Some("json") => KvValue::Json(
            serde_json::from_str::<Value>(&value).map_err(|error| wrong(format!("as json: {error}")))?.to_string(),
        ),
        Some(other) => return Err(wrong(format!("as {other}: text, int or json"))),
    };
    let ttl = given.span("ttl")?;
    reach::write(dir, "mcp", move |client| {
        let database = database.as_deref().map(|database| client.database(database)).transpose()?;
        let bucket = client.bucket(&bucket, database)?;
        client.set(bucket, &key, &value, ttl)
    })?;
    Ok(json!({ "set": true }))
}

fn jobs(dir: &Path, name: &str, given: &Arguments) -> Result<Value, Refused> {
    let (queue, database) = (given.needed("queue")?, given.text("database"));
    if let Some(database) = &database {
        crate::sql::database_held(dir, database)?;
    }
    if name == "jobs_cancel" {
        let id = given.needed("id")?;
        let cancelled = reach::write(dir, "mcp", move |client| {
            let database = database.as_deref().map(|database| client.database(database)).transpose()?;
            let queue = client.queue(&queue, database)?;
            client.cancel(queue, &id)
        })?;
        return Ok(json!({ "cancelled": cancelled }));
    }
    let job =
        NewJob { value: given.json("job")?.to_string(), id: given.text("id"), delay: given.span("delay")?, at: None };
    let added = reach::write(dir, "mcp", move |client| {
        let database = database.as_deref().map(|database| client.database(database)).transpose()?;
        let queue = client.queue(&queue, database)?;
        client.add(queue, &job)
    })?;
    Ok(json!({ "added": added }))
}

fn files(dir: &Path, name: &str, given: &Arguments) -> Result<Value, Refused> {
    let (set, path) = (given.needed("set")?, given.needed("path")?);
    if name == "files_delete" {
        let deleted = reach::write(dir, "mcp", move |client| {
            let files = client.files(&set)?;
            client.delete_file(files, &path)
        })?;
        return Ok(json!({ "deleted": deleted }));
    }
    let (text, content_type) = (given.needed("text")?, given.text("type"));
    let info = reach::write(dir, "mcp", move |client| {
        let files = client.files(&set)?;
        client.put(files, &path, (&mut text.as_bytes(), text.len() as u64), content_type.as_deref())
    })?;
    Ok(json!({ "size": info.size, "etag": info.etag, "type": info.content_type }))
}

fn events(dir: &Path, given: &Arguments) -> Result<Value, Refused> {
    let stream = given.needed("stream")?;
    let level = match given.text("level") {
        Some(level) => Some(
            crate::logs::level_named(&level)
                .map(|level| i64::from(level.number()))
                .ok_or_else(|| wrong(format!("level {level}: trace, debug, info, warn or error")))?,
        ),
        None => None,
    };
    let fields = given.0.get("fields").map(Value::to_string);
    let event = Event { name: given.needed("name")?, fields, level, at: None, id: given.text("id") };
    let held = reach::write(dir, "mcp", move |client| client.append(&stream, std::slice::from_ref(&event)))?;
    Ok(json!({ "written": held == 0 }))
}

fn config(dir: &Path, name: &str, given: &Arguments) -> Result<Value, Refused> {
    let config = given.needed("config")?;
    if name == "config_reset" {
        let path = given.text("path").unwrap_or_default();
        let version = reach::write(dir, "mcp", move |client| {
            let (handle, _) = client.config(&config)?;
            client.reset_config(handle, &path)
        })?;
        return Ok(json!({ "version": version }));
    }
    let given_changes = given.json("changes")?;
    let paths = given_changes.as_object().ok_or_else(|| wrong("changes is an object of paths and values"))?;
    let changes = crate::config::changes_of(dir, &config, paths)?;
    let version = reach::write(dir, "mcp", move |client| {
        let (handle, version) = client.config(&config)?;
        client.update_config(handle, version, &changes)
    })?;
    Ok(json!({ "version": version }))
}
