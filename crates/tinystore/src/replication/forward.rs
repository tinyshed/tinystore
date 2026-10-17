//! Writes a node passes toward a file's home.
//!
//! A handle on a copy makes its call into bytes, an engine's tag first, and
//! hands it to its node, which sends it over the link to its `from`. The
//! node there runs the call through the same engine's handle: it is the home
//! and commits it, or holds a copy and passes it on in turn. The answer
//! comes back with where the file's log stood once the call ended, and each
//! node on the way answers only once its own copy holds that entry, so that
//! a read on the node that wrote sees the write.
//!
//! Only some engines' calls pass on so far, kv's buckets and counters, sql's
//! statements, search's documents, jobs' adds, sets, updates and cancels,
//! and blobs' files; a write of any other on a copy is refused, as before.

use std::time::{Duration, Instant};

use super::message::Message;
use super::node::State;
use crate::{Error, ErrorKind, Result, Store};

/// The tag of a file a copy waits for and its home has not made: the home
/// opens the engine that keeps it.
pub(crate) const MAKE: u8 = 0;
/// The tag of a call of kv's, the first byte of what passes on.
#[cfg(feature = "kv")]
pub(crate) const KV: u8 = 1;
/// The tag of sql's statements.
#[cfg(feature = "sql")]
pub(crate) const SQL: u8 = 2;
/// The tag of search's documents.
#[cfg(feature = "search")]
pub(crate) const SEARCH: u8 = 3;
/// The tag of jobs' calls.
#[cfg(feature = "jobs")]
pub(crate) const JOBS: u8 = 4;
/// The tag of blobs' calls.
#[cfg(feature = "blobs")]
pub(crate) const BLOBS: u8 = 5;

/// How long a write passed on waits for its home's answer.
const ANSWER_WAIT: Duration = Duration::from_secs(30);
/// How long a write answered waits for this node's copy to hold it, after
/// which it answers anyway: the write committed, the copy is behind.
const APPLIED_WAIT: Duration = Duration::from_secs(10);

/// What a write passed on came to.
pub(crate) struct Outcome {
    pub(crate) result: std::result::Result<Vec<u8>, Error>,
    pub(crate) waits: Vec<(String, u64)>,
}

impl Outcome {
    pub(crate) fn into_message(self, id: u64) -> Message {
        let outcome = self.result.map_err(|error| (error.kind(), error.what().to_owned()));
        Message::Written { id, outcome, waits: self.waits }
    }
}

/// Passes `op`, a write on this node's copy of `file`, to the file's home,
/// and gives the answer's bytes once the copy holds the write.
pub(crate) fn pass_on(store: &Store, file: &str, op: Vec<u8>) -> Result<Vec<u8>> {
    let Some(node) = store.node() else {
        return Err(Error::invalid(format!("{file} is a copy, and the store is no node to pass its writes on")));
    };
    if node.options.from.is_empty() {
        return Err(Error::new(
            ErrorKind::Unavailable,
            format!("{file} is a copy, and this node takes from no node its writes could go to"),
        ));
    }
    // the home's error names what failed there already; this node's own says the file
    let written = node.upstream.write(op, ANSWER_WAIT).map_err(|error| error.within(file))?;
    let answer = written.outcome.map_err(|(kind, why)| Error::new(kind, why))?;
    for (name, sequence) in &written.waits {
        wait_applied(node, name, *sequence);
    }
    Ok(answer)
}

/// Asks the home to make the file `name`, which this node waits for and the
/// node it takes from does not serve yet.
pub(crate) fn make(node: &State, name: &str) -> Result<()> {
    let mut out = super::Out(vec![MAKE]);
    out.text(name);
    let written = node.upstream.write(out.0, ANSWER_WAIT)?;
    written.outcome.map(drop).map_err(|(kind, why)| Error::new(kind, why))
}

/// The tag of the engine whose migrations' history is `history`, which a
/// database's file may keep the tables of.
pub(crate) fn tag_of(history: &str) -> Option<u8> {
    match history {
        #[cfg(feature = "kv")]
        "kv" => Some(KV),
        #[cfg(feature = "jobs")]
        "jobs" => Some(JOBS),
        _ => None,
    }
}

/// Asks the home of a database's file `file` to open the engine `tag` names
/// in it, whose tables a copy found its home had not made yet.
pub(crate) fn make_in(store: &Store, file: &str, tag: u8) -> Result<()> {
    let mut out = super::Out(vec![MAKE]);
    out.text(file);
    out.byte(tag);
    pass_on(store, file, out.0).map(drop)
}

/// Opens the engine `tag` names in a database's file `file`, which makes its
/// tables there.
fn make_in_here(lenders: Lenders<'_>, file: &str, tag: u8) -> Result<()> {
    let Some(database) = file.strip_prefix("sql/").and_then(|name| name.strip_suffix(".db")) else {
        return Err(Error::invalid(format!("{file} is no database's file to keep an engine's tables")));
    };
    let home = lenders(database)?;
    match tag {
        #[cfg(feature = "kv")]
        KV => crate::kv::forward::make_in(&home),
        #[cfg(feature = "jobs")]
        JOBS => crate::jobs::forward::make_in(&home),
        _ => {
            let _ = home;
            Err(Error::invalid(format!("an engine this node keeps in no database's file, tag {tag}")))
        }
    }
}

/// Waits until this node's copy of `name` holds entry `sequence`, a bound at
/// most.
fn wait_applied(node: &State, name: &str, sequence: u64) {
    let deadline = Instant::now() + APPLIED_WAIT;
    loop {
        let seen = node.notify.seen();
        match node.copy(name) {
            Some(copy) if copy.applied() >= sequence => return,
            None => return,
            Some(_) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            tracing::warn!(
                target: "tinystore",
                file = name,
                entry = sequence,
                "a write its home committed answered before this node's copy held it: the copy is behind"
            );
            return;
        }
        node.notify.wait_past(seen, deadline - now);
    }
}

/// Runs a write another node passed to this one, through the engine's own
/// handle, which passes it on again where this node holds a copy.
pub(crate) fn run(state: &State, op: &[u8]) -> Outcome {
    let ran = match state.store() {
        Some(store) => run_on(&store, op),
        None => Err(Error::closed("the node's store")),
    };
    match ran {
        Ok((answer, file)) => {
            let waits = state.point_of(&file).map(|point| vec![(file, point.sequence)]).unwrap_or_default();
            Outcome { result: Ok(answer), waits }
        }
        Err(error) => Outcome { result: Err(error), waits: Vec::new() },
    }
}

/// Opens the engine that keeps the file `name`, which makes it.
fn make_here(store: &Store, name: &str) -> Result<()> {
    #[cfg(feature = "kv")]
    if name == crate::kv::forward::FILE {
        return crate::kv::forward::make(store);
    }
    #[cfg(feature = "jobs")]
    if name == crate::jobs::forward::FILE {
        return crate::jobs::forward::make(store);
    }
    #[cfg(feature = "search")]
    if name == crate::search::forward::FILE {
        return crate::search::forward::make(store);
    }
    #[cfg(feature = "sql")]
    if let Some(database) = name.strip_prefix("sql/").and_then(|name| name.strip_suffix(".db")) {
        return store.database(database).open().map(drop);
    }
    let _ = store;
    Err(Error::not_found(format!("{name}: no engine of this node keeps such a file")))
}

/// Where a database's file lends kv and jobs their tables, by the
/// database's name: what a write passed on of a bucket or a queue kept there
/// opens in.
pub(crate) type Lenders<'a> = &'a dyn Fn(&str) -> Result<crate::engine::Home>;

/// The file a database's name keeps, as a node names it.
pub(crate) fn database_file(name: &str) -> String {
    format!("sql/{name}.db")
}

/// The database's name a lending file's owner says, `sql app` for `app`.
pub(crate) fn database_of(owner: &str) -> Option<&str> {
    owner.strip_prefix("sql ")
}

fn run_on(store: &Store, op: &[u8]) -> Result<(Vec<u8>, String)> {
    #[cfg(feature = "sql")]
    let lenders = |name: &str| -> Result<crate::engine::Home> {
        let database = store.database(name).open()?;
        Ok(crate::engine::Home::Shared(std::sync::Arc::clone(database.shared())))
    };
    #[cfg(not(feature = "sql"))]
    let lenders = |name: &str| -> Result<crate::engine::Home> {
        Err(Error::invalid(format!("a write of database {name}'s file, and this node has no sql")))
    };
    let _ = &lenders;
    let Some((&tag, body)) = op.split_first() else {
        return Err(Error::corrupt("a write passed on holds nothing"));
    };
    match tag {
        MAKE => {
            let mut at = super::In { bytes: body, at: 0 };
            let file = at.text()?;
            if at.done() {
                make_here(store, &file)?;
            } else {
                make_in_here(&lenders, &file, at.byte()?)?;
            }
            Ok((Vec::new(), file))
        }
        #[cfg(feature = "kv")]
        KV => crate::kv::forward::run(store, &lenders, body),
        #[cfg(feature = "sql")]
        SQL => crate::sql::forward::run(store, body),
        #[cfg(feature = "blobs")]
        BLOBS => crate::blobs::forward::run(store, body).map(|answer| (answer, crate::blobs::forward::FILE.to_owned())),
        #[cfg(feature = "jobs")]
        JOBS => crate::jobs::forward::run(store, &lenders, body),
        #[cfg(feature = "search")]
        SEARCH => {
            crate::search::forward::run(store, body).map(|answer| (answer, crate::search::forward::FILE.to_owned()))
        }
        _ => {
            let _ = (store, body);
            Err(Error::new(ErrorKind::Invalid, format!("a write of an engine this node does not have, tag {tag}")))
        }
    }
}
