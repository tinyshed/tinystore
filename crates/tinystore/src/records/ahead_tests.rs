use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use super::super::engine::execute;
use super::super::fixture::{SECOND, fixture};
use crate::records::{Event, Events, List, NewEvent, maintain};
use crate::{Options, Store, TestClock};

/// The events of a read, as many as `wanted`, its pages read one after
/// another as `page` gives them.
fn one_after_another(list: &List, wanted: usize) -> Vec<Event> {
    let (mut events, mut next) = (Vec::new(), None);
    while events.len() < wanted {
        let page = next.map_or_else(|| list.clone(), |next: String| list.clone().after(&next)).page().unwrap();
        events.extend(page.events);
        let Some(after) = page.next else { break };
        next = Some(after);
    }
    events.truncate(wanted);
    events
}

// contract: REC-04
#[test]
fn ranges_read_at_once_find_each_event_once_in_order_as_pages_read_one_after_another() {
    let fixture = fixture();
    let api = fixture.events("api");
    let start = fixture.now() - SECOND * 600;
    // ten events a second, one in 97 a needle: packed into blocks, then more waiting in heads
    let event = |n: u32| {
        let text = if n.is_multiple_of(97) { format!("a Needle, {n}") } else { format!("hay {n}") };
        NewEvent::new("log").message(&text).at(start + Duration::from_millis(u64::from(n) * 100))
    };
    api.append_all((0..4000).map(event)).unwrap();
    fixture.pass(Duration::from_secs(3601));
    maintain(&fixture.store).unwrap();
    for part in 0..4 {
        api.append_all((4000 + part * 100..4100 + part * 100).map(event)).unwrap();
    }
    assert!(fixture.rows("blocks") >= 4 && fixture.rows("heads") == 4);
    for newest in [false, true] {
        for (search, limit) in [(Some("needle"), 1), (Some("needle"), 4), (Some("needle"), 1000), (None, 300)] {
            let list = api.list().limit(limit);
            let list = if newest { list.newest() } else { list };
            let list = search.map_or(list.clone(), |text| list.search(text));
            let expected = one_after_another(&list, usize::MAX);
            assert_eq!(expected.len(), if search.is_some() { 46 } else { 4400 });
            for wide in 2..=5 {
                let (page, _) = list.read_ahead(wide).unwrap();
                let rest =
                    page.next.map_or_else(Vec::new, |next| one_after_another(&list.clone().after(&next), usize::MAX));
                assert_eq!(
                    [page.events, rest].concat(),
                    expected,
                    "{wide} at once, a page of {limit}, newest {newest}"
                );
            }
            let walked: Vec<Event> = list.clone().all().map(Result::unwrap).collect();
            assert_eq!(walked, expected, "all, a page of {limit}, newest {newest}");
        }
    }
    assert_eq!(api.list().search("NEEDLE").count().unwrap(), 46);
}

// contract: DUR-08
#[test]
fn a_read_ahead_finds_each_event_once_while_heads_pack_and_segments_merge() {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let fixture = fixture();
    let api = fixture.events("api");
    let start = fixture.now() - Duration::from_secs(24 * 3600);
    // a round is fifty events a second apart, one of them a needle
    let round = |n: u64| {
        let at = move |place: u64| start + Duration::from_secs(n * 50 + place);
        let text = |place: u64| if place == 7 { "a needle" } else { "hay" };
        (0..50).map(move |place| NewEvent::new(&format!("{n}-{place}")).message(text(place)).at(at(place)))
    };
    for n in 0..20 {
        api.append_all(round(n)).unwrap();
    }
    let (rounds, reads, done) = (AtomicUsize::new(20), AtomicUsize::new(0), AtomicBool::new(false));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                let before = rounds.load(Ordering::Acquire);
                let needles = api.list().search("needle").limit(100).all();
                let walked: Vec<String> = needles.map(|event| event.unwrap().name).collect();
                let names: BTreeSet<&str> = walked.iter().map(String::as_str).collect();
                assert_eq!(names.len(), walked.len(), "a needle read twice: {walked:?}");
                assert!(walked.len() >= before, "{} needles of the {before} rounds before the read", walked.len());
                reads.fetch_add(1, Ordering::Release);
            }
        });
        let mut merged = 0;
        for n in 20..40 {
            // a read between each round, unless the reader stopped
            let (read, waited) = (reads.load(Ordering::Acquire), std::time::Instant::now());
            while reads.load(Ordering::Acquire) == read && waited.elapsed() < Duration::from_secs(2) {
                std::thread::yield_now();
            }
            api.append_all(round(n)).unwrap();
            rounds.store(n as usize + 1, Ordering::Release);
            fixture.pass(Duration::from_secs(3601));
            merged += maintain(&fixture.store).unwrap().merged;
        }
        done.store(true, Ordering::Release);
        assert!(merged > 0, "segments merged meanwhile");
    });
    assert_eq!(api.list().search("needle").count().unwrap(), 40);
}

#[test]
fn a_block_that_no_longer_reads_fails_a_read_ahead_after_the_events_before_it_as_pages_one_after_another() {
    let fixture = fixture();
    let api = fixture.events("api");
    let start = fixture.now() - SECOND * 600;
    let event = |n: u64| {
        let text = if n % 500 == 7 { "a needle" } else { "hay" };
        NewEvent::new("log").message(text).at(start + Duration::from_millis(n * 50))
    };
    api.append_all((0..8000).map(event)).unwrap();
    fixture.pass(Duration::from_secs(3601));
    maintain(&fixture.store).unwrap();
    // the fifth block of eight, zeroed as a disk that lost it would leave it
    let zero = "update blocks set body = zeroblob(length(body))
        where id = (select id from blocks order by first_at limit 1 offset 4)";
    fixture.records().file.write(0, move |tx| execute(tx, zero, [], "zero a block").map(drop)).unwrap();
    let list = api.list().search("needle");
    let (mut paged, mut next) = (Vec::new(), None);
    let failed = loop {
        let asked = next.as_deref().map_or_else(|| list.clone(), |after| list.clone().after(after));
        match asked.page() {
            Ok(page) => {
                paged.extend(page.events);
                next = Some(page.next.expect("the pages go past the block that no longer reads"));
            }
            Err(error) => break error.kind(),
        }
    };
    let (mut walked, mut read_ahead) = (list.all(), Vec::new());
    let failed_ahead = loop {
        match walked.next().expect("all goes past the block that no longer reads") {
            Ok(event) => read_ahead.push(event),
            Err(error) => break error.kind(),
        }
    };
    assert_eq!((paged.len(), failed), (9, crate::ErrorKind::Corrupt));
    assert_eq!((read_ahead, failed_ahead), (paged, failed), "the events before it come first");
    assert!(walked.next().is_none(), "and the read ends at it");
}

#[test]
fn ranges_read_at_once_wait_for_the_stores_memory_when_it_holds_one_page_of_them() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    // a count's page holds ten blocks, ten thousand events, and reserves 10 MiB of the 16
    let options =
        Options { clock: Some(clock.clone()), memory: Some(16 << 20), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    let api = store.events("api").open().unwrap();
    let start = store.now() - Duration::from_secs(600);
    let event = |n: u64| {
        let text = if n % 9_000 == 4_500 { "a needle" } else { "hay" };
        NewEvent::new("log").message(text).at(start + Duration::from_millis(n))
    };
    for thousands in 0..60 {
        api.append_all((thousands * 1000..(thousands + 1) * 1000).map(event)).unwrap();
    }
    clock.advance(Duration::from_secs(3601));
    maintain(&store).unwrap();
    assert_eq!(api.list().search("needle").count().unwrap(), 7);
    assert_eq!(store.memory().used(), 0);
}

// contract: STORE-09
#[test]
fn the_threads_that_read_ahead_read_under_the_cancellation_tokens_of_their_call() {
    use std::sync::atomic::Ordering;

    let fixture = fixture();
    let api = fixture.events("api");
    // a head row a second, each a page of one event: a range each
    for n in 0..12 {
        api.event("log").at(fixture.now() - SECOND * (12 - n)).append(&serde_json::json!({ "n": n })).unwrap();
    }
    let list = api.list().limit(1);
    let before = super::RANGES_UNDER_TOKENS.load(Ordering::Relaxed);
    let token = crate::CancellationToken::new();
    let (page, _) = token.run(|| list.read_ahead(4)).unwrap();
    assert!((1..=4).contains(&page.events.len()), "the ranges handed out are taken back: {page:?}");
    // without the tokens, a cancel would stop them only once every range handed out was read
    assert!(super::RANGES_UNDER_TOKENS.load(Ordering::Relaxed) > before, "a thread read a range outside the tokens");

    let cancelled = crate::CancellationToken::new();
    cancelled.cancel();
    let refused = cancelled.run(|| list.read_ahead(4)).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Cancelled);
}

/// The kinds of HDFS's lines, each after how many in a hundred lines are of
/// it: `{block}` is a block's id, `{from}` and `{to}` hosts, `{n}` a number.
const HDFS_LINES: [(u64, &str); 9] = [
    (20, "INFO dfs.DataNode$DataXceiver: Receiving block blk_{block} src: /{from}:54106 dest: /{to}:50010"),
    (20, "INFO dfs.DataNode$PacketResponder: PacketResponder {n} for block blk_{block} terminating"),
    (20, "INFO dfs.DataNode$PacketResponder: Received block blk_{block} of size {n} from /{from}"),
    (
        20,
        "INFO dfs.FSNamesystem: BLOCK* NameSystem.addStoredBlock: blockMap updated: \
         {to}:50010 is added to blk_{block} size {n}",
    ),
    (
        7,
        "INFO dfs.FSNamesystem: BLOCK* NameSystem.allocateBlock: \
         /user/root/rand/_temporary/_task_200811092030_0001_m_{n}_0/part-{n}. blk_{block}",
    ),
    (4, "INFO dfs.DataBlockScanner: Verification succeeded for blk_{block}"),
    (4, "INFO dfs.FSDataset: Deleting block blk_{block} file /mnt/hadoop/dfs/data/current/subdir{n}/blk_{block}"),
    (4, "INFO dfs.FSNamesystem: BLOCK* NameSystem.delete: blk_{block} is added to invalidSet of {to}:50010"),
    (1, "WARN dfs.DataNode$DataXceiver: {to}:50010:Got exception while serving blk_{block} to /{from}:"),
];

/// Lines like HDFS's, each naming a block by its id, a dozen lines an id.
struct Hdfs {
    state: u64,
    /// The ids lines are written about now, each with how many named it.
    live: Vec<(i64, u32)>,
}

impl Hdfs {
    fn new() -> Hdfs {
        let mut hdfs = Hdfs { state: 0x2545_f491_4f6c_dd1d, live: Vec::new() };
        hdfs.live = (0..256).map(|_| (hdfs.random() as i64, 0)).collect();
        hdfs
    }

    /// splitmix64, so that every run writes the same lines.
    fn random(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.random() % bound
    }

    fn ip(&mut self) -> String {
        format!("10.25{}.{}.{}", self.below(2), self.below(256), self.below(256))
    }

    /// The line written `second` seconds into the corpus, and the id it names.
    fn line(&mut self, second: u64) -> (String, i64) {
        let slot = self.below(self.live.len() as u64) as usize;
        let (block, named) = self.live[slot];
        self.live[slot] = if named == 11 { (self.random() as i64, 0) } else { (block, named + 1) };
        // HDFS's lines begin at 20:30 on 2008-11-09
        let clock = 20 * 3600 + 30 * 60 + second;
        let (day, time) = (9 + clock / 86_400, clock % 86_400);
        let (hours, minutes, seconds) = (time / 3600, time / 60 % 60, time % 60);
        let head = format!("0811{day:02} {hours:02}{minutes:02}{seconds:02} {}", self.below(300));
        let (mut kind, mut text) = (self.below(100), HDFS_LINES[0].1);
        for (share, line) in HDFS_LINES {
            if kind < share {
                text = line;
                break;
            }
            kind -= share;
        }
        let text = text.replace("{block}", &block.to_string()).replace("{n}", &self.below(67_108_864).to_string());
        let text = text.replace("{from}", &self.ip()).replace("{to}", &self.ip());
        (format!("{head} {text}"), block)
    }
}

/// A store of `lines` lines like HDFS's over two days, packed, and a block id
/// a dozen of them name: written into `dir`, or found there from an earlier
/// run of as many lines.
fn hdfs_store(dir: &std::path::Path, lines: u64) -> (Store, Events, String) {
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    let store = Store::open(dir, options).unwrap();
    let api = store.events("hdfs").open().unwrap();
    let written = !api.list().limit(10_000).page().unwrap().events.is_empty();
    let (span, mut hdfs, mut needle) = (2 * 86_400, Hdfs::new(), None);
    let start = store.now() - Duration::from_secs(span + 3600);
    let mut batch = Vec::with_capacity(4096);
    for n in 0..lines {
        let second = n * span / lines;
        let (line, block) = hdfs.line(second);
        if n == lines / 2 {
            needle = Some(format!("blk_{block}"));
        }
        if !written {
            batch.push(NewEvent::new("log").at(start + Duration::from_secs(second)).message(&line));
            if batch.len() == batch.capacity() {
                api.append_all(batch.drain(..)).unwrap();
            }
        }
    }
    if !written {
        api.append_all(batch).unwrap();
        clock.advance(Duration::from_secs(2 * 3600));
        while maintain(&store).unwrap().packed > 0 {}
    }
    (store, api, needle.unwrap())
}

/// What a search of ten million lines like HDFS's, two days of them, takes
/// read a page after another, as `all` and the counts read before they read
/// ahead, and read ahead, in turns: a block's id a dozen lines name counted,
/// the first hundred lines that name it, and a word a fifth of them hold
/// counted. Run by hand with `--release -- --ignored --nocapture`;
/// `TINYSTORE_SEARCH_DIR` keeps the store there for the next run, and
/// `TINYSTORE_SEARCH_LINES` says how many lines it holds.
#[test]
#[ignore = "a timing, run by hand"]
fn a_search_of_ten_million_lines_a_page_after_another_and_read_ahead() {
    const PASSES: usize = 7;
    let temporary = tempfile::tempdir().unwrap();
    let kept = std::env::var_os("TINYSTORE_SEARCH_DIR").map(std::path::PathBuf::from);
    let lines = std::env::var("TINYSTORE_SEARCH_LINES").map_or(10_000_000, |lines| lines.parse().unwrap());
    let (_store, api, needle) = hdfs_store(kept.as_deref().unwrap_or(temporary.path()), lines);
    let (rare, common) = (api.list().search(&needle), api.list().search("receiving"));
    let first_lines = rare.clone().limit(100);
    let listed: Vec<Event> = first_lines.clone().all().take(100).map(Result::unwrap).collect();
    assert_eq!(listed, one_after_another(&first_lines, 100));
    type Read = Box<dyn Fn() -> usize>;
    let count = |list: List| move || list.count().unwrap() as usize;
    let walked = |list: List, wanted: usize| move || one_after_another(&list, wanted).len();
    let listed = |list: List| move || list.clone().all().take(100).count();
    let reads: [(&str, Read, Read); 3] = [
        ("a rare word counted", Box::new(walked(rare.clone().limit(10_000), usize::MAX)), Box::new(count(rare))),
        ("its first 100 lines", Box::new(walked(first_lines.clone(), 100)), Box::new(listed(first_lines))),
        ("a common word counted", Box::new(walked(common.clone().limit(10_000), usize::MAX)), Box::new(count(common))),
    ];
    println!("{lines} lines, {} threads a read, the median of {PASSES} passes in turns", api.records.threads());
    let timed = |read: &dyn Fn() -> usize| {
        let started = std::time::Instant::now();
        let found = read();
        (started.elapsed().as_secs_f64() * 1000.0, found)
    };
    for (name, before, now) in &reads {
        let (mut old, mut new, mut found) = (Vec::new(), Vec::new(), 0);
        for _ in 0..PASSES {
            let ((old_ms, found_before), (new_ms, found_now)) = (timed(before), timed(now));
            assert_eq!(found_before, found_now, "{name}");
            old.push(old_ms);
            new.push(new_ms);
            found = found_now;
        }
        old.sort_by(f64::total_cmp);
        new.sort_by(f64::total_cmp);
        let spread =
            |passes: &[f64]| format!("{:>6.0} ms ({:.0} to {:.0})", passes[PASSES / 2], passes[0], passes[PASSES - 1]);
        let faster = old[PASSES / 2] / new[PASSES / 2];
        println!(
            "{name:<22} found {found:>8}  page after page {}  read ahead {}  {faster:.1}x",
            spread(&old),
            spread(&new)
        );
    }
}
