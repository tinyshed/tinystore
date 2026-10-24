use std::task::Waker;

use rusqlite::params;
use sha2::{Digest, Sha256};

use super::*;
use crate::ErrorKind;
use crate::aio::Store;
use crate::blobs::fixture::fixture;

/// The next item of a stream, awaited, as `StreamExt::next` gives it.
async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

#[tokio::test]
async fn a_page_and_a_usage_answer_as_the_blocking_ones_do() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let photos = store.files("media").open().unwrap().folder("users").folder(7);
    for name in ["photos/1.jpg", "photos/10.jpg", "photos/2.jpg", "notes.md"] {
        photos.put(name, name.to_owned()).await.unwrap();
    }
    let blocking = photos.blocking();
    assert_eq!(
        photos.list().prefix("photos/1").page().await.unwrap(),
        blocking.list().prefix("photos/1").page().unwrap()
    );
    let first = photos.list().limit(2).page().await.unwrap();
    assert_eq!(first, blocking.list().limit(2).page().unwrap());
    let after = first.next.clone().unwrap();
    assert_eq!(photos.list().after(&after).page().await.unwrap(), blocking.list().after(&after).page().unwrap());
    assert_eq!(photos.usage().await.unwrap(), blocking.usage().unwrap());
    let refused = photos.list().limit(PAGE_FILES + 1).page().await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    photos.clear().await.unwrap();
    assert_eq!(blocking.usage().unwrap().count, 0);
}

#[tokio::test]
async fn every_file_streams_once_in_the_byte_order_of_their_paths_across_pages() {
    let f = fixture();
    let docs = Store::from(f.store.clone()).files("docs").open().unwrap();
    let set = docs.blocking().set;
    // past a page, in one transaction: a commit a file would take a while
    f.blobs()
        .file
        .transaction(|tx| {
            for n in 0..=PAGE_FILES as i64 {
                let (id, sha) = (1_000_000 + n, Sha256::digest([0_u8]).to_vec());
                tx.execute(
                    "insert into contents (id, names, size, sha256, inline) values (?1, 1, 1, ?2, 1)",
                    params![id, sha],
                )
                .and_then(|_| tx.execute("insert into bodies (id, bytes) values (?1, x'00')", [id]))
                .and_then(|_| {
                    tx.execute(
                        "insert into files (set_id, path, revision, content, size, sha256, type, modified)
                                values (?1, ?2, 0, ?3, 1, ?4, 'application/octet-stream', 0)",
                        params![set, format!("{n:05}.md"), id, sha],
                    )
                })
                .map_err(|error| crate::Error::internal(error.to_string()))?;
            }
            Ok::<_, crate::Error>(())
        })
        .unwrap();
    let mut all = docs.all();
    let mut walked = Vec::new();
    while let Some(file) = next(&mut all).await {
        walked.push(file.unwrap().path);
    }
    let blocking: Vec<String> = docs.blocking().all().map(|file| file.unwrap().path).collect();
    assert_eq!(walked.len(), PAGE_FILES + 1);
    assert_eq!(walked, blocking);
}

#[test]
fn a_stream_of_files_ends_when_its_store_closes() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let docs = store.files("docs").open().unwrap();
    docs.blocking().put("a", b"x").unwrap();
    let mut all = docs.all();
    store.blocking().close().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let closed = runtime.block_on(next(&mut all)).expect("the page's failure is an item");
    assert_eq!(closed.unwrap_err().kind(), ErrorKind::Closed);
    let after = Pin::new(&mut all).poll_next(&mut Context::from_waker(Waker::noop()));
    assert!(matches!(after, Poll::Ready(None)), "and then it ends");
}
