use rusqlite::params;
use sha2::{Digest, Sha256};

use super::*;
use crate::Durability;
use crate::blobs::fixture::{fixture, fixture_with};
use crate::blobs::maintain;

#[test]
fn a_folder_is_whole_segments_never_a_prefix_of_text() {
    let f = fixture();
    let media = f.files("media");
    for user in [4, 42, 420] {
        media.folder("users").folder(user).put("a.jpg", user.to_string()).unwrap();
    }
    let four = media.folder("users").folder(4);
    let page = four.list().page().unwrap();
    assert_eq!(page.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), ["a.jpg"]);
    assert_eq!(four.usage().unwrap(), Usage { count: 1, size: 1 });
    four.clear().unwrap();
    assert_eq!(media.usage().unwrap().count, 2, "users/42 and users/420 stay");
    assert!(media.head("users/42/a.jpg").unwrap().is_some());
}

#[test]
fn a_prefix_is_text_within_the_folder() {
    let f = fixture();
    let photos = f.files("media").folder("users").folder(7);
    for name in ["photos/1.jpg", "photos/10.jpg", "photos/2.jpg", "notes.md"] {
        photos.put(name, b"x").unwrap();
    }
    let page = photos.list().prefix("photos/1").page().unwrap();
    assert_eq!(page.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), ["photos/1.jpg", "photos/10.jpg"]);
}

#[test]
fn pages_walk_every_file_once_in_the_byte_order_of_their_paths() {
    let f = fixture_with(Some(Durability::Os));
    let docs = f.files("docs");
    let names: Vec<String> = (0..25).map(|n| format!("{n}.md")).collect();
    for name in &names {
        docs.put(name, b"x").unwrap();
    }
    let mut sorted = names.clone();
    sorted.sort();
    let first = docs.list().limit(10).page().unwrap();
    assert_eq!(first.files.len(), 10);
    let second = docs.list().limit(10).after(first.next.clone().unwrap()).page().unwrap();
    assert_eq!(second.files[0].path, sorted[10]);
    let walked: Vec<String> = docs.all().map(|file| file.unwrap().path).collect();
    assert_eq!(walked, sorted);
    assert_eq!(docs.list().limit(1001).page().unwrap_err().kind(), crate::ErrorKind::Invalid);
}

#[test]
fn a_clear_past_its_bound_hides_every_file_at_once_and_maintenance_removes_them() {
    let f = fixture();
    let docs = f.files("docs").folder("big");
    let blobs = f.blobs();
    let set = docs.set;
    blobs
        .file
        .transaction(|tx| {
            for n in 0..=CLEAR_AT_ONCE as i64 {
                let id = 1_000_000 + n;
                let sha = Sha256::digest([0_u8]).to_vec();
                tx.execute(
                    "insert into contents (id, names, size, sha256, inline) values (?1, 1, 1, ?2, 1)",
                    params![id, sha],
                )
                .and_then(|_| tx.execute("insert into bodies (id, bytes) values (?1, x'00')", [id]))
                .and_then(|_| {
                    tx.execute(
                        "insert into files (set_id, path, revision, content, size, sha256, type, modified)
                                values (?1, ?2, 0, ?3, 1, ?4, 'application/octet-stream', 0)",
                        params![set, format!("big/{n:05}"), id, sha],
                    )
                })
                .map_err(|error| crate::Error::internal(error.to_string()))?;
            }
            Ok::<_, crate::Error>(())
        })
        .unwrap();
    assert_eq!(docs.usage().unwrap().count, CLEAR_AT_ONCE as u64 + 1);
    docs.clear().unwrap();
    assert_eq!(docs.usage().unwrap().count, 0, "hidden at once");
    assert!(docs.list().page().unwrap().files.is_empty());
    assert_eq!(docs.head("00007").unwrap(), None);
    docs.put("after", b"written after the clear").unwrap();
    assert_eq!(docs.usage().unwrap().count, 1, "a file written after a clear is a new file");
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.cleared, CLEAR_AT_ONCE + 1);
    assert_eq!((f.rows("files"), f.rows("cleared"), f.rows("bodies")), (1, 0, 1));
}
