//! A declaration as the core reads it, whichever language wrote it: its
//! groups and settings, and of each setting its kind, rules, default and
//! marks. A Rust program's comes from its value serialized with the marks
//! (`marks::marked`); an SDK's will come over the wire as data.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::marks::{LIVE, REQUIRED, SECRET, SECRET_REQUIRED};

/// The longest path of a setting, its keys joined by dots.
pub(super) const MOST_PATH: usize = 256;

/// The settings and groups of a group, by key.
pub(crate) type Group = BTreeMap<String, Node>;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Group(Group),
    Setting(Setting),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Setting {
    pub(crate) kind: Kind,
    pub(crate) rules: Rules,
    /// What the setting is when no layer gives it; none for a required
    /// setting, and for a field of a list's item that its language fills.
    pub(crate) default: Option<Value>,
    pub(crate) required: bool,
    pub(crate) live: bool,
    pub(crate) secret: bool,
}

/// What a setting holds. A span is kept as milliseconds; a list is one
/// setting, given whole; an object is a map of any keys, given whole.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    Text,
    Integer,
    Number,
    Boolean,
    Duration,
    List(Box<Item>),
    Object,
    /// Whatever a layer gives: a setting whose default is null.
    Any,
}

/// The items of a list: values of one kind, or groups of settings.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    Value { kind: Kind, rules: Rules },
    Group(Group),
}

/// What one value says of itself: a range of a number, a span or a list's
/// length, and the texts a setting allows.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Rules {
    pub(crate) min: Option<f64>,
    pub(crate) max: Option<f64>,
    pub(crate) one_of: Option<Vec<String>>,
}

/// The milliseconds of a std `Duration`, which serde writes as its seconds
/// and nanoseconds, and which a config keeps and reads as a span.
pub(crate) fn duration_millis(map: &Map<String, Value>) -> Option<u64> {
    let (secs, nanos) = (map.get("secs")?.as_u64()?, map.get("nanos")?.as_u64()?);
    (map.len() == 2).then(|| secs.saturating_mul(1000).saturating_add(nanos / 1_000_000))
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Declaration {
    pub(crate) root: Group,
}

impl Declaration {
    /// A Rust program's declaration: its value as `marks::marked` writes it.
    pub(crate) fn of_marked(value: &Value) -> Result<Declaration, String> {
        match value {
            Value::Object(map) => Ok(Declaration { root: group(map, "", Place::Root)? }),
            other => Err(format!("a declaration is a struct of settings, not {}", shown(other))),
        }
    }

    /// The node at a path of keys joined by dots, `limits.rps`.
    pub(crate) fn node(&self, path: &str) -> Option<&Node> {
        let mut keys = path.split('.');
        let mut node = self.root.get(keys.next()?)?;
        for key in keys {
            let Node::Group(group) = node else { return None };
            node = group.get(key)?;
        }
        Some(node)
    }

    /// The declaration as JSON, as kv.db keeps it: each setting with its
    /// kind, rules, default and marks, a secret's default left out.
    pub(crate) fn to_json(&self) -> Value {
        group_json(&self.root)
    }

    pub(crate) fn setting(&self, path: &str) -> Option<&Setting> {
        match self.node(path)? {
            Node::Setting(setting) => Some(setting),
            Node::Group(_) => None,
        }
    }

    /// Every setting and its path, in the order of their keys.
    pub(crate) fn settings(&self) -> Vec<(String, &Setting)> {
        let mut settings = Vec::new();
        collect(&self.root, "", &mut settings);
        settings
    }
}

fn group_json(group: &Group) -> Value {
    let entries = group.iter().map(|(key, node)| {
        let value = match node {
            Node::Group(inner) => json!({ "group": group_json(inner) }),
            Node::Setting(setting) => setting_json(setting),
        };
        (key.clone(), value)
    });
    Value::Object(entries.collect())
}

fn setting_json(setting: &Setting) -> Value {
    let mut map = Map::new();
    map.insert("kind".to_owned(), kind_json(&setting.kind));
    rules_json(&setting.rules, &mut map);
    if let Some(default) = setting.default.as_ref().filter(|_| !setting.secret) {
        map.insert("default".to_owned(), default.clone());
    }
    for (mark, on) in [("required", setting.required), ("live", setting.live), ("secret", setting.secret)] {
        if on {
            map.insert(mark.to_owned(), Value::Bool(true));
        }
    }
    Value::Object(map)
}

fn kind_json(kind: &Kind) -> Value {
    let name = match kind {
        Kind::Text => "text",
        Kind::Integer => "integer",
        Kind::Number => "number",
        Kind::Boolean => "boolean",
        Kind::Duration => "duration",
        Kind::Object => "object",
        Kind::Any => "any",
        Kind::List(item) => {
            return match &**item {
                Item::Value { kind, rules } => {
                    let mut map = Map::from_iter([("kind".to_owned(), kind_json(kind))]);
                    rules_json(rules, &mut map);
                    json!({ "list": map })
                }
                Item::Group(group) => json!({ "list": { "group": group_json(group) } }),
            };
        }
    };
    Value::String(name.to_owned())
}

fn rules_json(rules: &Rules, map: &mut Map<String, Value>) {
    let bounds = [("min", rules.min), ("max", rules.max)];
    for (name, bound) in bounds {
        if let Some(number) = bound.and_then(serde_json::Number::from_f64) {
            map.insert(name.to_owned(), Value::Number(number));
        }
    }
    if let Some(allowed) = &rules.one_of {
        map.insert("oneOf".to_owned(), json!(allowed));
    }
}

/// Where a group stands, which says which marks its settings may carry: a
/// field of a list's item is neither live nor a secret, since a list is one
/// setting.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Place {
    Root,
    Item,
}

fn collect<'a>(group: &'a Group, path: &str, settings: &mut Vec<(String, &'a Setting)>) {
    for (key, node) in group {
        let at = joined(path, key);
        match node {
            Node::Group(inner) => collect(inner, &at, settings),
            Node::Setting(setting) => settings.push((at, setting)),
        }
    }
}

pub(crate) fn joined(path: &str, key: &str) -> String {
    if path.is_empty() { key.to_owned() } else { format!("{path}.{key}") }
}

/// The name a person may have meant by `given`, among `names`: one edit away
/// for a name of three to six letters, two for a longer one, none shorter.
/// It is only said: no name is read as another.
pub(crate) fn meant<S: AsRef<str>>(given: &str, names: impl IntoIterator<Item = S>) -> Option<String> {
    let edits = match given.chars().count() {
        0..3 => 0,
        3..7 => 1,
        _ => 2,
    };
    crate::typos::Dictionary::new(names).nearest(given, edits).map(|near| near.word)
}

/// What follows a problem's words when a name was perhaps misspelt.
pub(crate) fn did_you_mean(meant: Option<String>) -> String {
    meant.map_or_else(String::new, |name| format!("; did you mean {name}?"))
}

fn group(map: &Map<String, Value>, path: &str, place: Place) -> Result<Group, String> {
    let mut settings = Group::new();
    for (key, value) in map {
        let at = joined(path, key);
        if key.is_empty() || key.contains('.') {
            return Err(format!("{at:?}: a setting's key is not empty and has no dot"));
        }
        if at.len() > MOST_PATH {
            return Err(format!("{at}: a path past the {MOST_PATH} bytes a setting's takes"));
        }
        let node = match value {
            Value::Object(inner) if is_group(inner) => Node::Group(group(inner, &at, place)?),
            _ => Node::Setting(setting(value, &at, place)?),
        };
        settings.insert(key.clone(), node);
    }
    Ok(settings)
}

/// A struct's fields, not a map of any keys, a span or a mark.
fn is_group(map: &Map<String, Value>) -> bool {
    !map.is_empty() && mark(map).is_none() && duration_millis(map).is_none()
}

/// The key and the value of a mark, when `map` is one.
fn mark(map: &Map<String, Value>) -> Option<(&str, &Value)> {
    let (key, value) = map.iter().next()?;
    (map.len() == 1 && key.starts_with('\0')).then_some((key.as_str(), value))
}

/// The marks a setting carries, read from the outermost in.
#[derive(Default)]
struct Marks {
    required: bool,
    live: bool,
    secret: bool,
    seen: usize,
}

impl Marks {
    /// Takes the next mark in, refusing the orders that mean nothing: `live`
    /// is outermost, and a secret is neither live nor marked required again.
    fn take(&mut self, key: &str, path: &str, place: Place) -> Result<(), String> {
        let refused = |why: &str| Err(format!("{path}: {why}"));
        match key {
            LIVE if place == Place::Item => return refused("a list's item has no live field: the list is one setting"),
            LIVE if self.seen > 0 => return refused("live is the outermost mark"),
            LIVE => self.live = true,
            REQUIRED if self.required => return refused("required twice"),
            REQUIRED => self.required = true,
            SECRET | SECRET_REQUIRED if place == Place::Item => return refused("a list's item holds no secret"),
            SECRET | SECRET_REQUIRED if self.live => return refused("a secret is never live"),
            SECRET | SECRET_REQUIRED if self.required => return refused("a secret is required already"),
            SECRET | SECRET_REQUIRED => (self.secret, self.required) = (true, key == SECRET_REQUIRED),
            other => return refused(&format!("{other:?} is no mark")),
        }
        self.seen += 1;
        Ok(())
    }
}

fn setting(value: &Value, path: &str, place: Place) -> Result<Setting, String> {
    let mut marks = Marks::default();
    let mut value = value;
    while let Value::Object(map) = value {
        let Some((key, inner)) = mark(map) else { break };
        marks.take(key, path, place)?;
        value = inner;
    }
    if let Value::Object(map) = value
        && is_group(map)
    {
        return Err(format!("{path}: a mark goes on a setting, not on a group"));
    }
    let kind = kind(value, path)?;
    if marks.secret && kind != Kind::Text {
        return Err(format!("{path}: a secret is text"));
    }
    let default = match place {
        _ if marks.required => None,
        Place::Root => Some(unmarked(value)),
        Place::Item => None,
    };
    Ok(Setting {
        kind,
        rules: Rules::default(),
        default,
        required: marks.required,
        live: marks.live,
        secret: marks.secret,
    })
}

fn kind(value: &Value, path: &str) -> Result<Kind, String> {
    Ok(match value {
        Value::String(_) => Kind::Text,
        Value::Bool(_) => Kind::Boolean,
        Value::Number(number) if number.is_f64() => Kind::Number,
        Value::Number(_) => Kind::Integer,
        Value::Null => Kind::Any,
        Value::Array(items) => Kind::List(Box::new(item(items.first(), path)?)),
        Value::Object(map) if duration_millis(map).is_some() => Kind::Duration,
        Value::Object(map) if map.is_empty() => Kind::Object,
        Value::Object(_) => return Err(format!("{path}: a mark inside a value")),
    })
}

/// A list's items as its first item shows them; a list with none takes any.
fn item(first: Option<&Value>, path: &str) -> Result<Item, String> {
    let at = format!("{path}[0]");
    match first {
        None => Ok(Item::Value { kind: Kind::Any, rules: Rules::default() }),
        Some(Value::Object(map)) if is_group(map) => Ok(Item::Group(group(map, &at, Place::Item)?)),
        Some(value) => Ok(Item::Value { kind: kind(value, &at)?, rules: Rules::default() }),
    }
}

/// A value serialized with its marks, as a config keeps it: each mark's
/// value in its place, and a span as its milliseconds.
pub(crate) fn unmarked(value: &Value) -> Value {
    match value {
        Value::Object(map) => match (mark(map), duration_millis(map)) {
            (Some((_, inner)), _) => unmarked(inner),
            (None, Some(millis)) => Value::from(millis),
            (None, None) => Value::Object(map.iter().map(|(key, value)| (key.clone(), unmarked(value))).collect()),
        },
        Value::Array(items) => Value::Array(items.iter().map(unmarked).collect()),
        other => other.clone(),
    }
}

/// A value as an error quotes it, short.
pub(crate) fn shown(value: &Value) -> String {
    let text = value.to_string();
    match text.char_indices().nth(64) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

#[cfg(test)]
#[path = "declaration_tests.rs"]
mod tests;
