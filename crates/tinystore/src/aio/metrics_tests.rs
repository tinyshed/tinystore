use std::pin::{Pin, pin};
use std::sync::{Arc, Barrier, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use futures_core::Stream;

use super::*;
use crate::ErrorKind;
use crate::aio::Store;
use crate::metrics::fixture::{Fixture, HOUR, MINUTE, bits, fixture};

/// The fixture's store with its calls awaited, and its metrics.
fn awaited(f: &Fixture) -> (Store, Metrics) {
    let store = Store::from(f.store.clone());
    let stats = store.metrics().unwrap();
    (store, stats)
}

/// A series as its name, labels and samples' bits.
type Seen = (String, Labels, Vec<(i64, u64)>);

/// Each series of a read as it was seen.
fn seen(read: &[Series]) -> Vec<Seen> {
    read.iter().map(|series| (series.name().to_owned(), series.labels().clone(), bits(series))).collect()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap()
}

/// A store of a megabyte of memory, which a test holds to make calls wait
/// for it, its calls awaited, and its metrics.
fn budgeted() -> (tempfile::TempDir, Store, Metrics) {
    let dir = tempfile::tempdir().unwrap();
    let options = crate::Options { background: false, memory: Some(1 << 20), ..crate::Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    let stats = store.metrics().unwrap();
    (dir, store, stats)
}

fn wait_until(holds: impl Fn() -> bool, call: &str, what: &str) {
    within(Duration::from_secs(10), holds, call, what);
}

fn within(span: Duration, holds: impl Fn() -> bool, call: &str, what: &str) {
    let deadline = Instant::now() + span;
    while !holds() {
        assert!(Instant::now() < deadline, "{call}: it never {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn every_awaited_call_answers_as_its_blocking_twin_on_the_same_store() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    let now = f.now();
    let cpu = |host: &str, value: f64| {
        Series::gauge("cpu").label("host", host).sample(now - 2 * MINUTE, value).sample(now - MINUTE, value + 1.0)
    };
    let odd = Series::gauge("odd").sample(now - MINUTE, -0.0).sample(now, f64::from_bits(0x7ff8_0000_0000_beef));
    stats.ingest(&[cpu("web-1", 0.25), cpu("web-2", 0.5), odd]).await.unwrap();
    let busy = Description { unit: "%".into(), help: "Busy time of a core.".into() };
    stats.describe("cpu", busy.clone()).await.unwrap();

    let blocking = stats.blocking();
    let (query, twin) = (|| stats.query().name("cpu").since(HOUR), || blocking.query().name("cpu").since(HOUR));
    assert_eq!(seen(&query().read().await.unwrap().series), seen(&twin().read().unwrap().series));
    let odd = stats.query().name("odd").since(HOUR).read().await.unwrap().series;
    assert_eq!(seen(&odd), seen(&blocking.query().name("odd").since(HOUR).read().unwrap().series), "bit for bit");
    let every = || query().every(5 * MINUTE);
    assert_eq!(
        every().aggregate(Op::Avg).await.unwrap().groups,
        twin().every(5 * MINUTE).aggregate(Op::Avg).unwrap().groups
    );
    assert_eq!(query().latest().await.unwrap(), twin().latest().unwrap());
    assert_eq!(every().explain(Op::Avg).await.unwrap(), twin().every(5 * MINUTE).explain(Op::Avg).unwrap());
    assert_eq!(query().explain(None).await.unwrap(), twin().explain(None).unwrap());

    assert_eq!(stats.names().page().await.unwrap(), blocking.names().page().unwrap());
    assert_eq!(stats.label_names("cpu").page().await.unwrap(), blocking.label_names("cpu").page().unwrap());
    let hosts = stats.label_values("cpu", "host").prefix("web-").limit(1).page().await.unwrap();
    assert_eq!(hosts, blocking.label_values("cpu", "host").prefix("web-").limit(1).page().unwrap());
    assert_eq!(stats.description("cpu").await.unwrap(), busy);
    assert_eq!(stats.description("cpu").await.unwrap(), blocking.description("cpu").unwrap());
    assert_eq!(stats.damaged().await.unwrap(), blocking.damaged().unwrap());

    assert!(stats.delete("cpu", [("host", "web-2")]).await.unwrap(), "the store held it");
    assert!(!blocking.delete("cpu", [("host", "web-2")]).unwrap(), "the awaited delete removed it");
    assert!(!stats.delete("cpu", [("host", "web-2")]).await.unwrap());
    f.store.close().unwrap();
}

#[tokio::test]
async fn an_awaited_call_fails_as_its_blocking_twin_does() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    let refused = stats.ingest(&[Series::gauge("tinystore_cpu").sample(f.now(), 1.0)]).await.unwrap_err();
    let twin = stats.blocking().ingest(&[Series::gauge("tinystore_cpu").sample(f.now(), 1.0)]).unwrap_err();
    assert_eq!((refused.kind(), refused.to_string()), (twin.kind(), twin.to_string()));
    let past = stats.ingest(&[Series::gauge("cpu").sample(f.now() + HOUR, 1.0)]).await.unwrap_err();
    assert_eq!(past.kind(), ErrorKind::TooNew, "{past}");
    let unit = Description { unit: "u".repeat(33), help: String::new() };
    assert_eq!(stats.describe("cpu", unit).await.unwrap_err().kind(), ErrorKind::Invalid);
    let aggregate = stats.query().name("cpu").since(HOUR).every(MINUTE).read().await.unwrap_err();
    assert_eq!(aggregate.kind(), ErrorKind::Invalid, "every is an aggregate's: {aggregate}");
    f.store.close().unwrap();
}

#[test]
fn a_query_is_checked_in_its_first_poll_where_since_becomes_its_start() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    stats.blocking().ingest(&[Series::gauge("cpu").sample(f.now(), 1.0)]).unwrap();
    let mut unnamed = pin!(stats.query().since(HOUR).read());
    match unnamed.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(Err(error)) => assert_eq!(error.kind(), ErrorKind::Invalid, "{error}"),
        other => panic!("a query by no name nor label answers in its first poll, without the pool: {other:?}"),
    }
    // Made now and first polled an hour later, its half hour is the one
    // before its poll, which the sample is older than.
    let made = stats.query().name("cpu").since(30 * MINUTE).read();
    f.pass(HOUR);
    assert!(runtime().block_on(made).unwrap().series.is_empty());
    assert_eq!(stats.blocking().query().name("cpu").since(2 * HOUR).read().unwrap().series.len(), 1);
    f.store.close().unwrap();
}

/// Holds every reader of the metrics' file until the barrier given back is
/// waited on.
fn every_reader_held(stats: &crate::metrics::Metrics) -> (Arc<Barrier>, Vec<JoinHandle<Result<()>>>) {
    let readers = stats.file().readers_at_most();
    let (hold, release) = (Arc::new(Barrier::new(readers + 1)), Arc::new(Barrier::new(readers + 1)));
    let holders = (0..readers)
        .map(|_| {
            let (stats, hold, release) = (stats.clone(), Arc::clone(&hold), Arc::clone(&release));
            thread::spawn(move || {
                stats.file().read(|_| {
                    hold.wait();
                    release.wait();
                    Ok(())
                })
            })
        })
        .collect();
    hold.wait();
    (release, holders)
}

type Call = Pin<Box<dyn Future<Output = ()> + Send>>;

fn call<T>(future: impl Future<Output = Result<T>> + Send + 'static) -> Call {
    Box::pin(async move {
        let _ = future.await;
    })
}

// contract: STORE-09
#[test]
fn a_read_whose_future_is_dropped_stops_waiting_for_a_reader_and_leaves_its_line() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    stats.blocking().ingest(&[Series::gauge("cpu").label("host", "web-1").sample(f.now(), 1.0)]).unwrap();
    let (release, holders) = every_reader_held(stats.blocking());
    let query = || stats.query().name("cpu").since(HOUR);
    let calls = [
        ("read", call(query().read())),
        ("aggregate", call(query().aggregate(Op::Avg))),
        ("latest", call(query().latest())),
        ("explain", call(query().explain(None))),
        ("a page of names", call(stats.names().page())),
        ("a page of labels", call(stats.label_names("cpu").page())),
        ("a page of values", call(stats.label_values("cpu", "host").page())),
        ("damaged", call(stats.damaged())),
        // a point read with no reader idle goes to the pool, and stops there as any read
        ("description", call(stats.description("cpu"))),
    ];
    let file = stats.blocking().file();
    for (name, mut future) in calls {
        assert!(future.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending(), "{name}");
        wait_until(|| file.readers_waiting() == 1, name, "waited for a reader on the pool");
        drop(future);
        // well within the readers' ten seconds of patience, which a read that went on would wait out
        let stopped = || file.readers_waiting() == 0;
        within(Duration::from_secs(3), stopped, name, "stopped waiting once its future was dropped");
    }
    release.wait();
    for holder in holders {
        holder.join().unwrap().unwrap();
    }
    f.store.close().unwrap();
}

// contract: STORE-09
#[test]
fn a_read_whose_future_is_dropped_while_it_waits_for_memory_leaves_the_line_at_once() {
    let (_dir, store, stats) = budgeted();
    stats.blocking().ingest(&[Series::gauge("cpu").label("host", "web-1").sample(store.now(), 1.0)]).unwrap();
    let memory = store.blocking().memory();
    let held = memory.try_reserve(memory.limit() - memory.used(), "a test's hold").unwrap();
    let query = || stats.query().name("cpu").since(HOUR);
    let calls = [
        ("read", call(query().read())),
        ("aggregate", call(query().aggregate(Op::Avg))),
        ("latest", call(query().latest())),
    ];
    for (name, mut future) in calls {
        assert!(future.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending(), "{name}");
        wait_until(|| memory.waiting() == 1, name, "waited for memory on the pool");
        drop(future);
        // well within the ten seconds of patience, which a read that went on would wait out
        within(Duration::from_secs(3), || memory.waiting() == 0, name, "left the line once its future was dropped");
    }
    assert_eq!(memory.used(), held.bytes(), "no read took a byte");
    drop(held);
    store.blocking().close().unwrap();
}

// contract: STORE-10
// contract: DUR-11
#[test]
fn ingests_that_wait_for_memory_hold_no_thread_of_the_pool_and_commit_once_it_comes_back() {
    let (_dir, store, stats) = budgeted();
    let memory = store.blocking().memory();
    let held = memory.try_reserve(memory.limit(), "a test's hold").unwrap();
    let (at, waiting) = (store.now(), crate::pool::MOST + 4);
    for host in 0..waiting {
        let mut ingest = pin!(stats.ingest(&[Series::gauge("cpu").label("host", host.to_string()).sample(at, 1.0)]));
        assert!(ingest.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
    }
    wait_until(|| memory.waiting() == waiting, "ingests dropped after their first poll", "waited for memory");
    // More ingests wait than the pool has threads, and a read still finds one.
    let page = runtime().block_on(async { tokio::time::timeout(Duration::from_secs(5), stats.names().page()).await });
    assert!(page.expect("a page of names ran on the pool").unwrap().names.is_empty(), "no ingest wrote yet");
    drop(held);
    let blocking = stats.blocking();
    let written = || blocking.query().name("cpu").since(HOUR).read().unwrap().series.len() == waiting;
    wait_until(written, "ingests dropped while they waited for memory", "committed");
    store.blocking().close().unwrap();
}

#[test]
fn the_samples_of_a_text_that_give_no_time_take_the_clock_at_the_first_poll_not_at_the_parse() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    // Every thread of the pool is busy, so the text waits for one to be parsed.
    let threads = crate::pool::MOST;
    let (busy, free) = (Arc::new(Barrier::new(threads + 1)), Arc::new(Barrier::new(threads + 1)));
    for _ in 0..threads {
        let (busy, free) = (Arc::clone(&busy), Arc::clone(&free));
        stats.pool.run(Box::new(move || {
            busy.wait();
            free.wait();
        }));
    }
    busy.wait();
    let polled_at = f.now();
    let mut ingest = pin!(stats.prometheus().ingest("queue_length 3\n"));
    assert!(ingest.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
    f.pass(MINUTE);
    free.wait();
    runtime().block_on(ingest).unwrap();
    let read = stats.blocking().query().name("queue_length").since(HOUR).read().unwrap().series;
    assert_eq!(bits(&read[0]), [(crate::unix_millis(polled_at), 3f64.to_bits())]);
    f.store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_description_answers_in_its_first_poll_where_a_reader_is_idle() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    let busy = Description { unit: "%".into(), help: "Busy time of a core.".into() };
    stats.blocking().describe("cpu", busy.clone()).unwrap();
    stats.blocking().description("cpu").unwrap(); // a read leaves its reader idle
    let mut described = pin!(stats.description("cpu"));
    match described.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(Ok(description)) => assert_eq!(description, busy),
        other => panic!("one row by its key, a reader idle, answers in the poll: {other:?}"),
    }
    f.store.close().unwrap();
}

// contract: DUR-11
#[test]
fn an_ingest_polled_once_commits_though_its_future_is_dropped_and_one_never_polled_writes_nothing() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    let at = f.now();
    drop(stats.ingest(&[Series::gauge("never").sample(at, 1.0)]));
    {
        let mut polled = pin!(stats.ingest(&[Series::gauge("polled").sample(at, 2.0)]));
        let _ = polled.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    }
    let blocking = stats.blocking();
    let written = || !blocking.query().name("polled").since(HOUR).read().unwrap().series.is_empty();
    wait_until(written, "an ingest dropped after its first poll", "committed");
    assert!(blocking.query().name("never").since(HOUR).read().unwrap().series.is_empty());
    let names: Vec<String> = blocking.names().page().unwrap().names.into_iter().map(|info| info.name).collect();
    assert_eq!(names, ["polled"], "the ingest never polled made no name");
    f.store.close().unwrap();
}

// contract: STORE-10
#[tokio::test]
async fn a_flush_awaited_writes_the_totals_on_the_stores_pool_never_on_the_thread_that_polls() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    let read_on = Arc::new(Mutex::new(Vec::new()));
    let noting = Arc::clone(&read_on);
    let queue = stats.gauge_func("queue_length", move || {
        noting.lock().unwrap().push(thread::current().name().map(str::to_owned));
        3.0
    });
    queue.open().unwrap();
    let requests = stats.counter("http_requests_total").open().unwrap();
    requests.with(&[("route", "/notes")]).inc();
    // spawned, so that the future is Send and 'static, as a program's runtime asks
    tokio::spawn(stats.flush()).await.unwrap().unwrap();
    let read_on = read_on.lock().unwrap().clone();
    assert_eq!(read_on, [Some(crate::pool::THREAD.to_owned())], "the write of the totals ran on the pool");
    let read = stats.query().name("http_requests_total").since(HOUR).read().await.unwrap().series;
    assert_eq!(read[0].samples().last().map(|sample| sample.value), Some(1.0));
    f.store.close().unwrap();
}

#[tokio::test]
async fn a_text_ingested_awaited_writes_what_its_blocking_ingest_writes() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    let text = r#"# TYPE http_requests_total counter
http_requests_total{route="/notes"} 1027
# TYPE queue_length gauge
queue_length -0
"#;
    stats.prometheus().label("instance", "awaited").ingest(text).await.unwrap();
    stats.blocking().prometheus().label("instance", "blocking").ingest(text).unwrap();
    for name in ["http_requests_total", "queue_length"] {
        let read = |instance| stats.blocking().query().name(name).filter("instance", instance).since(HOUR).read();
        let (awaited, blocking) = (read("awaited").unwrap().series, read("blocking").unwrap().series);
        assert_eq!(awaited.len(), 1, "{name}");
        assert_eq!((awaited[0].kind(), bits(&awaited[0])), (blocking[0].kind(), bits(&blocking[0])), "{name}");
    }
    let broken = stats.prometheus().ingest("queue_length one").await.unwrap_err();
    assert_eq!(broken.kind(), ErrorKind::Invalid, "{broken}");
    f.store.close().unwrap();
}

#[tokio::test]
async fn a_handle_crosses_from_the_blocking_api_and_fails_closed_once_its_store_closed() {
    let f = fixture();
    let blocking = f.metrics();
    let stats = Metrics::from(blocking.clone());
    stats.ingest(&[Series::gauge("cpu").sample(f.now(), 0.5)]).await.unwrap();
    let read = blocking.query().name("cpu").since(HOUR).read().unwrap().series;
    assert_eq!(seen(&read), seen(&stats.query().name("cpu").since(HOUR).read().await.unwrap().series));

    Store::from(f.store.clone()).close().await.unwrap();
    let closed = stats.ingest(&[Series::gauge("cpu").sample(f.now(), 0.7)]).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let made_after = Metrics::from(blocking.clone());
    let closed = made_after.query().name("cpu").since(HOUR).read().await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let Fixture { store, dir, .. } = f;
    drop(store);
    // no store holds the engine now: a handle made from it has a pool that runs nothing
    let gone = Metrics::from(blocking);
    assert_eq!(gone.flush().await.unwrap_err().kind(), ErrorKind::Closed);
    assert_eq!(gone.description("cpu").await.unwrap_err().kind(), ErrorKind::Closed);
    drop(dir);
}

// contract: MET-17
#[tokio::test]
async fn an_awaited_page_walks_on_as_a_stream_as_its_blocking_twin_walks() {
    let f = fixture();
    let (_store, stats) = awaited(&f);
    let now = f.now();
    let written: Vec<Series> = ["a", "b"]
        .into_iter()
        .map(|host| {
            let mut series = Series::gauge("paged").label("host", host);
            series.extend((0..500u32).map(|index| (now - Duration::from_secs(u64::from(index)), f64::from(index))));
            series
        })
        .collect();
    stats.ingest(&written).await.unwrap();
    let query = || stats.query().name("paged").since(HOUR).limit(300);
    let mut walk = query().read().await.unwrap().all();
    let mut walked = Vec::new();
    while let Some(series) = std::future::poll_fn(|context| Pin::new(&mut walk).poll_next(context)).await {
        walked.push(series.unwrap());
    }
    let blocking: Vec<Series> = stats
        .blocking()
        .query()
        .name("paged")
        .since(HOUR)
        .limit(300)
        .read()
        .unwrap()
        .all()
        .map(Result::unwrap)
        .collect();
    assert_eq!(seen(&walked), seen(&blocking));
    assert_eq!(walked.iter().map(|series| series.samples().len()).sum::<usize>(), 1000);
    let groups = query().every(MINUTE).limit(3).aggregate(Op::Avg).await.unwrap();
    let mut walk = groups.all();
    let mut buckets = 0;
    while let Some(group) = std::future::poll_fn(|context| Pin::new(&mut walk).poll_next(context)).await {
        buckets += group.unwrap().buckets.len();
    }
    let once = stats.query().name("paged").since(HOUR).every(MINUTE).aggregate(Op::Avg).await.unwrap();
    assert_eq!(buckets, once.groups[0].buckets.len());
}
