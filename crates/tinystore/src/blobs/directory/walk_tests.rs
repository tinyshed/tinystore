use super::super::fixture::{at, fixture, link_folder};
use super::super::names::{IN_MEMORY, MOST_HELD};
use super::*;
use crate::blobs::Change;

fn told(scan: Scan) -> (u64, u64, u64, u64) {
    (scan.added, scan.changed, scan.removed, scan.skipped)
}

// contract: BLOB-10
// contract: BLOB-11
#[test]
fn a_scan_tells_what_appeared_changed_and_went_and_writes_nothing_in_the_folder() {
    let f = fixture();
    f.write("a.txt", b"a", at(1));
    f.write("b/1.mkv", b"one", at(2));
    f.write("b/2.mkv", b"two", at(3));
    f.write("d.txt", b"d", at(4));
    let movies = f.movies();
    let before = f.picture();
    let first = movies.scan().unwrap();
    assert_eq!(told(first), (4, 0, 0, 0));
    assert_eq!((first.count, first.size), (4, 8));
    assert_eq!(f.picture(), before, "a scan wrote under the directory's path");

    f.remove("b/2.mkv");
    f.remove("d.txt");
    f.write("c.txt", b"c", at(5));
    f.write("a.txt", b"aa", at(1)); // its size alone moved
    f.write("b/1.mkv", b"one", at(9)); // its time alone moved
    let before = f.picture();
    assert_eq!(told(movies.scan().unwrap()), (1, 2, 2, 0));
    assert_eq!(told(movies.scan().unwrap()), (0, 0, 0, 0), "a scan that finds nothing tells nothing");
    assert_eq!(f.picture(), before);

    let listed: Vec<String> = movies.list().page().unwrap().files.into_iter().map(|file| file.path).collect();
    assert_eq!(listed, ["a.txt", "b/1.mkv", "c.txt"]);
    // a reader that walks from the start ends with what the list holds
    let page = movies.changes().page().unwrap();
    let there: Vec<&str> =
        page.items.iter().filter(|item| item.change != Change::Removed).map(|item| item.path.as_str()).collect();
    let mut there = there;
    there.sort_unstable();
    assert_eq!(there, listed);
}

#[test]
fn a_change_with_its_size_and_its_time_kept_is_not_seen() {
    let f = fixture();
    f.write("film.mkv", b"first", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    f.write("film.mkv", b"other", at(1));
    assert_eq!(told(movies.scan().unwrap()), (0, 0, 0, 0), "rsync's quick check, and its blind spot");
}

#[test]
fn a_scan_walks_in_the_byte_order_of_whole_paths() {
    let f = fixture();
    for path in ["a0", "a-c", "a/b", "a.txt", "B", "é"] {
        f.write(path, b"x", at(1));
    }
    let movies = f.movies();
    movies.scan().unwrap();
    let page = movies.changes().page().unwrap();
    let order: Vec<&str> = page.items.iter().map(|item| item.path.as_str()).collect();
    assert_eq!(order, ["B", "a-c", "a.txt", "a/b", "a0", "é"]);
}

// contract: BLOB-12
#[test]
fn a_missing_folder_or_one_found_empty_while_its_list_is_not_fails_and_changes_nothing() {
    let f = fixture();
    f.write("a/1.mkv", b"one", at(1));
    f.write("b.mkv", b"b", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    let cursor = movies.changes().page().unwrap().next;

    f.remove("a");
    f.remove("b.mkv");
    let empty = movies.scan().unwrap_err();
    assert_eq!(empty.kind(), ErrorKind::Unavailable, "{empty}");
    assert!(empty.to_string().starts_with("directory movies: "), "{empty}");
    assert_eq!(movies.usage().unwrap().count, 2, "a refused scan changed the list");
    assert!(movies.changes().after(&cursor).page().unwrap().items.is_empty());

    let gone = f.path().join("gone");
    let missing = f.store.directory("missing").path(&gone).open().unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::Invalid, "{missing}");

    // a folder truly emptied is told with forget, and then scans empty
    movies.forget().unwrap();
    assert_eq!(told(movies.scan().unwrap()), (0, 0, 0, 0));
    let page = movies.changes().after(&cursor).page().unwrap();
    assert!(page.items.iter().all(|item| item.change == Change::Removed && item.info.is_none()));
    assert_eq!(page.items.len(), 2);
}

// contract: BLOB-12
#[test]
fn a_folder_gone_inside_the_directory_is_believed() {
    let f = fixture();
    f.write("Dune (2021)/Dune.mkv", b"dune", at(1));
    f.write("Heat.mkv", b"heat", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    f.remove("Dune (2021)");
    assert_eq!(told(movies.scan().unwrap()), (0, 0, 1, 0));
}

// contract: BLOB-12
#[test]
fn a_link_is_neither_listed_nor_entered_and_keeps_what_the_list_holds_under_it() {
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
    f.write("films/a.mkv", b"a", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    // the folder of films becomes a link to another place
    f.remove("films");
    if !link_folder(outside.path(), &f.path().join("films")) {
        eprintln!("skipped: this process may not make a link");
        return;
    }
    let before = f.picture();
    let scan = movies.scan().unwrap();
    assert_eq!(told(scan), (0, 0, 0, 1), "a link is skipped and its folder's files are kept");
    assert!(movies.head("films/secret.txt").unwrap().is_none(), "a scan followed a link");
    assert!(movies.head("films/a.mkv").unwrap().is_some());
    assert!(movies.get("films/a.mkv").unwrap().is_none(), "a read followed a link");
    assert_eq!(f.picture(), before);
}

#[test]
fn every_scans_on_the_stores_background_work_while_a_handle_is_open() {
    let (dir, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(folder.path().join("film.mkv"), b"x").unwrap();
    let store = crate::Store::open(dir.path(), crate::Options::default()).unwrap();
    let movies =
        store.directory("movies").path(folder.path()).every(std::time::Duration::from_millis(20)).open().unwrap();
    let waited = std::time::Instant::now();
    while movies.usage().unwrap().count == 0 {
        assert!(waited.elapsed() < std::time::Duration::from_secs(10), "no scan ran on the background work");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(movies);
    store.close().unwrap();
}

#[test]
fn what_ignore_names_is_neither_listed_nor_counted() {
    let f = fixture();
    f.write("film.mkv", b"film", at(1));
    f.write("film.mkv.part", b"half", at(1));
    f.write(".cache/x", b"x", at(1));
    let movies = f.open(|options| options.ignore(["*.part", ".*"]));
    assert_eq!(told(movies.scan().unwrap()), (1, 0, 0, 0));
}

#[test]
fn a_name_past_the_bounds_of_a_path_is_skipped_and_counted() {
    let f = fixture();
    let deep = (0..16).map(|n| n.to_string()).collect::<Vec<_>>().join("/");
    f.write(&format!("{deep}/too-deep.mkv"), b"x", at(1));
    f.write("ok.mkv", b"x", at(1));
    let movies = f.movies();
    let scan = movies.scan().unwrap();
    assert_eq!((scan.added, scan.skipped), (1, 1));
}

// contract: BLOB-16
#[test]
fn a_folder_past_the_names_sorted_in_memory_holds_a_buffer_of_them_and_scans_the_same() {
    let f = fixture();
    let files = 3 * IN_MEMORY + 5;
    for n in 0..files {
        f.write(&format!("big/{n:05}.jpg"), b"x", at(1));
    }
    f.write("big.txt", b"x", at(1));
    f.write("big0", b"x", at(1));
    let movies = f.movies();
    MOST_HELD.with(|most| most.set(0));
    assert_eq!(movies.scan().unwrap().added, files as u64 + 2);
    assert!(MOST_HELD.with(std::cell::Cell::get) <= IN_MEMORY + 1, "a folder's names held in memory whole");
    let mut cursor = None::<String>;
    let mut order = Vec::new();
    loop {
        let call = movies.changes();
        let page = match &cursor {
            Some(cursor) => call.after(cursor).page(),
            None => call.page(),
        }
        .unwrap();
        if page.items.is_empty() {
            break;
        }
        order.extend(page.items.into_iter().map(|item| item.path));
        cursor = Some(page.next);
    }
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted, "the table sorted a folder out of the order of whole paths");
    assert_eq!(order.first().map(String::as_str), Some("big.txt"));
    assert_eq!(order.last().map(String::as_str), Some("big0"));
    std::fs::remove_file(f.path().join(format!("big/{:05}.jpg", IN_MEMORY + 1))).unwrap();
    assert_eq!(told(movies.scan().unwrap()), (0, 0, 1, 0));
}

#[test]
fn a_scan_of_a_folder_of_a_directory_or_without_a_folder_of_this_machine_is_refused() {
    let f = fixture();
    f.write("a/b.mkv", b"x", at(1));
    let movies = f.movies();
    assert_eq!(movies.folder("a").scan().unwrap_err().kind(), ErrorKind::Invalid);
    movies.scan().unwrap();
    let elsewhere = f.store.directory("movies").open().unwrap();
    assert_eq!(elsewhere.scan().unwrap_err().kind(), ErrorKind::Unavailable);
    assert_eq!(elsewhere.usage().unwrap().count, 1, "a directory without its folder still lists");
    assert_eq!(elsewhere.get("a/b.mkv").unwrap_err().kind(), ErrorKind::Unavailable);
}
