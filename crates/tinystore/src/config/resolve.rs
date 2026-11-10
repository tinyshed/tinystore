//! A config's value: what its layers give, then the changes kept in the
//! store applied in the order they were made, each taken while the value it
//! makes still reads as the program's type and passes its `validate`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::declaration::{Declaration, Group, Item, Kind, Node, Setting, at, gives_secret, holds_secret};
use super::explain::Origin;
use super::hub::{Change, Kept};
use super::layers::{Base, Layer, Stacked, not_read_from, set};
use super::marks::marked;
use super::rules;

/// A program's own check of its whole value, which says why it refuses one.
pub(super) type Validate<T> = Box<dyn Fn(&T) -> Result<(), String> + Send + Sync>;

/// How a value reaches its program: as a Rust type reads it, each span the
/// seconds and nanoseconds of a `Duration`, or as data, each span its
/// milliseconds, for an SDK that spells them in its own language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum View {
    Rust,
    #[cfg_attr(
        not(all(feature = "config", any(test, feature = "wire", feature = "records"))),
        expect(dead_code, reason = "only an SDK over the wire and a store's logger send it")
    )]
    Data,
}

/// What a config's value is made with: its declaration, the program's check
/// of it, and the view it reaches the program in.
pub(super) struct Making<'a, T> {
    pub(super) declaration: &'a Declaration,
    pub(super) validate: Option<&'a Validate<T>>,
    pub(super) view: View,
}

/// A config's value, and what of the kept changes it took.
#[derive(Debug)]
pub(super) struct Resolved<T> {
    pub(super) value: Arc<T>,
    /// The value as the config keeps it: each span as its milliseconds.
    pub(super) tree: Value,
    /// The changes it took, by the setting's path.
    pub(super) applied: BTreeMap<String, Change>,
    /// The changes it left out, and why.
    pub(super) ignored: Vec<(String, Change, String)>,
    /// The changes of settings this declaration does not have, which another
    /// program's may.
    pub(super) foreign: Vec<String>,
}

impl<T: Serialize + DeserializeOwned> Making<'_, T> {
    /// The value the layers alone make, or every problem with it, each named
    /// by the layer whose value broke it.
    pub(super) fn layered(&self, base: &Base) -> Result<(T, Value), Vec<String>> {
        let tree = base.tree();
        let value = self.built(&tree).map_err(|why| self.culprits(base, &tree, &why))?;
        if let Some(validate) = self.validate {
            validate(&value).map_err(|why| vec![why])?;
        }
        Ok((value, tree))
    }

    /// The value with each kept change that keeps it good, from the value
    /// the layers make.
    pub(super) fn resolve(&self, layered: (&Arc<T>, &Value), kept: &Kept) -> Resolved<T> {
        let (mut value, mut tree) = (Arc::clone(layered.0), layered.1.clone());
        let (mut applied, mut ignored, mut foreign) = (BTreeMap::new(), Vec::new(), Vec::new());
        for (path, change) in kept.in_order() {
            if self.declaration.node(path).is_none() {
                foreign.push(path.clone());
                continue;
            }
            match self.applied_to(&tree, path, &change.value) {
                Ok((next, next_tree)) => {
                    (value, tree) = (Arc::new(next), next_tree);
                    applied.insert(path.clone(), change.clone());
                }
                Err(why) => ignored.push((path.clone(), change.clone(), why)),
            }
        }
        Resolved { value, tree, applied, ignored, foreign }
    }

    /// `tree` with `value` at the live setting `path`, and the value it makes.
    fn applied_to(&self, tree: &Value, path: &str, value: &Value) -> Result<(T, Value), String> {
        let setting = self.declaration.setting(path).ok_or_else(|| "a group, not a setting".to_owned())?;
        if setting.secret {
            return Err("a secret, which only a layer gives".to_owned());
        }
        if !setting.live {
            return Err("not live".to_owned());
        }
        let value = rules::check(setting, value)?;
        let mut next = tree.clone();
        set(&mut next, path, value);
        let built = self.built(&next).map_err(|why| said(setting, &why))?;
        // Another program's type of a map may hold no secret where this one's
        // does, so its change is read as this type writes it, with its marks.
        let read = marked(&built).map_err(|_| "does not write as its type".to_owned())?;
        if at(&read, path).is_some_and(holds_secret) {
            return Err("it holds a secret, which only a layer gives".to_owned());
        }
        if let Some(validate) = self.validate {
            validate(&built)?;
        }
        Ok((built, next))
    }

    /// The program's value from what the config keeps, in its view.
    pub(super) fn built(&self, tree: &Value) -> Result<T, String> {
        let viewed = match self.view {
            View::Rust => rust_view(self.declaration, tree),
            View::Data => tree.clone(),
        };
        serde_json::from_value(viewed).map_err(|error| error.to_string())
    }

    /// What broke the value the layers make, each named by its layer: every
    /// value a layer gave is put back to its default, then given again alone.
    fn culprits(&self, base: &Base, tree: &Value, why: &str) -> Vec<String> {
        let settings: BTreeMap<String, &Setting> = self.declaration.settings_of(&base.shape).into_iter().collect();
        // A problem no one value is found to make, as of a required setting,
        // which has no default to put back, may quote any setting's value.
        let unplaced = if settings.values().any(|setting| setting.may_hold_secret()) {
            without_value(why)
        } else {
            why.to_owned()
        };
        let given: Vec<(&String, &Stacked, &Value)> = base
            .stacks
            .iter()
            .filter_map(|(path, stack)| {
                let last = stack.last().filter(|last| last.from != Origin::Declaration)?;
                let default = settings.get(path)?.default.as_ref()?;
                Some((path, last, default))
            })
            .collect();
        let mut clean = tree.clone();
        for (path, _, default) in &given {
            set(&mut clean, path, (*default).clone());
        }
        if self.built(&clean).is_err() {
            return vec![unplaced];
        }
        let found: Vec<String> = given
            .iter()
            .filter_map(|(path, last, _)| {
                let mut alone = clean.clone();
                set(&mut alone, path, last.value.clone());
                let why = self.built(&alone).err()?;
                let shown = settings.get(path.as_str()).map_or_else(|| why.clone(), |setting| said(setting, &why));
                Some(named(&last.from, path, &shown))
            })
            .collect();
        if found.is_empty() { vec![unplaced] } else { found }
    }
}

/// What serde says of a value a setting was given, as an error says it: of a
/// secret, nothing; of a value that may hold one, the type's words alone.
fn said(setting: &Setting, why: &str) -> String {
    if setting.secret {
        "a secret that does not read as its type".to_owned()
    } else if setting.kind.hides_secret() {
        without_value(why)
    } else {
        why.to_owned()
    }
}

/// serde's complaint in the words of the type alone: the field it misses, or
/// what it expected and which complaint it is. The value serde quotes is
/// left out, and any other message, a type's own, which may quote anything,
/// is not said.
///
/// ```text
/// invalid type: integer `482913`, expected a string   invalid type, expected a string
/// unknown variant `s3cr3t`, expected `sms` or `call`  unknown variant, expected `sms` or `call`
/// missing field `token`                               missing field `token`
/// relative URL without a base                         does not read as its type
/// ```
fn without_value(why: &str) -> String {
    const COMPLAINTS: [&str; 5] =
        ["invalid type", "invalid value", "invalid length", "unknown variant", "unknown field"];
    let missing = why.strip_prefix("missing field `").and_then(|rest| rest.strip_suffix('`'));
    if missing.is_some_and(|field| !field.contains('`')) {
        return why.to_owned();
    }
    // the last `, expected ` is serde's own: a quoted string before it may hold one too
    let typed = why.rsplit_once(", expected ").and_then(|(complaint, expected)| {
        let complaint = COMPLAINTS.into_iter().find(|known| complaint.starts_with(*known))?;
        Some(format!("{complaint}, expected {expected}"))
    });
    typed.unwrap_or_else(|| "does not read as its type".to_owned())
}

impl<T: Serialize + DeserializeOwned> Making<'_, T> {
    /// `value` put at `path` of `tree` and read as the program's type, then
    /// written back with its marks: what the type makes of that value, such
    /// as where its secrets are. None when the value does not read as the
    /// type, and null when the type writes nothing at `path`.
    pub(super) fn written_at(&self, tree: &Value, path: &str, value: &Value) -> Option<Value> {
        let mut probe = tree.clone();
        set(&mut probe, path, value.clone());
        let read = marked(&self.built(&probe).ok()?).ok()?;
        Some(at(&read, path).cloned().unwrap_or(Value::Null))
    }

    /// Refuses each value that a file that refuses secrets gave a map, a list
    /// or any value whose type holds a secret inside, which the declaration
    /// cannot see: a `BTreeMap` of a struct with a `Secret` field. Each such
    /// value is read as the program's type, whose marks say where its
    /// secrets are, as `explain` finds them.
    pub(super) fn secrets_inside(&self, base: &Base, layers: &[Layer]) -> Result<(), Vec<String>> {
        let refusing: Vec<String> = layers
            .iter()
            .filter_map(|layer| match layer {
                Layer::File(file) if !file.gives_secrets() => Some(file.shown()),
                _ => None,
            })
            .collect();
        if refusing.is_empty() {
            return Ok(());
        }
        let tree = base.tree();
        let mut found = Vec::new();
        for (path, setting) in self.declaration.settings_of(&base.shape) {
            if !matches!(setting.kind, Kind::Object | Kind::List(_) | Kind::Any) {
                continue;
            }
            for stacked in base.stacks.get(&path).into_iter().flatten() {
                let Origin::File { path: file } = &stacked.from else { continue };
                let read = || self.written_at(&tree, &path, &stacked.value);
                if refusing.contains(file) && read().is_some_and(|read| gives_secret(&stacked.value, &read)) {
                    found.push(format!("{file}: {}", not_read_from(file, &path, layers)));
                }
            }
        }
        if found.is_empty() { Ok(()) } else { Err(found) }
    }
}

/// A problem as the layer whose value it is about says it.
fn named(from: &Origin, path: &str, why: &str) -> String {
    match from {
        Origin::Env { .. } => format!("{from}: {why}"),
        Origin::File { .. } | Origin::Layer { .. } => format!("{from}: {path}: {why}"),
        Origin::Declaration | Origin::Change { .. } => format!("{path}: {why}"),
    }
}

/// The value as serde reads it into a Rust program's type: each span as the
/// seconds and nanoseconds `std::time::Duration` is written with.
fn rust_view(declaration: &Declaration, tree: &Value) -> Value {
    let mut tree = tree.clone();
    view_group(&declaration.root, &mut tree);
    tree
}

fn view_group(group: &Group, tree: &mut Value) {
    let Value::Object(map) = tree else { return };
    for (key, node) in group {
        // read before the value is borrowed to change it
        let picked = match node {
            Node::Chosen(chosen) => chosen.named(map.get(&chosen.by)).map(|(_, inner)| inner),
            _ => None,
        };
        let Some(value) = map.get_mut(key) else { continue };
        match node {
            Node::Group(inner) => view_group(inner, value),
            Node::Setting(setting) => view_kind(&setting.kind, value),
            Node::Entries(entries) => {
                if let Value::Object(each) = value {
                    each.values_mut().for_each(|entry| view_group(&entries.item, entry));
                }
            }
            Node::Chosen(_) => picked.into_iter().for_each(|inner| view_group(inner, value)),
        }
    }
}

fn view_kind(kind: &Kind, value: &mut Value) {
    match (kind, value) {
        (Kind::Duration, value) => {
            if let Some(millis) = value.as_u64() {
                *value = json!({ "secs": millis / 1000, "nanos": millis % 1000 * 1_000_000 });
            }
        }
        (Kind::List(item), Value::Array(items)) => {
            for item_value in items {
                match &**item {
                    Item::Value { kind, .. } => view_kind(kind, item_value),
                    Item::Group(group) => view_group(group, item_value),
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::without_value;

    // contract: CFG-06
    #[test]
    fn serde_is_said_in_the_types_words_and_never_with_the_value_it_quotes() {
        let said = [
            ("invalid type: integer `482913`, expected a string", "invalid type, expected a string"),
            (r#"invalid value: string "s3, expected cr3t", expected u16"#, "invalid value, expected u16"),
            ("unknown variant `s3cr3t`, expected `sms` or `call`", "unknown variant, expected `sms` or `call`"),
            ("missing field `token`", "missing field `token`"),
            ("missing field `s3cr3t` or `token`", "does not read as its type"),
            ("relative URL without a base", "does not read as its type"),
            ("not a port, expected 1 to 65535: s3cr3t", "does not read as its type"),
        ];
        for (serde, shown) in said {
            assert_eq!(without_value(serde), shown, "{serde}");
        }
    }
}
