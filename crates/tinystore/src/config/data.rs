//! A config as an SDK sends it over the wire: its declaration as data, in the
//! JSON `Declaration::to_json` writes, and its layers with what the program
//! read itself, a file's text and its variables, since those belong to the
//! program and not to the server.
//!
//! A Rust program's [`Declaration`] as data is this declaration too.
//!
//! ```text
//! declaration  { "port": { "kind": "integer", "default": 8080, "min": 1 },
//!                "db": { "group": { "url": { "kind": "text", "secret": true, "required": true } } },
//!                "hosts": { "kind": { "list": { "kind": "text" } }, "default": [], "max": 20 },
//!                "instance": { "kind": "object", "default": {}, "entries": { "host": { "kind": "text", "default": "" } } },
//!                "plugin": { "kind": "text", "default": "postgres" },
//!                "settings": { "kind": "object", "default": {}, "chosenBy": "plugin",
//!                              "groups": { "postgres": { "port": { "kind": "integer", "default": 5432 } } } } }
//! layers       [ { "object": { "port": 80 }, "place": 2 },
//!                { "file": { "name": "app.yaml", "text": "port: 9000\n", "optional": false } },
//!                { "env": { "prefix": "APP", "vars": { "port": "PORT" }, "variables": { "PORT": "3000" },
//!                           "files": { "/run/secrets/db": "postgres://…" }, "dotenv": [] } } ]
//! ```

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::declaration::{
    Chosen, Declaration, Entries, Group, Item, Kind, Node, Place, Rules, Setting, declared_path, did_you_mean, joined,
    shown,
};
use super::files::{Walked, walk_entries};
use super::rules;
#[cfg(all(feature = "config", any(test, feature = "wire", feature = "records")))]
use super::{env::Env, files::File, layers::Layer};
use crate::typos::meant;

/// The declaration an SDK sent, its defaults checked against their own
/// kinds and rules and kept as the config keeps a value.
pub(crate) fn declaration(value: &Value) -> Result<Declaration, String> {
    let map = value.as_object().ok_or_else(|| "a declaration is a map of settings".to_owned())?;
    Ok(Declaration { root: group(map, "", Place::Root)? })
}

fn group(map: &Map<String, Value>, path: &str, place: Place) -> Result<Group, String> {
    let mut settings = Group::new();
    for (key, value) in map {
        let at = declared_path(path, key)?;
        let node = value.as_object().ok_or_else(|| format!("{at}: {} is no setting", shown(value)))?;
        let node = match (node.get("group"), node.get("entries"), node.get("chosenBy")) {
            (Some(Value::Object(inner)), ..) => Node::Group(group(inner, &at, place)?),
            (Some(other), ..) => return Err(format!("{at}: a group of {} is no map", shown(other))),
            (None, Some(Value::Object(item)), _) => Node::Entries(entries(node, item, &at, place)?),
            (None, Some(other), _) => return Err(format!("{at}: an entry of {} is no map", shown(other))),
            (None, None, Some(by)) => Node::Chosen(chosen(node, by, &at, place)?),
            (None, None, None) => Node::Setting(setting(node, &at, place)?),
        };
        settings.insert(key.clone(), node);
    }
    link_choosers(&mut settings, path)?;
    Ok(settings)
}

/// A group whose settings a setting beside it chooses among `groups` by
/// their names, each read as a group: never in a list's item or in another
/// chosen group. A path inside one names its group: `settings(postgres).port`.
fn chosen(map: &Map<String, Value>, by: &Value, path: &str, place: Place) -> Result<Chosen, String> {
    if matches!(place, Place::Item | Place::Chosen) {
        return Err(format!("{path}: a chosen group stands among settings, not in a list's item or a chosen group"));
    }
    let by = by.as_str().filter(|by| !by.is_empty());
    let by = by.ok_or_else(|| format!("{path}: chosenBy is the key of the setting beside it that chooses"))?;
    let groups = match map.get("groups") {
        Some(Value::Object(groups)) if !groups.is_empty() => groups,
        _ => return Err(format!("{path}: its groups are a map from each value of {by} to its group")),
    };
    let mut read = BTreeMap::new();
    for (name, inner) in groups {
        let at = format!("{path}({name})");
        match inner {
            Value::Object(inner) if !name.is_empty() => read.insert(name.clone(), group(inner, &at, Place::Chosen)?),
            Value::Object(_) => return Err(format!("{path}: a group's name is not empty")),
            other => return Err(format!("{at}: {} is no group", shown(other))),
        };
    }
    Ok(Chosen { by: by.to_owned(), groups: read })
}

/// Checks each chosen group's chooser, a text setting beside it that is
/// neither a secret nor live, and makes the names of its groups the only
/// values it takes; a `oneOf` of its own names none but them.
fn link_choosers(settings: &mut Group, path: &str) -> Result<(), String> {
    let chosen: Vec<(String, String, Vec<String>)> = settings
        .iter()
        .filter_map(|(key, node)| match node {
            Node::Chosen(chosen) => Some((key.clone(), chosen.by.clone(), chosen.groups.keys().cloned().collect())),
            _ => None,
        })
        .collect();
    let keys: Vec<String> = settings.keys().cloned().collect();
    for (key, by, names) in chosen {
        let (at, chooser_at) = (joined(path, &key), joined(path, &by));
        let chooser = match settings.get_mut(&by) {
            Some(Node::Setting(chooser)) if chooser.kind == Kind::Text && !chooser.secret && !chooser.live => chooser,
            Some(_) => {
                return Err(format!("{at}: {chooser_at} chooses its group, so it is text, neither secret nor live"));
            }
            None => {
                let near = meant(&by, &keys).map(|near| joined(path, &near));
                return Err(format!(
                    "{at}: {chooser_at}, which chooses its group, is no setting{}",
                    did_you_mean(near)
                ));
            }
        };
        match &chooser.rules.one_of {
            Some(allowed) => {
                if let Some(other) = allowed.iter().find(|allowed| !names.contains(allowed)) {
                    return Err(format!("{chooser_at}: {other:?} names none of the groups of {at}"));
                }
            }
            None => chooser.rules.one_of = Some(names),
        }
        if let Some(default) = &chooser.default {
            rules::check(chooser, default).map_err(|why| format!("{chooser_at}: its default: {why}"))?;
        }
    }
    Ok(())
}

/// A map of entries: its `kind` and `default` are what an older release
/// reads it as, a map given whole, and `default` the entries the declaration
/// gives, each walked as a file's entry is.
fn entries(map: &Map<String, Value>, item: &Map<String, Value>, path: &str, place: Place) -> Result<Entries, String> {
    if place != Place::Root {
        return Err(format!("{path}: entries stand among settings, not in an entry or a list's item"));
    }
    let item = group(item, &declared_path(path, "*")?, Place::Entry)?;
    let given = match map.get("default") {
        None => Map::new(),
        Some(Value::Object(given)) => given.clone(),
        Some(other) => return Err(format!("{path}: entries of {} are no map", shown(other))),
    };
    let entries = Entries { item, given };
    let mut walked = Walked::default();
    walk_entries(&entries, &entries.given, path, &mut walked);
    match walked.problems.into_iter().next() {
        Some(problem) => Err(format!("its default: {problem}")),
        None => Ok(entries),
    }
}

fn setting(map: &Map<String, Value>, path: &str, place: Place) -> Result<Setting, String> {
    let refused = |why: &str| format!("{path}: {why}");
    let kind = kind(map.get("kind").unwrap_or(&Value::Null), path)?;
    let rules = rules_of(map).map_err(|why| refused(&why))?;
    let flag = |name: &str| map.get(name).and_then(Value::as_bool).unwrap_or(false);
    let (required, live, secret) = (flag("required"), flag("live"), flag("secret"));
    match () {
        () if secret && live => return Err(refused("a secret is never live")),
        () if secret && kind != Kind::Text => return Err(refused("a secret is text")),
        () if place == Place::Item && (live || secret) => {
            return Err(refused("a list's item has no live or secret field"));
        }
        () if place == Place::Entry && live => return Err(refused("an entry's field is not live: the layers give it")),
        () if place == Place::Chosen && live => {
            return Err(refused("a chosen group's field is not live: the layers give it"));
        }
        () if required && map.contains_key("default") => return Err(refused("a required setting has no default")),
        () if place != Place::Item && !required && !map.contains_key("default") => {
            return Err(refused("a setting has a default, or is required"));
        }
        () => {}
    }
    let mut setting = Setting { kind, rules, default: None, required, live, secret };
    if let Some(default) = map.get("default") {
        // A rule's complaint quotes the value, which a secret's must not show.
        let checked = rules::check(&setting, default).map_err(|why| match setting.secret {
            true => refused("its default breaks a rule"),
            false => refused(&format!("its default: {why}")),
        });
        setting.default = Some(checked?);
    }
    Ok(setting)
}

/// A setting's kind; a problem with it, or with its list's items, names
/// where it stands.
fn kind(value: &Value, path: &str) -> Result<Kind, String> {
    Ok(match value.as_str() {
        Some("text") => Kind::Text,
        Some("integer") => Kind::Integer,
        Some("number") => Kind::Number,
        Some("boolean") => Kind::Boolean,
        Some("duration") => Kind::Duration,
        Some("object") => Kind::Object,
        Some("any") => Kind::Any,
        _ => match value.get("list") {
            Some(Value::Object(item)) => Kind::List(Box::new(list_item(item, path)?)),
            _ => return Err(format!("{path}: {} is no kind", shown(value))),
        },
    })
}

fn list_item(map: &Map<String, Value>, path: &str) -> Result<Item, String> {
    let items = format!("{path}[]");
    match map.get("group") {
        Some(Value::Object(group_map)) => Ok(Item::Group(group(group_map, &items, Place::Item)?)),
        Some(other) => Err(format!("{items}: a group of {} is no map", shown(other))),
        None => {
            let kind = kind(map.get("kind").unwrap_or(&Value::Null), &items)?;
            Ok(Item::Value { kind, rules: rules_of(map).map_err(|why| format!("{items}: {why}"))? })
        }
    }
}

fn rules_of(map: &Map<String, Value>) -> Result<Rules, String> {
    let bound = |name: &str| match map.get(name) {
        None => Ok(None),
        Some(value) => value.as_f64().map(Some).ok_or_else(|| format!("its {name} {} is no number", shown(value))),
    };
    let one_of = match map.get("oneOf") {
        None => None,
        Some(Value::Array(texts)) => Some(
            texts
                .iter()
                .map(|text| text.as_str().map(str::to_owned).ok_or_else(|| format!("{} is no text", shown(text))))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Some(other) => return Err(format!("its oneOf {} is no list", shown(other))),
    };
    Ok(Rules { min: bound("min")?, max: bound("max")?, one_of })
}

/// The layers an SDK sent, in the order the program wrote them.
#[cfg(all(feature = "config", any(test, feature = "wire", feature = "records")))]
pub(crate) fn layers(value: &Value) -> Result<Vec<Layer>, String> {
    let layers = value.as_array().ok_or_else(|| "layers are a list".to_owned())?;
    layers.iter().map(layer).collect()
}

#[cfg(all(feature = "config", any(test, feature = "wire", feature = "records")))]
fn layer(value: &Value) -> Result<Layer, String> {
    let map = value.as_object().ok_or_else(|| format!("{} is no layer", shown(value)))?;
    if let Some(object) = map.get("object") {
        let object = object.as_object().ok_or_else(|| "an object layer is a map".to_owned())?;
        let place = map.get("place").and_then(Value::as_u64).unwrap_or(0);
        return Ok(Layer::Object { place: usize::try_from(place).unwrap_or(usize::MAX), value: object.clone() });
    }
    if let Some(Value::Object(file)) = map.get("file") {
        let name = text(file, "name")?;
        let optional = file.get("optional").and_then(Value::as_bool).unwrap_or(false);
        let given = file.get("text").and_then(Value::as_str).map(str::to_owned);
        return Ok(Layer::File(File::sent(name, optional, given)));
    }
    if let Some(Value::Object(env)) = map.get("env") {
        return env_layer(env).map(Layer::Env);
    }
    Err(format!("{} is no layer: an object, a file or the environment", shown(value)))
}

#[cfg(all(feature = "config", any(test, feature = "wire", feature = "records")))]
fn env_layer(env: &Map<String, Value>) -> Result<Env, String> {
    let prefix = env.get("prefix").and_then(Value::as_str).unwrap_or_default();
    let mut sent = Env::prefix(prefix);
    for (path, name) in texts(env, "vars")? {
        sent = sent.var(&path, &name);
    }
    let dotenv = match env.get("dotenv") {
        None => Vec::new(),
        Some(Value::Array(files)) => files
            .iter()
            .map(|file| {
                let file = file.as_object().ok_or_else(|| "a .env file is a map".to_owned())?;
                Ok((text(file, "name")?, text(file, "text")?))
            })
            .collect::<Result<Vec<_>, String>>()?,
        Some(other) => return Err(format!("dotenv {} is no list", shown(other))),
    };
    Ok(sent.sent(texts(env, "variables")?, texts(env, "files")?.into_iter().collect(), dotenv))
}

/// A map of texts, none when absent.
#[cfg(all(feature = "config", any(test, feature = "wire", feature = "records")))]
fn texts(map: &Map<String, Value>, name: &str) -> Result<Vec<(String, String)>, String> {
    match map.get(name) {
        None => Ok(Vec::new()),
        Some(Value::Object(pairs)) => pairs
            .iter()
            .map(|(key, value)| {
                let value = value.as_str().ok_or_else(|| format!("{name}: {key}: {} is no text", shown(value)))?;
                Ok((key.clone(), value.to_owned()))
            })
            .collect(),
        Some(other) => Err(format!("{name}: {} is no map of texts", shown(other))),
    }
}

#[cfg(all(feature = "config", any(test, feature = "wire", feature = "records")))]
fn text(map: &Map<String, Value>, name: &str) -> Result<String, String> {
    map.get(name).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| format!("{name} is missing"))
}

#[cfg(all(test, feature = "config"))]
#[path = "data_tests.rs"]
mod tests;
