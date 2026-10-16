//! A config's environment: a setting's variable is a prefix and its path in
//! upper snake case, `APP_LIMITS_MAX_RPS` for `limits.maxRps`, or a name the
//! program gives it, such as the `PORT` platforms set. `NAME_FILE` gives a
//! setting its file's text, as Docker and Kubernetes pass secrets.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde_json::Value;

use super::declaration::{Declaration, Setting, did_you_mean, meant};
use super::rules;

/// The longest text a config takes from a variable.
const MOST_BYTES: usize = 16 << 10;

/// The variables a config reads: the prefix's, those `var` names, from the
/// process, or from what `from` gives in its place, as a test does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    prefix: String,
    names: Vec<(String, String)>,
    files: Vec<Dotenv>,
    from: Option<Vec<(String, String)>>,
    /// The texts of the files `NAME_FILE` variables name, as a program's SDK
    /// read them; none when the core reads them itself.
    sent: Option<Texts>,
}

/// A `.env` file: one the core reads, or one a program's SDK read and sent.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Dotenv {
    Path(PathBuf),
    #[cfg_attr(
        not(any(test, feature = "wire", feature = "records")),
        expect(dead_code, reason = "only an SDK over the wire and a store's logger send it")
    )]
    Sent {
        name: String,
        text: String,
    },
}

impl Env {
    /// Variables named by `prefix` and each setting's path: `APP_PORT`.
    pub fn prefix(prefix: impl Into<String>) -> Env {
        Env { prefix: prefix.into(), ..Env::default() }
    }

    /// The setting at `path` reads `name` in place of its prefixed variable.
    pub fn var(mut self, path: &str, name: &str) -> Env {
        self.names.push((path.to_owned(), name.to_owned()));
        self
    }

    /// `.env` files, whose variables count where the process has none; one
    /// that does not exist is skipped, a later one over an earlier.
    pub fn files<P: Into<PathBuf>>(mut self, files: impl IntoIterator<Item = P>) -> Env {
        self.files.extend(files.into_iter().map(|file| Dotenv::Path(file.into())));
        self
    }

    /// These variables in place of the process's: the only environment the
    /// config reads.
    pub fn from<K: Into<String>, V: Into<String>>(mut self, variables: impl IntoIterator<Item = (K, V)>) -> Env {
        self.from = Some(variables.into_iter().map(|(name, value)| (name.into(), value.into())).collect());
        self
    }
}

impl Env {
    /// What a program's SDK read where the program runs: its variables, the
    /// texts of the files their `NAME_FILE` name, and its `.env` files.
    #[cfg_attr(
        not(any(test, feature = "wire", feature = "records")),
        expect(dead_code, reason = "only an SDK over the wire and a store's logger send it")
    )]
    pub(super) fn sent(mut self, variables: Vec<(String, String)>, files: Texts, dotenv: Vec<(String, String)>) -> Env {
        self.files = dotenv.into_iter().map(|(name, text)| Dotenv::Sent { name, text }).collect();
        self.from = Some(variables);
        self.sent = Some(files);
        self
    }

    /// The variable that gives the setting at `path`.
    pub(super) fn name_of(&self, path: &str) -> String {
        let given = self.names.iter().rev().find(|(named, _)| named == path);
        given.map_or_else(|| prefixed(&self.prefix, path), |(_, name)| name.clone())
    }
}

impl From<&str> for Env {
    fn from(prefix: &str) -> Env {
        Env::prefix(prefix)
    }
}

/// Where a variable's text came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    /// The process's environment, or what `from` gave.
    Process,
    DotEnv(String),
    /// The file `NAME_FILE` names.
    File(String),
}

/// What the environment gave: each setting's value by its path, with the
/// variable and where its text came from, and the variables with the prefix
/// that name no setting.
#[derive(Debug, Default)]
pub(super) struct Read {
    pub(super) given: Vec<(String, Value, String, Origin)>,
    pub(super) unused: Vec<Unused>,
}

/// A variable with the prefix that names no setting, and the variable of a
/// setting it may have meant.
#[derive(Clone, Debug)]
pub(super) struct Unused {
    pub(super) name: String,
    meant: Option<String>,
}

impl std::fmt::Display for Unused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let near = did_you_mean(self.meant.clone());
        write!(f, "{}: a variable with the prefix that names no setting{near}", self.name)
    }
}

/// The texts of the files `NAME_FILE` variables name, by their path.
pub(super) type Texts = HashMap<String, String>;

/// A variable's text and the `.env` file it came from, if one did.
type Variables = HashMap<String, (String, Option<String>)>;

/// What `env` gives the settings of `declaration`, or every problem with it.
pub(super) fn read(env: &Env, declaration: &Declaration) -> Result<Read, Vec<String>> {
    let variables = variables(env)?;
    let names = names(env, declaration)?;
    let mut read = Read::default();
    let mut problems = Vec::new();
    for (path, (name, setting)) in &names {
        match variable(env, &variables, name) {
            Ok(None) => {}
            Ok(Some((text, origin, shown))) => match parsed(setting, &text) {
                Ok(value) => read.given.push((path.clone(), value, shown, origin)),
                Err(why) => problems.push(format!("{shown}: {why}")),
            },
            Err(problem) => problems.push(problem),
        }
    }
    read.unused = unused(env, &variables, &names);
    if problems.is_empty() { Ok(read) } else { Err(problems) }
}

/// A variable's text as its setting keeps it; the text of a secret is never
/// quoted.
fn parsed(setting: &Setting, text: &str) -> Result<Value, String> {
    if text.len() > MOST_BYTES {
        return Err(format!("{} bytes, past the {MOST_BYTES} a setting takes", text.len()));
    }
    rules::parse(setting, text).map_err(|why| if setting.secret { "its text does not read".to_owned() } else { why })
}

/// Every setting's variable by the setting's path, each named once.
fn names<'a>(env: &Env, declaration: &'a Declaration) -> Result<BTreeMap<String, (String, &'a Setting)>, Vec<String>> {
    let mut problems = Vec::new();
    let mut names = BTreeMap::new();
    for (path, setting) in declaration.settings() {
        names.insert(path.clone(), (env.name_of(&path), setting));
    }
    for (path, _) in &env.names {
        if declaration.setting(path).is_none() {
            let near = meant(path, declaration.settings().into_iter().map(|(path, _)| path));
            problems.push(format!("a variable is named for {path}, which is no setting{}", did_you_mean(near)));
        }
    }
    let mut taken: HashMap<String, &str> = HashMap::new();
    for (path, (name, _)) in &names {
        if let Some(other) = taken.insert(key(name), path) {
            problems.push(format!("{name} would give both {other} and {path}"));
        }
    }
    if problems.is_empty() { Ok(names) } else { Err(problems) }
}

/// A setting's variable: the prefix and its path in upper snake case.
pub(super) fn prefixed(prefix: &str, path: &str) -> String {
    let name = upper_snake(path);
    if prefix.is_empty() { name } else { format!("{prefix}_{name}") }
}

/// A path's words apart in capitals, as Go, Bun and Python spelled them: a
/// dot, a dash, a capital after a small letter or a digit, or one before a
/// small letter after capitals begins a word.
///
/// ```text
/// dbUrl → DB_URL     HTTPServer → HTTP_SERVER     v2Api → V2_API     limits.maxRps → LIMITS_MAX_RPS
/// ```
pub(super) fn upper_snake(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let mut name = String::with_capacity(path.len() + 4);
    for (at, &c) in chars.iter().enumerate() {
        if c == '.' || c == '-' {
            name.push('_');
            continue;
        }
        if at > 0 && c.is_uppercase() {
            let before = chars[at - 1];
            let small_after = chars.get(at + 1).is_some_and(|after| after.is_lowercase());
            if before.is_lowercase() || before.is_ascii_digit() || (before.is_uppercase() && small_after) {
                name.push('_');
            }
        }
        name.extend(c.to_uppercase());
    }
    name
}

/// A name as variables are looked up: Windows does not tell `Path` from
/// `PATH`, and neither does a config there.
fn key(name: &str) -> String {
    if cfg!(windows) { name.to_uppercase() } else { name.to_owned() }
}

/// The variables of the process, or those `from` gives, over the `.env`
/// files' own.
fn variables(env: &Env) -> Result<Variables, Vec<String>> {
    let mut variables = Variables::new();
    for file in &env.files {
        let (shown, text) = match file {
            Dotenv::Sent { name, text } => (name.clone(), text.clone()),
            Dotenv::Path(path) => match std::fs::read_to_string(path) {
                Ok(text) => (path.display().to_string(), text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(vec![format!("{}: {error}", path.display())]),
            },
        };
        let read = dotenv(&text).map_err(|why| vec![format!("{shown}: {why}")])?;
        variables.extend(read.into_iter().map(|(name, value)| (key(&name), (value, Some(shown.clone())))));
    }
    let process: Vec<(String, String)> = match &env.from {
        Some(given) => given.clone(),
        None => std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .collect(),
    };
    variables.extend(process.into_iter().map(|(name, value)| (key(&name), (value, None))));
    Ok(variables)
}

/// A variable's text, where it came from and how `explain` names it; none
/// when neither `NAME` nor `NAME_FILE` is set, and both set is refused.
fn variable(env: &Env, variables: &Variables, name: &str) -> Result<Option<(String, Origin, String)>, String> {
    let file_name = format!("{name}_FILE");
    match (variables.get(&key(name)), variables.get(&key(&file_name))) {
        (Some(_), Some(_)) => Err(format!("{name} and {file_name} are both set")),
        (Some((text, None)), None) => Ok(Some((text.clone(), Origin::Process, name.to_owned()))),
        (Some((text, Some(dotenv))), None) => Ok(Some((text.clone(), Origin::DotEnv(dotenv.clone()), name.to_owned()))),
        (None, Some((path, _))) => {
            let text = match &env.sent {
                Some(sent) => sent
                    .get(path)
                    .cloned()
                    .ok_or_else(|| format!("{file_name}: {path}: the program could not read it"))?,
                None => std::fs::read_to_string(path).map_err(|error| format!("{file_name}: {path}: {error}"))?,
            };
            Ok(Some((without_newline(text), Origin::File(path.clone()), file_name)))
        }
        (None, None) => Ok(None),
    }
}

/// A file's text without the one newline an editor or `echo` ends it with.
fn without_newline(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

/// The variables with the prefix that name no setting: a misspelt name is
/// said, not ignored.
fn unused(env: &Env, variables: &Variables, names: &BTreeMap<String, (String, &Setting)>) -> Vec<Unused> {
    if env.prefix.is_empty() {
        return Vec::new();
    }
    let start = key(&format!("{}_", env.prefix));
    let read: Vec<String> = names.values().flat_map(|(name, _)| [key(name), key(&format!("{name}_FILE"))]).collect();
    let mut unused: Vec<String> = variables
        .iter()
        .filter(|(name, _)| name.starts_with(&start) && !read.contains(name))
        .map(|(name, _)| name.clone())
        .collect();
    unused.sort();
    unused
        .into_iter()
        .map(|name| Unused { meant: meant(&name, names.values().map(|(name, _)| name.as_str())), name })
        .collect()
}

/// The variables a `.env` file sets, a later name over an earlier:
///
/// ```text
/// # a comment
/// export PORT=3000
/// URL=postgres://localhost/app  # a comment after a space
/// GREETING="two\nlines"         # \n, \r, \t, \" and \\ inside double quotes
/// PATTERN='$literal\n'          # nothing is read inside single quotes
/// ```
///
/// A quoted value may run over lines.
pub(super) fn dotenv(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut values = Vec::new();
    let (mut rest, mut line) = (text, 1);
    while !rest.is_empty() {
        let (assigned, after) = assignment(rest).map_err(|why| format!("line {line}: {why}"))?;
        line += rest[..rest.len() - after.len()].matches('\n').count();
        if let Some(pair) = assigned {
            values.retain(|(name, _): &Assigned| *name != pair.0);
            values.push(pair);
        }
        rest = after;
    }
    Ok(values)
}

/// A variable's name and the value a line sets it to.
type Assigned = (String, String);

/// One line, or the lines of a quoted value: the name and value it sets,
/// none for a blank or a comment, and the text after it.
fn assignment(text: &str) -> Result<(Option<Assigned>, &str), String> {
    let (line, rest) = text.split_once('\n').unwrap_or((text, ""));
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok((None, rest));
    }
    let equals = line.find('=').ok_or_else(|| format!("{line:?} is no NAME=value"))?;
    let name = line[..equals].trim();
    let name = name.strip_prefix("export ").map_or(name, str::trim);
    if !is_variable_name(name) {
        return Err(format!("{name:?} is no variable's name"));
    }
    let value_start =
        equals + 1 + (line[equals + 1..].len() - line[equals + 1..].trim_start_matches([' ', '\t']).len());
    let opening = line.as_bytes().get(value_start).copied();
    if let Some(quote @ (b'"' | b'\'')) = opening {
        let (value, after) = quoted(&text[value_start..], quote)?;
        return Ok((Some((name.to_owned(), value)), after));
    }
    let mut value = &line[value_start..];
    if let Some(comment) = value.find(" #") {
        value = &value[..comment];
    }
    Ok((Some((name.to_owned(), value.trim_end_matches([' ', '\t', '\r']).to_owned())), rest))
}

/// A quoted value from its opening quote, and the text after the line its
/// closing quote is on.
fn quoted(text: &str, quote: u8) -> Result<(String, &str), String> {
    let bytes = text.as_bytes();
    let mut value = Vec::new();
    let mut at = 1;
    while at < bytes.len() {
        match bytes[at] {
            byte if byte == quote => {
                let after = text[at + 1..].split_once('\n').map_or("", |(_, after)| after);
                return Ok((String::from_utf8_lossy(&value).into_owned(), after));
            }
            b'\\' if quote == b'"' && at + 1 < bytes.len() => {
                at += 1;
                value.push(unescaped(bytes[at]));
            }
            byte => value.push(byte),
        }
        at += 1;
    }
    Err(format!("a value opened with {} is never closed", quote as char))
}

fn unescaped(byte: u8) -> u8 {
    match byte {
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        other => other,
    }
}

/// Letters, digits after the first, and `_`.
fn is_variable_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().enumerate().all(|(at, c)| c == '_' || c.is_alphabetic() || (at > 0 && c.is_ascii_digit()))
}

#[cfg(test)]
#[path = "env_tests.rs"]
mod tests;
