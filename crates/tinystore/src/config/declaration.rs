//! A declaration as the core reads it, whichever language wrote it: its
//! groups and settings, and of each setting its kind, rules, default and
//! marks. A Rust program's comes from its value serialized with the marks
//! (`marks::marked`), or as data, the JSON an SDK sends over the wire, which
//! `data` reads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::{Map, Value, json};

use super::layers::set;
use super::marks::{ENTRIES, ENTRIES_GIVEN, ENTRIES_ITEM, LIVE, REQUIRED, SECRET, SECRET_REQUIRED};
use crate::Error;

/// The longest path of a setting, its keys joined by dots.
pub(super) const MOST_PATH: usize = 256;

/// The settings and groups of a group, by key.
pub(crate) type Group = BTreeMap<String, Node>;

/// The keys of the entries the declaration and the layers made, by the path
/// of their map.
pub(crate) type Keys = BTreeMap<String, BTreeSet<String>>;

/// The name of the group each chosen group's chooser picked, by the chosen
/// group's path.
pub(crate) type Choices = BTreeMap<String, String>;

/// What the layers made of a declaration whose shape they decide: the
/// entries of each map, and the group each chooser picked.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Shape {
    pub(crate) entries: Keys,
    pub(crate) chosen: Choices,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Group(Group),
    Setting(Setting),
    Entries(Entries),
    Chosen(Chosen),
}

/// A group whose settings are those of one of `groups`, the one a text
/// setting beside it names: an instance's `settings` are the group of the
/// plugin its `plugin` names. That chooser takes no value but a group's name.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chosen {
    pub(crate) by: String,
    pub(crate) groups: BTreeMap<String, Group>,
}

impl Chosen {
    /// The group `siblings` choose, the settings of the group the chosen one
    /// stands in, as a layer gave them: its chooser's value there, else the
    /// chooser's default in `group`.
    pub(crate) fn picked<'a>(&'a self, siblings: &Map<String, Value>, group: &Group) -> Option<(&'a str, &'a Group)> {
        let default = match group.get(&self.by) {
            Some(Node::Setting(chooser)) => chooser.default.as_ref(),
            _ => None,
        };
        self.named(siblings.get(&self.by).filter(|value| !value.is_null()).or(default))
    }

    /// The group a chooser's value names, and its name.
    pub(crate) fn named(&self, value: Option<&Value>) -> Option<(&str, &Group)> {
        let (name, group) = self.groups.get_key_value(value?.as_str()?)?;
        Some((name.as_str(), group))
    }
}

/// A chosen group of a declaration, wherever the entries it stands in put it.
pub(crate) struct ChosenAt<'a> {
    pub(crate) path: String,
    /// The path of the setting that chooses its group.
    pub(crate) chooser: String,
    pub(crate) chosen: &'a Chosen,
    /// The group its chooser's default picks.
    pub(crate) default: Option<&'a str>,
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

/// A program's settings declared as data, the JSON every SDK sends, for
/// settings whose shape the program learns only as it runs, such as the
/// settings of a plugin it loads. A config opened with one reads the same
/// layers, rules, secrets and `explain` as one declared by a type, and gives
/// its value as a `serde_json::Value`.
///
/// ```
/// use serde_json::json;
/// use tinystore::config::{self, Declaration, Env};
///
/// # fn main() -> tinystore::Result<()> {
/// let postgres = json!({
///     "host": { "kind": "text", "default": "localhost" },
///     "port": { "kind": "integer", "default": 5432, "min": 1, "max": 65535 },
///     "password": { "kind": "text", "secret": true, "required": true },
/// });
/// let stripe = json!({ "key": { "kind": "text", "secret": true, "required": true } });
/// let declaration = Declaration::from_json(json!({
///     "instance": { "entries": {
///         "plugin": { "kind": "text", "required": true },
///         "settings": { "chosenBy": "plugin", "groups": { "postgres": postgres, "stripe": stripe } },
///     } },
/// }))?;
/// let variables = [
///     ("DASHBIN_INSTANCE", r#"{ "aimee": { "plugin": "postgres" } }"#),
///     ("DASHBIN_INSTANCE_AIMEE_SETTINGS_PASSWORD", "s3cret"),
/// ];
/// let config = config::load("dashbin", declaration).env(Env::prefix("DASHBIN").from(variables)).open()?;
/// assert_eq!(config.get()["instance"]["aimee"]["settings"]["port"], 5432);
/// # Ok(())
/// # }
/// ```
///
/// A setting is `{ "kind": …, "default": … }`, its kind `text`, `integer`,
/// `number`, `boolean`, `duration`, `object`, `any` or `{ "list": … }`, with
/// `required`, `secret`, `live`, `min`, `max` and `oneOf` beside it;
/// `"default": null` declares one with no value, null until a layer gives
/// it, as an `Option` left `None`. A group is `{ "group": { … } }`, a map of
/// entries `{ "entries": { … } }`, and a group a setting beside it chooses
/// `{ "chosenBy": "plugin", "groups": { … } }`. A declaration that means
/// nothing is refused here, naming where.
#[derive(Clone, PartialEq)]
pub struct Declaration {
    pub(crate) root: Group,
}

impl Declaration {
    pub fn from_json(declaration: Value) -> crate::Result<Declaration> {
        super::data::declaration(&declaration).map_err(|why| Error::invalid(format!("the declaration: {why}")))
    }

    /// A Rust program's declaration: its value as `marks::marked` writes it.
    pub(crate) fn of_marked(value: &Value) -> Result<Declaration, String> {
        match value {
            Value::Object(map) => Ok(Declaration { root: group(map, "", Place::Root)? }),
            other => Err(format!("a declaration is a struct of settings, not {}", shown(other))),
        }
    }

    /// Makes a secret of each setting a Rust program's value leaves `None`
    /// whose type holds one. `Option<Secret<String>>` serializes `None` as a
    /// bare null, which says nothing of the `Secret` inside, so the setting
    /// would read any value and print it. Each null setting is given text
    /// once in the value as the config keeps it, and `reads_secret`, the
    /// program's type reading that value at a path, says whether the text
    /// became a secret.
    pub(crate) fn find_optional_secrets(&mut self, marked: &Value, reads_secret: impl Fn(&Value, &str) -> bool) {
        let probe = Probe { marked, reads_secret: &reads_secret };
        probe.group(&mut self.root, "", &unmarked(marked));
    }

    /// The node at a path of keys joined by dots, `limits.rps`, or a field of
    /// an entry, `instance.aimee.settings.password`, whichever entry it is,
    /// and of a chosen group, of the first of its groups that has it.
    pub(crate) fn node(&self, path: &str) -> Option<&Node> {
        let mut keys = path.split('.');
        let mut node = self.root.get(keys.next()?)?;
        while let Some(key) = keys.next() {
            node = match node {
                Node::Group(group) => group.get(key)?,
                // `key` names the entry, and the next key its field
                Node::Entries(entries) => entries.item.get(keys.next()?)?,
                Node::Chosen(chosen) => chosen.groups.values().find_map(|group| group.get(key))?,
                Node::Setting(_) => return None,
            };
        }
        Some(node)
    }

    /// The declaration as JSON, as kv.db keeps it: each setting with its
    /// kind, rules, default and marks, a secret's default kept as none.
    pub(crate) fn to_json(&self) -> Value {
        group_json(&self.root)
    }

    pub(crate) fn setting(&self, path: &str) -> Option<&Setting> {
        match self.node(path)? {
            Node::Setting(setting) => Some(setting),
            Node::Group(_) | Node::Entries(_) | Node::Chosen(_) => None,
        }
    }

    /// Every setting and its path, in the order of their keys; no entry's,
    /// and no chosen group's.
    pub(crate) fn settings(&self) -> Vec<(String, &Setting)> {
        self.settings_of(&Shape::default())
    }

    /// Every setting and its path, in the order of their keys, the fields of
    /// each entry `shape` names and of each group it says was chosen among
    /// them.
    pub(crate) fn settings_of(&self, shape: &Shape) -> Vec<(String, &Setting)> {
        let mut settings = Vec::new();
        collect(&self.root, "", shape, &mut settings);
        settings
    }

    /// Every map of entries and its path.
    pub(crate) fn entries(&self) -> Vec<(String, &Entries)> {
        let mut found = Vec::new();
        entries_in(&self.root, "", &mut found);
        found
    }

    /// Every chosen group, of each entry `keys` names too.
    pub(crate) fn chosen(&self, keys: &Keys) -> Vec<ChosenAt<'_>> {
        let mut found = Vec::new();
        chosen_in(&self.root, "", keys, &mut found);
        found
    }

    /// The group each chooser picks: its value as `value` gives it by its
    /// path, else its default.
    pub(crate) fn choose<'v>(&self, keys: &Keys, value: impl Fn(&str) -> Option<&'v Value>) -> Choices {
        let picked = |at: &ChosenAt<'_>| match value(&at.chooser) {
            Some(given) => at.chosen.named(Some(given)).map(|(name, _)| name.to_owned()),
            None => at.default.map(str::to_owned),
        };
        self.chosen(keys).into_iter().filter_map(|at| Some((at.path.clone(), picked(&at)?))).collect()
    }
}

/// A declaration as JSON, without a secret's default, which `Debug` would
/// otherwise print.
impl fmt::Debug for Declaration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Declaration").field(&self.to_json()).finish()
    }
}

fn group_json(group: &Group) -> Value {
    let entries = group.iter().map(|(key, node)| {
        let value = match node {
            Node::Group(inner) => json!({ "group": group_json(inner) }),
            Node::Setting(setting) => setting_json(setting),
            Node::Entries(entries) => entries_json(entries),
            Node::Chosen(chosen) => chosen_json(chosen),
        };
        (key.clone(), value)
    });
    Value::Object(entries.collect())
}

/// A chosen group as a map given whole, for an older release, its groups
/// beside it, which that release leaves out.
fn chosen_json(chosen: &Chosen) -> Value {
    let groups: Map<String, Value> =
        chosen.groups.iter().map(|(name, group)| (name.clone(), group_json(group))).collect();
    json!({ "kind": "object", "default": {}, "chosenBy": chosen.by, "groups": groups })
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
        // a group no chooser picks cannot say which of its fields are secrets
        Some(Node::Chosen(chosen)) => {
            chosen.picked(map, group).map(|(_, fields)| (key.clone(), without_secrets(fields, inner)))
        }
        _ => Some((key.clone(), inner.clone())),
    });
    Value::Object(kept.collect())
}

fn setting_json(setting: &Setting) -> Value {
    let mut map = Map::new();
    map.insert("kind".to_owned(), kind_json(&setting.kind));
    rules_json(&setting.rules, &mut map);
    // A secret's default is never kept, and a setting with none would not
    // read back, so it is kept as one with no value.
    let kept = |default: &Value| if setting.secret { Value::Null } else { default.clone() };
    if let Some(default) = &setting.default {
        map.insert("default".to_owned(), kept(default));
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
/// setting; a field of an entry or of a chosen group may be a secret, and is
/// never live, since only the layers make an entry or choose a group.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Place {
    Root,
    Item,
    Entry,
    Chosen,
}

fn collect<'a>(group: &'a Group, path: &str, shape: &Shape, settings: &mut Vec<(String, &'a Setting)>) {
    for (key, node) in group {
        let at = joined(path, key);
        match node {
            Node::Group(inner) => collect(inner, &at, shape, settings),
            Node::Setting(setting) => settings.push((at, setting)),
            Node::Entries(entries) => {
                for entry in shape.entries.get(&at).into_iter().flatten() {
                    collect(&entries.item, &joined(&at, entry), shape, settings);
                }
            }
            Node::Chosen(chosen) => {
                if let Some(inner) = shape.chosen.get(&at).and_then(|name| chosen.groups.get(name)) {
                    collect(inner, &at, shape, settings);
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
            Node::Setting(_) | Node::Chosen(_) => {}
        }
    }
}

fn chosen_in<'a>(group: &'a Group, path: &str, keys: &Keys, found: &mut Vec<ChosenAt<'a>>) {
    for (key, node) in group {
        let at = joined(path, key);
        match node {
            Node::Group(inner) => chosen_in(inner, &at, keys, found),
            Node::Entries(entries) => {
                for entry in keys.get(&at).into_iter().flatten() {
                    chosen_in(&entries.item, &joined(&at, entry), keys, found);
                }
            }
            Node::Chosen(chosen) => {
                let default = chosen.picked(&Map::new(), group).map(|(name, _)| name);
                found.push(ChosenAt { path: at, chooser: joined(path, &chosen.by), chosen, default });
            }
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
    match kind {
        _ if !marks.secret || kind == Kind::Text => {}
        // `Secret::new(None)`, whose mark a probe never needs to find
        Kind::Any => return Err(format!("{path}: a secret is text; one with no value is Option<Secret<String>>")),
        _ => return Err(format!("{path}: a secret is text")),
    }
    let default = match place {
        _ if marks.required => None,
        Place::Root | Place::Entry | Place::Chosen => Some(unmarked(value)),
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

/// The value at a path of keys joined by dots, through a map of entries'
/// mark to its entries in a value written with its marks.
pub(crate) fn at<'a>(tree: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(tree, |node, key| match node.get(ENTRIES) {
        Some(entries) => entries.get(ENTRIES_GIVEN)?.get(key),
        None => node.get(key),
    })
}

/// What finds the secrets a Rust program's value leaves `None`: the value
/// with its marks, and its type, which says whether text at a path of a
/// value it reads is a secret.
struct Probe<'a> {
    marked: &'a Value,
    reads_secret: &'a dyn Fn(&Value, &str) -> bool,
}

/// The key of the entry a probe makes in each map of entries.
const PROBED: &str = "probed";

impl Probe<'_> {
    /// Each null setting of `group`, read in `tree`, a value the program's
    /// type reads, which has the group at `path`.
    fn group(&self, group: &mut Group, path: &str, tree: &Value) {
        for (key, node) in group.iter_mut() {
            let here = joined(path, key);
            match node {
                Node::Group(inner) => self.group(inner, &here, tree),
                // an entry as its item's default makes it, read there
                Node::Entries(entries) => {
                    let entry = joined(&here, PROBED);
                    let item = at(self.marked, &here).and_then(|map| map.get(ENTRIES)?.get(ENTRIES_ITEM));
                    let mut with_entry = tree.clone();
                    set(&mut with_entry, &entry, item.map(unmarked).unwrap_or_default());
                    self.group(&mut entries.item, &entry, &with_entry);
                }
                Node::Setting(setting) if setting.kind == Kind::Any && setting.default == Some(Value::Null) => {
                    if (self.reads_secret)(tree, &here) {
                        (setting.kind, setting.secret) = (Kind::Text, true);
                    }
                }
                Node::Setting(_) | Node::Chosen(_) => {}
            }
        }
    }
}

/// `given` with each value at a place where `read` holds a secret as `***`.
/// `read` is the same value as the program's type writes it with its marks,
/// which finds a secret inside a map, a list or any value, where the
/// declaration has one setting given whole.
pub(crate) fn masked(given: &Value, read: &Value) -> Value {
    each_secret(given, read, &mut |_| Value::from("***"))
}

/// Whether `given` has a value at a place where `read`, the same value as
/// the program's type writes it with its marks, holds a secret: a map that
/// gave an entry's host alone gives no secret, though its type holds one.
pub(crate) fn gives_secret(given: &Value, read: &Value) -> bool {
    let mut gives = false;
    each_secret(given, read, &mut |value| {
        gives |= !value.is_null();
        value.clone()
    });
    gives
}

/// `given` with each value at a place where `read` holds a secret made
/// what `secret` makes of it, the rest as it was given.
fn each_secret(given: &Value, read: &Value, secret: &mut dyn FnMut(&Value) -> Value) -> Value {
    if let Value::Object(map) = read
        && let Some((key, inner)) = mark(map)
    {
        return if key == SECRET || key == SECRET_REQUIRED { secret(given) } else { each_secret(given, inner, secret) };
    }
    match (given, read) {
        (Value::Object(given), Value::Object(read)) => {
            let each = given.iter().map(|(key, value)| {
                let made = read.get(key).map_or_else(|| value.clone(), |read| each_secret(value, read, secret));
                (key.clone(), made)
            });
            Value::Object(each.collect())
        }
        (Value::Array(given), Value::Array(read)) => {
            let each = given.iter().zip(read).map(|(value, read)| each_secret(value, read, secret));
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
