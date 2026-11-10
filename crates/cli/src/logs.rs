//! `tinystore logs`: the application's logs and events from its store's
//! `records.db`, printed as its logger prints them, and with `-f` the new
//! ones as they arrive. It reads beside the program or server that holds the
//! directory and writes nothing.
//!
//! ```text
//! 11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200
//! 11:02:15.911 ERROR api  payment failed  orderId=981 reason="card declined"
//! ```
//!
//! Following reads again every half second from a little before the newest
//! line printed, since another process's line may commit late with an
//! earlier time, and skips the lines it printed already.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiff::Zoned;
use nu_ansi_term::{Color, Style};
use tinystore::console::{escaped, lines_of, push_below};
use tinystore::mcp::logs::json;
pub(crate) use tinystore::mcp::logs::level_named;
use tinystore::records::{AllEvents, Event, Level, List};
use tinystore::{ErrorKind, Store};

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;
use crate::lines::{Terminal, paint};

/// How many of the last lines print without `-n` or `--since`.
const LAST: usize = 20;
/// How often following reads again.
const EVERY: Duration = Duration::from_millis(500);
/// How far before the newest line printed following reads again: a line
/// another process logged may commit up to a second late, its buffer's.
const LATE: Duration = Duration::from_secs(10);
/// What ends every colour a terminal shows.
const RESET: &str = "\x1b[0m";

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::LOGS, args, logs)
}

/// What `logs` was asked.
struct Wanted {
    last: Option<usize>,
    level: Option<Level>,
    since: Option<Duration>,
    grep: Option<String>,
    stream: Option<String>,
    follow: bool,
    json: bool,
}

fn logs(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_or_here(0)?;
    if !rest.is_empty() {
        return Err(wrong(format!("one directory, not {} too", rest.join(" "))));
    }
    let wanted = Wanted {
        last: asked
            .value("-n")
            .map(|count| count.parse().map_err(|_| wrong(format!("-n {count}: a count"))))
            .transpose()?,
        level: asked.value("--level").map(level).transpose()?,
        since: asked.value("--since").map(asked::span).transpose().map_err(wrong)?,
        grep: asked.value("--grep").map(str::to_owned),
        stream: asked.value("--stream").map(str::to_owned),
        follow: asked.has("-f"),
        json: asked.has("--json"),
    };
    let printer = Printer::new(wanted.json);
    let mut printed = Printed::default();
    match read(&dir, |events| first(&narrowed(events.list(), &wanted, None), &wanted))? {
        Some(events) => {
            for event in events {
                printer.print(&event)?;
                printed.add(&event);
            }
        }
        None => {
            outln!("{} holds no logs yet: a program's logger keeps them, as store.logger(\"api\") does", dir.display())?
        }
    }
    if wanted.follow {
        follow(&dir, &wanted, &printer, printed)?;
    }
    Ok(())
}

/// Prints the lines that arrive from now on, until Ctrl+C. The first read
/// again meets lines of before the first page that it left out, and passes
/// over those as old as the newest it printed.
fn follow(dir: &Path, wanted: &Wanted, printer: &Printer, mut printed: Printed) -> Done {
    let started = printed.newest.unwrap_or_else(SystemTime::now);
    let mut first = true;
    loop {
        let from = printed.newest.unwrap_or(started) - LATE;
        let found = read(dir, |events| {
            narrowed(events.list(), wanted, Some(from)).all().collect::<tinystore::Result<Vec<_>>>()
        })?;
        for event in found.unwrap_or_default() {
            if printed.add(&event) && (!first || event.at > started) {
                printer.print(&event)?;
            }
        }
        first = false;
        printed.forget_before(from);
        std::thread::sleep(EVERY);
    }
}

/// What `read` makes of the store's events, the store opened again beside
/// whoever holds it, so that it meets the streams made since; none while the
/// store keeps no events.
fn read<T>(dir: &Path, read: impl FnOnce(&AllEvents) -> tinystore::Result<T>) -> Result<Option<T>, asked::Refused> {
    let store = Store::open_beside(dir).map_err(|error| match error.kind() {
        ErrorKind::NotFound => failed(format!("{}: no such directory", dir.display())),
        _ => error.into(),
    })?;
    let events = match store.all_events() {
        Ok(events) => events,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(read(&events)?))
}

fn level(name: &str) -> Result<Level, asked::Refused> {
    level_named(name).ok_or_else(|| wrong(format!("--level {name}: trace, debug, info, warn or error")))
}

/// The read every page of `logs` makes, from `from` on when following.
fn narrowed(mut list: List, wanted: &Wanted, from: Option<SystemTime>) -> List {
    if let Some(level) = wanted.level {
        list = list.min_level(level);
    }
    if let Some(stream) = &wanted.stream {
        list = list.streams([stream.as_str()]);
    }
    if let Some(grep) = &wanted.grep {
        list = list.search(grep);
    }
    match (from, wanted.since) {
        (Some(from), _) => list.from(from),
        (None, Some(since)) => list.since(since),
        (None, None) => list,
    }
}

/// The lines printed first, oldest first: every line of `--since` unless
/// `-n` says how many of the last, or the last twenty.
fn first(list: &List, wanted: &Wanted) -> tinystore::Result<Vec<Event>> {
    let last = match (wanted.last, wanted.since) {
        (Some(last), _) => last,
        (None, Some(_)) => return list.clone().all().collect(),
        (None, None) => LAST,
    };
    let mut events: Vec<Event> = list.clone().newest().all().take(last).collect::<Result<_, _>>()?;
    events.reverse();
    Ok(events)
}

/// The lines printed lately, so that a read again prints each once: by what
/// each holds, counted, so that two alike both print.
#[derive(Default)]
struct Printed {
    seen: HashMap<String, usize>,
    /// What this read met of each, counted again from none at every read.
    met: HashMap<String, usize>,
    newest: Option<SystemTime>,
}

impl Printed {
    /// Counts the line met; true when it was not printed before.
    fn add(&mut self, event: &Event) -> bool {
        let key = identity(event);
        let met = self.met.entry(key.clone()).or_default();
        *met += 1;
        let seen = self.seen.entry(key).or_default();
        let new = *met > *seen;
        if new {
            *seen = *met;
            self.newest = self.newest.max(Some(event.at));
        }
        new
    }

    /// Lets go of the lines before the next read's start, and starts the
    /// next read's counts.
    fn forget_before(&mut self, from: SystemTime) {
        let from = nanos(from);
        self.seen.retain(|key, _| {
            key.split_once(' ').and_then(|(at, _)| at.parse::<u128>().ok()).is_some_and(|at| at >= from)
        });
        self.met.clear();
    }
}

/// What tells two lines apart: their time, stream, name, message and fields.
fn identity(event: &Event) -> String {
    let fields: Vec<String> = event.fields.iter().map(|(key, json)| format!("{key}={json}")).collect();
    format!(
        "{} {}\u{0}{}\u{0}{}\u{0}{}",
        nanos(event.at),
        event.stream,
        event.name,
        event.message.as_deref().unwrap_or_default(),
        fields.join("\u{0}")
    )
}

fn nanos(at: SystemTime) -> u128 {
    at.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos())
}

fn today() -> String {
    Zoned::now().strftime("%Y-%m-%d").to_string()
}

/// How a line prints: as the console's pretty line, or one JSON object.
pub(crate) struct Printer {
    colours: bool,
    json: bool,
    /// A line of another day says its date.
    today: String,
}

impl Printer {
    /// A printer for this terminal, or of JSON lines when `json`.
    pub(crate) fn new(json: bool) -> Printer {
        Printer { colours: Terminal::of_stdout().colours(), json, today: today() }
    }

    pub(crate) fn print(&self, event: &Event) -> Done {
        if self.json { outln!("{}", json(event)) } else { outln!("{}", self.pretty(event)) }
    }

    /// `11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200`, and
    /// below it, as the console lays them out, the message's further lines
    /// and each value of several lines.
    fn pretty(&self, event: &Event) -> String {
        let paint = |style: Style, text: &str| paint(self.colours, style, text);
        let dim = Style::new().dimmed();
        let at = jiff::Timestamp::try_from(event.at).map(|at| at.to_zoned(jiff::tz::TimeZone::system()));
        let time = match at {
            Ok(at) if at.strftime("%Y-%m-%d").to_string() == self.today => at.strftime("%H:%M:%S%.3f").to_string(),
            Ok(at) => at.strftime("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            Err(_) => "?".to_owned(),
        };
        let level = event.level.map_or_else(|| "EVENT".to_owned(), |level| level.name().to_ascii_uppercase());
        let shade = match event.level {
            Some(level) if level >= Level::ERROR => Color::Red.bold(),
            Some(level) if level >= Level::WARN => Color::Yellow.normal(),
            Some(level) if level >= Level::INFO => Color::Cyan.normal(),
            Some(_) => Color::Purple.dimmed(),
            None => Color::Green.normal(),
        };
        let mut message = lines_of(event.message.as_deref().unwrap_or_default());
        let said = message.next().filter(|first| !first.is_empty()).unwrap_or(&event.name);
        let mut line = format!(
            "{} {} {}  {}",
            paint(dim, &time),
            paint(shade, &format!("{level:<5}")),
            paint(Style::new().bold(), &event.stream),
            shown(said, self.colours)
        );
        let (fields, below) = self.fields(event);
        if !fields.is_empty() {
            line.push_str("  ");
            line.push_str(&fields.join(" "));
        }
        push_below(&mut line, None, message, |out, part| out.push_str(&shown(part, self.colours)));
        for (key, text) in below {
            let show = |out: &mut String, part: &str| out.push_str(&paint(dim, &shown(part, self.colours)));
            push_below(&mut line, Some(key), lines_of(&text), show);
        }
        line
    }

    /// An event's fields and context as its line shows them, and those whose
    /// text is of several lines, which go below it instead, each by its key.
    fn fields<'e>(&self, event: &'e Event) -> (Vec<String>, Vec<(&'e str, String)>) {
        let (mut fields, mut below) = (Vec::new(), Vec::new());
        for (key, json) in event.fields.iter().chain(event.context.iter()) {
            match value(json, self.colours) {
                (text, true) => below.push((key, text)),
                (text, false) => {
                    let key = paint(self.colours, Style::new().dimmed(), format!("{}=", shown(key, self.colours)));
                    fields.push(format!("{key}{text}"));
                }
            }
        }
        (fields, below)
    }
}

/// How a field's value shows on a pretty line, and whether it is text of
/// several lines, which goes below the line instead, as it is: a string bare
/// unless it holds a space, a quote or an `=`, and then in quotes as JSON
/// spells it; anything else as its JSON. Text in it shows as [`shown`] shows
/// a stored text.
fn value(json: &str, colours: bool) -> (String, bool) {
    let Ok(text) = serde_json::from_str::<String>(json) else { return (escaped(json), false) };
    if text.contains('\n') {
        return (text, true);
    }
    let plain = with_colours(&text, false, str::to_owned);
    if !plain.is_empty() && !plain.contains([' ', '"', '=']) {
        return (shown(&text, colours), false);
    }
    let spelled = |part: &str| {
        let json = serde_json::Value::from(part).to_string();
        escaped(&json[1..json.len() - 1])
    };
    (format!("\"{}\"", with_colours(&text, colours, spelled)), false)
}

/// A text the store holds as `logs` shows it: as the console shows what a
/// program logged, each control character but a tab, and DEL, as its escape,
/// but for the colours another program wrote into its output.
///
/// ```text
/// "\x1b[31mfailed\x1b[0m\r\x1b[2K"  →  failed, in red where logs paints, then \r\u001b[2K
/// ```
fn shown(text: &str, colours: bool) -> String {
    with_colours(text, colours, escaped)
}

/// `text` with each colour sequence in it, `ESC [` then digits, `;` and `:`
/// alone and then `m`, left out where `logs` paints no colours, so that a
/// pipe gets no escape byte, and let through where it does, a reset after
/// the text then, so that a colour it never reset ends with it rather than
/// colour what prints next; `spell` writes what lies between them.
fn with_colours(text: &str, colours: bool, spell: impl Fn(&str) -> String) -> String {
    let (mut out, mut rest, mut painted) = (String::with_capacity(text.len()), text, false);
    while let Some((start, end)) = colour_in(rest) {
        out.push_str(&spell(&rest[..start]));
        if colours {
            out.push_str(&rest[start..end]);
            painted = true;
        }
        rest = &rest[end..];
    }
    out.push_str(&spell(rest));
    if painted {
        out.push_str(RESET);
    }
    out
}

/// Where the first colour sequence of `text` begins and where it ends.
fn colour_in(text: &str) -> Option<(usize, usize)> {
    text.match_indices("\x1b[").find_map(|(start, _)| {
        let after = &text[start + 2..];
        let length = after.find(|c: char| !matches!(c, '0'..='9' | ';' | ':'))?;
        after[length..].starts_with('m').then_some((start, start + 2 + length + 1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinystore::Options;

    fn wanted() -> Wanted {
        Wanted { last: None, level: None, since: None, grep: None, stream: None, follow: false, json: false }
    }

    #[test]
    fn a_line_prints_as_the_loggers_pretty_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let api = store.events("api").open().unwrap();
        api.event("log")
            .level(Level::WARN)
            .message("slow request")
            .append(&serde_json::json!({ "requestId": "7f3a", "ms": 1200, "why": "a b" }))
            .unwrap();
        let beside = Store::open_beside(dir.path()).unwrap();
        let events = first(&narrowed(beside.all_events().unwrap().list(), &wanted(), None), &wanted()).unwrap();
        assert_eq!(events.len(), 1);
        let printer = Printer { colours: false, json: false, today: today() };
        let line = printer.pretty(&events[0]);
        assert!(line.ends_with(" WARN  api  slow request  ms=1200 requestId=7f3a why=\"a b\""), "{line}");
        let object: serde_json::Value = serde_json::from_str(&json(&events[0])).unwrap();
        assert_eq!((object["level"].as_str(), object["ms"].as_u64()), (Some("WARN"), Some(1200)));
    }

    /// An event's pretty line with colours off and on.
    fn printed(event: &Event) -> (String, String) {
        let print = |colours| Printer { colours, json: false, today: today() }.pretty(event);
        (print(false), print(true))
    }

    /// Whether a printed event holds a control character a terminal acts on,
    /// an escape byte among them unless `escapes` lets it through, or a line
    /// below its own that does not begin indented, as a line of its own would.
    fn raw(printed: &str, escapes: bool) -> bool {
        let acts =
            |line: &str| line.chars().any(|c| (c < ' ' && c != '\t' && !(escapes && c == '\x1b')) || c == '\x7f');
        let mut lines = printed.split('\n');
        lines.next().is_some_and(acts) || lines.any(|line| acts(line) || !line.starts_with("    "))
    }

    #[test]
    fn logs_prints_a_stored_messages_control_characters_as_their_escapes_and_its_colours_only_where_it_paints() {
        // a stored message; how logs shows it with colours off; and with them on
        let cases = [
            ("\x1b[2Kerased", r"\u001b[2Kerased", r"\u001b[2Kerased"),
            ("one\rtwo", r"one\rtwo", r"one\rtwo"),
            ("del\x7f", r"del\u007f", r"del\u007f"),
            ("tab\there", "tab\there", "tab\there"),
            ("first\nsec\rond\r\nthird\r\n", "first\n    sec\\rond\n    third", "first\n    sec\\rond\n    third"),
            ("\x1b[31mfailed\x1b[0m", "failed", "\x1b[31mfailed\x1b[0m\x1b[0m"),
            ("\x1b[31mfailed", "failed", "\x1b[31mfailed\x1b[0m"),
            (
                "\x1b[31mred one\nred two\x1b[0m",
                "red one\n    red two",
                "\x1b[31mred one\x1b[0m\n    red two\x1b[0m\x1b[0m",
            ),
            ("\x1b[38;5;196mx\x1b[m", "x", "\x1b[38;5;196mx\x1b[m\x1b[0m"),
            ("\x1b[38:2::255:0:0mx", "x", "\x1b[38:2::255:0:0mx\x1b[0m"),
            ("\x1b[?25lhidden", r"\u001b[?25lhidden", r"\u001b[?25lhidden"),
            ("\x1b]52;c;aGk=\x07", r"\u001b]52;c;aGk=\u0007", r"\u001b]52;c;aGk=\u0007"),
            ("\x1b[31", r"\u001b[31", r"\u001b[31"),
            ("été\x1b[1mgras", "étégras", "été\x1b[1mgras\x1b[0m"),
        ];
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let worker = store.events("worker").open().unwrap();
        for (message, ..) in cases {
            worker.event("log").level(Level::ERROR).message(message).append(&serde_json::json!({})).unwrap();
        }
        let beside = Store::open_beside(dir.path()).unwrap();
        let events = first(&narrowed(beside.all_events().unwrap().list(), &wanted(), None), &wanted()).unwrap();
        assert_eq!(events.len(), cases.len());
        let mut wrong = Vec::new();
        for (event, (message, off, on)) in events.iter().zip(cases) {
            let (plain, painted) = printed(event);
            let right = plain.ends_with(&format!("ERROR worker  {off}")) && painted.ends_with(&format!("  {on}"));
            if !right || raw(&plain, false) || raw(&painted, true) {
                wrong.push(format!("{message:?}: {plain:?} {painted:?}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn events_list_prints_a_stored_events_name_and_values_so_too() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let audit = store.events("audit").open().unwrap();
        let fields = serde_json::json!({
            "note": "a b\x7f",
            "reason": "\x1b[31mcard \"declined\"\x1b[0m",
            "tint": "\x1b[32mok\x1b[0m",
            "who": "ann\n12:00:00.000 INFO  api  admin signed in\n",
            "z\x1b[2J": 1,
        });
        audit.event("\x1b[31msignup\x1b[0m\x1b[2K\r").append(&fields).unwrap();
        let beside = Store::open_beside(dir.path()).unwrap();
        let page = beside.all_events().unwrap().list().streams(["audit"]).page().unwrap();
        let (plain, painted) = printed(&page.events[0]);
        // a value of several lines goes below the line, each of its lines indented
        let below = "\n    who: ann\n    12:00:00.000 INFO  api  admin signed in";
        let shown = r#"note="a b\u007f" reason="card \"declined\"" tint=ok z\u001b[2J=1"#;
        assert!(plain.ends_with(&format!("EVENT audit  signup\\u001b[2K\\r  {shown}{below}")), "{plain:?}");
        let dim = |text: &str| paint(true, Style::new().dimmed(), text);
        let (note, reason, tint, z) = (dim("note="), dim("reason="), dim("tint="), dim(r"z\u001b[2J="));
        let (red, green, reset) = ("\x1b[31m", "\x1b[32m", "\x1b[0m");
        let shown = format!(
            r#"{note}"a b\u007f" {reason}"{red}card \"declined\"{reset}{reset}" {tint}{green}ok{reset}{reset} {z}1"#
        );
        let below = format!("\n    {}\n    {}", dim("who: ann"), dim("12:00:00.000 INFO  api  admin signed in"));
        let named = format!("{red}signup{reset}\\u001b[2K\\r{reset}");
        assert!(painted.ends_with(&format!("  {named}  {shown}{below}")), "{painted:?}");
        assert!(!raw(&plain, false) && !raw(&painted, true), "{plain:?} {painted:?}");
    }

    #[test]
    fn following_prints_each_line_once_and_two_alike_twice() {
        let at = SystemTime::now();
        let event = Event {
            at,
            stream: "api".to_owned(),
            id: None,
            name: "log".to_owned(),
            level: Some(Level::INFO),
            message: Some("tick".to_owned()),
            fields: tinystore::records::Fields::default(),
            context: tinystore::records::Fields::default(),
            trace_id: None,
            span_id: None,
        };
        let mut printed = Printed::default();
        assert!(printed.add(&event));
        assert!(printed.add(&event), "a second alike in the same read");
        printed.forget_before(at - LATE);
        assert!(!printed.add(&event) && !printed.add(&event), "both printed already");
        assert!(printed.add(&event), "a third came");
    }
}
