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

use std::process::ExitCode;

use serde_json::json;
use tinystore::Store;
use tinystore::client::NewJob;
use tinystore::mcp::jobs::{Listing, listed, state_word};

use crate::asked::{self, Asked, Done, Verb, failed, wrong};
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
        value: asked::json_given(given, "a job")?,
        id: asked.value("--id").map(str::to_owned),
        delay: asked.value("--delay").map(asked::span).transpose().map_err(wrong)?,
        at: asked.value("--at").map(window::time).transpose().map_err(wrong)?,
    };
    if job.delay.is_some() && job.at.is_some() {
        return Err(wrong("--delay or --at, not both"));
    }
    let (named, sent) = (queue.clone(), job.clone());
    let added = reach::write_by_id(&dir, "jobs add", job.id.as_deref(), move |client| {
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
            outln!("{}", tinystore::mcp::jobs::json(job))?;
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
