//! `tinystore config`: a config as the store keeps it, from a shell. The
//! store keeps the last declaration a program opened a config with and the
//! changes made to its live settings since; the program's own files and
//! environment it never sees. `explain` reads that beside whoever holds the
//! directory; `update` and `reset` change the live settings through the
//! server that holds it, or a sidecar they start, so that the programs
//! running take the change at once.
//!
//! ```text
//! tinystore config ./data explain chat
//! tinystore config ./data update chat model.max_tokens=2048
//! tinystore config ./data reset chat model.max_tokens
//! ```

use std::path::Path;
use std::process::ExitCode;

use serde_json::{Map, Value, json};
use tinystore::Store;
use tinystore::config::Config;

use crate::asked::{self, Asked, Done, Refused, Verb, failed, wrong};
use crate::{help, reach};

const VERBS: [Verb; 3] = [
    Verb { word: "explain", command: &help::CONFIG_EXPLAIN, run: explain },
    Verb { word: "update", command: &help::CONFIG_UPDATE, run: update },
    Verb { word: "reset", command: &help::CONFIG_RESET, run: reset },
];

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run_verb("config", &VERBS, args)
}

/// Each setting's value as the store makes it, where it came from, and what
/// the store cannot know.
fn explain(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("config", "explain", true)?;
    let [name] = &rest[..] else {
        return Err(wrong("explain takes a config's name: explain chat"));
    };
    let (_store, config) = kept(&dir, name)?;
    let explained = config.explain();
    let version = config.get().version();
    if asked.has("--json") {
        let settings: Vec<Value> = explained
            .settings
            .iter()
            .map(|line| json!({ "path": line.path, "value": line.value, "from": line.from, "over": line.over }))
            .collect();
        return outln!(
            "{}",
            json!({ "name": name, "version": version, "settings": settings, "notes": explained.notes })
        );
    }
    outln!("config {name} · version {version}")?;
    for line in explained.to_string().lines() {
        outln!("  {line}")?;
    }
    asked::say("config explain", "the program's own files and environment are its own: its value may differ");
    Ok(())
}

/// Changes live settings through the server that holds the directory:
/// `path=text`, read by the setting's kind as an environment variable is,
/// or `path:=json`.
fn update(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("config", "update", false)?;
    let Some((name, pairs)) = rest.split_first().filter(|(_, pairs)| !pairs.is_empty()) else {
        return Err(wrong("update takes a config and its settings: update chat model.max_tokens=2048"));
    };
    reach::store_at(&dir)?;
    let (_store, config) = kept(&dir, name)?;
    let mut changes = Map::new();
    for pair in pairs {
        let (path, value) = given(&config, pair)?;
        nested(&mut changes, &path, value)?;
    }
    let (named, sent) = (name.clone(), Value::Object(changes).to_string());
    let version = reach::write(&dir, "config update", move |client| {
        let (config, version) = client.config(&named)?;
        client.update_config(config, version, &sent)
    })?;
    outln!("updated config {name} to version {version}: {}", pairs.join(" "))
}

/// Drops the changes kept of a setting or a group, or of every setting.
fn reset(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("config", "reset", false)?;
    let (Some(name), path, None) = (rest.first(), rest.get(1), rest.get(2)) else {
        return Err(wrong("reset takes a config and a setting at most: reset chat model.max_tokens"));
    };
    reach::store_at(&dir)?;
    kept(&dir, name)?;
    let (named, at) = (name.clone(), path.cloned().unwrap_or_default());
    let version = reach::write(&dir, "config reset", move |client| {
        let (config, _) = client.config(&named)?;
        client.reset_config(config, &at)
    })?;
    match path {
        Some(path) => outln!("reset {path} of config {name}: version {version}"),
        None => outln!("reset every change of config {name}: version {version}"),
    }
}

/// The config as the store keeps it, read beside whoever holds it, and the
/// store it stays open with; one no program opened here is refused, naming
/// those it keeps.
fn kept(dir: &Path, name: &str) -> Result<(Store, Config<Value>), Refused> {
    asked::existing(dir)?;
    let store = Store::open_beside(dir).map_err(failed)?;
    match tinystore::client::config_kept(&store, name).map_err(failed)? {
        Some(config) => Ok((store, config)),
        None => {
            let known = tinystore::client::configs(&store).unwrap_or_default();
            let names: Vec<&str> = known.iter().map(String::as_str).collect();
            let held = if known.is_empty() { "none".to_owned() } else { known.join(", ") };
            Err(failed(format!(
                "{} keeps no config {name}{}: a program opens it first; it keeps {held}",
                dir.display(),
                crate::meant(name, &names)
            )))
        }
    }
}

/// A setting and its value as `update` was given them: `path=text` by the
/// setting's kind, `path:=json` as it is.
fn given(config: &Config<Value>, pair: &str) -> Result<(String, Value), Refused> {
    if let Some((path, json)) = pair.split_once(":=") {
        let value = serde_json::from_str(json).map_err(|error| wrong(format!("{pair}: JSON after :=, {error}")))?;
        return Ok((path.to_owned(), value));
    }
    let Some((path, text)) = pair.split_once('=') else {
        return Err(wrong(format!("{pair}: a setting and its value, as model.max_tokens=2048")));
    };
    let value = tinystore::client::config_value(config, path, text).map_err(wrong)?;
    Ok((path.to_owned(), value))
}

/// Puts `value` at `path` of the changes, each of its groups an object.
fn nested(changes: &mut Map<String, Value>, path: &str, value: Value) -> Result<(), Refused> {
    let (groups, setting) = path.rsplit_once('.').map_or(("", path), |(groups, setting)| (groups, setting));
    let mut at = changes;
    for group in groups.split('.').filter(|group| !group.is_empty()) {
        let entry = at.entry(group.to_owned()).or_insert_with(|| Value::Object(Map::new()));
        let Value::Object(inner) = entry else {
            return Err(wrong(format!("{path}: {group} was given a value, and is a group here")));
        };
        at = inner;
    }
    at.insert(setting.to_owned(), value);
    Ok(())
}
