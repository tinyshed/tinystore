use std::io::{Read, Seek, SeekFrom};

use super::fixture::{at, fixture, link_file, link_folder};
use super::*;
use crate::blobs::Usage;
use crate::blobs::fixture::HOUR;

#[test]
fn a_directory_and_a_set_of_files_never_share_a_name() {
    let f = fixture();
    f.store.files("avatars").open().unwrap();
    let taken = f.store.directory("avatars").path(f.path()).open().unwrap_err();
    assert_eq!(taken.kind(), ErrorKind::Invalid, "{taken}");
    f.movies();
    let taken = f.store.files("movies").open().unwrap_err();
    assert_eq!(taken.kind(), ErrorKind::Invalid, "{taken}");
    assert_eq!(f.store.directory("Movies").open().unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn a_path_that_is_not_a_folder_fails_the_open() {
    let f = fixture();
    f.write("film.mkv", b"x", at(1));
    let error = f.store.directory("film").path(f.path().join("film.mkv")).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert!(error.to_string().contains("film.mkv"), "{error}");
}

#[test]
fn opening_walks_nothing_and_lists_nothing_before_a_scan() {
    let f = fixture();
    f.write("film.mkv", b"x", at(1));
    let movies = f.movies();
    assert_eq!(movies.usage().unwrap(), Usage::default());
    assert!(movies.get("film.mkv").unwrap().is_none(), "a get read a path no scan listed");
}

// contract: BLOB-13
#[test]
fn a_file_reads_as_the_disk_holds_it_with_an_etag_of_its_version() {
    let f = fixture();
    f.write("Dune (2021)/Dune.mkv", b"spice must flow", at(7));
    let movies = f.movies();
    movies.scan().unwrap();
    let before = f.picture();
    let mut film = movies.get("Dune (2021)/Dune.mkv").unwrap().unwrap();
    let info = film.info().clone();
    assert_eq!(info.size, 15);
    assert_eq!(info.etag, version_etag(15, nanos_of(at(7))));
    assert!(info.etag.starts_with("\"d-f-"), "{}", info.etag);
    assert_eq!((info.content_type.as_str(), info.expires, info.meta.len()), ("application/octet-stream", None, 0));
    assert_eq!(movies.head("Dune (2021)/Dune.mkv").unwrap(), Some(info.clone()));
    assert_eq!(film.read_all().unwrap(), b"spice must flow");
    let mut range = [0; 4];
    film.seek(SeekFrom::Start(6)).unwrap();
    film.read_exact(&mut range).unwrap();
    assert_eq!(&range, b"must");
    drop(film);
    let folder = movies.folder("Dune (2021)");
    assert_eq!(folder.head("Dune.mkv").unwrap().unwrap().path, "Dune.mkv");
    assert_eq!(folder.list().page().unwrap().files.len(), 1);
    assert_eq!(folder.usage().unwrap(), Usage { count: 1, size: 15 });
    assert_eq!(f.picture(), before, "a read wrote under the directory's path");
}

// contract: BLOB-13
#[test]
fn a_read_of_a_file_that_moved_since_its_get_fails_conflict() {
    let f = fixture();
    f.write("film.mkv", b"first version", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    let mut film = movies.get("film.mkv").unwrap().unwrap();
    let mut piece = [0; 5];
    film.read_exact(&mut piece).unwrap();
    f.write("film.mkv", b"a second version", at(2));
    let error = film.read(&mut piece).unwrap_err();
    let error = error.into_inner().unwrap().downcast::<Error>().unwrap();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    assert!(error.to_string().starts_with("directory movies: \"film.mkv\": "), "{error}");
    // a get again opens the file as it is now, which the list does not hold yet
    let again = movies.get("film.mkv").unwrap().unwrap();
    assert_eq!(again.info().size, 16);
    assert_eq!(movies.head("film.mkv").unwrap().unwrap().size, 13);
}

// contract: BLOB-13
#[test]
fn a_growing_directorys_read_goes_up_to_the_size_now_and_brings_it() {
    let f = fixture();
    f.write("rec.ts", b"0123", at(1));
    let movies = f.open(|options| options.growing(true));
    movies.scan().unwrap();
    let mut recording = movies.get("rec.ts").unwrap().unwrap();
    let mut piece = [0; 4];
    recording.read_exact(&mut piece).unwrap();
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(f.path().join("rec.ts")).unwrap();
        file.write_all(b"4567").unwrap();
        file.set_modified(at(2)).unwrap();
    }
    recording.read_exact(&mut piece).unwrap();
    assert_eq!(&piece, b"4567");
    assert_eq!(recording.info().size, 8, "a piece of a growing file brings its size");
    assert_eq!(recording.info().etag, version_etag(8, nanos_of(at(2))));
    assert_eq!(recording.read(&mut piece).unwrap(), 0);
}

#[test]
fn a_file_gone_since_the_scan_is_none() {
    let f = fixture();
    f.write("film.mkv", b"x", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    f.remove("film.mkv");
    assert!(movies.get("film.mkv").unwrap().is_none());
    assert!(movies.head("film.mkv").unwrap().is_some(), "head answers from the list");
}

#[test]
fn paths_are_the_lists_compared_byte_for_byte() {
    let f = fixture();
    f.write("Dune.mkv", b"x", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    assert!(movies.get("dune.mkv").unwrap().is_none(), "a disk that folds case opened another name");
    assert!(movies.get("../Dune.mkv").is_err());
}

// contract: BLOB-14
#[test]
fn a_path_swapped_for_a_link_between_the_scan_and_the_read_is_refused() {
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("film.mkv"), b"not the folder's").unwrap();
    f.write("films/film.mkv", b"the folder's", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    f.remove("films");
    if !link_folder(outside.path(), &f.path().join("films")) {
        eprintln!("skipped: this process may not make a link");
        return;
    }
    assert!(movies.get("films/film.mkv").unwrap().is_none(), "a read left the directory's folder");
    // a link in the file's own place is refused as well
    f.remove("films");
    std::fs::create_dir(f.path().join("films")).unwrap();
    if link_file(&outside.path().join("film.mkv"), &f.path().join("films/film.mkv")) {
        assert!(movies.get("films/film.mkv").unwrap().is_none(), "a read followed a file's link");
    }
}

// contract: BLOB-11
#[test]
fn changes_keep_a_copy_in_step_rather_than_log_each_event() {
    let f = fixture();
    f.write("kept.mkv", b"k", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    let cursor = movies.changes().page().unwrap().next;

    f.write("new.mkv", b"n", at(1)); // added, then changed
    movies.scan().unwrap();
    f.write("new.mkv", b"nn", at(2));
    f.write("brief.mkv", b"b", at(1)); // added, then removed
    f.write("kept.mkv", b"kk", at(2)); // changed twice
    movies.scan().unwrap();
    f.remove("brief.mkv");
    f.write("kept.mkv", b"kkk", at(3));
    movies.scan().unwrap();

    let page = movies.changes().after(&cursor).page().unwrap();
    let mut told: Vec<(&str, Change)> = page.items.iter().map(|item| (item.path.as_str(), item.change)).collect();
    told.sort_unstable_by_key(|(path, _)| *path);
    assert_eq!(told, [("brief.mkv", Change::Removed), ("kept.mkv", Change::Changed), ("new.mkv", Change::Added)]);
    let kept = page.items.iter().find(|item| item.path == "kept.mkv").unwrap();
    assert_eq!(kept.info.as_ref().map(|info| info.size), Some(3));

    let after = movies.changes().after(&page.next).page().unwrap();
    assert_eq!((after.items.len(), after.next.as_str()), (0, page.next.as_str()), "next is always there");
    assert_eq!(movies.changes().after("x").page().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(movies.folder("a").changes().page().unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn a_removed_files_line_is_kept_30_days_and_a_cursor_before_its_purge_is_too_old() {
    let f = fixture();
    f.write("a.mkv", b"a", at(1));
    f.write("b.mkv", b"b", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    let cursor = movies.changes().page().unwrap().next;
    f.remove("a.mkv");
    movies.scan().unwrap();
    let blobs = Blobs::of(&f.store).unwrap();
    f.clock.advance(changes::REMOVED_KEPT - HOUR);
    assert_eq!(blobs.purge_removed().unwrap(), 0);
    assert_eq!(movies.changes().after(&cursor).page().unwrap().items.len(), 1);
    f.clock.advance(2 * HOUR);
    assert_eq!(blobs.purge_removed().unwrap(), 1);
    let error = movies.changes().after(&cursor).page().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TooOld, "{error}");
    // read again from the start, every file there now is told added
    let page = movies.changes().page().unwrap();
    let told: Vec<(&str, Change)> = page.items.iter().map(|item| (item.path.as_str(), item.change)).collect();
    assert_eq!(told, [("b.mkv", Change::Added)]);
    assert!(movies.changes().after(&page.next).page().is_ok());
}

#[test]
fn the_list_stays_through_a_restart_and_a_directory_opened_again_with_a_new_path() {
    let f = fixture();
    f.write("a/film.mkv", b"film", at(1));
    f.movies().scan().unwrap();
    f.store.close().unwrap();
    let options = crate::Options { clock: Some(f.clock.clone()), background: false, ..crate::Options::default() };
    let store = crate::Store::open(f.dir.path(), options).unwrap();
    let moved = tempfile::tempdir().unwrap();
    std::fs::create_dir(moved.path().join("a")).unwrap();
    std::fs::copy(f.path().join("a/film.mkv"), moved.path().join("a/film.mkv")).unwrap();
    std::fs::File::options().write(true).open(moved.path().join("a/film.mkv")).unwrap().set_modified(at(1)).unwrap();
    let movies = store.directory("movies").path(moved.path()).open().unwrap();
    assert_eq!(movies.usage().unwrap().count, 1);
    assert_eq!(movies.scan().unwrap().added + movies.scan().unwrap().changed, 0, "the moved folder is the same files");
    assert_eq!(movies.get("a/film.mkv").unwrap().unwrap().read_all().unwrap(), b"film");
    drop(movies);
    store.close().unwrap();
}

// contract: BLOB-13
#[cfg(feature = "replication")]
#[test]
fn a_copy_reads_only_a_file_whose_size_and_time_are_the_lists() {
    let f = fixture();
    f.write("film.mkv", b"the home's", at(1));
    let movies = f.movies();
    movies.scan().unwrap();
    let growing = f.store.directory("live").path(f.path()).growing(true).open().unwrap();
    growing.scan().unwrap();
    Blobs::of(&f.store).unwrap().file.become_copy().unwrap();
    assert_eq!(movies.get("film.mkv").unwrap().unwrap().read_all().unwrap(), b"the home's");
    f.write("film.mkv", b"another film", at(1));
    let differs = movies.get("film.mkv").unwrap_err();
    assert_eq!(differs.kind(), ErrorKind::Unavailable, "{differs}");
    assert_eq!(growing.get("film.mkv").unwrap_err().kind(), ErrorKind::Unavailable);
    assert_eq!(movies.scan().unwrap_err().kind(), ErrorKind::Unavailable, "a copy scanned");
    assert_eq!(movies.forget().unwrap_err().kind(), ErrorKind::Unavailable);
}
