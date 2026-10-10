use std::io::Write as _;

use rusqlite::params;

use super::*;
use crate::blobs::fixture::{HOUR, bytes, fixture, flip};
use crate::{Error, ErrorKind};

#[test]
fn maintenance_removes_expired_files_and_their_bytes() {
    let f = fixture();
    let reports = f.files("reports");
    reports.key("a").ttl(HOUR).put(bytes(1 << 20)).unwrap();
    reports.key("b").ttl(HOUR).put(b"small").unwrap();
    reports.put("kept", b"no term").unwrap();
    f.clock.advance(2 * HOUR);
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.expired, 2);
    assert!(f.objects().is_empty());
    assert_eq!((f.rows("files"), f.rows("contents"), f.rows("bodies")), (1, 1, 1));
}

#[test]
fn bytes_no_file_names_are_removed_when_their_removal_failed_before() {
    let f = fixture();
    let docs = f.files("docs");
    docs.put("a", bytes(1 << 20)).unwrap();
    let path = f.objects().remove(0);
    let content = i64::from_str_radix(&path.file_name().unwrap().to_string_lossy(), 16).unwrap();
    // as a commit whose removal failed leaves it: no name, the file still there
    f.blobs()
        .file
        .write(0, move |tx| {
            tx.execute("delete from files", [])
                .and_then(|_| tx.execute("update contents set names = 0 where id = ?1", [content]))
                .map(drop)
                .map_err(|error| Error::internal(error.to_string()))
        })
        .unwrap();
    assert!(path.exists());
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.removed, 1);
    assert!(!path.exists());
    assert_eq!(f.rows("contents"), 0);
}

#[test]
fn an_open_removes_what_a_process_that_died_left() {
    let f = fixture();
    let docs = f.files("docs");
    let kept = bytes(1 << 20);
    docs.put("kept", &kept).unwrap();
    // an upload that died part way, and one that died between its rename and its commit
    let mut died = docs.upload("died").unwrap();
    died.write_all(&bytes(1 << 20)).unwrap();
    std::mem::forget(died);
    let blobs = f.blobs();
    let orphan = blobs.take_id().unwrap();
    std::fs::write(blobs.disk.upload_path(orphan), b"renamed, never committed").unwrap();
    blobs.disk.place(orphan).unwrap();
    drop(blobs);
    assert_eq!((f.uploads().len(), f.objects().len()), (1, 2));
    let f = f.reopen();
    let docs = f.files("docs");
    assert_eq!((f.uploads().len(), f.objects().len()), (0, 1));
    assert_eq!(docs.get("kept").unwrap().unwrap().read_all().unwrap(), kept);
}

#[test]
fn the_scrub_finds_a_changed_byte_and_a_read_of_it_is_corrupt() {
    let f = fixture();
    let docs = f.files("docs");
    docs.put("large", bytes(512 << 10)).unwrap();
    docs.put("small", b"inline and fine").unwrap();
    flip(&f.objects()[0], 4096);
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.damaged, 1);
    assert!(done.scrubbed >= 512 << 10);
    let error = docs.get("large").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
    assert_eq!(docs.get("small").unwrap().unwrap().read_all().unwrap(), b"inline and fine");
    assert_eq!(maintain(&f.store).unwrap().damaged, 0, "a damaged content is judged once");
}

#[test]
fn a_changed_inline_body_is_found_by_the_scrub() {
    let f = fixture();
    let docs = f.files("docs");
    docs.put("a", bytes(1000)).unwrap();
    f.blobs()
        .file
        .write(0, |tx| {
            tx.execute("update bodies set bytes = ?1", params![bytes(1000).iter().map(|b| b ^ 1).collect::<Vec<u8>>()])
                .map(drop)
                .map_err(|error| Error::internal(error.to_string()))
        })
        .unwrap();
    assert_eq!(maintain(&f.store).unwrap().damaged, 1);
}
