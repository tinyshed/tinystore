use std::pin::pin;
use std::sync::{Barrier, mpsc};
use std::task::Waker;
use std::time::{Instant, UNIX_EPOCH};

use serde_json::json;

use super::*;
use crate::records::fixture::{Fixture, SECOND, fixture, names};
use crate::sqlite::execute_cached;
use crate::{Options, TestClock};

const HOUR: Duration = Duration::from_secs(3600);

/// A store's records through the async API, beside the blocking store of the
/// same fixture.
fn awaited(fixture: &Fixture) -> Store {
    Store::from(fixture.store.clone())
}

/// The next item of a stream, as `StreamExt::next` gives it.
async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

/// Says that `future` can be handed to `tokio::spawn`.
fn spawnable<F: Future + Send + 'static>(future: F) -> F {
    future
}

/// Polls `future` once, on no executor, and says whether it is still pending.
fn polled_once<F: Future>(future: Pin<&mut F>) -> bool {
    future.poll(&mut Context::from_waker(Waker::noop())).is_pending()
}

/// Waits up to ten seconds for `holds`.
fn eventually(what: &str, holds: impl Fn() -> bool) {
    let started = Instant::now();
    while !holds() {
        assert!(started.elapsed() < Duration::from_secs(10), "{what} never held");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Requests of three routes over the last minute, one a second.
fn requests(fixture: &Fixture) -> Vec<NewEvent> {
    let start = fixture.now() - SECOND * 60;
    let route = |n: u32| if n.is_multiple_of(3) { "/login" } else { "/notes" };
    let request =
        |n: u32| NewEvent::new("request").at(start + SECOND * n).fields(&json!({ "route": route(n), "ms": n }));
    (0..30).map(request).collect()
}

#[tokio::test]
async fn awaited_appends_and_reads_answer_as_the_blocking_calls_do_on_the_same_store() {
    let fixture = fixture();
    let store = awaited(&fixture);
    let api = store.events("api").keep(Duration::from_hours(24 * 30)).open().unwrap();
    spawnable(api.append("signed.in", &json!({ "user": 42, "route": "/login" }))).await.unwrap();
    let slow = api.event("request").at(fixture.now() - SECOND * 90).level(Level::WARN).message("slow request");
    let context = json!({ "request": "r-1" });
    slow.context(&context).append(&json!({ "route": "/notes", "ms": 1200 })).await.unwrap();
    api.append_all(requests(&fixture)).await.unwrap();

    let (list, blocking) = (api.list().since(HOUR), api.blocking().list().since(HOUR));
    assert_eq!(spawnable(list.page()).await.unwrap(), blocking.page().unwrap());
    assert_eq!(
        list.clone().newest().limit(5).page().await.unwrap(),
        blocking.clone().newest().limit(5).page().unwrap()
    );
    assert_eq!(spawnable(list.count()).await.unwrap(), 32);
    assert_eq!(list.clone().min_level(Level::WARN).count().await.unwrap(), 1);
    assert_eq!(spawnable(list.top("route")).await.unwrap(), blocking.top("route").unwrap());
    let per_minute = Aggregate::new().every(Duration::from_mins(1)).by(["route"]).stats(["ms"]).distinct(["ms"]);
    assert_eq!(spawnable(list.aggregate(&per_minute)).await.unwrap(), blocking.aggregate(&per_minute).unwrap());
    assert_eq!(list.field_names().await.unwrap(), blocking.field_names().unwrap());
    assert_eq!(list.field_names_prefix("r").await.unwrap(), ["request", "route"]);
    let filtered = list.clone().filter("route", &"/login").names(["request"]);
    assert_eq!(
        filtered.count().await.unwrap(),
        blocking.clone().filter("route", &"/login").names(["request"]).count().unwrap()
    );

    let mut walked = Vec::new();
    let mut all = list.clone().limit(7).all();
    while let Some(event) = next(&mut all).await {
        walked.push(event.unwrap());
    }
    let expected: Vec<Event> = blocking.clone().limit(7).all().map(Result::unwrap).collect();
    assert_eq!((walked.len(), &walked), (32, &expected), "all walks every page as the iterator does");
    assert!(next(&mut all).await.is_none(), "and stays ended");
}

#[tokio::test]
async fn awaited_polls_and_the_streams_answer_as_the_blocking_calls_do() {
    let fixture = fixture();
    let store = awaited(&fixture);
    let api = store.events("api").open().unwrap();
    api.append_all(requests(&fixture)).await.unwrap();
    store.events("audit").open().unwrap().append("role.changed", &json!({ "user": 42 })).await.unwrap();
    fixture.pass(HOUR + SECOND);
    crate::records::maintain(&fixture.store).unwrap();

    let (all, blocking) = (store.all_events().unwrap(), fixture.store.all_events().unwrap());
    assert_eq!(spawnable(all.streams()).await.unwrap(), blocking.streams().unwrap());
    let first = spawnable(all.poll(None)).await.unwrap();
    assert_eq!(first, blocking.poll(None).unwrap());
    let cursor = Some(first.cursor.as_str());
    assert_eq!(all.poll_limit(cursor, 5).await.unwrap(), blocking.poll_limit(cursor, 5).unwrap());
    assert_eq!(spawnable(all.damaged()).await.unwrap(), Vec::new());
    let refused = all.poll_limit(None, 0).await.unwrap_err();
    assert_eq!(refused.to_string(), blocking.poll_limit(None, 0).unwrap_err().to_string());
}

#[tokio::test]
async fn an_awaited_append_refuses_what_the_blocking_one_refuses_naming_the_same() {
    let fixture = fixture();
    let store = awaited(&fixture);
    let audit = store.events("audit").open().unwrap();
    let old = fixture.now() - Duration::from_hours(24 * 30);
    let refused = audit.event("role.changed").at(old).append(&()).await.unwrap_err();
    let blocking = audit.blocking().event("role.changed").at(old).append(&()).unwrap_err();
    assert_eq!((refused.kind(), refused.to_string()), (ErrorKind::TooOld, blocking.to_string()));

    let batch = || [NewEvent::new("signup"), NewEvent::new("")];
    let refused = audit.append_all(batch()).await.unwrap_err();
    assert_eq!(refused.to_string(), audit.blocking().append_all(batch()).unwrap_err().to_string());
    assert!(refused.to_string().contains("event 1"), "{refused}");
    assert!(audit.list().page().await.unwrap().events.is_empty(), "all of them or none");
}

// contract: DUR-04
// contract: DUR-11
#[test]
fn a_polled_append_commits_though_its_future_is_dropped_and_one_never_polled_writes_nothing() {
    let fixture = fixture();
    let store = awaited(&fixture);
    let audit = store.events("audit").open().unwrap();
    drop(audit.append("never.polled", &()));
    {
        let appending = pin!(audit.append("polled.once", &()));
        polled_once(appending);
    }
    let found = || names(&audit.blocking().list().page().unwrap().events).join(",");
    eventually("the polled append committed", || found() == "polled.once");
    audit.blocking().append("after", &()).unwrap();
    assert_eq!(found(), "polled.once,after", "nothing else was written meanwhile");
}

// contract: STORE-10
// contract: DUR-11
#[test]
fn an_append_waits_for_the_stores_memory_on_no_thread_and_once_polled_commits_though_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let options = Options { clock: Some(clock), memory: Some(1 << 20), background: false, ..Options::default() };
    let blocking = crate::Store::open(dir.path(), options).unwrap();
    let store = Store::from(blocking.clone());
    let audit = store.events("audit").open().unwrap();
    let held = blocking.memory().try_reserve(1 << 20, "a test's hold of all of it").unwrap();
    {
        let appending = pin!(audit.append("waited", &json!({ "user": 42 })));
        let started = Instant::now();
        assert!(polled_once(appending), "it waits for the memory");
        assert!(started.elapsed() < Duration::from_secs(1), "and its poll did not wait for it");
    }
    assert!(audit.blocking().list().page().unwrap().events.is_empty(), "nothing is written while it waits");
    drop(held);
    let found = || names(&audit.blocking().list().page().unwrap().events).join(",");
    eventually("the append that waited committed", || found() == "waited");
    eventually("the append gave its memory back", || blocking.memory().used() == 0);
    blocking.close().unwrap();
}

/// Polls `future` once and drops it, and says how long the poll took.
fn poll_once_and_drop<F: Future>(future: F) -> Duration {
    let future = pin!(future);
    let started = Instant::now();
    polled_once(future);
    started.elapsed()
}

// contract: STORE-10
// contract: DUR-11
#[test]
fn an_append_compresses_on_the_pool_while_another_holds_the_codec_or_past_a_row_and_commits_though_dropped() {
    let fixture = fixture();
    let store = awaited(&fixture);
    let audit = store.events("audit").open().unwrap();
    // 200 events of 2 KiB each, past the 256 KiB of a head row a poll compresses at most
    let note = "x".repeat(2048);
    let export = |part: u32| NewEvent::new("export").fields(&json!({ "part": part, "note": note }));
    let heavy: Vec<NewEvent> = (0..200).map(export).collect();
    // A logger's flush compresses up to 4 MiB with the codec every append
    // shares: a poll that waited for it would wait here for five seconds.
    let (release, holder) = fixture.hold_codec();
    let waited = poll_once_and_drop(audit.append("signed.in", &json!({ "user": 42 })));
    assert!(waited < Duration::from_secs(1), "its poll waited {waited:?} for the codec");
    let waited = poll_once_and_drop(audit.append_all(heavy));
    assert!(waited < Duration::from_secs(1), "its poll waited {waited:?} for the codec");
    assert_eq!(audit.blocking().list().count().unwrap(), 0, "nothing is written while the codec is held");
    drop(release);
    holder.join().unwrap();
    eventually("both appends committed", || audit.blocking().list().count().unwrap() == 201);
}

// contract: STORE-05
// contract: DUR-11
#[test]
fn an_append_commits_while_every_thread_of_the_pool_waits_for_the_memory_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options { memory: Some(1 << 20), background: false, ..Options::default() };
    let blocking = crate::Store::open(dir.path(), options).unwrap();
    let store = Store::from(blocking.clone());
    let audit = store.events("audit").open().unwrap();
    // Every thread of the pool runs a read that, once let go, waits for all
    // of the store's memory, as a records read whose page needs it does.
    let (pool, memory, most) = (Pool::of(&blocking).unwrap(), Arc::clone(blocking.memory()), crate::pool::MOST);
    let (go, (running, ran), (ended, read)) = (Arc::new(Barrier::new(most + 1)), mpsc::channel(), mpsc::channel());
    for _ in 0..most {
        let (go, running, ended, memory) = (Arc::clone(&go), running.clone(), ended.clone(), Arc::clone(&memory));
        pool.run(Box::new(move || {
            running.send(()).unwrap();
            go.wait();
            let reserved = memory.reserve(1 << 20, "records: a read", crate::memory::PATIENCE);
            ended.send(reserved.map(drop)).unwrap();
        }));
    }
    for _ in 0..most {
        ran.recv_timeout(Duration::from_secs(10)).expect("every thread of the pool runs a read");
    }
    // the append takes its bytes in its poll, and its commit finds every thread busy
    let started = Instant::now();
    poll_once_and_drop(audit.append("signed.in", &json!({ "user": 42 })));
    go.wait();
    for _ in 0..most {
        let reserved = read.recv_timeout(Duration::from_secs(20)).expect("a read never ended");
        reserved.expect("a read waited its patience out for the memory the append held");
    }
    let waited = started.elapsed();
    assert!(waited < Duration::from_secs(5), "the reads waited {waited:?} for the append's commit");
    assert_eq!(names(&audit.blocking().list().page().unwrap().events), ["signed.in"]);
    blocking.close().unwrap();
}

/// Holds every reader of records.db until the barrier it returns is waited
/// on, so that a read waits in line for one.
fn hold_every_reader(fixture: &Fixture) -> (Arc<Barrier>, Vec<std::thread::JoinHandle<()>>) {
    let records = fixture.records();
    let readers = records.file.readers_at_most();
    let (entered, release) = (Arc::new(Barrier::new(readers + 1)), Arc::new(Barrier::new(readers + 1)));
    let holders = (0..readers)
        .map(|_| {
            let (records, entered, release) = (Arc::clone(&records), Arc::clone(&entered), Arc::clone(&release));
            std::thread::spawn(move || {
                let held = records.file.read(|_| {
                    entered.wait();
                    release.wait();
                    Ok(())
                });
                held.unwrap();
            })
        })
        .collect();
    entered.wait();
    (release, holders)
}

// contract: STORE-09
#[test]
fn a_read_or_a_walk_whose_future_is_dropped_leaves_the_line_for_a_reader_at_once() {
    let fixture = fixture();
    let store = awaited(&fixture);
    let api = store.events("api").open().unwrap();
    api.blocking().append_all(requests(&fixture)).unwrap();
    let records = fixture.records();
    let (release, holders) = hold_every_reader(&fixture);
    // the reader's patience is ten seconds: a read its drop did not stop would wait them out
    {
        let counting = pin!(api.list().count());
        assert!(polled_once(counting));
        eventually("the count waits for a reader", || records.file.readers_waiting() == 1);
    }
    eventually("the dropped count left the line", || records.file.readers_waiting() == 0);
    {
        let mut walking = api.list().all();
        assert!(Pin::new(&mut walking).poll_next(&mut Context::from_waker(Waker::noop())).is_pending());
        eventually("the walk waits for a reader", || records.file.readers_waiting() == 1);
    }
    eventually("the dropped walk left the line", || records.file.readers_waiting() == 0);
    release.wait();
    holders.into_iter().for_each(|holder| holder.join().unwrap());
}

// contract: REC-08
#[tokio::test]
async fn a_tail_wakes_for_what_is_appended_as_the_blocking_tail_gives_it_and_ends_when_the_store_closes() {
    let fixture = fixture();
    let store = awaited(&fixture);
    let all = store.all_events().unwrap();
    let mut tail = all.list().streams(["api"]).min_level(Level::WARN).tail().unwrap();
    let mut blocking = all.blocking().list().streams(["api"]).min_level(Level::WARN).tail().unwrap();
    // a task waits for the next batch before anything is appended: the append wakes it
    let waiting = tokio::spawn(async move {
        let batch = next(&mut tail).await;
        (tail, batch)
    });
    tokio::task::yield_now().await;
    let api = store.events("api").open().unwrap();
    api.event("slow").level(Level::WARN).append(&json!({ "ms": 1200 })).await.unwrap();
    api.event("fine").level(Level::INFO).append(&()).await.unwrap();
    let (mut tail, batch) = within_ten_seconds("the append woke the tail", waiting).await.unwrap();
    let batch = batch.expect("a batch, not the tail's end").unwrap();
    assert_eq!(names(&batch.events), ["slow"]);
    assert_eq!(Some(batch), blocking.next_within(Duration::ZERO).unwrap());

    let ending = tokio::spawn(async move { next(&mut tail).await.is_none() });
    tokio::task::yield_now().await;
    drop(blocking);
    store.close().await.unwrap();
    let ended = within_ten_seconds("the close woke the tail", ending).await.unwrap();
    assert!(ended, "the tail ends when the store closes");
}

// contract: REC-04
// contract: REC-08
#[test]
fn a_tail_gives_events_of_one_time_in_a_reads_order_when_a_blocking_and_an_awaited_append_commit_together() {
    // An awaited append is answered inside its commit, and a blocking one's
    // thread wakes after that: handed to the tails at their answers, the
    // awaited event came first, where every read gives the blocking one first.
    let fixture = fixture();
    let store = awaited(&fixture);
    let (api, web) = (fixture.events("api"), store.events("web").open().unwrap());
    let mut tail = fixture.store.all_events().unwrap().list().tail().unwrap();
    let (at, records) = (fixture.now(), fixture.records());
    let writer = records.file.hold_writer();
    let blocking = std::thread::spawn(move || api.event("blocking").at(at).append(&()));
    eventually("the blocking append queued", || records.file.writes_waiting() == 1);
    let mut appending = pin!(web.event("awaited").at(at).append(&()));
    assert!(polled_once(appending.as_mut()), "it queues behind the blocking one");
    assert_eq!(records.file.writes_waiting(), 2, "both wait for one commit");
    drop(writer);
    blocking.join().unwrap().unwrap();
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(appending).unwrap();

    let mut tailed = Vec::new();
    while tailed.len() < 2 {
        let batch = tail.next_within(SECOND).unwrap().expect("a batch of what was appended");
        tailed.extend(batch.events.into_iter().map(|event| event.name));
    }
    let read = fixture.store.all_events().unwrap().list().page().unwrap();
    assert_eq!(names(&read.events), ["blocking", "awaited"], "the order their commit wrote them");
    assert_eq!(tailed, ["blocking", "awaited"], "and the tail's");
}

/// What `future` gives, or a failure naming what never happened.
async fn within_ten_seconds<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), future).await.unwrap_or_else(|_| panic!("{what}: never"))
}

#[tokio::test]
async fn lines_written_and_flushed_become_the_events_the_blocking_writer_makes() {
    let fixture = fixture();
    let store = awaited(&fixture);
    let output = concat!(
        "2026-09-26 12:00:01,500 ERROR request failed\n",
        "Traceback (most recent call last):\n",
        "  File \"app.py\", line 3, in handle\n",
        "ValueError: bad input\n",
        "{\"level\":30,\"msg\":\"ok\"}\n",
        "level=warn msg=\"slow request\" ms=1200\n",
    );
    let mut out = store.events("worker").open().unwrap().lines().unwrap();
    for piece in output.as_bytes().chunks(7) {
        out.write(piece).unwrap();
    }
    spawnable(out.flush()).await.unwrap();
    let mut blocking = fixture.store.events("blocking").open().unwrap().lines().unwrap();
    std::io::Write::write_all(&mut blocking, output.as_bytes()).unwrap();
    std::io::Write::flush(&mut blocking).unwrap();

    let events = |stream: &str| {
        let page = fixture.store.events(stream).open().unwrap().list().page().unwrap();
        page.events.into_iter().map(|event| Event { stream: String::new(), ..event }).collect::<Vec<_>>()
    };
    assert_eq!(names(&events("worker")), ["log", "json", "logfmt"]);
    assert_eq!(events("worker"), events("blocking"));
    assert_eq!(out.dropped(), 0);
}

#[tokio::test]
async fn what_no_longer_reads_is_listed_and_dropped_as_the_blocking_calls_do() {
    let fixture = fixture();
    let store = awaited(&fixture);
    let audit = store.events("audit").open().unwrap();
    audit.append("signup", &()).await.unwrap();
    let zero = "update heads set body = zeroblob(length(body))";
    fixture.records().file.write(0, move |tx| execute_cached(tx, zero, [], "zero the bodies").map(drop)).unwrap();
    let failed = audit.list().page().await.unwrap_err();
    assert_eq!(
        (failed.kind(), failed.to_string()),
        (ErrorKind::Corrupt, audit.blocking().list().page().unwrap_err().to_string())
    );

    let all = store.all_events().unwrap();
    let damaged = all.damaged().await.unwrap();
    assert_eq!(damaged, all.blocking().damaged().unwrap());
    let [damage] = damaged.as_slice() else { panic!("{damaged:?}") };
    spawnable(all.drop(damage)).await.unwrap();
    assert!(audit.list().page().await.unwrap().events.is_empty());
    assert!(all.damaged().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_handle_crosses_to_and_from_the_blocking_api_and_answers_closed_once_its_store_closed() {
    let fixture = fixture();
    let blocking = fixture.store.events("audit").open().unwrap();
    let audit = Events::from(blocking.clone());
    audit.append("crossed", &()).await.unwrap();
    assert_eq!(names(&blocking.list().page().unwrap().events), ["crossed"]);
    assert_eq!(names(&audit.blocking().list().page().unwrap().events), ["crossed"]);
    let all = AllEvents::from(fixture.store.all_events().unwrap());
    assert_eq!(all.list().count().await.unwrap(), all.blocking().list().count().unwrap());

    fixture.store.close().unwrap();
    let closed = audit.append("after", &()).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let closed = all.list().count().await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let closed = Events::from(blocking).append("later", &()).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
}
