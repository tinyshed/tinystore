use std::sync::Arc;
use std::time::Instant;

use super::super::fixture::{DAY, SECOND, fixture, names};
use super::*;
use crate::records::NewEvent;
use crate::sqlite::division::Division;
use crate::{Clock, TestClock};

// contract: REC-09
#[test]
fn an_event_older_than_its_stream_keeps_is_never_read_and_maintenance_removes_its_row() {
    let fixture = fixture();
    let api = fixture.store.events("api").keep(DAY).open().unwrap();
    let audit = fixture.store.events("audit").keep(30 * DAY).open().unwrap();
    api.append("request", &()).unwrap();
    audit.append("role.changed", &()).unwrap();
    fixture.pass(DAY + SECOND);
    api.append("fresh", &()).unwrap();

    // gone from every read at once, though its row is still in the file
    assert_eq!(names(&api.list().page().unwrap().events), ["fresh"]);
    let all = fixture.store.all_events().unwrap();
    assert_eq!(names(&all.list().page().unwrap().events), ["role.changed", "fresh"]);
    assert_eq!(fixture.rows("heads"), 3);

    // the same pass packs audit's event, which has waited its hour
    assert_eq!(
        maintain(&fixture.store).unwrap(),
        Maintenance { expired: 1, packed: 1, segments: 1, ..Maintenance::default() }
    );
    assert_eq!((fixture.rows("heads"), fixture.rows("segments")), (1, 1));
    assert_eq!(names(&all.list().page().unwrap().events), ["role.changed", "fresh"]);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance::default());
}

#[test]
fn a_row_goes_once_its_newest_event_is_past_what_its_stream_keeps() {
    let fixture = fixture();
    let api = fixture.store.events("api").keep(DAY).open().unwrap();
    let early = fixture.now() - DAY + SECOND;
    api.append_all([NewEvent::new("early").at(early), NewEvent::new("late")]).unwrap();
    fixture.pass(SECOND * 2);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance::default());
    assert_eq!(names(&api.list().page().unwrap().events), ["late"]);
    fixture.pass(DAY);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance { expired: 2, ..Maintenance::default() });
    assert_eq!(fixture.rows("heads"), 0);
}

#[test]
fn maintenance_with_nothing_to_remove_commits_nothing() {
    let fixture = fixture();
    fixture.events("api").append("request", &()).unwrap();
    let commits = fixture.records().file.commits();
    maintain(&fixture.store).unwrap();
    assert_eq!(fixture.records().file.commits(), commits);
}

/// The records a process killed now would leave: the file and its log
/// copied while the store is open, as a crash leaves them on disk.
fn crash_image(fixture: &super::super::fixture::Fixture) -> tempfile::TempDir {
    let image = tempfile::tempdir().unwrap();
    for name in ["records.db", "records.db-wal"] {
        let from = fixture.dir.path().join(name);
        if from.exists() {
            std::fs::copy(from, image.path().join(name)).unwrap();
        }
    }
    image
}

fn names_after_crash(fixture: &super::super::fixture::Fixture, image: &tempfile::TempDir) -> Vec<String> {
    let options = crate::Options { clock: Some(fixture.clock.clone()), background: false, ..crate::Options::default() };
    let store = Store::open(image.path(), options).unwrap();
    let events = store.all_events().unwrap().list().page().unwrap().events;
    events.into_iter().map(|event| event.name).collect()
}

// contract: DUR-01, REC-03
#[test]
fn an_abrupt_exit_loses_no_appended_event_before_or_after_a_pack() {
    let fixture = fixture();
    let audit = fixture.events("audit");
    audit.append_all([NewEvent::new("a"), NewEvent::new("b")]).unwrap();
    let before = crash_image(&fixture);
    fixture.pass(Duration::from_secs(3601));
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1);
    audit.append("c", &()).unwrap();
    let after = crash_image(&fixture);
    assert_eq!(names_after_crash(&fixture, &before), ["a", "b"]);
    assert_eq!(names_after_crash(&fixture, &after), ["a", "b", "c"], "packed and waiting alike");
}

// contract: DUR-08
#[test]
fn a_reader_finds_each_event_once_while_heads_pack_and_segments_merge() {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let fixture = fixture();
    let api = fixture.events("api");
    let all = fixture.store.all_events().unwrap();
    let (done, reads) = (AtomicBool::new(false), AtomicUsize::new(0));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                let events = all.list().limit(1000).page().unwrap().events;
                let names: BTreeSet<&str> = events.iter().map(|event| event.name.as_str()).collect();
                assert_eq!(names.len(), events.len(), "an event read twice");
                assert_eq!(events.len() % 50, 0, "a round of 50 read in part: {}", events.len());
                reads.fetch_add(1, Ordering::Relaxed);
            }
        });
        let mut merged = 0;
        for round in 0..8 {
            api.append_all((0..50).map(|n| NewEvent::new(&format!("{round}-{n}")))).unwrap();
            fixture.pass(Duration::from_secs(3601));
            merged += maintain(&fixture.store).unwrap().merged;
        }
        done.store(true, Ordering::Release);
        assert!(merged > 0, "segments merged meanwhile");
    });
    assert!(reads.load(Ordering::Relaxed) > 0);
}

#[test]
fn a_blocks_id_is_never_given_twice_though_its_block_left() {
    let fixture = fixture();
    let api = fixture.store.events("api").keep(DAY).open().unwrap();
    api.append("first", &()).unwrap();
    fixture.pass(Duration::from_secs(3601));
    maintain(&fixture.store).unwrap();
    fixture.pass(DAY);
    maintain(&fixture.store).unwrap();
    assert_eq!(fixture.rows("blocks"), 0, "expired");
    api.append("second", &()).unwrap();
    fixture.pass(Duration::from_secs(3601));
    maintain(&fixture.store).unwrap();
    let ids = fixture.records().file.read(|connection| {
        Ok(connection
            .query_row("select min(id), max(id) from blocks", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap())
    });
    assert_eq!(ids.unwrap(), (2_i64, 2_i64));
}

const HOUR: Duration = Duration::from_hours(1);

/// What a stream holds: each event's name, time and message, in order.
fn read_all(events: &crate::records::Events) -> Vec<(String, std::time::SystemTime, Option<String>)> {
    let all = events.list().limit(10_000).all().map(Result::unwrap);
    all.map(|event| (event.name, event.at, event.message)).collect()
}

/// Ten thousand events in twenty appends, packed: the heads' rows they were
/// appended as leave their pages free.
fn packed_stream(fixture: &super::super::fixture::Fixture) -> crate::records::Events {
    let api = fixture.events("api");
    for batch in 0..20 {
        let event = |n| NewEvent::new("request").message(&format!("GET /orders/{batch}/{n} 200 {}ms", n % 97));
        api.append_all((0..500).map(event)).unwrap();
    }
    fixture.pass(HOUR);
    assert_eq!(maintain(&fixture.store).unwrap().packed, 10_000);
    api
}

// contract: DUR-09
#[test]
fn the_pages_its_heads_left_go_back_once_no_append_took_them_for_a_while_and_every_read_stays() {
    let fixture = fixture();
    let api = packed_stream(&fixture);
    let packed = Division::of(&fixture.records().file);
    assert!(packed.free > 20, "{packed:?}");
    let read = read_all(&api);

    // the window the pack's pass began in held no free page; the next holds them all
    fixture.pass(Duration::from_mins(10));
    maintain(&fixture.store).unwrap();
    assert_eq!(Division::of(&fixture.records().file).free, packed.free, "kept for the appends to come");
    fixture.pass(Duration::from_mins(10));
    maintain(&fixture.store).unwrap();
    let after = Division::of(&fixture.records().file);
    assert!(after.free == 0 && after.pages <= packed.pages - packed.free, "{packed:?} {after:?}");
    assert_eq!(read_all(&api), read);
}

/// A store of the measurement below: how its records.db was made and how
/// its passes run, and what they cost.
struct Way {
    name: &'static str,
    _dir: tempfile::TempDir,
    store: Store,
    api: crate::records::Events,
    pass: fn(&Records) -> Result<()>,
    passes: Vec<Duration>,
    appends: Duration,
    written: u64,
}

impl Way {
    fn open(name: &'static str, made_before: bool, pass: fn(&Records) -> Result<()>, clock: &Arc<TestClock>) -> Way {
        let dir = tempfile::tempdir().unwrap();
        if made_before {
            crate::sqlite::division::made_as_before(&dir.path().join("records.db"), 1024);
        }
        let options = crate::Options { clock: Some(clock.clone()), background: false, ..crate::Options::default() };
        let store = Store::open(dir.path(), options).unwrap();
        let api = store.events("api").open().unwrap();
        Way { name, _dir: dir, store, api, pass, passes: Vec::new(), appends: Duration::ZERO, written: 0 }
    }

    fn records(&self) -> Arc<Records> {
        Records::of(&self.store).unwrap()
    }

    fn append(&mut self, events: &[NewEvent]) {
        let (started, written) = (Instant::now(), crate::sqlite::division::written());
        self.api.append_all(events.iter().cloned()).unwrap();
        self.appends += started.elapsed();
        self.written += crate::sqlite::division::written().zip(written).map_or(0, |(now, then)| now - then);
    }

    fn pass(&mut self) {
        let (records, written) = (self.records(), crate::sqlite::division::written());
        let started = Instant::now();
        (self.pass)(&records).unwrap();
        self.passes.push(started.elapsed());
        self.written += crate::sqlite::division::written().zip(written).map_or(0, |(now, then)| now - then);
    }
}

fn pass_of_0_1_0(records: &Records) -> Result<()> {
    records.expire_pack_and_merge().map(drop)
}

fn pass_of_now(records: &Records) -> Result<()> {
    records.maintain().map(drop)
}

/// A pseudo-random sequence, xorshift64*, the same every run.
struct Draws(u64);

impl Draws {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// A second of a web service's access log, `count` events from `at` on.
fn access_log(draws: &mut Draws, at: std::time::SystemTime, count: u64) -> Vec<NewEvent> {
    const ROUTES: [&str; 10] = [
        "/",
        "/login",
        "/api/orders",
        "/api/orders/{}",
        "/api/users/{}",
        "/api/cart",
        "/static/app.js",
        "/api/search",
        "/health",
        "/api/items/{}",
    ];
    const STATUSES: [u64; 10] = [200, 200, 200, 200, 200, 200, 201, 304, 404, 500];
    let event = |n: u64, draws: &mut Draws| {
        let path = ROUTES[draws.below(10) as usize].replace("{}", &draws.below(100_000).to_string());
        let method = if draws.below(5) == 0 { "POST" } else { "GET" };
        let (status, ms, request) = (STATUSES[draws.below(10) as usize], draws.below(400), draws.next());
        NewEvent::new("request")
            .at(at + Duration::from_millis(n * 1000 / count))
            .message(&format!("{method} {path} {status} {ms}ms"))
            .fields(&serde_json::json!({ "method": method, "path": path, "status": status, "ms": ms,
                "request": format!("{request:016x}") }))
    };
    (0..count).map(|n| event(n, draws)).collect()
}

/// What a stream leaves in records.db, three ways on the same events: a file
/// v0.1.0 made under v0.1.0's passes; the same file under this release's,
/// which vacuums it once; a file this release made. A web service's access
/// log, 100 events a second for `TINYSTORE_SECONDS` seconds, 1800 unless it
/// says, a pass each minute, the ways in turns; then the stream stops, its
/// last head packs an hour later, and passes follow a minute apart for half
/// an hour. Run by hand with `--release -- --ignored --nocapture`; on Linux
/// it also says the bytes each way wrote.
#[test]
#[ignore = "a measurement, run by hand"]
fn what_a_stream_leaves_in_its_file() {
    let seconds: u64 = std::env::var("TINYSTORE_SECONDS").ok().and_then(|seconds| seconds.parse().ok()).unwrap_or(1800);
    let clock = Arc::new(TestClock::new(std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let mut ways = [
        Way::open("0.1.0", true, pass_of_0_1_0, &clock),
        Way::open("0.1.0's file, passes now", true, pass_of_now, &clock),
        Way::open("now", false, pass_of_now, &clock),
    ];
    let mut draws = Draws(0x9e37_79b9_7f4a_7c15);
    for second in 0..seconds {
        let events = access_log(&mut draws, clock.now(), 100);
        let turn = second as usize % ways.len();
        ways.rotate_left(turn);
        ways.iter_mut().for_each(|way| way.append(&events));
        clock.set(clock.now() + SECOND).unwrap();
        if (second + 1) % 60 == 0 {
            ways.iter_mut().for_each(Way::pass);
        }
        ways.rotate_right(turn);
    }
    print_ways(&ways, "live, after its last pass");
    let live: Vec<(usize, u64)> = ways.iter().map(|way| (way.passes.len(), way.written)).collect();
    clock.set(clock.now() + HOUR).unwrap();
    for _ in 0..30 {
        ways.iter_mut().for_each(Way::pass);
        clock.set(clock.now() + Duration::from_mins(1)).unwrap();
    }
    print_ways(&ways, "archived, its last head packed and half an hour of passes after");
    let counts: Vec<usize> = ways.iter().map(|way| way.api.list().limit(10_000).all().count()).collect();
    assert!(counts.iter().all(|&count| count as u64 == seconds * 100), "{counts:?}");
    ways.iter().zip(live).for_each(|(way, live)| print_costs(way, live));
}

fn print_ways(ways: &[Way], moment: &str) {
    for way in ways {
        let division = Division::of(&way.records().file);
        division.print(&format!("{} — {moment}", way.name), 10);
    }
}

/// What a way cost: its appends, its passes, the bytes it wrote under the
/// load and after, which `live` says of the load, and its file once closed.
fn print_costs(way: &Way, (passes, written): (usize, u64)) {
    println!(
        "{}: appends {:?} in all; {}",
        way.name,
        way.appends,
        crate::sqlite::division::passes(&way.passes, passes)
    );
    if way.written > 0 {
        println!("  written: {} KiB under the load, {} KiB after", written / 1024, (way.written - written) / 1024);
    }
    way.store.close().unwrap();
    let size = std::fs::metadata(way._dir.path().join("records.db")).unwrap().len();
    println!("  closed: records.db {:.0} KiB", size as f64 / 1024.0);
}
