//! What an ingest holds of the heap against what it reserves of the store's
//! memory: a process of its own, so that its allocator counts this test's
//! bytes alone.
#![cfg(feature = "metrics")]
#![allow(unsafe_code, reason = "an allocator that counts what the system's hands out")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use tinystore::metrics::Series;
use tinystore::{Options, Store};

/// The system's allocator, counting the bytes it holds and the most it held.
struct Counting;

static HELD: AtomicU64 = AtomicU64::new(0);
static MOST: AtomicU64 = AtomicU64::new(0);

// SAFETY: every call is the system allocator's, with the caller's own
// arguments; the counters beside it only add and take away sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let held = HELD.fetch_add(layout.size() as u64, Ordering::Relaxed) + layout.size() as u64;
        MOST.fetch_max(held, Ordering::Relaxed);
        // SAFETY: the layout is the caller's, as `alloc`'s contract asks.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HELD.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: the pointer came from `alloc` above with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The counters are the process's: one test measures at a time.
static MEASURING: Mutex<()> = Mutex::new(());

/// The most heap `call` held past what was held before it, and the most of
/// the store's memory it reserved meanwhile, read by a thread beside it.
fn measured(store: &Store, call: impl FnOnce()) -> (u64, u64) {
    let memory = Arc::clone(store.memory());
    let (done, reserved) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0)));
    let reserved_before = memory.used();
    let watching = {
        let (done, reserved) = (Arc::clone(&done), Arc::clone(&reserved));
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                reserved.fetch_max(memory.used(), Ordering::SeqCst);
                std::hint::spin_loop();
            }
        })
    };
    let before = HELD.load(Ordering::SeqCst);
    MOST.store(before, Ordering::SeqCst);
    call();
    let most = MOST.load(Ordering::SeqCst);
    done.store(true, Ordering::SeqCst);
    watching.join().unwrap();
    (most - before, reserved.load(Ordering::SeqCst).saturating_sub(reserved_before))
}

fn text(lines: usize, line: impl Fn(usize) -> String) -> String {
    (0..lines).map(line).collect()
}

/// A histogram's family as an exporter serves it, described.
fn histogram(family: usize) -> String {
    let mut lines = format!(
        "# HELP rpc_{family}_seconds How long a call of method {family} took, from its first byte to its last.\n\
         # UNIT rpc_{family}_seconds seconds\n# TYPE rpc_{family}_seconds histogram\n"
    );
    for (count, le) in ["0.005", "0.01", "0.05", "0.1", "0.5", "1", "5", "10", "+Inf"].iter().enumerate() {
        lines += &format!("rpc_{family}_seconds_bucket{{code=\"200\",le=\"{le}\"}} {count}\n");
    }
    lines + &format!("rpc_{family}_seconds_sum{{code=\"200\"}} 4.2\nrpc_{family}_seconds_count{{code=\"200\"}} 9\n")
}

/// Texts of each shape an exporter serves, small enough for a debug build.
fn texts() -> Vec<(&'static str, String)> {
    let ten_labels = |line: usize| {
        let labels: Vec<String> = (0..10).map(|label| format!("label_{label}=\"value-{label}-{line:08}\"")).collect();
        format!("requests_total{{{}}} {line}\n", labels.join(","))
    };
    vec![
        ("one name, a label a line", text(20_000, |line| format!("up{{shard=\"{line}\"}} 1\n"))),
        ("ten labels a line", text(3_000, ten_labels)),
        ("a name a line", text(10_000, |line| format!("gauge_{line} 1\n"))),
        ("a described name a line", text(5_000, |line| format!("# HELP load_{line} Busy.\nload_{line} 1\n"))),
        ("histograms", text(300, histogram)),
        ("quoted names", text(5_000, |line| format!("{{\"http.server.{line}\", \"url.scheme\"=\"https\"}} 1\n"))),
    ]
}

/// A store whose memory is bounded, so that it counts what is reserved.
fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let options = Options { background: false, memory: Some(8 << 30), ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    (dir, store)
}

// contract: STORE-05
#[test]
fn an_exposition_reserves_the_heap_its_reading_and_its_write_hold() {
    let _alone = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    for (shape, text) in texts() {
        let (_dir, store) = store();
        let metrics = store.metrics().unwrap();
        for scrape in ["first", "second"] {
            let web = metrics.prometheus().label("instance", "web-1");
            let (heap, reserved) = measured(&store, || web.ingest(&text).unwrap());
            println!("{shape}, {scrape} scrape: heap {heap}, reserved {reserved}");
            assert!(heap <= reserved, "{shape}, {scrape} scrape: held {heap} bytes, reserved {reserved}");
        }
    }
}

// contract: STORE-05
#[test]
fn an_ingest_reserves_the_heap_its_write_holds() {
    let _alone = MEASURING.lock().unwrap_or_else(PoisonError::into_inner);
    let (_dir, store) = store();
    let metrics = store.metrics().unwrap();
    let series: Vec<Series> = (0..10_000)
        .map(|line| Series::gauge(format!("disk_{}", line % 100)).label("host", format!("web-{line}")))
        .map(|series| series.sample(SystemTime::now(), 1.0))
        .collect();
    for write in ["first", "second"] {
        let (heap, reserved) = measured(&store, || metrics.ingest(&series).unwrap());
        println!("series, {write} write: heap {heap}, reserved {reserved}");
        assert!(heap <= reserved, "series, {write} write: held {heap} bytes, reserved {reserved}");
    }
}
