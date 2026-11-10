//! The tools of `tinystore mcp` that write, a tool an operation, each marked
//! as writing and, where it removes or replaces something, as removing: the
//! library's server offers one only as `--write` names it, and an agent's
//! host asks its person for each as it would for any other tool. A flag alone
//! is no boundary for an agent that also has a shell. Each writes through the
//! server that holds the directory, or a sidecar it starts, as the command
//! line's writes do.
//!
//! ```text
//! claude mcp add tinystore -- tinystore mcp ./data --write kv_set,jobs_add
//! ```

use std::path::Path;

use serde_json::{Value, json};
use tinystore::CancellationToken;
use tinystore::client::{Event, KvValue, NewJob};
use tinystore::mcp::Tool;

use crate::asked::{self, Refused, wrong};
use crate::reach;

/// Each tool that writes, what it does, and whether it removes or replaces
/// something.
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

/// The tools that write to the store in `dir`, each with what it takes and
/// its mark.
pub(crate) fn tools(dir: &Path) -> Vec<Tool> {
    TOOLS
        .iter()
        .map(|&(name, does, removes)| {
            let (properties, required) = inputs(name);
            let takes = json!({ "type": "object", "properties": properties, "required": required });
            let dir = dir.to_owned();
            let write = move |given: &Value, _: &CancellationToken| {
                written(&dir, name, &Arguments(given)).map_err(|refused| refused.to_string().into())
            };
            if removes { Tool::removes(name, does, takes, write) } else { Tool::writes(name, does, takes, write) }
        })
        .collect()
}

fn text(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn any(description: &str) -> Value {
    json!({ "description": description })
}

fn object(description: &str) -> Value {
    json!({ "type": "object", "description": description })
}

fn database() -> Value {
    text("the database the program opened it from, as app, when it is kept there")
}

/// What a tool takes, and what of it is required.
fn inputs(name: &str) -> (Value, Vec<&'static str>) {
    match name {
        "kv_set" | "kv_delete" => kv_inputs(name),
        "jobs_add" | "jobs_cancel" => jobs_inputs(name),
        "files_put" | "files_delete" => files_inputs(name),
        "events_append" => events_inputs(),
        "channels_publish" => (
            json!({
                "channel": text("the channel"),
                "message": any("the message, any JSON"),
                "under": text("the branch, its owners as 42/devices"),
            }),
            vec!["channel", "message"],
        ),
        "config_update" | "config_reset" => config_inputs(name),
        _ => {
            let statement = text("one statement that writes");
            (json!({ "database": text("the database, as app"), "statement": statement }), vec!["database", "statement"])
        }
    }
}

fn kv_inputs(name: &str) -> (Value, Vec<&'static str>) {
    if name == "kv_delete" {
        return (
            json!({ "bucket": text("the bucket"), "key": text("the key"), "database": database() }),
            vec!["bucket", "key"],
        );
    }
    (
        json!({
            "bucket": text("the bucket"),
            "key": text("the key"),
            "value": text("the value, as text"),
            "as": text("text, int or json: what the value is; text when absent"),
            "ttl": text("expire it this long from now, as 1h"),
            "database": database(),
        }),
        vec!["bucket", "key", "value"],
    )
}

fn jobs_inputs(name: &str) -> (Value, Vec<&'static str>) {
    if name == "jobs_cancel" {
        return (
            json!({ "queue": text("the queue"), "id": text("the job's id"), "database": database() }),
            vec!["queue", "id"],
        );
    }
    (
        json!({
            "queue": text("the queue"),
            "job": any("the job, any JSON"),
            "id": text("what names the job; cancel needs it"),
            "delay": text("run it this long from now, as 1m"),
            "database": database(),
        }),
        vec!["queue", "job"],
    )
}

fn files_inputs(name: &str) -> (Value, Vec<&'static str>) {
    if name == "files_delete" {
        return (
            json!({ "set": text("the set of files"), "path": text("the file's path in it") }),
            vec!["set", "path"],
        );
    }
    (
        json!({
            "set": text("the set of files"),
            "path": text("the file's path in it"),
            "text": text("its text"),
            "type": text("its content type, as text/markdown"),
        }),
        vec!["set", "path", "text"],
    )
}

fn events_inputs() -> (Value, Vec<&'static str>) {
    (
        json!({
            "stream": text("the stream"),
            "name": text("the event's name, as role.changed"),
            "fields": object("what it carries"),
            "level": text("trace, debug, info, warn or error"),
            "id": text("what names it in its stream"),
        }),
        vec!["stream", "name"],
    )
}

fn config_inputs(name: &str) -> (Value, Vec<&'static str>) {
    if name == "config_reset" {
        let path = text("a setting or a group; every change when absent");
        return (json!({ "config": text("the config"), "path": path }), vec!["config"]);
    }
    let changes = "each live setting's path and its value, as {\"model.max_tokens\": \"2048\"}: text is read by \
                   the setting's kind, other JSON taken as it is";
    (json!({ "config": text("the config"), "changes": object(changes) }), vec!["config", "changes"])
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
    let id = given.text("id");
    let job = NewJob { value: given.json("job")?.to_string(), id: id.clone(), delay: given.span("delay")?, at: None };
    let added = reach::write_by_id(dir, "mcp", id.as_deref(), move |client| {
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
    let id = given.text("id");
    let event = Event { name: given.needed("name")?, fields, level, at: None, id: id.clone() };
    let held = reach::write_by_id(dir, "mcp", id.as_deref(), move |client| {
        client.append(&stream, std::slice::from_ref(&event))
    })?;
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
