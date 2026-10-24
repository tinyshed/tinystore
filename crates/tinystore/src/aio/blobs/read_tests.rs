use std::io::{Read, Seek};
use std::pin::pin;
use std::task::Waker;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::aio::Store;
use crate::aio::blobs::Files;
use crate::blobs::fixture::{Fixture, bytes, fixture, flip};
use crate::{CancellationToken, ErrorKind};

/// A file of 1 MiB and 10 bytes, a piece of it short: four pieces and a tail.
const SIZE: usize = (1 << 20) + 10;

fn awaited(f: &Fixture) -> (Store, Files) {
    let store = Store::from(f.store.clone());
    let docs = store.files("docs").open().unwrap();
    docs.blocking().put("large", bytes(SIZE)).unwrap();
    docs.blocking().put("small", bytes(100)).unwrap();
    (store, docs)
}

/// The next item of a stream, awaited, as `StreamExt::next` gives it.
async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

async fn pieces(file: &mut StoredFile) -> Vec<Result<Vec<u8>>> {
    let mut pieces = Vec::new();
    while let Some(piece) = next(file).await {
        pieces.push(piece);
    }
    pieces
}

#[tokio::test]
async fn a_file_streams_in_pieces_of_256_kib_that_make_it_whole() {
    let f = fixture();
    let (_store, docs) = awaited(&f);
    let mut large = docs.get("large").await.unwrap().unwrap();
    let read: Vec<Vec<u8>> = pieces(&mut large).await.into_iter().map(Result::unwrap).collect();
    assert_eq!(read.iter().map(Vec::len).collect::<Vec<_>>(), [PIECE, PIECE, PIECE, PIECE, 10]);
    assert_eq!(read.concat(), bytes(SIZE));
    assert!(next(&mut large).await.is_none(), "a stream read to its end stays ended");
    let mut small = docs.get("small").await.unwrap().unwrap();
    let read: Vec<Vec<u8>> = pieces(&mut small).await.into_iter().map(Result::unwrap).collect();
    assert_eq!(read, [bytes(100)], "a file kept inline is one piece");
}

// contract: BLOB-02
#[tokio::test]
async fn a_stream_of_a_changed_byte_fails_corrupt_before_its_end() {
    let f = fixture();
    let (_store, docs) = awaited(&f);
    flip(&f.objects()[0], 1000);
    let mut changed = docs.get("large").await.unwrap().unwrap();
    let read = pieces(&mut changed).await;
    let handed: usize = read.iter().filter_map(|piece| piece.as_ref().ok()).map(Vec::len).sum();
    assert!(handed < SIZE, "the last piece is held back");
    let error = read.last().unwrap().as_ref().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
    assert_eq!(read.iter().filter(|piece| piece.is_err()).count(), 1, "the stream ends at its failure");
    let error = changed.read_all().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
}

#[tokio::test]
async fn a_range_reads_as_the_blocking_one_does_and_is_not_checked() {
    let f = fixture();
    let (_store, docs) = awaited(&f);
    let mut blocking = docs.blocking().get("large").unwrap().unwrap();
    let large = docs.get("large").await.unwrap().unwrap();
    for (at, length) in [(0, 64 << 10), (SIZE as u64 - 5, 64), (SIZE as u64, 8)] {
        let (mut awaited, mut read) = (vec![0; length], vec![0; length]);
        let got = large.read_at(&mut awaited, at).await.unwrap();
        assert_eq!(got, blocking.read_at(&mut read, at).unwrap(), "at {at}");
        assert_eq!(awaited[..got], read[..got]);
    }
    let small = docs.get("small").await.unwrap().unwrap();
    let mut range = [0; 10];
    assert_eq!(small.read_at(&mut range, 90).await.unwrap(), 10);
    assert_eq!(range, bytes(100)[90..]);

    flip(&f.objects()[0], (1 << 20) + 2);
    let mut changed = docs.get("large").await.unwrap().unwrap();
    assert_eq!(changed.seek(SeekFrom::Start(1 << 20)).unwrap(), 1 << 20);
    let tail: Vec<u8> = pieces(&mut changed).await.into_iter().map(Result::unwrap).collect::<Vec<_>>().concat();
    let mut expected = vec![0; 10];
    blocking.seek(SeekFrom::Start(1 << 20)).unwrap();
    blocking.read_exact(&mut expected).unwrap();
    assert_eq!(tail, expected, "a range hands out the bytes on disk, changed or not");
}

// contract: STORE-10
#[test]
fn a_get_answers_a_file_kept_inline_in_its_first_poll_and_opens_one_on_disk_on_the_pool() {
    let f = fixture();
    let (_store, docs) = awaited(&f);
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    runtime.block_on(docs.head("small")).unwrap(); // a read leaves its reader idle
    let mut context = Context::from_waker(Waker::noop());
    let mut small = pin!(docs.get("small"));
    assert!(matches!(small.as_mut().poll(&mut context), Poll::Ready(Ok(Some(_)))), "answered in its poll");
    let mut large = pin!(docs.get("large"));
    assert!(large.as_mut().poll(&mut context).is_pending(), "its file is opened on the pool");
    let opened = runtime.block_on(large).unwrap().unwrap();
    assert_eq!(opened.info().size, SIZE as u64);
}

// contract: STORE-09
#[tokio::test]
async fn a_whole_read_under_a_cancelled_token_stops_and_fails_cancelled() {
    let f = fixture();
    let (_store, docs) = awaited(&f);
    let large = docs.get("large").await.unwrap().unwrap();
    let token = CancellationToken::new();
    token.cancel();
    let refused = token.run(|| large.read_all()).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Cancelled, "{refused}");
    assert_eq!(large.read_all().await.unwrap(), bytes(SIZE), "a read under no token reads it whole");
}

// contract: STORE-09
#[test]
fn a_whole_read_whose_future_is_dropped_stops_before_its_next_piece() {
    let f = fixture();
    let (_store, docs) = awaited(&f);
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let large = runtime.block_on(docs.get("large")).unwrap().unwrap();
    let held = alone(&large.file);
    {
        let mut read = pin!(large.read_all());
        assert!(read.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
        // the pool takes the read, which waits for the reader this test holds
        thread::sleep(Duration::from_millis(20));
    }
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Arc::strong_count(&large.file) > 1 {
        assert!(Instant::now() < deadline, "the dropped read never let go of the file");
        thread::sleep(Duration::from_millis(1));
    }
    let stopped_at = alone(&large.file).stream_position().unwrap();
    assert_eq!(stopped_at, 0, "it stopped before its first piece, rather than read the file whole");
}

#[tokio::test]
async fn a_stream_of_a_file_on_disk_ends_when_its_store_closes() {
    let f = fixture();
    let (store, docs) = awaited(&f);
    let mut large = docs.get("large").await.unwrap().unwrap();
    let mut small = docs.get("small").await.unwrap().unwrap();
    store.close().await.unwrap();
    let closed = next(&mut large).await.expect("the piece's failure is an item").unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    assert!(closed.to_string().starts_with(r#"files docs: "large": a piece of its stream"#), "{closed}");
    assert!(next(&mut large).await.is_none(), "and then it ends");
    assert_eq!(next(&mut small).await.unwrap().unwrap(), bytes(100), "a file kept inline is in memory already");
}
