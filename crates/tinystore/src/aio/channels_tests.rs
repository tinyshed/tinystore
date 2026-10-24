//! A channel's messages read as a stream, beside the blocking loop of the
//! same store.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Wake, Waker};

use super::*;
use crate::aio::Store;
use crate::testing::PATIENCE;
use crate::{ErrorKind, Options};

fn opened(dir: &tempfile::TempDir) -> Store {
    Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap()
}

async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    let next = std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context));
    tokio::time::timeout(PATIENCE, next).await.expect("the stream answered within its patience")
}

fn values<T>(messages: Vec<Result<Message<T>>>) -> Vec<T> {
    messages.into_iter().map(|message| message.unwrap().value).collect()
}

// contract: CHAN-01
#[tokio::test]
async fn each_hears_what_is_published_after_it_in_order_as_the_blocking_loop_does() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let chat = store.channel::<String>("chat").open().unwrap();
    chat.under("room-7").publish(&"before anyone listened".to_owned()).unwrap();
    let mut awaited = chat.under("room-7").each().unwrap();
    let blocking = chat.blocking().under("room-7").each().unwrap();
    for text in ["a", "b", "c"] {
        chat.under("room-7").publish(&text.to_owned()).unwrap();
    }
    chat.under("room-8").publish(&"another room".to_owned()).unwrap();

    let mut heard = Vec::new();
    for _ in 0..3 {
        heard.push(next(&mut awaited).await.unwrap());
    }
    let blocking_heard: Vec<_> = blocking.take(3).collect();
    assert_eq!(values(heard), ["a", "b", "c"]);
    assert_eq!(values(blocking_heard), ["a", "b", "c"]);

    let mut below = chat.recursive().each().unwrap();
    chat.under("room-9").publish(&"below".to_owned()).unwrap();
    let message = next(&mut below).await.unwrap().unwrap();
    assert_eq!((message.value.as_str(), message.branch.as_slice()), ("below", ["room-9".to_owned()].as_slice()));
    store.close().await.unwrap();
}

/// A waker that says whether it was woken.
struct Woken(AtomicBool);

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A context whose waker says whether it was woken.
fn watched() -> (Arc<Woken>, Waker) {
    let woken = Arc::new(Woken(AtomicBool::new(false)));
    (Arc::clone(&woken), Waker::from(woken))
}

fn polled<T: DeserializeOwned>(each: &mut Each<T>, waker: &Waker) -> Poll<Option<Result<Message<T>>>> {
    Pin::new(each).poll_next(&mut Context::from_waker(waker))
}

// contract: STORE-10
#[tokio::test]
async fn a_stream_parks_no_thread_while_it_waits_and_a_publish_wakes_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let chat = store.channel::<u32>("chat").open().unwrap();
    let mut each = chat.each().unwrap();
    let (woken, waker) = watched();
    assert!(polled(&mut each, &waker).is_pending(), "nothing published: it waits on no thread");

    let publisher = chat.blocking().clone();
    std::thread::spawn(move || publisher.publish(&7).unwrap()).join().unwrap();
    assert!(woken.0.load(Ordering::SeqCst), "a publish on another thread woke it");
    let Poll::Ready(Some(Ok(message))) = polled(&mut each, &waker) else { panic!("the message, at the next poll") };
    assert_eq!(message.value, 7);

    chat.publish(&8).unwrap();
    // spawned, so that the stream is Send, as a program's runtime asks
    let listening = tokio::spawn(async move { next(&mut each).await.map(|message| message.map(|m| m.value)) });
    assert_eq!(listening.await.unwrap().unwrap().unwrap(), 8);
    store.close().await.unwrap();
}

// contract: CHAN-02
#[tokio::test]
async fn a_stream_that_fell_behind_is_told_exactly_how_many_it_missed_before_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let chat = store.channel::<u32>("chat").open().unwrap();
    let missed = Arc::new(Mutex::new(Vec::new()));
    let told = Arc::clone(&missed);
    let mut each = chat.on_missed(move |missed| told.lock().unwrap().push(missed.count)).each().unwrap();
    for n in 0..1030 {
        chat.publish(&n).unwrap();
    }
    let first = next(&mut each).await.unwrap().unwrap();
    assert_eq!(*missed.lock().unwrap(), [6], "told before the next message");
    assert_eq!(first.value, 6, "the log kept the newest 1,024");
    store.close().await.unwrap();
}

// contract: CHAN-04
#[tokio::test]
async fn a_message_that_does_not_read_is_an_err_item_and_the_next_follows() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let names = store.channel::<String>("events").open().unwrap();
    let numbers = store.channel::<u32>("events").open().unwrap();
    let mut each = names.each().unwrap();
    numbers.publish(&42).unwrap();
    names.publish(&"after".to_owned()).unwrap();

    let unread = next(&mut each).await.unwrap().unwrap_err();
    assert_eq!(unread.kind(), ErrorKind::Corrupt, "{unread}");
    assert!(unread.to_string().contains("channel events"), "it names the channel: {unread}");
    assert_eq!(next(&mut each).await.unwrap().unwrap().value, "after");
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_stream_ends_once_its_stopper_stops_it_or_its_store_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let chat = store.channel::<u32>("chat").open().unwrap();
    let mut stopped = chat.each().unwrap();
    let stopper = stopped.stopper();
    let (woken, waker) = watched();
    assert!(polled(&mut stopped, &waker).is_pending());
    std::thread::spawn(move || stopper.stop()).join().unwrap();
    assert!(woken.0.load(Ordering::SeqCst), "the stopper woke it");
    assert!(matches!(polled(&mut stopped, &waker), Poll::Ready(None)), "stopped from another thread, it ended");

    let mut closing = chat.each().unwrap();
    let (woken, waker) = watched();
    assert!(polled(&mut closing, &waker).is_pending());
    store.close().await.unwrap();
    assert!(woken.0.load(Ordering::SeqCst), "the store's close woke it");
    assert!(matches!(polled(&mut closing, &waker), Poll::Ready(None)), "the store closed, it ended");
    let refused = chat.each().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Closed, "{refused}");
}

#[tokio::test]
async fn a_blocking_channel_crosses_to_the_async_api_as_the_same_channel() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let blocking = store.blocking().channel::<u32>("chat").open().unwrap();
    let chat = Channel::from(blocking.clone());
    let mut awaited = chat.under("room-7").each().unwrap();
    let mut beside = chat.blocking().under("room-7").each().unwrap();
    drop(chat.under("room-7").each().unwrap()); // a stream dropped stops listening, and nothing else
    blocking.under("room-7").publish(&1).unwrap();
    assert_eq!(next(&mut awaited).await.unwrap().unwrap().value, 1, "published through the blocking handle");
    assert_eq!(beside.next().unwrap().unwrap().value, 1);
    store.close().await.unwrap();
}

// contract: CHAN-03
#[cfg(feature = "kv")]
#[tokio::test]
async fn a_message_published_in_an_awaited_transaction_is_heard_after_its_commit_never_after_a_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let events = store.channel::<String>("orders").open().unwrap();
    let orders = store.blocking().bucket::<String>("orders").open().unwrap();
    let mut each = events.each().unwrap();

    let (publishing, writing) = (events.clone(), orders.clone());
    store
        .tx(move |tx| -> Result<()> {
            tx.with(&writing).set("o-1", &"placed".to_owned())?;
            tx.with(&publishing).publish(&"o-1 placed".to_owned())
        })
        .await
        .unwrap();
    let (publishing, writing) = (events.clone(), orders.clone());
    let refused = store
        .tx(move |tx| -> Result<()> {
            tx.with(&writing).set("o-2", &"placed".to_owned())?;
            tx.with(&publishing).publish(&"o-2 placed".to_owned())?;
            Err(crate::Error::invalid("the order was refused"))
        })
        .await;
    assert!(refused.is_err());
    events.publish(&"after".to_owned()).unwrap();

    assert_eq!(next(&mut each).await.unwrap().unwrap().value, "o-1 placed");
    assert_eq!(next(&mut each).await.unwrap().unwrap().value, "after", "the rolled back message never came");
    assert_eq!(orders.get("o-1").unwrap().as_deref(), Some("placed"));
    let waited = tokio::time::timeout(std::time::Duration::from_millis(20), next(&mut each)).await;
    assert!(waited.is_err(), "nothing else");
    store.close().await.unwrap();
}
