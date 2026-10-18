//! A config's files: TOML, YAML or JSON by the extension, parsed by the core
//! so that every language reads one alike, and walked against the
//! declaration, every key one of its settings.

use std::io::ErrorKind as IoKind;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use yaml_rust2::{Yaml, YamlLoader};

use super::declaration::{Declaration, Group, Node, did_you_mean, joined, meant, shown};
use super::rules;

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
    /// The text a program's SDK read and sent, none for a file it found
    /// absent; a file the core reads itself has none.
    sent: Option<Option<String>>,
}

impl File {
    /// A file that may be absent: then it declares nothing, and `explain`
    /// says it was absent.
    pub fn optional(path: impl Into<PathBuf>) -> File {
        File { path: path.into(), optional: true, sent: None }
    }

    fn required(path: PathBuf) -> File {
        File { path, optional: false, sent: None }
    }

    /// A file a program's SDK read where the program runs: its name as the
    /// program wrote it, and its text, none when it was absent.
    #[cfg_attr(
        not(any(test, feature = "wire", feature = "records")),
        expect(dead_code, reason = "only an SDK over the wire and a store's logger send it")
    )]
    pub(super) fn sent(name: String, optional: bool, text: Option<String>) -> File {
        File { path: name.into(), optional, sent: Some(text) }
    }

    /// The file as an error names it: its path as the program wrote it.
    pub(super) fn shown(&self) -> String {
        self.path.display().to_string()
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

/// What a file gave: each setting's value by its path, or that it was absent.
#[derive(Debug, Default)]
pub(super) struct Read {
    pub(super) given: Vec<(String, Value)>,
    pub(super) absent: bool,
}

/// What `file` gives the settings of `declaration`, or every problem with it.
pub(super) fn read(file: &File, declaration: &Declaration) -> Result<Read, Vec<String>> {
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
    let (mut given, mut problems) = (Vec::new(), Vec::new());
    walk(&declaration.root, &tree, "", &mut given, &mut problems);
    let problems: Vec<String> = problems.into_iter().map(|problem| format!("{name}: {problem}")).collect();
    if problems.is_empty() { Ok(Read { given, absent: false }) } else { Err(problems) }
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
        other => Err(format!("{} is not a map of settings", shown(&other))),
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

/// What a file's tree gives each setting, and what it gives that no setting
/// takes. A null declares nothing, as an empty YAML key does.
pub(super) fn walk(
    group: &Group,
    tree: &Map<String, Value>,
    path: &str,
    given: &mut Vec<(String, Value)>,
    problems: &mut Vec<String>,
) {
    for (key, value) in tree {
        let at = joined(path, key);
        match (group.get(key), value) {
            (_, Value::Null) => {}
            (None, _) => {
                let near = meant(key, group.keys()).map(|near| joined(path, &near));
                problems.push(format!("{at}: no such setting{}", did_you_mean(near)));
            }
            (Some(Node::Group(inner)), Value::Object(map)) => walk(inner, map, &at, given, problems),
            (Some(Node::Group(_)), other) => {
                problems.push(format!("{at}: {} is not a group of settings", shown(other)))
            }
            (Some(Node::Setting(setting)), value) => match rules::check(setting, value) {
                Ok(value) => given.push((at, value)),
                Err(_) if setting.secret => problems.push(format!("{at}: a secret is text")),
                Err(why) => problems.push(format!("{at}: {why}")),
            },
        }
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
