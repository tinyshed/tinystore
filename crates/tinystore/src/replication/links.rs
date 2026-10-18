//! A running node's links, written to `log/links` every few seconds so that
//! `tinystore node status` shows them beside what the directory holds: the
//! node it takes from while a link reaches it, and the nodes taking from it
//! with the entry each says it holds of each file. A node that stops removes
//! the file; one that died leaves it, as old as its last write says.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use super::DIR;
use super::node::State;
use crate::sync::lock;
use crate::{Error, Result};

/// The file in a node's `log/` its links are written to.
const FILE: &str = "links";
const EVERY: Duration = Duration::from_secs(5);

/// A running node's links, as it last wrote them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Links {
    /// When the node wrote them, in milliseconds since 1970.
    pub written: u64,
    /// The node it takes from, while a link reaches it.
    pub from: Option<String>,
    /// The nodes taking from it now.
    pub followers: Vec<Follower>,
    /// The synchronous commits it answered with no copy holding them.
    pub unheld: u64,
}

/// A node taking files from this one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Follower {
    /// Where its link comes from, `host:port`.
    pub address: String,
    /// The last entry it says it holds of each file it copies.
    pub applied: BTreeMap<String, u64>,
}

/// The links of the node running in `dir`, as it last wrote them; none when
/// no node runs there, or ran and stopped.
pub fn links(dir: &Path) -> Result<Option<Links>> {
    let path = dir.join(DIR).join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::io(path.display().to_string(), error)),
    };
    let unreadable = |why: &str| Error::corrupt(format!("{}: {why}", path.display()));
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| unreadable(&error.to_string()))?;
    Ok(Some(read(&value).ok_or_else(|| unreadable("not a node's links"))?))
}

fn read(value: &Value) -> Option<Links> {
    let followers = value.get("followers")?.as_array()?.iter().map(read_follower).collect::<Option<Vec<_>>>()?;
    Some(Links {
        written: value.get("written")?.as_u64()?,
        unheld: value.get("unheld").and_then(Value::as_u64).unwrap_or(0),
        from: value.get("from").and_then(Value::as_str).map(str::to_owned),
        followers,
    })
}

fn read_follower(value: &Value) -> Option<Follower> {
    let applied = value.get("applied")?.as_object()?;
    Some(Follower {
        address: value.get("address")?.as_str()?.to_owned(),
        applied: applied.iter().map(|(file, at)| Some((file.clone(), at.as_u64()?))).collect::<Option<_>>()?,
    })
}

/// Writes the node's links every few seconds until it stops, then removes
/// them.
pub(crate) fn keep_written(state: &Arc<State>) {
    let path = state.dir.join(DIR).join(FILE);
    while !state.stopping() {
        if let Err(error) = write(state) {
            tracing::debug!(target: "tinystore", %error, "a node could not write its links");
        }
        state.pause(EVERY);
    }
    let _ = std::fs::remove_file(path);
}

/// Writes the node's links whole, through a file of their own renamed over
/// the last.
fn write(state: &State) -> Result<()> {
    let logs = state.dir.join(DIR);
    std::fs::create_dir_all(&logs).map_err(|error| Error::io(logs.display().to_string(), error))?;
    let written = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    let followers: Vec<Value> = lock(&state.followers)
        .values()
        .map(|follower| {
            let applied: Map<String, Value> =
                follower.applied.iter().map(|(file, at)| (file.clone(), Value::from(*at))).collect();
            json!({ "address": follower.address, "applied": applied })
        })
        .collect();
    let unheld = state.acks.unheld.load(std::sync::atomic::Ordering::Relaxed);
    let from = lock(&state.upstream_at).clone();
    let links = json!({ "written": written, "from": from, "followers": followers, "unheld": unheld });
    let (path, next) = (logs.join(FILE), logs.join(format!("{FILE}.next")));
    std::fs::write(&next, links.to_string()).map_err(|error| Error::io(next.display().to_string(), error))?;
    std::fs::rename(&next, &path).map_err(|error| Error::io(path.display().to_string(), error))
}
