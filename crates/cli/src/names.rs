//! What a store holds by name, read beside whoever holds it: its buckets,
//! queues, sets of files, streams, metrics, databases and search indexes,
//! for `status --json` and for Tab's completion. A store that lacks an
//! engine's file holds none of its names, and an engine whose names do not
//! read gives none: a read beside makes nothing, and neither use may fail.

use std::path::Path;

use serde_json::{Value, json};
use tinystore::Store;

use crate::on_disk;

/// The metrics' names read at most.
const MOST_NAMES: usize = 1000;

#[derive(Debug, Default)]
pub(crate) struct Names {
    /// Each bucket with what it is for: values, encrypted, counters and the rest.
    pub(crate) buckets: Vec<(String, String)>,
    pub(crate) queues: Vec<String>,
    pub(crate) files: Vec<String>,
    pub(crate) streams: Vec<String>,
    pub(crate) metrics: Vec<String>,
    pub(crate) databases: Vec<String>,
    pub(crate) indexes: Vec<String>,
}

impl Names {
    /// The names the store at `dir` holds; none when no store is there.
    pub(crate) fn of(dir: &Path) -> Names {
        if !dir.join("LOCK").is_file() {
            return Names::default();
        }
        let Ok(store) = Store::open_beside(dir) else { return Names::default() };
        Names {
            buckets: tinystore::client::buckets(&store).unwrap_or_default(),
            queues: tinystore::client::queues(&store).unwrap_or_default(),
            files: tinystore::client::file_sets(&store).unwrap_or_default(),
            streams: streams(&store),
            metrics: metrics(&store),
            databases: on_disk::databases(dir),
            indexes: indexes(&store),
        }
    }

    pub(crate) fn json(&self) -> Value {
        let buckets: Vec<Value> =
            self.buckets.iter().map(|(name, role)| json!({ "name": name, "role": role })).collect();
        json!({
            "buckets": buckets,
            "queues": self.queues,
            "files": self.files,
            "streams": self.streams,
            "metrics": self.metrics,
            "databases": self.databases,
            "indexes": self.indexes,
        })
    }
}

fn streams(store: &Store) -> Vec<String> {
    if !store.dir().join("records.db").exists() {
        return Vec::new();
    }
    let held = store.all_events().and_then(|all| all.streams());
    held.map(|streams| streams.into_iter().map(|stream| stream.name).collect()).unwrap_or_default()
}

fn metrics(store: &Store) -> Vec<String> {
    let page = store.metrics().and_then(|metrics| metrics.names().limit(MOST_NAMES).page());
    page.map(|page| page.names.into_iter().map(|info| info.name).collect()).unwrap_or_default()
}

fn indexes(store: &Store) -> Vec<String> {
    if !store.dir().join("search.db").exists() {
        return Vec::new();
    }
    let listed = tinystore::search::list(store);
    listed.map(|indexes| indexes.into_iter().map(|index| index.name).collect()).unwrap_or_default()
}
