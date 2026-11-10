use std::io::Write as _;
use std::path::PathBuf;

use rusqlite::params;

use super::*;
use crate::blobs::fixture::{Fixture, HOUR, bytes, fixture, flip};
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
            tx.execute("delete from _tinystore_blobs_files", [])
                .and_then(|_| tx.execute("update _tinystore_blobs_contents set names = 0 where id = ?1", [content]))
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

// contract: BLOB-06
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

// contract: BLOB-06
#[test]
#[cfg(windows)]
fn an_open_leaves_to_maintenance_what_a_process_that_died_left_and_another_program_holds() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = fixture();
    let blobs = f.blobs();
    let orphan = blobs.take_id().unwrap();
    std::fs::write(blobs.disk.upload_path(orphan), b"renamed, never committed").unwrap();
    blobs.disk.place(orphan).unwrap();
    let path = blobs.disk.object_path(orphan);
    drop(blobs);
    // as a scanner holds a new file: shared with no one, so it cannot go
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
    let f = f.reopen();
    let docs = f.store.files("docs").open().expect("files open while another program holds what is left");
    docs.put("a", b"written beside it").unwrap();
    assert!(path.exists());
    drop(held);
    maintain(&f.store).unwrap();
    assert!(!path.exists(), "maintenance removes it once it is let go");
    let f = f.reopen();
    assert_eq!(f.files("docs").get("a").unwrap().unwrap().read_all().unwrap(), b"written beside it");
}

// contract: BLOB-02
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

// contract: ERR-07, BLOB-02
#[test]
#[cfg(unix)]
fn the_scrub_marks_a_file_the_disk_will_not_read_and_goes_past_it() {
    let f = fixture();
    let docs = f.files("docs");
    docs.put("unreadable", bytes(512 << 10)).unwrap();
    docs.put("changed", bytes(256 << 10)).unwrap();
    let [(unreadable, _), (changed, _)] = <[_; 2]>::try_from(contents(&f)).unwrap();
    // a directory in its place opens, and each read of it fails as a bad sector's does
    std::fs::remove_file(&unreadable).unwrap();
    std::fs::create_dir(&unreadable).unwrap();
    flip(&changed, 4096);
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.damaged, 2, "the file after one the disk will not read is scrubbed too");
    for path in ["unreadable", "changed"] {
        let error = docs.get(path).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Corrupt, "{path}: {error}");
    }
}

// contract: ERR-07, BLOB-02
#[test]
#[cfg(windows)]
fn the_scrub_passes_over_a_file_another_program_holds_and_reads_it_on_its_next_pass() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = fixture();
    let docs = f.files("docs");
    docs.put("held", bytes(512 << 10)).unwrap();
    docs.put("changed", bytes(256 << 10)).unwrap();
    let [(held, _), (changed, _)] = <[_; 2]>::try_from(contents(&f)).unwrap();
    flip(&changed, 4096);
    // as a scanner holds a file: shared with no one, so the scrub cannot open it
    let holder = std::fs::OpenOptions::new().read(true).share_mode(0).open(held).unwrap();
    let error = maintain(&f.store).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io, "the slice fails, for the store to log once: {error}");
    let damaged: Vec<bool> = contents(&f).into_iter().map(|(_, damaged)| damaged).collect();
    assert_eq!(damaged, [false, true], "a held file is not damaged, and the one after it is scrubbed");
    drop(holder);
    let done = maintain(&f.store).unwrap();
    assert_eq!((done.scrubbed, done.damaged), (512 << 10, 0), "the next pass reads the held file whole");
    assert_eq!(docs.get("held").unwrap().unwrap().read_all().unwrap(), bytes(512 << 10));
}

/// Each content's file and whether the scrub marked it damaged, in the order
/// of their ids.
fn contents(f: &Fixture) -> Vec<(PathBuf, bool)> {
    let blobs = f.blobs();
    let rows: Vec<(i64, bool)> = blobs
        .file
        .read(|connection| {
            let mut statement =
                connection.prepare("select id, damaged from _tinystore_blobs_contents order by id").unwrap();
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap();
    rows.into_iter().map(|(id, damaged)| (blobs.disk.object_path(id), damaged)).collect()
}

// contract: BLOB-02
#[test]
fn a_changed_inline_body_is_found_by_the_scrub() {
    let f = fixture();
    let docs = f.files("docs");
    docs.put("a", bytes(1000)).unwrap();
    f.blobs()
        .file
        .write(0, |tx| {
            tx.execute(
                "update _tinystore_blobs_bodies set bytes = ?1",
                params![bytes(1000).iter().map(|b| b ^ 1).collect::<Vec<u8>>()],
            )
            .map(drop)
            .map_err(|error| Error::internal(error.to_string()))
        })
        .unwrap();
    assert_eq!(maintain(&f.store).unwrap().damaged, 1);
}
