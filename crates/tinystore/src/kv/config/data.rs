//! A config as an SDK sends it over the wire: its declaration as data, in the
//! JSON `Declaration::to_json` writes, and its layers with what the program
//! read itself, a file's text and its variables, since those belong to the
//! program and not to the server.
//!
//! ```text
//! declaration  { "port": { "kind": "integer", "default": 8080, "min": 1 },
//!                "db": { "group": { "url": { "kind": "text", "secret": true, "required": true } } },
//!                "hosts": { "kind": { "list": { "kind": "text" } }, "default": [], "max": 20 } }
//! layers       [ { "object": { "port": 80 }, "place": 2 },
//!                { "file": { "name": "app.yaml", "text": "port: 9000\n", "optional": false } },
//!                { "env": { "prefix": "APP", "vars": { "port": "PORT" }, "variables": { "PORT": "3000" },
//!                           "files": { "/run/secrets/db": "postgres://…" }, "dotenv": [] } } ]
//! ```

use serde_json::{Map, Value};

use super::declaration::{Declaration, Group, Item, Kind, Node, Place, Rules, Setting, declared_path, shown};
use super::env::Env;
use super::files::File;
use super::layers::Layer;
use super::rules;

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
        let node = match node.get("group") {
            Some(Value::Object(inner)) => Node::Group(group(inner, &at, place)?),
            Some(other) => return Err(format!("{at}: a group of {} is no map", shown(other))),
            None => Node::Setting(setting(node, &at, place)?),
        };
        settings.insert(key.clone(), node);
    }
    Ok(settings)
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
        () if required && map.contains_key("default") => return Err(refused("a required setting has no default")),
        () if place == Place::Root && !required && !map.contains_key("default") => {
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
pub(crate) fn layers(value: &Value) -> Result<Vec<Layer>, String> {
    let layers = value.as_array().ok_or_else(|| "layers are a list".to_owned())?;
    layers.iter().map(layer).collect()
}

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

fn text(map: &Map<String, Value>, name: &str) -> Result<String, String> {
    map.get(name).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| format!("{name} is missing"))
}

#[cfg(test)]
#[path = "data_tests.rs"]
mod tests;
