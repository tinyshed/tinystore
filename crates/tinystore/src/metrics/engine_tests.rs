use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::metrics::Series;
use crate::{Options, Store};

#[test]
fn a_failed_open_lets_its_file_go_and_the_store_opens_one_engine() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    // a directory where the file goes: SQLite opens no file there
    std::fs::create_dir(dir.path().join("metrics.db")).unwrap();
    assert!(store.metrics().is_err());
    std::fs::remove_dir(dir.path().join("metrics.db")).unwrap();
    let (first, second) = (store.metrics().unwrap(), store.metrics().unwrap());
    let now = SystemTime::now();
    first.ingest(&[Series::gauge("cpu").sample(now - Duration::from_secs(1), 1.0)]).unwrap();
    let read = second.query().name("cpu").from(now - Duration::from_secs(60)).read().unwrap();
    assert_eq!(read.len(), 1, "two handles of one engine");
}

#[test]
fn the_engine_imports_no_other_engine() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/metrics");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for other in ["kv", "jobs", "sql", "blobs", "records", "console"] {
            let named = format!("crate::{other}");
            // a name ends where an identifier does: the root's sqlite begins as sql does
            let names = text.match_indices(&named).any(|(at, _)| {
                !text[at + named.len()..].starts_with(|next: char| next.is_alphanumeric() || next == '_')
            });
            assert!(!names, "{} names {named}", path.display());
        }
    }
}
