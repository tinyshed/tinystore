//! A config's value: what its layers give, then the changes kept in the
//! store applied in the order they were made, each taken while the value it
//! makes still reads as the program's type and passes its `validate`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::declaration::{Declaration, Group, Item, Kind, Node};
use super::hub::{Change, Kept};
use super::layers::{Base, Given, Source, set};
use super::rules;

/// A program's own check of its whole value, which says why it refuses one.
pub(super) type Validate<T> = Box<dyn Fn(&T) -> Result<(), String> + Send + Sync>;

/// How a value reaches its program: as a Rust type reads it, each span the
/// seconds and nanoseconds of a `Duration`, or as data, each span its
/// milliseconds, for an SDK that spells them in its own language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum View {
    Rust,
    #[cfg_attr(not(any(test, feature = "wire")), expect(dead_code, reason = "only an SDK sends it, over the wire"))]
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

impl<T: DeserializeOwned> Making<'_, T> {
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
        let built = self.built(&next)?;
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
        let given: Vec<(&String, &Given, &Value)> = base
            .stacks
            .iter()
            .filter_map(|(path, stack)| {
                let last = stack.last().filter(|last| last.from != Source::Declaration)?;
                let default = self.declaration.setting(path)?.default.as_ref()?;
                Some((path, last, default))
            })
            .collect();
        let mut clean = tree.clone();
        for (path, _, default) in &given {
            set(&mut clean, path, (*default).clone());
        }
        if self.built(&clean).is_err() {
            return vec![why.to_owned()];
        }
        let found: Vec<String> = given
            .iter()
            .filter_map(|(path, last, _)| {
                let mut alone = clean.clone();
                set(&mut alone, path, last.value.clone());
                let secret = self.declaration.setting(path).is_some_and(|setting| setting.secret);
                let shown =
                    |why: String| if secret { "a secret that does not read as its type".to_owned() } else { why };
                self.built(&alone).err().map(|why| named(&last.from, path, &shown(why)))
            })
            .collect();
        if found.is_empty() { vec![why.to_owned()] } else { found }
    }
}

/// A problem as the layer whose value it is about says it.
fn named(from: &Source, path: &str, why: &str) -> String {
    match from {
        Source::Variable { .. } => format!("{from}: {why}"),
        Source::File(_) | Source::Object(_) => format!("{from}: {path}: {why}"),
        Source::Declaration | Source::Change { .. } => format!("{path}: {why}"),
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
        let Some(value) = map.get_mut(key) else { continue };
        match node {
            Node::Group(inner) => view_group(inner, value),
            Node::Setting(setting) => view_kind(&setting.kind, value),
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
