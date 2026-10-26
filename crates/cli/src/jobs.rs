//! `tinystore jobs`: a store's queues from a shell. `add` and `cancel` write
//! through the server that holds the directory, or a sidecar they start;
//! `list` reads a queue beside whoever holds it. No worker runs here: a job
//! added runs in the program whose worker takes its queue.
//!
//! ```text
//! tinystore jobs ./data add emails '{"to": "ada@example.com"}' --id welcome-42 --delay 1m
//! tinystore jobs ./data list emails --state failed
//! tinystore jobs ./data cancel emails welcome-42
//! ```

use std::io::{IsTerminal as _, Read as _};
use std::process::ExitCode;

use serde_json::{Value, json};
use tinystore::Store;
use tinystore::client::NewJob;
use tinystore::jobs::{Filter, Job, Page, State};

use crate::asked::{self, Asked, Done, Refused, Verb, failed, wrong};
use crate::window::{self};
use crate::{help, reach};

const VERBS: [Verb; 3] = [
    Verb { word: "add", command: &help::JOBS_ADD, run: add },
    Verb { word: "list", command: &help::JOBS_LIST, run: list },
    Verb { word: "cancel", command: &help::JOBS_CANCEL, run: cancel },
];

/// The jobs a page of `list` holds unless `--limit` says.
const PAGE: usize = 100;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run_verb("jobs", &VERBS, args)
}

/// Adds a job through the server that holds the directory: its value JSON,
/// `-` reading it from stdin.
fn add(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("jobs", "add", false)?;
    let [queue, given] = &rest[..] else {
        return Err(wrong("add takes a queue and the job's JSON: add emails '{\"to\": \"ada@example.com\"}'"));
    };
    let database = asked.value("--database").map(str::to_owned);
    reach::store_at(&dir)?;
    if let Some(database) = &database {
        crate::sql::database_held(&dir, database)?;
    }
    let job = NewJob {
        value: json_of(given)?,
        id: asked.value("--id").map(str::to_owned),
        delay: asked.value("--delay").map(asked::span).transpose().map_err(wrong)?,
        at: asked.value("--at").map(window::time).transpose().map_err(wrong)?,
    };
    if job.delay.is_some() && job.at.is_some() {
        return Err(wrong("--delay or --at, not both"));
    }
    let (named, sent) = (queue.clone(), job.clone());
    let added = reach::write(&dir, "jobs add", move |client| {
        let database = database.as_deref().map(|database| client.database(database)).transpose()?;
        let queue = client.queue(&named, database)?;
        client.add(queue, &sent)
    })?;
    let due = match (job.delay, job.at) {
        (Some(delay), _) => format!(", due in {}", window::span_text(delay)),
        (_, Some(at)) => format!(", due at {}", window::clock(at, true)),
        _ => String::new(),
    };
    match (&job.id, added) {
        (Some(id), true) => outln!("added {id} to queue {queue}{due}"),
        (Some(id), false) => outln!("queue {queue} holds a job {id} already; nothing added"),
        (None, _) => {
            outln!("added a job to queue {queue}{due}")?;
            asked::say("jobs add", "a job without --id cannot be cancelled: nothing names it");
            Ok(())
        }
    }
}

/// Cancels a job by its id through the server that holds the directory.
fn cancel(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("jobs", "cancel", false)?;
    let [queue, id] = &rest[..] else {
        return Err(wrong("cancel takes a queue and a job's id: cancel emails welcome-42"));
    };
    let database = asked.value("--database").map(str::to_owned);
    reach::store_at(&dir)?;
    if let Some(database) = &database {
        crate::sql::database_held(&dir, database)?;
    }
    let (named, keyed) = (queue.clone(), id.clone());
    let cancelled = reach::write(&dir, "jobs cancel", move |client| {
        let database = database.as_deref().map(|database| client.database(database)).transpose()?;
        let queue = client.queue(&named, database)?;
        client.cancel(queue, &keyed)
    })?;
    if cancelled { outln!("cancelled {id} in queue {queue}") } else { outln!("queue {queue} holds no job {id}") }
}

fn list(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("jobs", "list", true)?;
    let (Some(queue), prefix, None) = (rest.first(), rest.get(1), rest.get(2)) else {
        return Err(wrong("list takes a queue and an id's prefix at most: list emails welcome-"));
    };
    asked::existing(&dir)?;
    let store = Store::open_beside(&dir).map_err(failed)?;
    let listing = Listing {
        queue,
        database: asked.value("--database"),
        prefix: prefix.map(String::as_str),
        state: asked.value("--state"),
    };
    let page = listed(&store, &listing, asked.count("--limit", PAGE)?, asked.value("--after")).map_err(failed)?;
    if asked.has("--json") {
        for job in &page.jobs {
            outln!("{}", shown(job))?;
        }
        return page.next.map_or(Ok(()), |next| outln!("{}", json!({ "next": next })));
    }
    if page.jobs.is_empty() {
        let state = listing.state.map_or_else(String::new, |state| format!("{state} "));
        return outln!("queue {queue} holds no {state}job");
    }
    let width = page.jobs.iter().map(|job| job.id.as_deref().unwrap_or("-").len()).max().unwrap_or(1).min(40);
    for job in &page.jobs {
        let id = job.id.as_deref().unwrap_or("-");
        let error = job.error.as_deref().map_or_else(String::new, |error| format!("  {error}"));
        outln!(
            "{id:width$}  {:9}  {}  attempt {}{error}",
            state_word(job.state),
            window::clock(job.at, true),
            job.attempt
        )?;
    }
    if let Some(next) = page.next {
        asked::say("jobs list", format!("more jobs; --after {next} reads on"));
    }
    Ok(())
}

/// The value of a job as given, which is JSON; `-` reads it from stdin.
fn json_of(given: &str) -> Result<String, Refused> {
    let text = if given == "-" {
        if std::io::stdin().is_terminal() {
            return Err(wrong("- reads the job's JSON on stdin, and stdin is a terminal"));
        }
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map_err(|error| failed(format!("stdin: {error}")))?;
        text
    } else {
        given.to_owned()
    };
    let value: Value = serde_json::from_str(&text).map_err(|error| wrong(format!("a job is JSON: {error}")))?;
    Ok(value.to_string())
}

/// The queue `list` and the MCP's `jobs_list` read, and which of its jobs.
pub(crate) struct Listing<'a> {
    pub(crate) queue: &'a str,
    pub(crate) database: Option<&'a str>,
    pub(crate) prefix: Option<&'a str>,
    pub(crate) state: Option<&'a str>,
}

/// A page of the queue's jobs that have ids, read beside whoever holds the
/// store.
pub(crate) fn listed(
    store: &Store,
    listing: &Listing,
    limit: usize,
    after: Option<&str>,
) -> Result<Page<Value>, String> {
    let opened = match listing.database {
        Some(database) => {
            crate::sql::opened(store, database).map_err(|refused| refused.to_string())?.queue(listing.queue)
        }
        None if !store.dir().join("jobs.db").exists() => {
            return Err(crate::files::none_kept(store.dir(), "jobs.db", "queue"));
        }
        None => store.queue::<Value>(listing.queue),
    };
    let opened = opened.open().map_err(|error| error.to_string())?;
    let mut filter = Filter::prefix(listing.prefix.unwrap_or_default()).limit(limit);
    if let Some(after) = after {
        filter = filter.after(after);
    }
    if let Some(state) = listing.state {
        filter = filter.state(match state {
            "scheduled" => State::Scheduled,
            "waiting" => State::Waiting,
            "running" => State::Running,
            "done" => State::Done,
            "failed" => State::Failed,
            other => return Err(format!("no state {other}: scheduled, waiting, running, done or failed")),
        });
    }
    opened.list(&filter).map_err(|error| error.to_string())
}

/// A job as JSON: its time whole and UTC.
pub(crate) fn shown(job: &Job<Value>) -> Value {
    json!({
        "id": job.id,
        "state": state_word(job.state),
        "at": window::utc(job.at),
        "attempt": job.attempt,
        "error": job.error,
        "group": job.group,
        "value": job.value,
    })
}

fn state_word(state: State) -> &'static str {
    match state {
        State::Scheduled => "scheduled",
        State::Waiting => "waiting",
        State::Running => "running",
        State::Done => "done",
        State::Failed => "failed",
        _ => "cancelled",
    }
}
