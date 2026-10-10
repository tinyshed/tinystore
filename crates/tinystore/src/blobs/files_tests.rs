use super::*;
use crate::blobs::fixture::{HOUR, bytes, fixture};
use crate::blobs::upload::INLINE;

#[test]
fn a_put_reads_back_whole_inline_and_in_a_file() {
    let f = fixture();
    let files = f.files("avatars");
    for size in [0, 1, INLINE, INLINE + 1, 3 << 20] {
        let body = bytes(size);
        let info = files.key("a.bin").content_type("image/png").meta("name", "cat.png").put(&body).unwrap();
        assert_eq!((info.path.as_str(), info.size, info.content_type.as_str()), ("a.bin", size as u64, "image/png"));
        assert_eq!(info.meta.get("name").map(String::as_str), Some("cat.png"));
        let mut stored = files.get("a.bin").unwrap().expect("the file just written");
        assert_eq!(stored.read_all().unwrap(), body, "{size} bytes");
        assert_eq!(stored.info(), &info);
        assert_eq!(files.head("a.bin").unwrap(), Some(info));
    }
    assert_eq!(f.objects().len(), 1, "a replaced file's bytes go");
    assert_eq!(f.rows("contents where names > 0"), 1);
}

#[test]
fn a_file_without_a_type_is_octets_and_its_etag_is_its_hash() {
    let f = fixture();
    let info = f.files("docs").put("a", b"abc").unwrap();
    assert_eq!(info.content_type, "application/octet-stream");
    assert_eq!(info.etag, "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\"");
}

#[test]
fn create_writes_only_where_nothing_is_and_says_so() {
    let f = fixture();
    let docs = f.files("docs");
    assert!(docs.create("a", b"first").unwrap());
    assert!(!docs.create("a", b"second").unwrap());
    assert!(!docs.create("a", bytes(INLINE + 1)).unwrap());
    assert_eq!(docs.get("a").unwrap().unwrap().read_all().unwrap(), b"first");
    assert!(f.objects().is_empty(), "a create that found a file leaves nothing");
    assert!(f.uploads().is_empty());
}

#[test]
fn of_two_writes_that_read_one_etag_one_conflicts() {
    let f = fixture();
    let docs = f.files("docs");
    let read = docs.put("a", b"1").unwrap();
    docs.key("a").if_match(&read.etag).put(b"2").unwrap();
    let error = docs.key("a").if_match(&read.etag).put(b"3").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    assert_eq!(docs.get("a").unwrap().unwrap().read_all().unwrap(), b"2");
    let error = docs.key("a").if_match(&read.etag).delete().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    let now = docs.head("a").unwrap().unwrap();
    docs.key("a").if_match(now.etag.trim_matches('"')).delete().unwrap();
    assert_eq!(docs.head("a").unwrap(), None);
    let error = docs.key("b").if_match(&now.etag).put(b"x").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "an ETag names a file that is there");
}

#[test]
fn a_copy_shares_the_bytes_and_outlives_its_source() {
    let f = fixture();
    let docs = f.files("docs");
    let body = bytes(1 << 20);
    docs.put("a", &body).unwrap();
    let copy = docs.copy("a", "b").unwrap();
    assert_eq!((copy.path.as_str(), copy.size), ("b", body.len() as u64));
    assert_eq!(f.objects().len(), 1, "no byte is copied");
    docs.delete("a").unwrap();
    assert_eq!(docs.get("b").unwrap().unwrap().read_all().unwrap(), body);
    assert_eq!(f.objects().len(), 1);
    docs.delete("b").unwrap();
    assert!(f.objects().is_empty(), "the bytes go with the last file that names them");
}

#[test]
fn copy_and_rename_never_replace_a_file_by_surprise() {
    let f = fixture();
    let docs = f.files("docs");
    docs.put("draft", b"new").unwrap();
    let old = docs.put("final", b"old").unwrap();
    for error in [docs.copy("draft", "final").unwrap_err(), docs.rename("draft", "final").unwrap_err()] {
        assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    }
    assert_eq!(docs.get("final").unwrap().unwrap().read_all().unwrap(), b"old");
    docs.key("final").if_match(&old.etag).rename_from("draft").unwrap();
    assert_eq!(docs.get("final").unwrap().unwrap().read_all().unwrap(), b"new");
    assert_eq!(docs.head("draft").unwrap(), None, "a rename leaves nothing behind");
    let error = docs.rename("draft", "other").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
}

#[test]
fn a_file_expires_by_its_own_term_or_its_files_term() {
    let f = fixture();
    let reports = f.store.files("reports").ttl(24 * HOUR).open().unwrap();
    reports.put("daily", b"x").unwrap();
    reports.key("weekly").ttl(7 * 24 * HOUR).put(b"y").unwrap();
    f.clock.advance(23 * HOUR);
    assert!(reports.head("daily").unwrap().is_some());
    f.clock.advance(2 * HOUR);
    assert_eq!(reports.head("daily").unwrap(), None);
    assert!(reports.get("daily").unwrap().is_none());
    assert!(reports.head("weekly").unwrap().is_some(), "a write's own term is not the files' ceiling");
    assert!(reports.expire("weekly", HOUR).unwrap());
    f.clock.advance(2 * HOUR);
    assert_eq!(reports.head("weekly").unwrap(), None);
    assert!(!reports.expire("weekly", HOUR).unwrap());
}

#[test]
fn a_write_again_lives_its_term_again() {
    let f = fixture();
    let reports = f.store.files("reports").ttl(24 * HOUR).open().unwrap();
    reports.put("daily", b"x").unwrap();
    f.clock.advance(20 * HOUR);
    reports.put("daily", b"y").unwrap();
    f.clock.advance(20 * HOUR);
    assert!(reports.head("daily").unwrap().is_some());
}

#[test]
fn a_path_or_a_name_that_is_not_one_is_refused() {
    let f = fixture();
    let docs = f.files("docs");
    for path in ["", "a//b", "../a", "a/./b", "a/", "a\u{7}b"] {
        let error = docs.put(path, b"x").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{path:?}: {error}");
    }
    for owner in ["", "4/2", ".."] {
        let error = docs.folder(owner).put("a", b"x").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{owner:?}: {error}");
    }
    let error = f.store.files("Avatars").open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
}

#[test]
fn an_error_names_the_files_and_the_path() {
    let f = fixture();
    let docs = f.files("docs").folder("users").folder(42);
    docs.put("a", b"1").unwrap();
    docs.put("b", b"2").unwrap();
    let error = docs.copy("a", "b").unwrap_err();
    assert!(error.what().starts_with("files docs: \"users/42/b\": "), "{error}");
}

#[test]
fn a_path_is_its_own_bytes_whatever_the_file_system() {
    let f = fixture();
    let docs = f.files("docs");
    docs.put("Photo.jpg", b"upper").unwrap();
    docs.put("photo.jpg", b"lower").unwrap();
    docs.put("CON", b"a name windows keeps").unwrap();
    assert_eq!(docs.get("Photo.jpg").unwrap().unwrap().read_all().unwrap(), b"upper");
    assert_eq!(docs.get("photo.jpg").unwrap().unwrap().read_all().unwrap(), b"lower");
    assert_eq!(docs.get("CON").unwrap().unwrap().read_all().unwrap(), b"a name windows keeps");
}

#[test]
fn a_put_that_returned_is_there_after_a_reopen() {
    let f = fixture();
    let docs = f.files("docs");
    let (small, large) = (bytes(100), bytes(INLINE * 4));
    docs.put("small", &small).unwrap();
    docs.put("large", &large).unwrap();
    let f = f.reopen();
    let docs = f.files("docs");
    assert_eq!(docs.get("small").unwrap().unwrap().read_all().unwrap(), small);
    assert_eq!(docs.get("large").unwrap().unwrap().read_all().unwrap(), large);
}
