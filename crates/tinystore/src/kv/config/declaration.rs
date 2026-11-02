//! A declaration as the core reads it, whichever language wrote it: its
//! groups and settings, and of each setting its kind, rules, default and
//! marks. A Rust program's comes from its value serialized with the marks
//! (`marks::marked`); an SDK's comes over the wire as data, which `data`
//! reads.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use super::marks::{ENTRIES, ENTRIES_GIVEN, ENTRIES_ITEM, LIVE, REQUIRED, SECRET, SECRET_REQUIRED};

/// The longest path of a setting, its keys joined by dots.
pub(super) const MOST_PATH: usize = 256;

/// The settings and groups of a group, by key.
pub(crate) type Group = BTreeMap<String, Node>;

/// The keys of the entries the declaration and the layers made, by the path
/// of their map.
pub(crate) type Keys = BTreeMap<String, BTreeSet<String>>;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Group(Group),
    Setting(Setting),
    Entries(Entries),
}

/// A map of named entries, each the group `item` declares, whose fields are
/// settings of their own: `instance.aimee.settings.password`. A layer gives
/// an entry field by field, over what the layers before it gave the entry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entries {
    pub(crate) item: Group,
    /// The entries the declaration gives, by key, each as the config keeps
    /// a value.
    pub(crate) given: Map<String, Value>,
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

    /// The node at a path of keys joined by dots, `limits.rps`, or a field of
    /// an entry, `instance.aimee.settings.password`, whichever entry it is.
    pub(crate) fn node(&self, path: &str) -> Option<&Node> {
        let mut keys = path.split('.');
        let mut node = self.root.get(keys.next()?)?;
        while let Some(key) = keys.next() {
            node = match node {
                Node::Group(group) => group.get(key)?,
                // `key` names the entry, and the next key its field
                Node::Entries(entries) => entries.item.get(keys.next()?)?,
                Node::Setting(_) => return None,
            };
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
            Node::Group(_) | Node::Entries(_) => None,
        }
    }

    /// Every setting and its path, in the order of their keys; no entry's.
    pub(crate) fn settings(&self) -> Vec<(String, &Setting)> {
        self.settings_of(&Keys::new())
    }

    /// Every setting and its path, in the order of their keys, the fields of
    /// each entry `keys` names among them.
    pub(crate) fn settings_of(&self, keys: &Keys) -> Vec<(String, &Setting)> {
        let mut settings = Vec::new();
        collect(&self.root, "", keys, &mut settings);
        settings
    }

    /// Every map of entries and its path.
    pub(crate) fn entries(&self) -> Vec<(String, &Entries)> {
        let mut found = Vec::new();
        entries_in(&self.root, "", &mut found);
        found
    }
}

fn group_json(group: &Group) -> Value {
    let entries = group.iter().map(|(key, node)| {
        let value = match node {
            Node::Group(inner) => json!({ "group": group_json(inner) }),
            Node::Setting(setting) => setting_json(setting),
            Node::Entries(entries) => entries_json(entries),
        };
        (key.clone(), value)
    });
    Value::Object(entries.collect())
}

/// A map of entries as a map given whole, for an older release, which
/// leaves out what it does not know: its entries' group, and the entries the
/// declaration gives, without their secrets, which the store never keeps.
fn entries_json(entries: &Entries) -> Value {
    let given = entries.given.iter().map(|(key, entry)| (key.clone(), without_secrets(&entries.item, entry)));
    json!({ "kind": "object", "default": Value::Object(given.collect()), "entries": group_json(&entries.item) })
}

fn without_secrets(group: &Group, value: &Value) -> Value {
    let Value::Object(map) = value else { return value.clone() };
    let kept = map.iter().filter_map(|(key, inner)| match group.get(key) {
        Some(Node::Setting(setting)) if setting.secret => None,
        Some(Node::Group(fields)) => Some((key.clone(), without_secrets(fields, inner))),
        _ => Some((key.clone(), inner.clone())),
    });
    Value::Object(kept.collect())
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
/// setting; a field of an entry may be a secret, and is never live, since
/// only the layers make an entry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Place {
    Root,
    Item,
    Entry,
}

fn collect<'a>(group: &'a Group, path: &str, keys: &Keys, settings: &mut Vec<(String, &'a Setting)>) {
    for (key, node) in group {
        let at = joined(path, key);
        match node {
            Node::Group(inner) => collect(inner, &at, keys, settings),
            Node::Setting(setting) => settings.push((at, setting)),
            Node::Entries(entries) => {
                for entry in keys.get(&at).into_iter().flatten() {
                    collect(&entries.item, &joined(&at, entry), keys, settings);
                }
            }
        }
    }
}

fn entries_in<'a>(group: &'a Group, path: &str, found: &mut Vec<(String, &'a Entries)>) {
    for (key, node) in group {
        match node {
            Node::Group(inner) => entries_in(inner, &joined(path, key), found),
            Node::Entries(entries) => found.push((joined(path, key), entries)),
            Node::Setting(_) => {}
        }
    }
}

/// The path of the entry `key` names in the map at `path`: a key that is not
/// empty and has no dot, which would read as a field, and a path of at most
/// `MOST_PATH` bytes.
pub(crate) fn entry_path(path: &str, key: &str) -> Result<String, String> {
    let at = joined(path, key);
    if key.is_empty() || key.contains('.') {
        return Err(format!("{path}: {key:?}: an entry's key is not empty and has no dot"));
    }
    if at.len() > MOST_PATH {
        return Err(format!("{at}: a path past the {MOST_PATH} bytes a setting's takes"));
    }
    Ok(at)
}

/// The path of the setting `key` names in the group at `path`, as a
/// declaration may give it: a key that is not empty and has no dot, a path of
/// at most `MOST_PATH` bytes.
pub(super) fn declared_path(path: &str, key: &str) -> Result<String, String> {
    let at = joined(path, key);
    if key.is_empty() || key.contains('.') {
        return Err(format!("{at:?}: a setting's key is not empty and has no dot"));
    }
    if at.len() > MOST_PATH {
        return Err(format!("{at}: a path past the {MOST_PATH} bytes a setting's takes"));
    }
    Ok(at)
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
        let at = declared_path(path, key)?;
        let node = match value {
            Value::Object(inner) if mark(inner).is_some_and(|(mark, _)| mark == ENTRIES) => {
                Node::Entries(marked_entries(&inner[ENTRIES], &at, place)?)
            }
            Value::Object(inner) if is_group(inner) => Node::Group(group(inner, &at, place)?),
            _ => Node::Setting(setting(value, &at, place)?),
        };
        settings.insert(key.clone(), node);
    }
    Ok(settings)
}

/// A map of entries as `Entries` writes it while the core reads: the default
/// of its item, which declares each entry's fields, and the entries the
/// declaration gives.
fn marked_entries(value: &Value, path: &str, place: Place) -> Result<Entries, String> {
    if place != Place::Root {
        return Err(format!("{path}: entries stand among settings, not in an entry or a list's item"));
    }
    let item = match value.get(ENTRIES_ITEM) {
        Some(Value::Object(item)) if is_group(item) => group(item, &declared_path(path, "*")?, Place::Entry)?,
        _ => return Err(format!("{path}: an entry is a struct of settings; a map of values is a BTreeMap")),
    };
    let mut given = Map::new();
    for (key, entry) in value.get(ENTRIES_GIVEN).and_then(Value::as_object).into_iter().flatten() {
        entry_path(path, key)?;
        given.insert(key.clone(), given_value(entry).unwrap_or_default());
    }
    Ok(Entries { item, given })
}

/// A value with its marks taken off, as `unmarked`, and without what a mark
/// says is required, `Secret::default()` and `Required`, which a layer gives:
/// a declaration's value of it is never counted as given.
fn given_value(value: &Value) -> Option<Value> {
    let Value::Object(map) = value else { return Some(unmarked(value)) };
    match mark(map) {
        Some((REQUIRED | SECRET_REQUIRED, _)) => None,
        Some((_, inner)) => given_value(inner),
        None if duration_millis(map).is_some() => Some(unmarked(value)),
        None => Some(Value::Object(
            map.iter().filter_map(|(key, inner)| Some((key.clone(), given_value(inner)?))).collect(),
        )),
    }
}

/// A struct's fields, not a map of any keys, a span or a mark.
fn is_group(map: &Map<String, Value>) -> bool {
    !map.is_empty() && mark(map).is_none() && duration_millis(map).is_none()
}

/// The key and the value of a mark, when `map` is one: of a map of entries,
/// its entries, without its item's default.
fn mark(map: &Map<String, Value>) -> Option<(&str, &Value)> {
    static NO_ENTRIES: Value = Value::Null;
    let (key, value) = map.iter().next()?;
    if map.len() != 1 || !key.starts_with('\0') {
        return None;
    }
    let held = if key == ENTRIES { value.get(ENTRIES_GIVEN).unwrap_or(&NO_ENTRIES) } else { value };
    Some((key.as_str(), held))
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
            ENTRIES => return refused("entries take no mark: each of their fields carries its own"),
            LIVE if place == Place::Item => return refused("a list's item has no live field: the list is one setting"),
            LIVE if place == Place::Entry => return refused("an entry's field is not live: the layers give it"),
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
        Place::Root | Place::Entry => Some(unmarked(value)),
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

/// `given` with each value at a place where `read` holds a secret as `***`.
/// `read` is the same value as the program's type writes it with its marks,
/// which finds a secret inside a map, a list or any value, where the
/// declaration has one setting given whole.
pub(crate) fn masked(given: &Value, read: &Value) -> Value {
    if let Value::Object(map) = read
        && let Some((key, inner)) = mark(map)
    {
        return if key == SECRET || key == SECRET_REQUIRED { Value::from("***") } else { masked(given, inner) };
    }
    match (given, read) {
        (Value::Object(given), Value::Object(read)) => {
            let each = given.iter().map(|(key, value)| {
                (key.clone(), read.get(key).map_or_else(|| value.clone(), |read| masked(value, read)))
            });
            Value::Object(each.collect())
        }
        (Value::Array(given), Value::Array(read)) => {
            let each = given.iter().zip(read).map(|(value, read)| masked(value, read));
            Value::Array(each.chain(given.iter().skip(read.len()).cloned()).collect())
        }
        _ => given.clone(),
    }
}

/// Whether a value as the program's type writes it with its marks holds a
/// secret anywhere inside.
pub(crate) fn holds_secret(read: &Value) -> bool {
    match read {
        Value::Object(map) => match mark(map) {
            Some((key, inner)) => key == SECRET || key == SECRET_REQUIRED || holds_secret(inner),
            None => map.values().any(holds_secret),
        },
        Value::Array(items) => items.iter().any(holds_secret),
        _ => false,
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
