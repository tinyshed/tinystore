//! What a config's layers give, read once when it opens: each setting's
//! values, oldest first, each with the layer that gave it, so that the last
//! is the setting's value and the others what it went over.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::declaration::{Choices, Declaration, Shape, joined};
use super::env::{self, Env};
use super::explain::Origin;
use super::files::{self, File, Picked, Walked};
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
    /// path of their map, every map of the declaration among them, and the
    /// group each chooser picked once every layer was read.
    pub(super) shape: Shape,
    /// The optional files that were absent.
    pub(super) absent: Vec<String>,
    /// The variables with the prefix that name no setting.
    pub(super) unused: Vec<env::Unused>,
    /// The required settings no layer gave, of a config opened as the store
    /// keeps it, whose program gives them from its own files or environment.
    pub(super) left_out: Vec<String>,
    /// What a layer gave a chosen group as a group its chooser did not pick
    /// in the end, dropped, each said once.
    pub(super) dropped: Vec<String>,
    /// The values the layers gave chosen groups' settings, oldest first,
    /// until the groups are chosen.
    picked: Vec<(Picked, Origin)>,
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
            let before = base.choices(declaration);
            if let Err(found) = base.take(layer, declaration, &before) {
                problems.extend(found);
            }
        }
        base.choose(declaration);
        base.shaped_defaults(declaration);
        if required {
            problems.extend(base.missing(declaration, layers));
        } else {
            let given = |path: &String| base.stacks.get(path).is_some_and(|stack| !stack.is_empty());
            let settings = declaration.settings_of(&base.shape).into_iter();
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
            self.shape.entries.entry(path.clone()).or_default();
            let mut walked = Walked::default();
            files::walk_entries(entries, &entries.given, &path, &mut walked);
            let found = std::mem::take(&mut walked.problems);
            problems.extend(found.into_iter().map(|problem| format!("the declaration: {problem}")));
            self.took(walked, &Origin::Declaration);
        }
        problems
    }

    /// The group each chooser picks with what the layers read so far gave it.
    fn choices(&self, declaration: &Declaration) -> Choices {
        declaration.choose(&self.shape.entries, |path| self.stacks.get(path)?.last().map(|stacked| &stacked.value))
    }

    /// Picks each chosen group's group, as its chooser is once every layer is
    /// read, and keeps what the layers gave its settings as that group; what
    /// they gave as another group is dropped, and said.
    fn choose(&mut self, declaration: &Declaration) {
        self.shape.chosen = self.choices(declaration);
        let choosers: BTreeMap<String, String> =
            declaration.chosen(&self.shape.entries).into_iter().map(|at| (at.path, at.chooser)).collect();
        for (picked, from) in std::mem::take(&mut self.picked) {
            let Some(group) = self.shape.chosen.get(&picked.node) else { continue };
            if *group == picked.group {
                self.push(picked.path, Stacked { value: picked.value, from });
                continue;
            }
            let chooser = &choosers[&picked.node];
            let by = self
                .stacks
                .get(chooser)
                .and_then(|stack| stack.last())
                .map_or(Origin::Declaration, |last| last.from.clone());
            let note = format!(
                "{}: what {from} gave as {} is dropped: {chooser} is {group}, from {by}",
                picked.node, picked.group
            );
            if !self.dropped.contains(&note) {
                self.dropped.push(note);
            }
        }
    }

    /// Each setting of an entry or a chosen group that the declaration did not
    /// give: its default, under what the layers gave it.
    fn shaped_defaults(&mut self, declaration: &Declaration) {
        for (path, setting) in declaration.settings_of(&self.shape) {
            let Some(default) = &setting.default else { continue };
            let stack = self.stacks.entry(path).or_default();
            if stack.first().is_none_or(|first| first.from != Origin::Declaration) {
                stack.insert(0, Stacked { value: default.clone(), from: Origin::Declaration });
            }
        }
    }

    /// Takes what a layer gives, each chosen group's settings read as
    /// `before` says the layers before it chose, unless the layer chooses.
    fn take(&mut self, layer: &Layer, declaration: &Declaration, before: &Choices) -> Result<(), Vec<String>> {
        match layer {
            Layer::Object { place, value } => {
                let mut walked = Walked::after(before);
                files::walk(&declaration.root, value, "", &mut walked);
                if !walked.problems.is_empty() {
                    let problems = walked.problems.into_iter();
                    return Err(problems.map(|problem| format!("layer {place}: {problem}")).collect());
                }
                self.took(walked, &Origin::Layer { place: *place });
            }
            Layer::File(file) => {
                let read = files::read(file, declaration, before)?;
                if read.absent {
                    self.absent.push(file.shown());
                }
                let walked =
                    Walked { given: read.given, picked: read.picked, entries: read.entries, ..Walked::default() };
                self.took(walked, &Origin::File { path: file.shown() });
            }
            Layer::Env(env) => {
                let read = env::read(env, declaration, &self.shape.entries, before)?;
                self.unused.extend(read.unused);
                self.made(read.entries);
                for (path, value, from) in read.given {
                    self.push(path, Stacked { value, from });
                }
                self.picked.extend(read.picked);
            }
        }
        Ok(())
    }

    /// Takes what a layer's tree gave, all of it from `from`.
    fn took(&mut self, walked: Walked, from: &Origin) {
        self.made(walked.entries);
        for (path, value) in walked.given {
            self.push(path, Stacked { value, from: from.clone() });
        }
        self.picked.extend(walked.picked.into_iter().map(|picked| (picked, from.clone())));
    }

    /// Takes the entries a layer made, each by the path of its map and its key.
    fn made(&mut self, entries: Vec<(String, String)>) {
        for (path, key) in entries {
            self.shape.entries.entry(path).or_default().insert(key);
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
        for (path, setting) in declaration.settings_of(&self.shape) {
            let value = self.stacks.get(&path).and_then(|stack| stack.last()).map(|stacked| &stacked.value);
            if setting.required && value.is_none_or(|value| value == "") {
                missing.push(format!("{path} is required: {}", givers(&path, layers)));
            }
        }
        missing
    }

    /// The value the layers make: each setting's last value, in its group,
    /// and every map of entries, entry and chosen group, though no field of
    /// it was given.
    pub(super) fn tree(&self) -> Value {
        let mut tree = Value::Object(Map::new());
        for (path, keys) in &self.shape.entries {
            set(&mut tree, path, Value::Object(Map::new()));
            for key in keys {
                set(&mut tree, &joined(path, key), Value::Object(Map::new()));
            }
        }
        for path in self.shape.chosen.keys() {
            set(&mut tree, path, Value::Object(Map::new()));
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
