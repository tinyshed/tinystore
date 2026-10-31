//! What a store holds by name, read beside whoever holds it: its buckets,
//! queues, sets of files, streams, metrics, databases, search indexes and
//! configs, for `status --json` and for Tab's completion, and with what each
//! holds for `status --names`. A store that lacks an engine's file holds none
//! of its names, and an engine whose names do not read gives none: a read
//! beside makes nothing, and no use may fail.

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tinystore::Store;
use tinystore::client::Held;

use crate::{on_disk, window};

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
    pub(crate) configs: Vec<String>,
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
            configs: tinystore::client::configs(&store).unwrap_or_default(),
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
            "configs": self.configs,
        })
    }
}

/// Each name with what it holds, for `status --names`: every bucket, queue,
/// set of files, stream and index is read whole, one name a read, so it
/// takes longer than `Names` on a big store. Each list starts with what
/// weighs most.
#[derive(Debug, Default)]
pub(crate) struct Holdings {
    pub(crate) buckets: Vec<Holding>,
    pub(crate) queues: Vec<Holding>,
    pub(crate) files: Vec<Holding>,
    pub(crate) streams: Vec<Holding>,
    pub(crate) metrics: Vec<Holding>,
    pub(crate) databases: Vec<Holding>,
    pub(crate) indexes: Vec<Holding>,
    pub(crate) configs: Vec<String>,
}

/// A name and what it holds.
#[derive(Debug, Default)]
pub(crate) struct Holding {
    pub(crate) name: String,
    /// A bucket's: values, encrypted values, counters and the rest.
    pub(crate) role: Option<String>,
    /// Its keys, jobs, files, events, series or documents; none for a database.
    pub(crate) count: Option<u64>,
    /// None for a metric: its blocks' bodies lie in rows that name no series,
    /// and only decoding every directory would sum them.
    pub(crate) bytes: Option<u64>,
    /// A stream's or a metric's.
    pub(crate) kept: Option<Kept>,
}

/// What a stream or a metric keeps, and from when to when it holds.
#[derive(Debug)]
pub(crate) struct Kept {
    pub(crate) keep: Duration,
    pub(crate) oldest: SystemTime,
    pub(crate) newest: SystemTime,
}

impl Holdings {
    /// What each name of the store at `dir` holds; none when no store is there.
    pub(crate) fn of(dir: &Path) -> Holdings {
        if !dir.join("LOCK").is_file() {
            return Holdings::default();
        }
        let Ok(store) = Store::open_beside(dir) else { return Holdings::default() };
        let mut holdings = Holdings {
            buckets: buckets_held(&store),
            queues: held(tinystore::client::queues_held(&store)),
            files: held(tinystore::client::file_sets_held(&store)),
            streams: streams_held(&store),
            metrics: metrics_held(&store),
            databases: databases_held(dir),
            indexes: held(tinystore::client::indexes_held(&store)),
            configs: tinystore::client::configs(&store).unwrap_or_default(),
        };
        for list in holdings.lists_mut() {
            list.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(b.count.cmp(&a.count)).then_with(|| a.name.cmp(&b.name)));
        }
        holdings
    }

    fn lists_mut(&mut self) -> [&mut Vec<Holding>; 7] {
        let Holdings { buckets, queues, files, streams, metrics, databases, indexes, configs: _ } = self;
        [buckets, queues, files, streams, metrics, databases, indexes]
    }

    /// The names an engine's file keeps, as `status` names the file, and what
    /// each counts: `kv`'s buckets count keys.
    pub(crate) fn of_file(&self, file: &str) -> (&[Holding], &'static str) {
        match file {
            "kv" => (&self.buckets, "key"),
            "jobs" => (&self.queues, "job"),
            "search" => (&self.indexes, "document"),
            "blobs" => (&self.files, "file"),
            "records" => (&self.streams, "event"),
            "metrics" => (&self.metrics, "series"),
            _ => (&[], ""),
        }
    }

    /// As `Names::json` lists them, each name an object with what it holds.
    pub(crate) fn json(&self) -> Value {
        let each = |list: &[Holding]| list.iter().map(Holding::json).collect::<Vec<Value>>();
        json!({
            "buckets": each(&self.buckets),
            "queues": each(&self.queues),
            "files": each(&self.files),
            "streams": each(&self.streams),
            "metrics": each(&self.metrics),
            "databases": each(&self.databases),
            "indexes": each(&self.indexes),
            "configs": self.configs,
        })
    }
}

impl Holding {
    fn json(&self) -> Value {
        let mut shown = json!({ "name": self.name, "count": self.count, "bytes": self.bytes });
        if let Some(role) = &self.role {
            shown["role"] = json!(role);
        }
        if let Some(kept) = &self.kept {
            shown["keep"] = json!(window::span_text(kept.keep));
            shown["oldest"] = json!(window::utc(kept.oldest));
            shown["newest"] = json!(window::utc(kept.newest));
        }
        shown
    }
}

/// The names the core read with their numbers; none when they do not read.
fn held(read: tinystore::Result<Vec<Held>>) -> Vec<Holding> {
    let held = read.unwrap_or_default().into_iter();
    held.map(|held| Holding { name: held.name, count: Some(held.count), bytes: Some(held.bytes), ..Holding::default() })
        .collect()
}

fn buckets_held(store: &Store) -> Vec<Holding> {
    let roles = tinystore::client::buckets(store).unwrap_or_default();
    let mut buckets = held(tinystore::client::buckets_held(store));
    for bucket in &mut buckets {
        bucket.role = roles.iter().find(|(name, _)| *name == bucket.name).map(|(_, role)| role.clone());
    }
    buckets
}

fn streams_held(store: &Store) -> Vec<Holding> {
    let mut streams = held(tinystore::client::streams_held(store));
    if streams.is_empty() {
        return streams;
    }
    let infos = store.all_events().and_then(|all| all.streams()).unwrap_or_default();
    for stream in &mut streams {
        let info = infos.iter().find(|info| info.name == stream.name);
        stream.kept = info.map(|info| Kept { keep: info.keep, oldest: info.retained_from, newest: info.newest });
    }
    streams
}

/// Every metric with its series, a page of names at a time.
fn metrics_held(store: &Store) -> Vec<Holding> {
    let Ok(metrics) = store.metrics() else { return Vec::new() };
    let mut held = Vec::new();
    let mut list = metrics.names().limit(MOST_NAMES);
    while let Ok(page) = list.clone().page() {
        held.extend(page.names.into_iter().map(|info| Holding {
            name: info.name,
            count: Some(info.series_count),
            kept: Some(Kept { keep: info.keep, oldest: info.retained_from, newest: info.newest }),
            ..Holding::default()
        }));
        let Some(next) = page.next else { break };
        list = list.after(next);
    }
    held
}

/// Each database with its file's bytes, its WAL's among them.
fn databases_held(dir: &Path) -> Vec<Holding> {
    let files = on_disk::of(dir).into_iter();
    let databases = files.filter_map(|file| Some((file.name.strip_prefix("sql/")?.to_owned(), file.bytes)));
    databases.map(|(name, bytes)| Holding { name, bytes: Some(bytes), ..Holding::default() }).collect()
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
