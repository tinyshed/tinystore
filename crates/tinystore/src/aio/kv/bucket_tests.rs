use std::pin::pin;
use std::task::Waker;

use futures_core::Stream;

use super::*;
use crate::kv::fixture::{Fixture, MINUTE, SECOND, Session, eventually, fixture, session};
use crate::{CancellationToken, Clock, ErrorKind};

/// The aio store of a fixture's store: the same store.
fn awaited(f: &Fixture) -> Store {
    Store::from(f.store.clone())
}

/// Polls `future` once, as an executor first does, with a waker that wakes
/// nothing.
fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

/// The next item of `stream`, awaited.
async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

async fn collected<V: Value>(mut all: All<V>) -> Vec<Result<Entry<V>>> {
    let mut entries = Vec::new();
    while let Some(entry) = next(&mut all).await {
        entries.push(entry);
    }
    entries
}

/// An error as a test compares it: its kind and what it says.
fn said<T: fmt::Debug>(outcome: Result<T>) -> (ErrorKind, String) {
    let error = outcome.unwrap_err();
    (error.kind(), error.to_string())
}

#[tokio::test]
async fn each_call_answers_as_its_blocking_twin_on_the_same_bucket() {
    let f = fixture();
    let store = awaited(&f);
    let sessions = store.bucket::<Session>("sessions").open().unwrap();
    let blocking = sessions.blocking();

    sessions.set("token", &session(42)).await.unwrap();
    assert_eq!(blocking.get("token").unwrap(), Some(session(42)));
    assert_eq!(sessions.get("token").await.unwrap(), blocking.get("token").unwrap());
    assert_eq!(sessions.entry("token").await.unwrap(), blocking.entry("token").unwrap());
    assert!(sessions.has("token").await.unwrap());
    assert!(!sessions.create("token", &session(7)).await.unwrap(), "a key that is there is false");
    assert!(sessions.create("other", &session(7)).await.unwrap());
    assert_eq!(sessions.take("other").await.unwrap(), Some(session(7)));
    assert_eq!(blocking.get("other").unwrap(), None, "taken in one write");
    assert_eq!(sessions.take("other").await.unwrap(), None, "a second take finds nothing");

    assert!(sessions.expire("token", MINUTE).await.unwrap());
    let expires = blocking.entry("token").unwrap().unwrap().expires_at;
    assert_eq!(expires, Some(f.clock.now() + MINUTE));
    assert!(sessions.delete("token").await.unwrap());
    assert!(!sessions.delete("token").await.unwrap());
    assert!(!sessions.expire("token", MINUTE).await.unwrap(), "no key to expire");

    let user = sessions.under(42);
    user.set("phone", &session(42)).await.unwrap();
    user.set("laptop", &session(42)).await.unwrap();
    assert_eq!(user.list(1, None).await.unwrap(), blocking.under(42).list(1, None).unwrap());
    assert_eq!(user.list(10, Some("laptop")).await.unwrap(), blocking.under(42).list(10, Some("laptop")).unwrap());
    user.clear().await.unwrap();
    assert!(blocking.under(42).list(10, None).unwrap().entries.is_empty(), "cleared at once");

    let seen = store.bucket::<()>("stripe-events").open().unwrap();
    assert!(seen.add("evt_1").await.unwrap());
    assert!(!seen.add("evt_1").await.unwrap(), "an event already handled");
    f.store.close().unwrap();
}

#[tokio::test]
async fn a_calls_refusal_is_its_blocking_twins_error() {
    let f = fixture();
    let store = awaited(&f);
    let sessions = store.bucket::<String>("sessions").open().unwrap();
    let blocking = sessions.blocking();

    let large = "x".repeat(2 << 20);
    assert_eq!(said(sessions.set("token", &large).await), said(blocking.set("token", &large)));
    assert_eq!(said(sessions.get("").await), said(blocking.get("")), "a key of no text");
    let take = sessions.key("token").ttl(MINUTE).take().await;
    assert_eq!(said(take), said(blocking.key("token").ttl(MINUTE).take()));
    let delete = sessions.key("token").ttl(MINUTE).delete().await;
    assert_eq!(said(delete), said(blocking.key("token").ttl(MINUTE).delete()));
    assert_eq!(said(sessions.key("token").expire().await), said(blocking.key("token").expire()));
    f.store.close().unwrap();
}

// contract: KV-05
#[tokio::test]
async fn a_key_call_writes_at_its_version_and_with_its_expiry_as_the_blocking_one() {
    let f = fixture();
    let store = awaited(&f);
    let settings = store.bucket::<String>("settings").open().unwrap();
    settings.set("home", &"light".to_owned()).await.unwrap();
    let seen = settings.entry("home").await.unwrap().unwrap();

    settings.key("home").if_version(seen.version).set(&"dark".to_owned()).await.unwrap();
    let stale = settings.key("home").if_version(seen.version).set(&"blue".to_owned()).await;
    let error = stale.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    assert!(error.to_string().contains("kv bucket settings"), "the error names its bucket: {error}");
    assert_eq!(settings.blocking().get("home").unwrap().as_deref(), Some("dark"));

    let until = f.clock.now() + MINUTE;
    assert!(settings.key("code").ttl(MINUTE).create(&"481-205".to_owned()).await.unwrap());
    assert_eq!(settings.blocking().entry("code").unwrap().unwrap().expires_at, Some(until));
    assert!(settings.key("code").expires_at(until + MINUTE).expire().await.unwrap());
    assert_eq!(settings.key("code").take().await.unwrap().as_deref(), Some("481-205"));
    let now = settings.entry("home").await.unwrap().unwrap();
    assert!(settings.key("home").if_version(now.version).delete().await.unwrap());
    f.store.close().unwrap();
}

#[tokio::test]
async fn all_streams_every_key_of_a_branch_a_page_at_a_time_as_the_blocking_iterator() {
    let f = fixture();
    let store = awaited(&f);
    let targets = store.bucket::<i64>("targets").open().unwrap();
    for i in 0..2500 {
        targets.blocking().under("eu").set(format!("{i:05}"), &i).unwrap();
    }
    targets.blocking().under("us").set("00001", &-1).unwrap();
    let streamed: Vec<Entry<i64>> =
        collected(targets.under("eu").all()).await.into_iter().collect::<Result<_>>().unwrap();
    let iterated: Vec<Entry<i64>> = targets.blocking().under("eu").all().collect::<Result<_>>().unwrap();
    assert_eq!(streamed.len(), 2500);
    assert_eq!(streamed, iterated);
    f.store.close().unwrap();
}

#[tokio::test]
async fn a_stream_ends_with_closed_once_its_store_closes() {
    let f = fixture();
    let store = awaited(&f);
    let targets = store.bucket::<i64>("targets").open().unwrap();
    for i in 0..1500 {
        targets.blocking().set(format!("{i:05}"), &i).unwrap();
    }
    let mut all = targets.all();
    assert_eq!(next(&mut all).await.unwrap().unwrap().value, 0, "the first page read");
    store.close().await.unwrap();
    let mut read = 1;
    let closed = loop {
        match next(&mut all).await.expect("the stream ends after its error, not before") {
            Ok(_) => read += 1,
            Err(error) => break error,
        }
    };
    assert_eq!(read, PAGE_KEYS, "the page read before the close, and no other");
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    assert!(next(&mut all).await.is_none(), "it ends after its error");
}

// contract: STORE-09
#[tokio::test]
async fn a_token_around_the_call_that_made_a_stream_stops_its_page_reads() {
    let f = fixture();
    let store = awaited(&f);
    let targets = store.bucket::<i64>("targets").open().unwrap();
    targets.set("a", &1).await.unwrap();
    let token = CancellationToken::new();
    let mut all = token.run(|| targets.all());
    token.cancel();
    let cancelled = next(&mut all).await.unwrap().unwrap_err();
    assert_eq!(cancelled.kind(), ErrorKind::Cancelled, "{cancelled}");
    assert!(next(&mut all).await.is_none());
    assert_eq!(collected(targets.all()).await.len(), 1, "a stream made outside the token reads on");
    f.store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_key_read_answers_in_its_first_poll_when_a_reader_is_idle() {
    let f = fixture();
    let store = awaited(&f);
    let sessions = store.bucket::<Session>("sessions").open().unwrap();
    sessions.blocking().set("token", &session(42)).unwrap();
    assert!(sessions.blocking().has("token").unwrap(), "a read leaves its reader idle");
    let _writer = f.hold_writer();
    let mut read = pin!(sessions.get("token"));
    assert!(matches!(poll_once(read.as_mut()), Poll::Ready(Ok(Some(_)))), "answered in the poll, writer held or not");
    let mut entry = pin!(sessions.entry("token"));
    assert!(matches!(poll_once(entry.as_mut()), Poll::Ready(Ok(Some(_)))));
    let mut has = pin!(sessions.has("token"));
    assert!(matches!(poll_once(has.as_mut()), Poll::Ready(Ok(true))));
    drop(_writer);
    f.store.close().unwrap();
}

// contract: KV-07, STORE-10
#[test]
fn an_idle_key_read_in_its_last_minute_is_renewed_on_the_pool_and_never_in_the_poll() {
    let f = fixture();
    let store = awaited(&f);
    let term = 30 * MINUTE;
    let sessions = store.bucket::<Session>("sessions").idle(term).open().unwrap();
    sessions.blocking().set("token", &session(42)).unwrap();
    f.clock.advance(term - 30 * SECOND);
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    {
        let writer = f.hold_writer();
        let mut read = pin!(sessions.get("token"));
        assert!(poll_once(read.as_mut()).is_pending(), "the renewal waits for the writer elsewhere");
        drop(writer);
        assert_eq!(runtime.block_on(read.as_mut()).unwrap(), Some(session(42)));
    }
    let renewed = sessions.blocking().entry("token").unwrap().unwrap().expires_at;
    assert_eq!(renewed, Some(f.clock.now() + term), "renewed before the read answered");
    f.store.close().unwrap();
}

// contract: DUR-04, DUR-11, STORE-10
#[test]
fn a_write_polled_once_queues_without_waiting_and_commits_though_its_future_is_dropped() {
    let f = fixture();
    let store = awaited(&f);
    let sessions = store.bucket::<Session>("sessions").open().unwrap();
    sessions.blocking().set("before", &session(1)).unwrap();
    {
        let _writer = f.hold_writer();
        let mut set = pin!(sessions.set("token", &session(42)));
        assert!(poll_once(set.as_mut()).is_pending(), "the poll queued the write and waited for no writer");
        let mut take = pin!(sessions.take("before"));
        assert!(poll_once(take.as_mut()).is_pending());
    }
    eventually("the dropped set committed", || sessions.blocking().get("token").unwrap().is_some());
    eventually("the dropped take committed", || sessions.blocking().get("before").unwrap().is_none());
    f.store.close().unwrap();
}

// contract: DUR-04, DUR-11
#[tokio::test]
async fn a_write_whose_future_is_never_polled_writes_nothing() {
    let f = fixture();
    let store = awaited(&f);
    let sessions = store.bucket::<Session>("sessions").open().unwrap();
    drop(sessions.set("never", &session(1)));
    drop(sessions.clear());
    sessions.set("after", &session(2)).await.unwrap();
    assert_eq!(sessions.get("never").await.unwrap(), None);
    assert_eq!(sessions.get("after").await.unwrap(), Some(session(2)));
    f.store.close().unwrap();
}

#[test]
fn writes_joined_in_one_task_share_commits() {
    let f = fixture();
    let store = awaited(&f);
    let sessions = store.bucket::<i64>("sessions").open().unwrap();
    let before = f.commits();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let values: Vec<i64> = (0..20).collect();
    let writes: Vec<_> = values.iter().map(|i| sessions.set(format!("token-{i}"), i)).collect();
    {
        // each queues in its first poll, the writer held, so that one commit can take them all
        let _writer = f.hold_writer();
        let mut joined = pin!(join(writes));
        assert!(poll_once(joined.as_mut()).is_pending());
        drop(_writer);
        runtime.block_on(joined).unwrap();
    }
    let commits = f.commits() - before;
    assert!(commits < 20, "20 writes took {commits} commits");
    assert_eq!(sessions.blocking().list(100, None).unwrap().entries.len(), 20);
    f.store.close().unwrap();
}

/// Polls every future in turn until all of them answered, as `join_all` does.
async fn join<F: Future<Output = Result<()>>>(writes: Vec<F>) -> Result<()> {
    let mut writes: Vec<_> = writes.into_iter().map(|write| Some(Box::pin(write))).collect();
    std::future::poll_fn(|context| {
        let mut pending = false;
        for slot in &mut writes {
            if let Some(write) = slot {
                match write.as_mut().poll(context) {
                    Poll::Ready(outcome) => {
                        outcome?;
                        *slot = None;
                    }
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending { Poll::Pending } else { Poll::Ready(Ok(())) }
    })
    .await
}

#[tokio::test]
async fn a_bucket_crosses_to_the_blocking_api_and_back_as_the_same_bucket() {
    let f = fixture();
    let blocking = f.store.bucket::<String>("sessions").open().unwrap();
    let sessions = Bucket::from(blocking.clone());
    sessions.set("42", &"awaited".to_owned()).await.unwrap();
    assert_eq!(blocking.get("42").unwrap().as_deref(), Some("awaited"));
    blocking.set("42", &"blocking".to_owned()).unwrap();
    assert_eq!(sessions.get("42").await.unwrap().as_deref(), Some("blocking"));
    assert_eq!(sessions.blocking().get("42").unwrap().as_deref(), Some("blocking"));
    tokio::spawn(sessions.set("43", &"spawned".to_owned())).await.unwrap().unwrap();
    assert!(blocking.has("43").unwrap(), "a call's future is Send and 'static");
    f.store.close().unwrap();
}

#[tokio::test]
async fn a_bucket_whose_store_is_gone_answers_closed() {
    let f = fixture();
    let blocking = f.store.bucket::<String>("sessions").open().unwrap();
    f.store.close().unwrap();
    drop(f);
    let sessions = Bucket::from(blocking);
    assert_eq!(sessions.set("42", &"late".to_owned()).await.unwrap_err().kind(), ErrorKind::Closed);
    assert_eq!(sessions.get("42").await.unwrap_err().kind(), ErrorKind::Closed);
    assert_eq!(sessions.list(10, None).await.unwrap_err().kind(), ErrorKind::Closed);
    let mut all = sessions.all();
    assert_eq!(next(&mut all).await.unwrap().unwrap_err().kind(), ErrorKind::Closed);
    assert!(next(&mut all).await.is_none());
}

#[tokio::test]
async fn a_transaction_takes_an_awaited_bucket_and_counters_through_with() {
    let f = fixture();
    let store = awaited(&f);
    let stock = store.bucket::<i64>("stock").open().unwrap();
    let orders = store.counters("orders").open().unwrap();
    stock.set("sku-1", &1).await.unwrap();

    let (taking, counting) = (stock.clone(), orders.clone());
    let place = move |tx: &crate::Tx<'_>| -> Result<bool> {
        let left = tx.with(&taking).get("sku-1")?.unwrap_or(0);
        if left < 1 {
            return Ok(false);
        }
        tx.with(&taking).set("sku-1", &(left - 1))?;
        tx.with(&counting).add("sku-1", 1)?;
        Ok(true)
    };
    let again = place.clone();
    assert!(store.tx(place).await.unwrap());
    assert!(!store.tx(again).await.unwrap(), "sold out: nothing written");
    assert_eq!(stock.get("sku-1").await.unwrap(), Some(0));
    assert_eq!(orders.get("sku-1").await.unwrap(), 1);

    let views = store.counters("views").flush_every(SECOND).open().unwrap();
    let refused = store.tx(move |tx| tx.with(&views).add("/notes", 1)).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "counters kept in memory join no transaction: {refused}");
    f.store.close().unwrap();
}

// contract: DUR-07
#[test]
fn an_awaited_call_inside_a_transaction_of_its_file_is_refused_rather_than_wait_for_itself() {
    let f = fixture();
    let store = awaited(&f);
    let stock = store.bucket::<i64>("stock").open().unwrap();
    stock.blocking().set("sku-1", &1).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let blocking = stock.blocking().clone();
    let refused = f.store.tx(|tx| -> Result<Vec<ErrorKind>> {
        tx.with(&blocking).set("sku-1", &2)?;
        let set = runtime.block_on(stock.set("sku-1", &3)).unwrap_err().kind();
        let page = runtime.block_on(stock.list(10, None)).unwrap_err().kind();
        Ok(vec![set, page])
    });
    assert_eq!(refused.unwrap(), [ErrorKind::Invalid, ErrorKind::Invalid]);
    assert_eq!(stock.blocking().get("sku-1").unwrap(), Some(2), "the transaction around them committed");
    f.store.close().unwrap();
}

#[tokio::test]
async fn an_awaited_reseal_answers_how_many_values_it_sealed_again() {
    let f = fixture();
    let store = awaited(&f);
    let passwords = store.bucket::<String>("passwords").encrypted().open().unwrap();
    passwords.set("db", &"hunter2".to_owned()).await.unwrap();
    assert_eq!(passwords.reseal().await.unwrap(), 0, "the sealing key sealed it");
    let plain = store.bucket::<String>("notes").open().unwrap();
    assert_eq!(said(plain.reseal().await).0, ErrorKind::Invalid);
    f.store.close().unwrap();
}
