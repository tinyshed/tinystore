//! `tinystore events`: a store's streams of events from a shell. `append`
//! writes one event given in words, or another program's output on stdin, a
//! line an event, through the server that holds the directory or a sidecar
//! it starts; `list` reads a stream beside whoever holds it.
//!
//! ```text
//! tinystore events ./data append audit role.changed user:=42 role=admin
//! my-worker 2>&1 | tinystore events ./data append worker
//! tinystore events ./data list audit --since 1h --where user:=42
//! ```

use std::io::{IsTerminal as _, Read};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tinystore::client::Event;
use tinystore::{ErrorKind, Store};

use crate::asked::{self, Asked, Done, Refused, Verb, failed, wrong};
use crate::help;
use crate::logs::{Printer, level_named};
use crate::reach;
use crate::window::{self, Window};

const VERBS: [Verb; 2] = [
    Verb { word: "append", command: &help::EVENTS_APPEND, run: append },
    Verb { word: "list", command: &help::EVENTS_LIST, run: list },
];

/// The events a page of `list` holds unless `--limit` says.
const PAGE: usize = 100;
/// The range `list` reads when none is given.
const LAST_HOUR: Duration = Duration::from_secs(3600);

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run_verb("events", &VERBS, args)
}

fn append(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("events", "append", false)?;
    let Some((stream, given)) = rest.split_first() else {
        return Err(wrong("append takes a stream: append audit role.changed user:=42"));
    };
    match given.split_first() {
        Some((name, fields)) => append_one(&asked, &dir, stream, name, fields),
        None => append_output(&asked, &dir, stream),
    }
}

/// Writes one event given in words; one whose id the stream holds writes
/// nothing, so that a command sent again is harmless.
fn append_one(asked: &Asked, dir: &Path, stream: &str, name: &str, fields: &[String]) -> Done {
    let level = asked.value("--level").map(level_of).transpose()?;
    let event = Event {
        name: name.to_owned(),
        fields: fields_of(fields)?.map(|fields| Value::Object(fields).to_string()),
        level,
        at: asked.value("--at").map(window::time).transpose().map_err(wrong)?,
        id: asked.value("--id").map(str::to_owned),
    };
    let id = event.id.clone().unwrap_or_default();
    let into = stream.to_owned();
    let held = reach::write(dir, "events append", move |client| client.append(&into, std::slice::from_ref(&event)))?;
    if held == 0 {
        outln!("appended {name} to {stream}")
    } else {
        outln!("{stream} holds the id {id}: nothing written")
    }
}

/// Writes stdin to the stream as `Events::lines` takes another program's
/// output, until stdin ends.
fn append_output(asked: &Asked, dir: &Path, stream: &str) -> Done {
    if let Some(flag) = ["--id", "--at", "--level"].into_iter().find(|flag| asked.has(flag)) {
        return Err(wrong(format!("{flag} names one event given in words; a line of stdin says its own")));
    }
    if std::io::stdin().is_terminal() {
        return Err(wrong(
            "append takes an event's name, or another program's output on stdin\n  my-worker 2>&1 | tinystore \
             events ./data append worker",
        ));
    }
    let into = stream.to_owned();
    let (dropped, lines) = reach::stream(dir, "events append", move |client| {
        let mut input = Lines { stdin: std::io::stdin().lock(), newlines: 0, open: false };
        let dropped = client.append_lines(&into, &mut input, |error| tinystore::Error::io("stdin", error))?;
        Ok((dropped, input.newlines + u64::from(input.open)))
    })?;
    let dropped = if dropped == 0 { String::new() } else { format!(", {dropped} of them dropped") };
    outln!("appended {} to {stream}{dropped}", counted(lines, "line"))
}

/// Stdin, its lines counted as they pass: those that ended, and whether the
/// last did not.
struct Lines<R> {
    stdin: R,
    newlines: u64,
    open: bool,
}

impl<R: Read> Read for Lines<R> {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        let read = self.stdin.read(into)?;
        if let Some(last) = into[..read].last() {
            self.newlines += into[..read].iter().filter(|byte| **byte == b'\n').count() as u64;
            self.open = *last != b'\n';
        }
        Ok(read)
    }
}

fn list(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("events", "list", true)?;
    let [stream] = &rest[..] else {
        return Err(wrong("list takes a stream: list audit"));
    };
    let window = Window::asked(&asked, LAST_HOUR)?;
    let most = asked.count("--limit", PAGE)?;
    asked::existing(&dir)?;
    let store = Store::open_beside(&dir).map_err(failed)?;
    // opening the stream itself would make it, which a read beside cannot
    let all = match store.all_events() {
        Ok(all) => all,
        Err(error) if error.kind() == ErrorKind::NotFound => return outln!("{} holds no events yet", dir.display()),
        Err(error) => return Err(failed(error)),
    };
    if !all.streams()?.iter().any(|held| held.name == *stream) {
        return outln!("{} holds no stream {stream}", dir.display());
    }
    let mut read = all.list().streams([stream.as_str()]).from(window.from).limit(most);
    if let Some(to) = window.to {
        read = read.to(to);
    }
    if let Some(level) = asked.value("--level") {
        read = read.min_level(level_named(level).ok_or_else(|| wrong(level_words(level)))?);
    }
    for given in asked.values("--where") {
        let (key, value) = field_of(&given)?;
        read = read.filter(&key, &value);
    }
    if let Some(after) = asked.value("--after") {
        read = read.after(after);
    }
    let page = read.page()?;
    let json = asked.has("--json");
    let printer = Printer::new(json);
    for event in &page.events {
        printer.print(event)?;
    }
    match page.next {
        Some(next) if json => outln!("{}", json!({ "next": next }))?,
        Some(next) => asked::say("events list", format!("a page of {most} events at most; --after {next} reads on")),
        None if page.events.is_empty() => outln!("no events of {stream} in {}", window.shown())?,
        None => {}
    }
    Ok(())
}

/// An event's fields as words give them: `role=admin` a text, `user:=42` any
/// JSON value.
fn fields_of(given: &[String]) -> Result<Option<Map<String, Value>>, Refused> {
    if given.is_empty() {
        return Ok(None);
    }
    let fields = given.iter().map(|word| field_of(word)).collect::<Result<_, _>>()?;
    Ok(Some(fields))
}

fn field_of(word: &str) -> Result<(String, Value), Refused> {
    if let Some((key, json)) = word.split_once(":=").filter(|(key, _)| !key.is_empty()) {
        let value = serde_json::from_str(json).map_err(|error| wrong(format!("{word}: {json} is no JSON: {error}")))?;
        return Ok((key.to_owned(), value));
    }
    match word.split_once('=').filter(|(key, _)| !key.is_empty()) {
        Some((key, text)) => Ok((key.to_owned(), Value::String(text.to_owned()))),
        None => Err(wrong(format!("{word}: a field and its value, as role=admin for text or user:=42 for JSON"))),
    }
}

fn level_of(name: &str) -> Result<i64, Refused> {
    level_named(name).map(|level| i64::from(level.number())).ok_or_else(|| wrong(level_words(name)))
}

fn level_words(name: &str) -> String {
    format!("--level {name}: trace, debug, info, warn or error")
}

fn counted(count: u64, what: &str) -> String {
    if count == 1 { format!("1 {what}") } else { format!("{count} {what}s") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_is_text_unless_it_says_json() {
        assert_eq!(field_of("role=admin").unwrap(), ("role".to_owned(), json!("admin")));
        assert_eq!(field_of("zip=02134").unwrap(), ("zip".to_owned(), json!("02134")), "nothing guessed");
        assert_eq!(field_of("user:=42").unwrap(), ("user".to_owned(), json!(42)));
        assert_eq!(field_of("tags:=[\"a\",\"b\"]").unwrap().1, json!(["a", "b"]));
        assert_eq!(field_of("note=a=b").unwrap().1, json!("a=b"));
        assert!(field_of("user:=forty").is_err() && field_of("admin").is_err() && field_of("=x").is_err());
    }

    #[test]
    fn stdins_lines_are_counted_as_they_pass_the_last_one_open_too() {
        let mut input = Lines { stdin: &b"one\ntwo\nthree"[..], newlines: 0, open: false };
        std::io::copy(&mut input, &mut std::io::sink()).unwrap();
        assert_eq!(input.newlines + u64::from(input.open), 3);
    }
}
