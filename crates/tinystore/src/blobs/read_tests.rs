use std::io::{Read, Seek, SeekFrom};

use rusqlite::params;

use super::*;
use crate::ErrorKind;
use crate::blobs::fixture::{bytes, fixture, flip};

#[test]
fn a_whole_read_of_a_changed_byte_fails_before_its_end() {
    let f = fixture();
    let docs = f.files("docs");
    let body = bytes(1 << 20);
    docs.put("a", &body).unwrap();
    flip(&f.objects()[0], 1000);
    let mut stored = docs.get("a").unwrap().unwrap();
    let mut handed = 0;
    let mut chunk = vec![0; 64 << 10];
    let failure = loop {
        match stored.read(&mut chunk) {
            Ok(0) => panic!("a changed file read to its end"),
            Ok(n) => handed += n,
            Err(error) => break error,
        }
    };
    assert!(handed < body.len(), "the last bytes are held back");
    let error = failure.into_inner().unwrap().downcast::<Error>().unwrap();
    assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
    assert!(error.what().starts_with("files docs: \"a\""), "{error}");
    let error = docs.get("a").unwrap().unwrap().read_all().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
}

#[test]
fn a_range_is_not_checked() {
    let f = fixture();
    let docs = f.files("docs");
    let body = bytes(1 << 20);
    docs.put("a", &body).unwrap();
    flip(&f.objects()[0], 1000);
    let mut stored = docs.get("a").unwrap().unwrap();
    let mut range = vec![0; 100];
    assert_eq!(stored.read_at(&mut range, 2000).unwrap(), 100);
    assert_eq!(range, body[2000..2100]);
    stored.seek(SeekFrom::Start(500_000)).unwrap();
    let mut rest = Vec::new();
    stored.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, body[500_000..]);
}

#[test]
fn an_inline_file_whose_bytes_changed_is_corrupt_when_read() {
    let f = fixture();
    let docs = f.files("docs");
    docs.put("a", bytes(1000)).unwrap();
    f.blobs()
        .file
        .write(0, |tx| {
            tx.execute("update bodies set bytes = ?1", params![bytes(999)])
                .map(drop)
                .map_err(|e| Error::internal(e.to_string()))
        })
        .unwrap();
    let error = docs.get("a").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
}

#[test]
fn a_reader_keeps_what_it_opened_through_a_replace_and_a_delete() {
    let f = fixture();
    let docs = f.files("docs");
    let (first, second) = (bytes(1 << 20), bytes((1 << 20) + 1));
    docs.put("a", &first).unwrap();
    docs.put("b", &first).unwrap();
    let mut replaced = docs.get("a").unwrap().unwrap();
    let mut deleted = docs.get("b").unwrap().unwrap();
    docs.put("a", &second).unwrap();
    docs.delete("b").unwrap();
    assert_eq!(replaced.read_all().unwrap(), first);
    assert_eq!(deleted.read_all().unwrap(), first);
    assert_eq!(docs.get("a").unwrap().unwrap().read_all().unwrap(), second);
}

#[test]
fn reads_at_from_several_threads_see_the_same_bytes() {
    let f = fixture();
    let docs = f.files("docs");
    let body = bytes(4 << 20);
    docs.put("a", &body).unwrap();
    let stored = docs.get("a").unwrap().unwrap();
    std::thread::scope(|scope| {
        for part in 0..8_usize {
            let (stored, body) = (&stored, &body);
            scope.spawn(move || {
                let at = part * (512 << 10);
                let mut read = vec![0; 512 << 10];
                let mut done = 0;
                while done < read.len() {
                    done += stored.read_at(&mut read[done..], (at + done) as u64).unwrap();
                }
                assert_eq!(read, body[at..at + read.len()]);
            });
        }
    });
}
