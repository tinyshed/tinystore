//! What a config's layers give, read once when it opens: each setting's
//! values, oldest first, each with the layer that gave it, so that the last
//! is the setting's value and the others what it went over.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::declaration::{Declaration, Keys, joined};
use super::env::{self, Env};
use super::explain::Origin;
use super::files::{self, File, Walked};
use super::rules::spelled;

/// An object in code, a file or the environment, in the order the program
/// wrote them. Only an SDK sends an object: Rust computes its declaration.
#[derive(Clone, Debug)]
pub(super) enum Layer {
    #[cfg_attr(
        not(all(feature = "config", any(test, feature = "wire", feature = "records"))),
        expect(dead_code, reason = "only an SDK over the wire and a store's logger send it")
    )]
    Object {
        place: usize,
        value: Map<String, Value>,
    },
    File(File),
    Env(Env),
}

/// What one layer gave one setting: its value as the config keeps it, a
/// secret's text included, which only `explain` turns into `***`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Stacked {
    pub(super) value: Value,
    pub(super) from: Origin,
}

/// What the layers gave, and what they read and did not use.
#[derive(Clone, Debug, Default)]
pub(super) struct Base {
    /// Each setting's values by its path, oldest first.
    pub(super) stacks: BTreeMap<String, Vec<Stacked>>,
    /// The keys of the entries the declaration and the layers made, by the
    /// path of their map, every map of the declaration among them.
    pub(super) entries: Keys,
    /// The optional files that were absent.
    pub(super) absent: Vec<String>,
    /// The variables with the prefix that name no setting.
    pub(super) unused: Vec<env::Unused>,
    /// The required settings no layer gave, of a config opened as the store
    /// keeps it, whose program gives them from its own files or environment.
    pub(super) left_out: Vec<String>,
}

impl Base {
    /// What `layers` give the settings of `declaration`, or every problem
    /// with them: each layer's, and every required setting none gave unless
    /// `required` is false, when those are left out.
    pub(super) fn read(declaration: &Declaration, layers: &[Layer], required: bool) -> Result<Base, Vec<String>> {
        let mut base = Base::default();
        for (path, setting) in declaration.settings() {
            let defaults =
                setting.default.iter().map(|value| Stacked { value: value.clone(), from: Origin::Declaration });
            base.stacks.insert(path, defaults.collect());
        }
        let mut problems = base.declared_entries(declaration);
        for layer in layers {
            if let Err(found) = base.take(layer, declaration) {
                problems.extend(found);
            }
        }
        base.entry_defaults(declaration);
        if required {
            problems.extend(base.missing(declaration, layers));
        } else {
            let given = |path: &String| base.stacks.get(path).is_some_and(|stack| !stack.is_empty());
            let settings = declaration.settings_of(&base.entries).into_iter();
            base.left_out =
                settings.filter(|(path, setting)| setting.required && !given(path)).map(|(path, _)| path).collect();
        }
        if problems.is_empty() { Ok(base) } else { Err(problems) }
    }

    /// The entries the declaration gives, each field it gives one of the
    /// declaration's values.
    fn declared_entries(&mut self, declaration: &Declaration) -> Vec<String> {
        let mut problems = Vec::new();
        for (path, entries) in declaration.entries() {
            self.entries.entry(path.clone()).or_default();
            let mut walked = Walked::default();
            files::walk_entries(entries, &entries.given, &path, &mut walked);
            problems.extend(walked.problems.into_iter().map(|problem| format!("the declaration: {problem}")));
            self.made(walked.entries);
            for (path, value) in walked.given {
                self.push(path, Stacked { value, from: Origin::Declaration });
            }
        }
        problems
    }

    /// Each entry's fields that the declaration did not give: their defaults,
    /// under what the layers gave them.
    fn entry_defaults(&mut self, declaration: &Declaration) {
        for (path, setting) in declaration.settings_of(&self.entries) {
            let Some(default) = &setting.default else { continue };
            let stack = self.stacks.entry(path).or_default();
            if stack.first().is_none_or(|first| first.from != Origin::Declaration) {
                stack.insert(0, Stacked { value: default.clone(), from: Origin::Declaration });
            }
        }
    }

    fn take(&mut self, layer: &Layer, declaration: &Declaration) -> Result<(), Vec<String>> {
        match layer {
            Layer::Object { place, value } => {
                let mut walked = Walked::default();
                files::walk(&declaration.root, value, "", &mut walked);
                if !walked.problems.is_empty() {
                    let problems = walked.problems.into_iter();
                    return Err(problems.map(|problem| format!("layer {place}: {problem}")).collect());
                }
                self.made(walked.entries);
                for (path, value) in walked.given {
                    self.push(path, Stacked { value, from: Origin::Layer { place: *place } });
                }
            }
            Layer::File(file) => {
                let read = files::read(file, declaration)?;
                if read.absent {
                    self.absent.push(file.shown());
                }
                self.made(read.entries);
                for (path, value) in read.given {
                    self.push(path, Stacked { value, from: Origin::File { path: file.shown() } });
                }
            }
            Layer::Env(env) => {
                let read = env::read(env, declaration, &self.entries)?;
                self.unused.extend(read.unused);
                self.made(read.entries);
                for (path, value, from) in read.given {
                    self.push(path, Stacked { value, from });
                }
            }
        }
        Ok(())
    }

    /// Takes the entries a layer made, each by the path of its map and its key.
    fn made(&mut self, entries: Vec<(String, String)>) {
        for (path, key) in entries {
            self.entries.entry(path).or_default().insert(key);
        }
    }

    fn push(&mut self, path: String, stacked: Stacked) {
        self.stacks.entry(path).or_default().push(stacked);
    }

    /// Every required setting no layer gave, or gave as empty text, named
    /// with what would give it: `region is required: set APP_REGION, or
    /// region in app.yaml`.
    fn missing(&self, declaration: &Declaration, layers: &[Layer]) -> Vec<String> {
        let mut missing = Vec::new();
        for (path, setting) in declaration.settings_of(&self.entries) {
            let value = self.stacks.get(&path).and_then(|stack| stack.last()).map(|stacked| &stacked.value);
            if setting.required && value.is_none_or(|value| value == "") {
                missing.push(format!("{path} is required: {}", givers(&path, layers)));
            }
        }
        missing
    }

    /// The value the layers make: each setting's last value, in its group,
    /// and every map of entries and entry, though no field of it was given.
    pub(super) fn tree(&self) -> Value {
        let mut tree = Value::Object(Map::new());
        for (path, keys) in &self.entries {
            set(&mut tree, path, Value::Object(Map::new()));
            for key in keys {
                set(&mut tree, &joined(path, key), Value::Object(Map::new()));
            }
        }
        for (path, stack) in &self.stacks {
            if let Some(stacked) = stack.last() {
                set(&mut tree, path, stacked.value.clone());
            }
        }
        tree
    }
}

/// What would give a setting: `set APP_REGION, or region in app.yaml`.
fn givers(path: &str, layers: &[Layer]) -> String {
    let ways: Vec<String> = layers
        .iter()
        .map(|layer| match layer {
            Layer::Env(env) => format!("set {}", env.name_of(path)),
            Layer::File(file) => format!("{path} in {}", file.shown()),
            Layer::Object { place, .. } => format!("{path} in layer {place}"),
        })
        .collect();
    if ways.is_empty() { "no layer gives it".to_owned() } else { ways.join(", or ") }
}

/// Sets the value at `path` of `tree`, making the groups on its way.
pub(super) fn set(tree: &mut Value, path: &str, value: Value) {
    let mut node = tree;
    let mut keys = path.split('.').peekable();
    while let Some(key) = keys.next() {
        if !node.is_object() {
            *node = Value::Object(Map::new());
        }
        let Value::Object(map) = node else { return };
        if keys.peek().is_none() {
            map.insert(key.to_owned(), value);
            return;
        }
        node = map.entry(key.to_owned()).or_insert_with(|| Value::Object(Map::new()));
    }
}

/// A value as `explain` shows it: a span spelled, anything else as JSON.
pub(super) fn shown_value(value: &Value, duration: bool) -> String {
    match value.as_u64() {
        Some(millis) if duration => spelled(millis),
        _ => value.to_string(),
    }
}
