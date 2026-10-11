//! Another program's output as events: each line a writer is given becomes
//! an event of its stream at the time the line arrived, and the lines of a
//! stack trace, a traceback or a JSON value printed over several are joined
//! into one.
//!
//! ```text
//! 2026-09-26 12:00:01,500 ERROR request failed   ┐
//! Traceback (most recent call last):             │ one event named log, its level error,
//!   File "app.py", line 3, in handle             │ its message the four lines
//! ValueError: bad input                          ┘
//! {"level":30,"msg":"ok"}                        an event named json: its fields level and msg
//! level=warn msg="slow request" ms=1200          an event named logfmt: its fields its pairs
//! ```

use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::buffer::{BUFFER, Buffer};
use super::event::{MAX_EVENT, MAX_KEYS, weight};
use super::events::Events;
use super::level::level_of;
use super::logfmt::logfmt_fields;
use crate::console::{Keep, Line, object_pairs, quoted};
use crate::{Clock, Error, Result, unix_nanos};

/// The names of the events lines become, which say how each spells its line
/// again: by its message, by a JSON object's fields, by logfmt's pairs.
const TEXT: &str = "log";
const JSON: &str = "json";
const LOGFMT: &str = "logfmt";
/// The lines one event joins at most.
const MOST_JOINED: usize = 1000;

impl Events {
    /// A writer of another program's output, a child's standard output or a
    /// file followed: each line it is given becomes an event of this stream,
    /// and a write never waits. What its buffer has no room for, and a line
    /// longer than an event holds, are dropped and counted. Dropped, it hands
    /// over what it holds, a last line without its newline too.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// let mut worker = std::process::Command::new("worker").stdout(std::process::Stdio::piped()).spawn()?;
    /// let mut out = store.events("worker").open()?.lines()?;
    /// std::io::copy(worker.stdout.as_mut().unwrap(), &mut out)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// An event named `log` keeps its text as its message, a line that is no
    /// UTF-8 with U+FFFD for each byte that spells nothing. A JSON object's
    /// fields and a logfmt line's pairs become the event's fields, named
    /// `json` or `logfmt`, when they spell the line again. Its level is found
    /// where pino, logfmt, glog, Redis, log4j, zerolog and Postgres write it.
    pub fn lines(&self) -> Result<Lines> {
        let buffer = Buffer::open(&self.records, self.stream.clone(), BUFFER, None)?;
        let writer = Arc::new(LineWriter {
            stream: self.stream.name.to_string(),
            clock: self.records.clock(),
            buffer,
            room: MAX_EVENT - weight(TEXT, None, &[], &[]),
            writing: Mutex::default(),
        });
        self.records.add_writer(&writer);
        Ok(Lines { writer })
    }
}

/// A writer of another program's lines into a stream, which never waits.
pub struct Lines {
    writer: Arc<LineWriter>,
}

impl fmt::Debug for Lines {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lines({})", self.writer.stream)
    }
}

impl Lines {
    /// The lines dropped since the writer began: a full buffer, a line
    /// longer than an event holds, one the file refused.
    pub fn dropped(&self) -> u64 {
        self.writer.buffer.dropped()
    }
}

impl io::Write for Lines {
    /// Takes every byte at once, and fails only once the writer or its store
    /// closed.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer.write(bytes).map_err(io::Error::other)?;
        Ok(bytes.len())
    }

    /// Writes the events of every line ended so far, and waits for them; a
    /// line without its newline yet stays.
    fn flush(&mut self) -> io::Result<()> {
        let joining = self.writer.lock().pending.take();
        self.writer.hand_over_now(joining);
        self.writer.buffer.write_waiting(false).map_err(io::Error::other)
    }
}

impl Drop for Lines {
    fn drop(&mut self) {
        self.writer.close();
    }
}

/// What a writer of lines holds between its writes, which the store's flush
/// asks for an event that stopped growing.
pub(super) struct LineWriter {
    stream: String,
    clock: Arc<dyn Clock>,
    buffer: Arc<Buffer>,
    /// The most text one event keeps: a longer line is dropped whole.
    room: usize,
    writing: Mutex<Writing>,
}

#[derive(Default)]
struct Writing {
    /// A line not ended yet.
    partial: Vec<u8>,
    /// The line being written cannot fit an event: the rest of it is dropped
    /// up to its newline.
    discarding: bool,
    pending: Option<Pending>,
    closed: bool,
}

/// An event whose lines are not all given yet.
struct Pending {
    text: Vec<u8>,
    /// When its first line arrived.
    at: i64,
    lines: usize,
    open: Brackets,
    /// A flush found it, and it has not grown since.
    waited: bool,
}

impl LineWriter {
    fn lock(&self) -> MutexGuard<'_, Writing> {
        self.writing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self, mut bytes: &[u8]) -> Result<()> {
        let mut writing = self.lock();
        if writing.closed {
            return Err(Error::closed(format!("lines of events {}", self.stream)));
        }
        while let Some(end) = bytes.iter().position(|byte| *byte == b'\n') {
            self.add_to_line(&mut writing, &bytes[..end]);
            let line = std::mem::take(&mut writing.partial);
            if !writing.discarding
                && let Some(ended) = self.feed(&mut writing, &line)
            {
                self.buffer.keep(self.event_of(ended));
            }
            writing.partial = line;
            writing.partial.clear();
            writing.discarding = false;
            bytes = &bytes[end + 1..];
        }
        self.add_to_line(&mut writing, bytes);
        Ok(())
    }

    /// Adds `part` to the line being written, or drops the whole line once it
    /// grows past what an event holds.
    fn add_to_line(&self, writing: &mut Writing, part: &[u8]) {
        if writing.discarding || part.is_empty() {
            return;
        }
        if part.len() > self.room - writing.partial.len() {
            writing.partial = Vec::new();
            writing.discarding = true;
            if let Some(ended) = writing.pending.take() {
                self.buffer.keep(self.event_of(ended));
            }
            self.buffer.count_dropped();
            return;
        }
        // grown by doubling, as a vector grows, but never past what an event holds
        let wanted = writing.partial.len() + part.len();
        if wanted > writing.partial.capacity() {
            let grown = wanted.max(2 * writing.partial.capacity()).min(self.room);
            writing.partial.reserve_exact(grown - writing.partial.len());
        }
        writing.partial.extend_from_slice(part);
    }

    /// Joins a line to the event being joined, or begins the next with it and
    /// gives back the event it ended.
    fn feed(&self, writing: &mut Writing, line: &[u8]) -> Option<Pending> {
        if let Some(pending) = &mut writing.pending
            && pending.joins(line, self.room)
        {
            pending.add(line);
            return None;
        }
        let at = unix_nanos(self.clock.now());
        let mut begun = Pending { text: Vec::new(), at, lines: 0, open: Brackets::default(), waited: false };
        begun.add(line);
        writing.pending.replace(begun)
    }

    /// Hands events over at a flush or as the writer closes, when the buffer
    /// is written next: even a full buffer holds them, as Go's flush took
    /// what waited before it asked its writers.
    fn hand_over_now(&self, events: impl IntoIterator<Item = Pending>) {
        events.into_iter().for_each(|event| self.buffer.keep_past_bound(self.event_of(event)));
    }

    /// Hands over an event that did not grow since the flush before.
    pub(super) fn hand_over_idle(&self) {
        let mut writing = self.lock();
        let Some(pending) = &mut writing.pending else { return };
        // marked now, it goes at the next flush unless a line joins it before
        if std::mem::replace(&mut pending.waited, true) {
            let idle = writing.pending.take();
            self.hand_over_now(idle);
        }
    }

    /// Hands over all it holds, a last line without its newline too, and
    /// takes nothing more.
    pub(super) fn close(&self) {
        let mut writing = self.lock();
        if writing.closed {
            return;
        }
        let line = std::mem::take(&mut writing.partial);
        let ended = if line.is_empty() || writing.discarding { None } else { self.feed(&mut writing, &line) };
        let joining = writing.pending.take();
        self.hand_over_now(ended.into_iter().chain(joining));
        writing.closed = true;
    }

    /// An event's line: a JSON object's fields or logfmt's pairs when they
    /// spell its text again and fit an event, and its text otherwise.
    fn event_of(&self, pending: Pending) -> Line {
        let text = match String::from_utf8(pending.text) {
            Ok(text) => text,
            Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
        };
        let fields = exact_fields(&text)
            .map(|fields| (JSON, fields))
            .or_else(|| logfmt_fields(&text).map(|fields| (LOGFMT, fields)))
            .filter(|(name, fields)| weight(name, None, fields, &[]) <= MAX_EVENT);
        let level = level_of(&text, fields.as_ref().map(|(_, fields)| fields.as_slice()));
        let (name, message, fields) = match fields {
            Some((name, fields)) => (name, None, fields),
            None => (TEXT, Some(text), Vec::new()),
        };
        Line {
            at: pending.at,
            stream: self.stream.clone(),
            name: name.to_owned(),
            level,
            message,
            fields,
            ..Line::default()
        }
    }
}

impl Pending {
    fn add(&mut self, line: &[u8]) {
        if self.lines > 0 {
            self.text.push(b'\n');
        }
        self.text.extend_from_slice(line);
        self.lines += 1;
        self.waited = false;
        if self.lines == 1 && (line.starts_with(b"{") || line.trim_ascii() == b"[") {
            self.open.json = true;
        }
        self.open.scan(line);
    }

    /// Whether a line belongs to the event before it: an indented line, the
    /// lines that open a traceback or name an exception or its cause, and
    /// every line of a JSON value its first line left open.
    ///
    /// ```text
    /// 2026-09-26 12:00:01,500 ERROR request failed   an event
    /// Traceback (most recent call last):             joins: a traceback
    ///   File "app.py", line 3, in handle             joins: indented
    /// ValueError: bad input                          joins: names an exception
    /// 2026-09-26 12:00:02,000 INFO next              an event
    /// {                                              an event, a JSON value begun on a line of its own
    /// "status": 200                                  joins: the value is open
    /// }                                              joins, and closes it
    /// [2026-09-26 12:00:03] info done                an event: brackets begin no value but a line's own
    /// ```
    fn joins(&self, line: &[u8], room: usize) -> bool {
        if self.lines >= MOST_JOINED || self.text.len() + 1 + line.len() > room {
            return false;
        }
        self.open.depth > 0 || line.starts_with(b" ") || line.starts_with(b"\t") || continues(line)
    }
}

/// Whether a stack trace goes on with `line`: a cause, a traceback, a
/// goroutine, or the name of an exception, alone or before a colon.
///
/// ```text
/// ValueError: bad input    java.io.IOException: disk    KeyError
/// ```
fn continues(line: &[u8]) -> bool {
    let opens = [&b"Caused by: "[..], b"Traceback (most recent call last):", b"goroutine "];
    if opens.iter().any(|opening| line.starts_with(opening)) {
        return true;
    }
    let name = line.split(|byte| *byte == b':').next().unwrap_or_default();
    let named = name.iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'$'));
    named && [&b"Error"[..], b"Exception", b"Warning", b"Throwable"].iter().any(|kind| name.ends_with(kind))
}

/// A JSON value followed over its lines.
#[derive(Default)]
struct Brackets {
    /// The first line began a value.
    json: bool,
    depth: i64,
    in_string: bool,
    escape: bool,
}

impl Brackets {
    fn scan(&mut self, line: &[u8]) {
        if !self.json {
            return;
        }
        for &byte in line {
            match byte {
                _ if self.escape => self.escape = false,
                b'\\' if self.in_string => self.escape = true,
                b'"' => self.in_string = !self.in_string,
                _ if self.in_string => {}
                b'{' | b'[' => self.depth += 1,
                b'}' | b']' => self.depth -= 1,
                _ => {}
            }
        }
        self.in_string &= self.depth > 0;
        self.json = self.depth > 0;
    }
}

/// A JSON object's fields when, written one after another without spaces,
/// they spell its text again: nothing of the line is lost.
///
/// ```text
/// {"level":30,"msg":"ok"}    → level 30, msg "ok"
/// { "level": 30 }            → stays text: its spaces would be lost
/// ```
fn exact_fields(text: &str) -> Option<Vec<(String, String)>> {
    if text.len() < 2 || !text.starts_with('{') || !text.ends_with('}') {
        return None;
    }
    let fields = object_pairs(text).ok().filter(|fields| !fields.is_empty() && fields.len() <= MAX_KEYS)?;
    let spelled: Vec<String> = fields.iter().map(|(key, value)| format!("{}:{value}", quoted(key))).collect();
    (format!("{{{}}}", spelled.join(",")) == text).then_some(fields)
}

#[cfg(test)]
#[path = "lines_tests.rs"]
mod tests;
