//! The tool `jobs_list`, and a queue's jobs as `tinystore jobs list` reads
//! them too, each as the JSON both answer with.

use std::sync::Arc;

use serde_json::{Value, json};

use super::given::none_kept;
use super::store::{Arguments, MOST, Opener, count, opened, schema, text, with_database};
use super::tool::Tool;
use crate::Store;
use crate::jobs::{Filter, Job, Page, State};

pub(super) fn tools(open: &Opener) -> Vec<Tool> {
    let open = Arc::clone(open);
    let takes = with_database(json!({
        "queue": text("the queue"),
        "prefix": text("the jobs whose ids start with it"),
        "state": text("scheduled, waiting, running, done or failed"),
        "limit": count("200 at most"),
    }));
    let description = "A queue's jobs that have ids, in the order of their ids, with their state, attempts and last \
                       error; or, with state failed and no prefix, its failed jobs, the last failed first.";
    vec![Tool::reads("jobs_list", description, schema(takes, &["queue"]), move |given| {
        Ok(page(&open, &Arguments { tool: "jobs_list", given })?)
    })]
}

fn page(open: &Opener, given: &Arguments) -> Result<Value, String> {
    let listing = Listing {
        queue: given.needed("queue")?,
        database: given.text("database"),
        prefix: given.text("prefix"),
        state: given.text("state"),
    };
    let page = listed(&opened(open)?, &listing, given.limit(MOST), None)?;
    Ok(Value::Array(page.jobs.iter().map(json).collect()))
}

/// The queue read, and which of its jobs.
#[derive(Debug)]
pub struct Listing<'a> {
    pub queue: &'a str,
    /// The database a program opened the queue from; jobs.db when none.
    pub database: Option<&'a str>,
    pub prefix: Option<&'a str>,
    /// A state's word, as `failed`.
    pub state: Option<&'a str>,
}

/// A page of the queue's jobs that have ids, `limit` of them at most, after
/// the id `after`.
pub fn listed(store: &Store, listing: &Listing, limit: usize, after: Option<&str>) -> Result<Page<Value>, String> {
    let opened = match listing.database {
        #[cfg(feature = "sql")]
        Some(database) => super::sql::opened(store, database)?.queue(listing.queue),
        #[cfg(not(feature = "sql"))]
        Some(database) => return Err(format!("database {database}: this program was built without sql's databases")),
        None if !store.dir().join("jobs.db").exists() => return Err(none_kept(store.dir(), "jobs.db", "queue")),
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
pub fn json(job: &Job<Value>) -> Value {
    let at = jiff::Timestamp::try_from(job.at).map_or_else(|_| String::new(), |at| at.to_string());
    json!({
        "id": job.id,
        "state": state_word(job.state),
        "at": at,
        "attempt": job.attempt,
        "error": job.error,
        "group": job.group,
        "value": job.value,
    })
}

pub fn state_word(state: State) -> &'static str {
    match state {
        State::Scheduled => "scheduled",
        State::Waiting => "waiting",
        State::Running => "running",
        State::Done => "done",
        State::Failed => "failed",
        _ => "cancelled",
    }
}
