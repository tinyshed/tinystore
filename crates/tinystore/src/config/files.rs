//! A config's files: TOML, YAML or JSON by the extension, parsed by the core
//! so that every language reads one alike, and walked against the
//! declaration, every key one of its settings.

use std::io::ErrorKind as IoKind;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use yaml_rust2::{Yaml, YamlLoader};

use super::declaration::{
    Choices, Chosen, Declaration, Entries, Group, Node, did_you_mean, entry_path, joined, shown_unless_secret,
};
use super::rules;
use crate::typos::meant;

/// The largest file a config reads.
const MOST_BYTES: u64 = 1 << 20;

/// A file a config reads: TOML, YAML or JSON by its extension, `.toml`,
/// `.yaml` or `.yml`, `.json` or `.jsonc`, a relative path the program's
/// working directory's. One that does not exist is refused unless it is
/// optional.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    path: PathBuf,
    optional: bool,
    /// Whether a secret may be read from it.
    secrets: bool,
    /// The text a program's SDK read and sent, none for a file it found
    /// absent; a file the core reads itself has none.
    sent: Option<Option<String>>,
}

impl File {
    /// A file that may be absent: then it declares nothing, and `explain`
    /// says it was absent.
    pub fn optional(path: impl Into<PathBuf>) -> File {
        File { path: path.into(), optional: true, secrets: true, sent: None }
    }

    /// Whether the file may give a secret. `false` refuses the config when
    /// the file gives one, for a file committed beside the code, whose
    /// secrets come from the environment: the error names the variable and
    /// its `_FILE` that give it instead, and never the secret.
    ///
    /// ```
    /// use tinystore::config::File;
    ///
    /// let committed = File::optional("dashbin.toml").secrets(false);
    /// let required = File::from("app.toml").secrets(false);
    /// ```
    pub fn secrets(mut self, secrets: bool) -> File {
        self.secrets = secrets;
        self
    }

    fn required(path: PathBuf) -> File {
        File { path, optional: false, secrets: true, sent: None }
    }

    /// A file a program's SDK read where the program runs: its name as the
    /// program wrote it, and its text, none when it was absent.
    #[cfg_attr(
        not(all(feature = "config", any(test, feature = "wire", feature = "records"))),
        expect(dead_code, reason = "only an SDK over the wire and a store's logger send it")
    )]
    pub(super) fn sent(name: String, optional: bool, text: Option<String>) -> File {
        File { path: name.into(), optional, secrets: true, sent: Some(text) }
    }

    /// The file as an error names it: its path as the program wrote it.
    pub(super) fn shown(&self) -> String {
        self.path.display().to_string()
    }

    pub(super) fn gives_secrets(&self) -> bool {
        self.secrets
    }
}

impl From<&str> for File {
    fn from(path: &str) -> File {
        File::required(path.into())
    }
}

impl From<String> for File {
    fn from(path: String) -> File {
        File::required(path.into())
    }
}

impl From<&Path> for File {
    fn from(path: &Path) -> File {
        File::required(path.into())
    }
}

impl From<PathBuf> for File {
    fn from(path: PathBuf) -> File {
        File::required(path)
    }
}

/// What a file gave: each setting's value by its path, each value of a
/// chosen group's setting, and the entries it made, or that it was absent.
#[derive(Debug, Default)]
pub(super) struct Read {
    pub(super) given: Vec<(String, Value)>,
    pub(super) picked: Vec<Picked>,
    pub(super) entries: Vec<(String, String)>,
    pub(super) absent: bool,
}

/// What `file` gives the settings of `declaration`, each chosen group's read
/// as its chooser in the file picks it, else as `before` says the layers
/// before it did, or every problem with it. A secret given by a file that
/// refuses secrets is one, which `refused` words by the secret's path.
pub(super) fn read(
    file: &File,
    declaration: &Declaration,
    before: &Choices,
    refused: &dyn Fn(&str) -> String,
) -> Result<Read, Vec<String>> {
    let name = file.shown();
    let read = match &file.sent {
        Some(sent) => check_size(sent.clone()),
        None => text(&file.path),
    };
    let text = match read {
        Ok(Some(text)) => text,
        Ok(None) if file.optional => return Ok(Read { absent: true, ..Read::default() }),
        Ok(None) => return Err(vec![format!("{name}: no such file; File::optional lets it be absent")]),
        Err(why) => return Err(vec![format!("{name}: {why}")]),
    };
    let tree = parse(&file.path, &text).map_err(|why| vec![format!("{name}: {why}")])?;
    let mut walked = Walked { refuses_secrets: !file.secrets, ..Walked::after(before) };
    walk(&declaration.root, &tree, "", &mut walked);
    let secrets = walked.secrets.iter().map(|path| refused(path));
    let problems: Vec<String> =
        walked.problems.into_iter().chain(secrets).map(|problem| format!("{name}: {problem}")).collect();
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Read { given: walked.given, picked: walked.picked, entries: walked.entries, absent: false })
}

fn past_size(size: u64) -> String {
    format!("{size} bytes, past the {MOST_BYTES} a config's file holds")
}

/// A sent file's text, refused past the size a file holds.
fn check_size(text: Option<String>) -> Result<Option<String>, String> {
    match text {
        Some(text) if text.len() as u64 > MOST_BYTES => Err(past_size(text.len() as u64)),
        other => Ok(other),
    }
}

/// The file's text, none when it does not exist.
fn text(path: &Path) -> Result<Option<String>, String> {
    let size = match std::fs::metadata(path) {
        Ok(found) => found.len(),
        Err(error) if error.kind() == IoKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if size > MOST_BYTES {
        return Err(past_size(size));
    }
    std::fs::read_to_string(path).map(Some).map_err(|error| error.to_string())
}

/// A file's settings as JSON, read by its extension.
fn parse(path: &Path, text: &str) -> Result<Map<String, Value>, String> {
    let extension = path.extension().and_then(|extension| extension.to_str()).map(str::to_ascii_lowercase);
    let tree = match extension.as_deref() {
        Some("toml") => toml_tree(text)?,
        Some("yaml" | "yml") => yaml_tree(text)?,
        Some("json" | "jsonc") => serde_json::from_str(&without_comments(text)).map_err(|error| error.to_string())?,
        _ => return Err("not .toml, .yaml, .yml, .json or .jsonc".to_owned()),
    };
    match tree {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(Map::new()),
        // the whole file may hold any of its settings' secrets
        other => Err(format!("{} is not a map of settings", shown_unless_secret(&other, true))),
    }
}

fn toml_tree(text: &str) -> Result<Value, String> {
    let table: toml::Table = toml::from_str(text).map_err(|error| error.to_string())?;
    Ok(Value::Object(table.into_iter().map(|(key, value)| (key, toml_value(value))).collect()))
}

fn toml_value(value: toml::Value) -> Value {
    match value {
        toml::Value::String(text) => Value::String(text),
        toml::Value::Integer(int) => Value::from(int),
        // JSON holds no infinity or NaN, and `Value::from` makes them null,
        // which reads as unset; as text they fail the setting's type instead.
        toml::Value::Float(float) if !float.is_finite() => Value::String(float.to_string()),
        toml::Value::Float(float) => Value::from(float),
        toml::Value::Boolean(flag) => Value::Bool(flag),
        toml::Value::Datetime(time) => Value::String(time.to_string()),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(toml_value).collect()),
        toml::Value::Table(table) => {
            Value::Object(table.into_iter().map(|(key, value)| (key, toml_value(value))).collect())
        }
    }
}

/// A YAML file's first document as JSON; YAML 1.2, so that `no` is text.
fn yaml_tree(text: &str) -> Result<Value, String> {
    let documents = YamlLoader::load_from_str(text).map_err(|error| error.to_string())?;
    documents.first().map_or(Ok(Value::Null), yaml_value)
}

fn yaml_value(yaml: &Yaml) -> Result<Value, String> {
    Ok(match yaml {
        Yaml::String(text) => Value::String(text.clone()),
        Yaml::Integer(int) => Value::from(*int),
        Yaml::Real(_) => yaml
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| "a number that is not finite".to_owned())?,
        Yaml::Boolean(flag) => Value::Bool(*flag),
        Yaml::Null => Value::Null,
        Yaml::Array(items) => Value::Array(items.iter().map(yaml_value).collect::<Result<_, _>>()?),
        Yaml::Hash(map) => {
            let pairs = map.iter().map(|(key, value)| Ok((yaml_key(key)?, yaml_value(value)?)));
            Value::Object(pairs.collect::<Result<_, String>>()?)
        }
        Yaml::Alias(_) | Yaml::BadValue => return Err("a value YAML could not read".to_owned()),
    })
}

fn yaml_key(key: &Yaml) -> Result<String, String> {
    match key {
        Yaml::String(text) => Ok(text.clone()),
        Yaml::Integer(int) => Ok(int.to_string()),
        Yaml::Boolean(flag) => Ok(flag.to_string()),
        _ => Err("a key that is not text".to_owned()),
    }
}

/// JSON with its comments and trailing commas taken out, each replaced by
/// spaces so that an error's line and column are the file's.
pub(super) fn without_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut at = 0;
    while at < bytes.len() {
        at = match bytes[at] {
            b'"' => past_string(bytes, at),
            b'/' if bytes.get(at + 1) == Some(&b'/') => blank(&mut out, at, line_end(bytes, at)),
            b'/' if bytes.get(at + 1) == Some(&b'*') => blank(&mut out, at, comment_end(bytes, at)),
            b',' if closes(bytes, at + 1) => blank(&mut out, at, at + 1),
            _ => at + 1,
        };
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// The place after a string that opens at `at`, its escapes skipped.
fn past_string(bytes: &[u8], at: usize) -> usize {
    let mut next = at + 1;
    while next < bytes.len() {
        match bytes[next] {
            b'\\' => next += 2,
            b'"' => return next + 1,
            _ => next += 1,
        }
    }
    next
}

fn line_end(bytes: &[u8], at: usize) -> usize {
    bytes[at..].iter().position(|&byte| byte == b'\n').map_or(bytes.len(), |end| at + end)
}

fn comment_end(bytes: &[u8], at: usize) -> usize {
    bytes[at + 2..].windows(2).position(|pair| pair == b"*/").map_or(bytes.len(), |end| at + 2 + end + 2)
}

/// Spaces over `out[from..to]`, its line ends kept.
fn blank(out: &mut [u8], from: usize, to: usize) -> usize {
    for byte in &mut out[from..to] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
    to
}

/// Whether what follows `at`, past spaces and comments, closes a list or a map.
fn closes(bytes: &[u8], mut at: usize) -> bool {
    while at < bytes.len() {
        match bytes[at] {
            b' ' | b'\t' | b'\n' | b'\r' => at += 1,
            b'/' if bytes.get(at + 1) == Some(&b'/') => at = line_end(bytes, at),
            b'/' if bytes.get(at + 1) == Some(&b'*') => at = comment_end(bytes, at),
            byte => return byte == b']' || byte == b'}',
        }
    }
    false
}

/// What a layer's tree gave: each setting's value by its path, each value
/// of a chosen group's setting, each entry it made by the path of its map
/// and its key, and what no setting takes.
#[derive(Debug, Default)]
pub(super) struct Walked {
    /// The group each chosen group was read as by the layers before this
    /// one, which its settings are read as unless this layer chooses.
    pub(super) before: Choices,
    /// Whether the layer is a file that may give no secret.
    pub(super) refuses_secrets: bool,
    pub(super) given: Vec<(String, Value)>,
    pub(super) picked: Vec<Picked>,
    pub(super) entries: Vec<(String, String)>,
    pub(super) problems: Vec<String>,
    /// The paths of the secrets a layer that refuses them gave, whatever
    /// their values, which are never read.
    pub(super) secrets: Vec<String>,
}

impl Walked {
    /// A walk of a layer read after the layers that chose `before`.
    pub(super) fn after(before: &Choices) -> Walked {
        Walked { before: before.clone(), ..Walked::default() }
    }
}

/// A value a layer gave a setting of a chosen group, with the group it was
/// read as: kept only if that group is the one its chooser picks once every
/// layer is read.
#[derive(Clone, Debug)]
pub(super) struct Picked {
    /// The chosen group's path.
    pub(super) node: String,
    pub(super) group: String,
    pub(super) path: String,
    pub(super) value: Value,
}

/// What a layer's tree gives each setting, and what it gives that no setting
/// takes. A null declares nothing, as an empty YAML key does. A chosen
/// group's settings are read as the group its chooser in the tree picks,
/// else as the layers before it chose.
pub(super) fn walk(group: &Group, tree: &Map<String, Value>, path: &str, walked: &mut Walked) {
    for (key, value) in tree {
        let at = joined(path, key);
        match (group.get(key), value) {
            (_, Value::Null) => {}
            (None, _) => {
                let near = meant(key, group.keys()).map(|near| joined(path, &near));
                walked.problems.push(format!("{at}: no such setting{}", did_you_mean(near)));
            }
            (Some(Node::Group(inner)), Value::Object(map)) => walk(inner, map, &at, walked),
            (Some(Node::Entries(entries)), Value::Object(map)) => walk_entries(entries, map, &at, walked),
            (Some(Node::Chosen(chosen)), Value::Object(map)) => {
                walk_chosen(chosen, map, &Stand { group, tree, path, at: &at }, walked);
            }
            (Some(node @ (Node::Group(_) | Node::Entries(_) | Node::Chosen(_))), other) => {
                let what = shown_unless_secret(other, node.may_hold_secret());
                walked.problems.push(format!("{at}: {what} is not a group of settings"));
            }
            (Some(Node::Setting(setting)), _) if setting.secret && walked.refuses_secrets => walked.secrets.push(at),
            (Some(Node::Setting(setting)), value) => match rules::check(setting, value) {
                Ok(value) => walked.given.push((at, value)),
                Err(_) if setting.secret => walked.problems.push(format!("{at}: a secret is text")),
                Err(why) => walked.problems.push(format!("{at}: {why}")),
            },
        }
    }
}

/// Where a chosen group stands in a layer's tree: the group it is a setting
/// of, what the layer gave that group, that group's path, and its own.
struct Stand<'a> {
    group: &'a Group,
    tree: &'a Map<String, Value>,
    path: &'a str,
    at: &'a str,
}

/// What a layer gives a chosen group's settings, each kept with the group it
/// was read as: the one its chooser in the layer names, else the one the
/// layers before chose, else its chooser's default. A chooser the layer gives
/// that names no group is refused where its own value is read.
fn walk_chosen(chosen: &Chosen, map: &Map<String, Value>, stand: &Stand<'_>, walked: &mut Walked) {
    let given = stand.tree.get(&chosen.by).filter(|value| !value.is_null());
    let default = || chosen.picked(&Map::new(), stand.group).map(|(name, _)| name.to_owned());
    let name = match given {
        Some(given) => chosen.named(Some(given)).map(|(name, _)| name.to_owned()),
        None => walked.before.get(stand.at).cloned().or_else(default),
    };
    let Some(name) = name else {
        if given.is_none() {
            let chooser = joined(stand.path, &chosen.by);
            walked.problems.push(format!("{}: given before {chooser}, which chooses its group", stand.at));
        }
        return;
    };
    let mut inner = Walked { refuses_secrets: walked.refuses_secrets, ..Walked::default() };
    walk(&chosen.groups[&name], map, stand.at, &mut inner);
    walked.problems.extend(inner.problems);
    walked.secrets.extend(inner.secrets);
    let at = stand.at;
    let picked =
        inner.given.into_iter().map(|(path, value)| Picked { node: at.to_owned(), group: name.clone(), path, value });
    walked.picked.extend(picked);
}

/// What a map of entries gives each field of each entry it names, over what
/// came before: an entry a later layer names again is merged, not replaced.
pub(super) fn walk_entries(entries: &Entries, map: &Map<String, Value>, path: &str, walked: &mut Walked) {
    for (key, value) in map {
        let at = match entry_path(path, key) {
            Ok(at) => at,
            Err(why) => {
                walked.problems.push(why);
                continue;
            }
        };
        match value {
            Value::Null => {}
            Value::Object(fields) => {
                walked.entries.push((path.to_owned(), key.clone()));
                walk(&entries.item, fields, &at, walked);
            }
            other => {
                let what = shown_unless_secret(other, entries.item.values().any(Node::may_hold_secret));
                walked.problems.push(format!("{at}: {what} is not a group of settings"));
            }
        }
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
