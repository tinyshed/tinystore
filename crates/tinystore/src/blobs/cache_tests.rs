//! A set of files as a cache: the counters every path keeps, the order a
//! ceiling evicts in, the bound a write commits within, a refusing ceiling,
//! the ceiling kept in the set and its change, and a trim.

use std::io::Write as _;
use std::time::Duration;

use rusqlite::params;
use sha2::{Digest, Sha256};

use crate::blobs::fixture::{Fixture, HOUR, bytes, fixture};
use crate::blobs::upload::INLINE;
use crate::blobs::{Files, Trim, WhenFull, maintain};
use crate::{Clock as _, ErrorKind};

/// The set's counters as its row keeps them, and as its rows sum.
fn counters(files: &Files) -> ((i64, i64), (i64, i64)) {
    let kept = "select count, size from _tinystore_blobs_sets where id = ?1";
    let summed = "select count(*), coalesce(sum(size), 0) from _tinystore_blobs_files where set_id = ?1";
    let read = |sql: &str| {
        files
            .blobs
            .file
            .read(|c| Ok(c.query_row(sql, [files.set], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()))
            .unwrap()
    };
    (read(kept), read(summed))
}

fn assert_counted(files: &Files, count: i64, size: i64) {
    let (kept, summed) = counters(files);
    assert_eq!(kept, summed, "the set's counters are its rows'");
    assert_eq!(kept, (count, size));
}

/// `count` files of one byte each, written in one transaction under
/// `prefix`, each its own content, as many puts would leave them.
fn many(files: &Files, prefix: &str, count: usize) {
    let (set, sha) = (files.set, Sha256::digest([0_u8]).to_vec());
    files
        .blobs
        .file
        .transaction(|tx| {
            for n in 0..count as i64 {
                let id = 1_000_000 + n;
                tx.execute(
                    "insert into _tinystore_blobs_contents (id, names, size, sha256, inline) values (?1, 1, 1, ?2, 1)",
                    params![id, sha],
                )
                .and_then(|_| tx.execute("insert into _tinystore_blobs_bodies (id, bytes) values (?1, x'00')", [id]))
                .and_then(|_| {
                    tx.execute(
                        "insert into _tinystore_blobs_files (set_id, path, revision, content, size, sha256, type, modified)
                            values (?1, ?2, 0, ?3, 1, ?4, 'application/octet-stream', 0)",
                        params![set, format!("{prefix}{n:05}"), id, sha],
                    )
                })
                .unwrap();
            }
            Ok::<_, crate::Error>(())
        })
        .unwrap();
}

fn paths(files: &Files) -> Vec<String> {
    files.list().page().unwrap().files.into_iter().map(|info| info.path).collect()
}

#[test]
fn every_path_that_adds_or_removes_a_file_keeps_the_sets_counters() {
    let f = fixture();
    let docs = f.files("docs");
    docs.put("a", bytes(100)).unwrap();
    docs.put("b", bytes(INLINE + 1)).unwrap();
    assert_counted(&docs, 2, 100 + INLINE as i64 + 1);
    docs.put("a", bytes(40)).unwrap();
    assert_counted(&docs, 2, 40 + INLINE as i64 + 1);
    docs.copy("a", "c").unwrap();
    assert_counted(&docs, 3, 80 + INLINE as i64 + 1);
    docs.rename("c", "d").unwrap();
    assert_counted(&docs, 3, 80 + INLINE as i64 + 1);
    docs.delete("b").unwrap();
    assert_counted(&docs, 2, 80);
    let mut upload = docs.upload("e").unwrap();
    upload.write_all(&bytes(10)).unwrap();
    upload.commit().unwrap();
    assert_counted(&docs, 3, 90);
    docs.key("f").ttl(HOUR).put(bytes(5)).unwrap();
    f.clock.advance(2 * HOUR);
    assert_counted(&docs, 4, 95);
    maintain(&f.store).unwrap();
    assert_counted(&docs, 3, 90);
    docs.folder("x").put("1", bytes(1)).unwrap();
    docs.folder("x").clear().unwrap();
    assert_counted(&docs, 3, 90);
    docs.clear().unwrap();
    assert_counted(&docs, 0, 0);
}

#[test]
fn a_clear_past_its_bound_counts_its_files_until_maintenance_deletes_them() {
    let f = fixture();
    let docs = f.files("docs");
    let count = super::super::list::CLEAR_AT_ONCE + 1;
    many(&docs, "", count);
    assert_counted(&docs, count as i64, count as i64);
    docs.clear().unwrap();
    assert_counted(&docs, count as i64, count as i64);
    maintain(&f.store).unwrap();
    assert_counted(&docs, 0, 0);
}

#[cfg(feature = "sql")]
#[test]
fn a_databases_transaction_counts_what_it_commits_and_nothing_it_rolls_back() {
    let f = fixture();
    let db = f.store.database("app").open().unwrap();
    let cache = db.files("cache").max_size(1000).open().unwrap();
    db.tx(|tx| -> crate::Result<()> {
        tx.with(&cache).put("a", bytes(10))?;
        tx.with(&cache).put("b", bytes(20))?;
        Ok(())
    })
    .unwrap();
    let failed = db.tx(|tx| -> crate::Result<()> {
        tx.with(&cache).put("c", bytes(30))?;
        tx.with(&cache).delete("a")?;
        Err(crate::Error::invalid("rolled back"))
    });
    assert!(failed.is_err());
    assert_counted(&cache, 2, 30);
    cache.put("c", bytes(980)).unwrap();
    assert_eq!(paths(&cache), ["b", "c"], "a database's set evicts as blobs.db's does");
    assert_counted(&cache, 2, 1000);
}

/// Moves the clock and writes the uses reads asked for, as the store's
/// background work does each second.
fn later(f: &Fixture, by: Duration) {
    f.clock.advance(by);
    f.blobs().flush_uses().unwrap();
}

// contract: BLOB-18
#[test]
fn a_write_past_the_ceiling_evicts_files_never_read_before_files_read() {
    let f = fixture();
    let pages = f.store.files("pages").max_size(300).open().unwrap();
    for path in ["a", "b", "c"] {
        pages.put(path, bytes(100)).unwrap();
        f.clock.advance(Duration::from_secs(60));
    }
    later(&f, 2 * HOUR);
    pages.get("a").unwrap();
    later(&f, 2 * HOUR);
    pages.head("b").unwrap();
    later(&f, Duration::ZERO);
    // c was never read: it goes first, then d, written after every read but
    // never read, before a and b, which rank a day ahead of their reads
    pages.put("d", bytes(100)).unwrap();
    assert_eq!(paths(&pages), ["a", "b", "d"]);
    pages.put("e", bytes(100)).unwrap();
    assert_eq!(paths(&pages), ["a", "b", "e"]);
    let usage = pages.usage().unwrap();
    assert_eq!((usage.size, usage.max_size, usage.evictions), (300, Some(300), 2));
}

// contract: BLOB-18
#[test]
fn a_file_read_long_ago_goes_before_one_read_today() {
    let f = fixture();
    let pages = f.store.files("pages").max_size(200).open().unwrap();
    pages.put("old", bytes(100)).unwrap();
    later(&f, 2 * HOUR);
    pages.get("old").unwrap();
    later(&f, 2 * HOUR);
    pages.get("old").unwrap();
    later(&f, Duration::from_secs(400 * 24 * 3600));
    pages.put("new", bytes(100)).unwrap();
    later(&f, 2 * HOUR);
    pages.get("new").unwrap();
    later(&f, Duration::ZERO);
    pages.put("next", bytes(100)).unwrap();
    assert_eq!(paths(&pages), ["new", "next"]);
}

// contract: BLOB-18
#[test]
fn a_file_written_again_ranks_at_its_write_not_at_the_uses_of_the_bytes_it_replaced() {
    let f = fixture();
    let pages = f.store.files("pages").max_size(200).open().unwrap();
    pages.put("a", bytes(100)).unwrap();
    later(&f, 2 * HOUR);
    pages.get("a").unwrap();
    later(&f, 2 * HOUR);
    pages.put("b", bytes(100)).unwrap();
    later(&f, 2 * HOUR);
    // a's first use ranked it 2 h in; written again now, it ranks now, after b
    pages.put("a", bytes(100)).unwrap();
    pages.put("c", bytes(100)).unwrap();
    assert_eq!(paths(&pages), ["a", "c"]);
}

// contract: BLOB-19
#[test]
fn a_file_larger_than_the_ceiling_is_refused_and_removes_nothing() {
    let f = fixture();
    let pages = f.store.files("pages").max_size(100).open().unwrap();
    pages.put("a", bytes(60)).unwrap();
    let error = pages.put("b", bytes(101)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert_eq!(error.fact("bound"), Some("100"));
    assert_eq!(paths(&pages), ["a"]);
    pages.put("c", bytes(100)).unwrap();
    assert_eq!(paths(&pages), ["c"]);
}

// contract: BLOB-19
#[test]
fn a_write_that_ten_thousand_evictions_cannot_fit_fails_and_rolls_back_its_evictions() {
    let f = fixture();
    let thumbs = f.files("thumbs");
    let count = super::EVICT_AT_ONCE + 10;
    many(&thumbs, "", count);
    thumbs.set_max_size(count as u64).apply().unwrap();
    // 10,005 bytes past a full set: 10,000 evictions free 10,000 of them
    let error = thumbs.put("big", bytes(count - 5)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert_counted(&thumbs, count as i64, count as i64);
    assert_eq!(thumbs.usage().unwrap().evictions, 0, "a write that failed evicted nothing");
    thumbs.put("small", b"y").unwrap();
    assert_counted(&thumbs, count as i64, count as i64);
    assert_eq!(thumbs.usage().unwrap().evictions, 1);
    // a ceiling lowered under the set, which maintenance trims a batch a commit
    f.blobs()
        .file
        .write(0, |tx| {
            crate::sqlite::execute_cached(tx, "update _tinystore_blobs_sets set max_size = 5", [], "a change by hand")
                .map(drop)
        })
        .unwrap();
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.evicted, count - 5);
    assert_counted(&thumbs, 5, 5);
}

// contract: BLOB-20
#[test]
fn the_ceiling_is_the_sets_and_an_open_with_another_conflicts() {
    let f = fixture();
    f.store.files("pages").max_size(512).open().unwrap();
    let error = f.store.files("pages").max_size(1024).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    assert!(error.to_string().contains("1024") && error.to_string().contains("512"), "{error}");
    let error = f.store.files("pages").when_full(WhenFull::Refuse).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    let plain = f.files("pages");
    assert_eq!(plain.usage().unwrap().max_size, Some(512));
    f.store.files("pages").max_size(512).open().unwrap();
    let f = f.reopen();
    let pages = f.files("pages");
    assert_eq!(pages.usage().unwrap().max_size, Some(512), "kept in the set's file");
}

// contract: BLOB-20
#[test]
fn set_max_size_changes_the_ceiling_when_it_holds_what_if_current_names() {
    let f = fixture();
    let pages = f.store.files("pages").max_size(300).open().unwrap();
    for path in ["a", "b", "c"] {
        pages.put(path, bytes(100)).unwrap();
    }
    pages.set_max_size(300).if_current(300).apply().unwrap();
    let error = pages.set_max_size(400).if_current(512).apply().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    pages.set_max_size(150).if_current(300).apply().unwrap();
    assert_eq!(paths(&pages), ["c"], "a lower ceiling evicts at once");
    assert_eq!(pages.usage().unwrap().evictions, 2);
    f.store.files("pages").max_size(150).open().unwrap();
    pages.remove_max_size().if_current(150).apply().unwrap();
    assert_eq!(pages.usage().unwrap().max_size, None);
    pages.put("d", bytes(1000)).unwrap();
    assert_eq!(paths(&pages), ["c", "d"]);
}

// contract: BLOB-23
#[test]
fn a_refusing_ceiling_fails_a_write_past_it_and_removes_nothing() {
    let f = fixture();
    let uploads = f.store.files("uploads").max_size(250).when_full(WhenFull::Refuse).open().unwrap();
    uploads.put("a", bytes(100)).unwrap();
    uploads.put("b", bytes(100)).unwrap();
    let error = uploads.put("c", bytes(100)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    let error = uploads.copy("a", "c").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "a copy counts its size: {error}");
    assert_eq!(paths(&uploads), ["a", "b"]);
    uploads.put("a", bytes(150)).unwrap();
    uploads.rename("a", "z").unwrap();
    // lowered below its size, it refuses what grows the set and takes what shrinks it
    uploads.set_max_size(100).apply().unwrap();
    assert_eq!(paths(&uploads), ["b", "z"], "a refusing ceiling evicts nothing");
    assert_eq!(uploads.put("c", b"1").unwrap_err().kind(), ErrorKind::Limit);
    uploads.put("z", bytes(10)).unwrap();
    let error = uploads.trim(0).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
}

// contract: BLOB-24
#[test]
fn trim_frees_room_in_the_evictions_order_and_keeps_the_ceiling() {
    let f = fixture();
    let pages = f.store.files("pages").max_size(1000).open().unwrap();
    for path in ["a", "b", "c", "d"] {
        pages.put(path, bytes(100)).unwrap();
        f.clock.advance(Duration::from_secs(1));
    }
    later(&f, 2 * HOUR);
    pages.get("a").unwrap();
    later(&f, Duration::ZERO);
    assert_eq!(pages.trim(250).unwrap(), Trim { evictions: 2, freed: 200 });
    assert_eq!(paths(&pages), ["a", "d"]);
    assert_eq!(pages.usage().unwrap().max_size, Some(1000));
    assert_eq!(pages.usage().unwrap().evictions, 0, "a trim counts in its own answer");
    let plain = f.files("plain");
    plain.put("x", bytes(10)).unwrap();
    assert_eq!(plain.trim(0).unwrap(), Trim { evictions: 1, freed: 10 }, "a set without a ceiling trims too");
}

// contract: BLOB-17
#[test]
fn an_idle_file_lives_from_its_last_get_or_head_and_a_list_renews_nothing() {
    let f = fixture();
    let day = Duration::from_secs(24 * 3600);
    let pages = f.store.files("pages").idle(30 * day).open().unwrap();
    pages.put("read", b"1").unwrap();
    pages.put("headed", b"2").unwrap();
    pages.put("listed", b"3").unwrap();
    for _ in 0..3 {
        later(&f, 20 * day);
        pages.get("read").unwrap().expect("renewed by its last get");
        pages.head("headed").unwrap().expect("renewed by its last head");
        let _ = pages.list().page().unwrap();
        let _ = pages.usage().unwrap();
        later(&f, Duration::ZERO);
    }
    later(&f, Duration::ZERO);
    assert_eq!(paths(&pages), ["headed", "read"]);
    later(&f, 31 * day);
    assert_eq!(pages.head("read").unwrap(), None);
}

#[test]
fn an_idle_file_in_its_last_minute_is_renewed_before_its_read_answers() {
    let f = fixture();
    let pages = f.store.files("pages").idle(HOUR).open().unwrap();
    pages.put("a", b"1").unwrap();
    f.clock.advance(HOUR - Duration::from_secs(30));
    let info = pages.head("a").unwrap().unwrap();
    assert_eq!(info.expires, Some(f.clock.now() + HOUR));
    f.clock.advance(Duration::from_secs(60));
    assert!(pages.head("a").unwrap().is_some(), "renewed without a flush");
}

#[cfg(feature = "replication")]
#[test]
fn a_copy_passes_its_ceilings_change_its_trim_and_its_reads_uses_to_its_home() {
    use crate::replication::Node;
    use crate::{Options, Store};
    let node = |dir: &std::path::Path, node: Node| {
        Store::open(dir, Options { node: Some(node), background: false, ..Options::default() }).unwrap()
    };
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), ..Node::default() });
    let from = vec![home.node().unwrap().address()];
    let copy = node(copy_dir.path(), Node { from, ..Node::default() });
    let at_home = home.files("pages").max_size(1000).open().unwrap();
    for page in ["a", "b", "c"] {
        at_home.put(page, bytes(100)).unwrap();
    }
    // written long ago, so that a read's use is due
    at_home
        .blobs
        .file
        .write(0, |tx| {
            crate::sqlite::execute_cached(tx, "update _tinystore_blobs_files set modified = 0", [], "a change by hand")
                .map(drop)
        })
        .unwrap();
    let started = std::time::Instant::now();
    let until = |what: &str, done: &dyn Fn() -> bool| {
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(30), "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let pages = loop {
        if let Ok(pages) = copy.files("pages").open() {
            break pages;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "the copy never took the set");
        std::thread::sleep(Duration::from_millis(5));
    };
    let written_long_ago = || pages.head("a").unwrap().is_some_and(|a| a.last_modified == std::time::UNIX_EPOCH);
    until("the copy never took the home's files", &written_long_ago);
    pages.get("a").unwrap().unwrap();
    pages.blobs.flush_uses().unwrap();
    let used = || {
        let sql = "select used from _tinystore_blobs_files where path = 'a'";
        at_home.blobs.file.read(|c| Ok(c.query_row(sql, [], |row| row.get::<_, Option<i64>>(0)).unwrap())).unwrap()
    };
    until("the home never recorded the copy's use", &|| used().is_some());
    pages.set_max_size(250).if_current(1000).apply().unwrap();
    assert_eq!(at_home.usage().unwrap().max_size, Some(250));
    assert_eq!(at_home.usage().unwrap().count, 2, "the home evicted down to the lower ceiling");
    assert_eq!(pages.trim(100).unwrap(), Trim { evictions: 1, freed: 100 });
    assert_eq!(at_home.usage().unwrap().count, 1);
    drop((pages, at_home));
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn ttl_and_idle_exclude_each_other() {
    let f = fixture();
    let error = f.store.files("pages").ttl(HOUR).idle(HOUR).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
}

#[test]
fn a_store_opened_beside_records_no_use() {
    let f = fixture();
    let pages = f.store.files("pages").max_size(100).open().unwrap();
    pages.put("a", b"1").unwrap();
    f.clock.advance(2 * HOUR);
    let beside = crate::Store::open_beside(f.dir.path()).unwrap();
    beside.files("pages").open().unwrap().get("a").unwrap().unwrap();
    assert!(beside.files("pages").max_size(100).open().is_ok(), "the ceiling it keeps");
    assert!(beside.files("pages").max_size(200).open().is_err());
    beside.close().unwrap();
    later(&f, Duration::ZERO);
    let used: Option<i64> = f
        .blobs()
        .file
        .read(|c| Ok(c.query_row("select used from _tinystore_blobs_files", [], |row| row.get(0)).unwrap()))
        .unwrap();
    assert_eq!(used, None);
}
