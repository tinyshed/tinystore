//! What a config's layers give, read once when it opens: each setting's
//! values, oldest first, each with the layer that gave it, so that the last
//! is the setting's value and the others what it went over.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};

use super::declaration::Declaration;
use super::env::{self, Env, Origin};
use super::files::{self, File};
use super::rules::spelled;

/// An object in code, a file or the environment, in the order the program
/// wrote them. Only an SDK sends an object: Rust computes its declaration.
#[derive(Clone, Debug)]
pub(super) enum Layer {
    #[cfg_attr(
        not(any(test, feature = "wire", feature = "records")),
        expect(dead_code, reason = "only an SDK over the wire and a store's logger send it")
    )]
    Object {
        place: usize,
        value: Map<String, Value>,
    },
    File(File),
    Env(Env),
}

/// Where a value came from, as `explain` names it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Source {
    Declaration,
    /// An object in code, by its place in the list, the declaration first.
    Object(usize),
    File(String),
    Variable {
        name: String,
        origin: Origin,
    },
    /// A change made while the program ran: the config's version that made
    /// it, and when, in unix milliseconds.
    Change {
        version: i64,
        at: i64,
    },
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Declaration => f.write_str("the declaration"),
            Source::Object(place) => write!(f, "layer {place}"),
            Source::File(name) => f.write_str(name),
            Source::Variable { name, origin: Origin::Process } => f.write_str(name),
            Source::Variable { name, origin: Origin::DotEnv(file) } => write!(f, "{name} in {file}"),
            Source::Variable { name, origin: Origin::File(path) } => write!(f, "{name}, {path}"),
            Source::Change { version, at } => write!(f, "a change, version {version}, {}", utc(*at)),
        }
    }
}

/// Unix milliseconds as a time in UTC: `2026-10-13 14:03:05 UTC`.
fn utc(millis: i64) -> String {
    let (days, seconds) = (millis.div_euclid(86_400_000), millis.rem_euclid(86_400_000) / 1000);
    let (year, month, day) = civil(days);
    let (hour, minute, second) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// The date `days` after 1970-01-01, by Howard Hinnant's `civil_from_days`.
fn civil(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * of_year + 2) / 153;
    let day = of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// What one layer gave one setting.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Given {
    pub(super) value: Value,
    pub(super) from: Source,
}

/// What the layers gave, and what they read and did not use.
#[derive(Clone, Debug, Default)]
pub(super) struct Base {
    /// Each setting's values by its path, oldest first.
    pub(super) stacks: BTreeMap<String, Vec<Given>>,
    /// The optional files that were absent.
    pub(super) absent: Vec<String>,
    /// The variables with the prefix that name no setting.
    pub(super) unused: Vec<String>,
}

impl Base {
    /// What `layers` give the settings of `declaration`, or every problem
    /// with them: each layer's, and every required setting none gave.
    pub(super) fn read(declaration: &Declaration, layers: &[Layer]) -> Result<Base, Vec<String>> {
        let mut base = Base::default();
        for (path, setting) in declaration.settings() {
            let defaults =
                setting.default.iter().map(|value| Given { value: value.clone(), from: Source::Declaration });
            base.stacks.insert(path, defaults.collect());
        }
        let mut problems = Vec::new();
        for layer in layers {
            if let Err(found) = base.take(layer, declaration) {
                problems.extend(found);
            }
        }
        problems.extend(base.missing(declaration, layers));
        if problems.is_empty() { Ok(base) } else { Err(problems) }
    }

    fn take(&mut self, layer: &Layer, declaration: &Declaration) -> Result<(), Vec<String>> {
        match layer {
            Layer::Object { place, value } => {
                let (mut given, mut problems) = (Vec::new(), Vec::new());
                files::walk(&declaration.root, value, "", &mut given, &mut problems);
                if !problems.is_empty() {
                    return Err(problems.into_iter().map(|problem| format!("layer {place}: {problem}")).collect());
                }
                for (path, value) in given {
                    self.push(path, Given { value, from: Source::Object(*place) });
                }
            }
            Layer::File(file) => {
                let read = files::read(file, declaration)?;
                if read.absent {
                    self.absent.push(file.shown());
                }
                for (path, value) in read.given {
                    self.push(path, Given { value, from: Source::File(file.shown()) });
                }
            }
            Layer::Env(env) => {
                let read = env::read(env, declaration)?;
                self.unused.extend(read.unused);
                for (path, value, name, origin) in read.given {
                    self.push(path, Given { value, from: Source::Variable { name, origin } });
                }
            }
        }
        Ok(())
    }

    fn push(&mut self, path: String, given: Given) {
        self.stacks.entry(path).or_default().push(given);
    }

    /// Every required setting no layer gave, or gave as empty text, named
    /// with what would give it: `region is required: set APP_REGION, or
    /// region in app.yaml`.
    fn missing(&self, declaration: &Declaration, layers: &[Layer]) -> Vec<String> {
        let mut missing = Vec::new();
        for (path, setting) in declaration.settings() {
            let value = self.stacks.get(&path).and_then(|stack| stack.last()).map(|given| &given.value);
            if setting.required && value.is_none_or(|value| value == "") {
                missing.push(format!("{path} is required: {}", givers(&path, layers)));
            }
        }
        missing
    }

    /// The value the layers make: each setting's last value, in its group.
    pub(super) fn tree(&self) -> Value {
        let mut tree = Value::Object(Map::new());
        for (path, stack) in &self.stacks {
            if let Some(given) = stack.last() {
                set(&mut tree, path, given.value.clone());
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
