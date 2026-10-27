use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::metrics::engine::Engine;
use crate::metrics::fixture::{DAY, HOUR, MINUTE, SECOND, bits, fixture};
use crate::metrics::{Series, maintain};
use crate::sqlite::division::Division;
use crate::{Clock, Options, Store, TestClock};

// contract: MET-12
#[test]
fn samples_past_what_their_name_keeps_go_and_the_rest_stay() {
    let f = fixture();
    let start = f.now();
    let series = f.gauge("cpu", 100, HOUR, |index| index as f64).label("host", "a");
    f.metrics().ingest(&[series]).unwrap();
    // 100 hours of samples up to now; 30 days later the oldest go
    f.pass(30 * DAY - 50 * HOUR);
    let done = maintain(&f.store).unwrap();
    assert_eq!((done.expired, done.reclaimed), (49, 0));
    let read = f.metrics().query().name("cpu").read().unwrap().series;
    assert_eq!(read[0].samples().len(), 51);
    assert_eq!(read[0].samples()[0].at, start - 50 * HOUR);
    assert_eq!(maintain(&f.store).unwrap(), Default::default(), "nothing more is due");
}

#[test]
fn a_series_left_with_nothing_goes_with_its_pairs_and_frees_its_place_and_its_names_kind() {
    let f = fixture();
    let now = f.now();
    let gone = Series::gauge("jobs").label("queue", "mail").label("region", "eu").sample(now, 1.0);
    let stays = Series::gauge("disk").label("region", "eu").sample(now + 20 * DAY, 1.0);
    f.metrics().ingest(&[gone]).unwrap();
    f.pass(20 * DAY);
    f.metrics().ingest(&[stays]).unwrap();
    assert_eq!((f.rows("series"), f.rows("label_values")), (2, 4));
    f.pass(11 * DAY);
    let done = maintain(&f.store).unwrap();
    assert_eq!((done.expired, done.reclaimed), (1, 1));
    assert_eq!(f.rows("series"), 1);
    assert_eq!(f.value("select series_count from store_state"), 1);
    // __name__=jobs and queue=mail go; region=eu stays with disk's series
    assert_eq!(f.rows("label_values"), 2);
    assert_eq!(f.value("select posting_count from label_values where name = 'region'"), 1);
    assert_eq!(f.rows("heads"), 1);
    // with no series left, the name may take another kind
    f.metrics().ingest(&[Series::counter("jobs").sample(f.now(), 1.0)]).unwrap();
}

#[test]
fn a_keep_changed_between_runs_moves_when_its_series_are_due() {
    let f = fixture();
    let now = f.now();
    let week = Duration::from_hours(7 * 24);
    f.metrics().ingest(&[Series::gauge("short").sample(now, 1.0).keep(2 * week)]).unwrap();
    f.pass(week + SECOND);
    assert_eq!(maintain(&f.store).unwrap().expired, 0);
    let f = f.reopen();
    f.metrics().ingest(&[Series::gauge("short").label("a", "b").sample(f.now(), 1.0).keep(week)]).unwrap();
    let done = maintain(&f.store).unwrap();
    assert_eq!((done.expired, done.reclaimed), (1, 1), "the first series is past a week now");
}

// contract: DUR-09
#[test]
fn the_pass_after_an_expiry_gives_the_pages_it_left_back_and_every_read_stays() {
    let f = fixture();
    let week = Duration::from_hours(7 * 24);
    let short = |host: u32| f.gauge("short", 2000, 10 * SECOND, |n| (n % 7) as f64).label("host", host).keep(week);
    for hosts in (0..50).collect::<Vec<u32>>().chunks(5) {
        f.metrics().ingest(&hosts.iter().map(|&host| short(host)).collect::<Vec<_>>()).unwrap();
    }
    f.metrics().ingest(&[f.gauge("stays", 2000, 10 * SECOND, |n| n as f64).label("host", "a")]).unwrap();
    maintain(&f.store).unwrap();
    f.pass(week + HOUR);
    assert_eq!(maintain(&f.store).unwrap().expired, 100_000);
    let expired = Division::of(&f.engine().file);
    assert!(expired.free > 5, "{expired:?}");
    let read = f.metrics().query().name("stays").since(8 * DAY).read().unwrap().series;

    // the pages stay free for a window of passes before they go back
    f.pass(10 * MINUTE);
    maintain(&f.store).unwrap();
    assert!(Division::of(&f.engine().file).free > 0, "kept a while for the writes to come");
    f.pass(10 * MINUTE);
    maintain(&f.store).unwrap();
    let after = Division::of(&f.engine().file);
    assert!(after.free == 0 && after.pages <= expired.pages - expired.free, "{expired:?} {after:?}");
    let again = f.metrics().query().name("stays").since(8 * DAY).read().unwrap().series;
    assert_eq!(bits(&again[0]), bits(&read[0]));
    assert_eq!(bits(&read[0]).len(), 2000);
}

fn heads_pages(f: &crate::metrics::fixture::Fixture) -> u64 {
    let division = Division::of(&f.engine().file);
    division.trees.iter().find(|tree| tree.name == "heads").map_or(0, |tree| tree.pages)
}

#[test]
fn heads_a_bulk_load_grew_leave_their_pages_once_packed() {
    let f = fixture();
    // samples written after the fact, before any pass: each head holds its series' two thousand
    let series = |host: u32| f.gauge("cpu", 2000, 10 * SECOND, |n| ((n * 7919 + host as usize) % 101) as f64);
    for hosts in (0..45).collect::<Vec<u32>>().chunks(5) {
        f.metrics().ingest(&hosts.iter().map(|&host| series(host).label("host", host)).collect::<Vec<_>>()).unwrap();
    }
    let grown = heads_pages(&f);
    let read = f.metrics().query().name("cpu").since(DAY).read().unwrap().series;
    maintain(&f.store).unwrap();
    let packed = heads_pages(&f);
    assert!(packed * 4 < grown, "the heads' pages: {grown} grown, {packed} packed");
    let again = f.metrics().query().name("cpu").since(DAY).read().unwrap().series;
    assert_eq!(again.iter().map(bits).collect::<Vec<_>>(), read.iter().map(bits).collect::<Vec<_>>());
}

/// A store of the measurement below: how its metrics.db was made and how
/// its passes run, and what they cost.
struct Way {
    name: &'static str,
    store: Store,
    pass: fn(&Store) -> crate::Result<()>,
    passes: Vec<Duration>,
    ingests: Duration,
    written: u64,
    // Last: fields drop in order, and Windows removes no file the store still holds.
    dir: tempfile::TempDir,
}

impl Way {
    fn open(
        name: &'static str,
        made_before: bool,
        pass: fn(&Store) -> crate::Result<()>,
        clock: &Arc<TestClock>,
    ) -> Way {
        let dir = tempfile::tempdir().unwrap();
        if made_before {
            crate::sqlite::division::made_as_before(&dir.path().join("metrics.db"), 4096);
        }
        let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
        let store = Store::open(dir.path(), options).unwrap();
        Way { name, dir, store, pass, passes: Vec::new(), ingests: Duration::ZERO, written: 0 }
    }

    fn ingest(&mut self, series: &[Series]) {
        let (started, written) = (Instant::now(), crate::sqlite::division::written());
        self.store.metrics().unwrap().ingest(series).unwrap();
        self.ingests += started.elapsed();
        self.written += crate::sqlite::division::written().zip(written).map_or(0, |(now, then)| now - then);
    }

    fn pass(&mut self) {
        let (started, written) = (Instant::now(), crate::sqlite::division::written());
        (self.pass)(&self.store).unwrap();
        self.passes.push(started.elapsed());
        self.written += crate::sqlite::division::written().zip(written).map_or(0, |(now, then)| now - then);
    }

    fn division(&self) -> Division {
        Division::of(&Engine::of(&self.store).unwrap().file)
    }
}

/// v0.1.0's passes, until one leaves no work: expiry and packing alone, the
/// heads they shrink rewritten in place.
fn passes_of_0_1_0(store: &Store) -> crate::Result<()> {
    let engine = Engine::of(store)?;
    let passes = || -> crate::Result<()> {
        while engine.expire_and_pack()?.1 {}
        Ok(())
    };
    crate::metrics::publish::IN_PLACE.set(true);
    let passed = passes();
    crate::metrics::publish::IN_PLACE.set(false);
    passed
}

fn passes_of_now(store: &Store) -> crate::Result<()> {
    maintain(store).map(drop)
}

/// A tick of the load below: a sample of each of 2,020 gauges, 101 hosts of
/// 20, each a walk of whole numbers in 0 to 100, as the TSBS devops corpus's.
fn tick(walks: &mut [u64], draws: &mut u64, now: std::time::SystemTime) -> Vec<Series> {
    let step = |walk: &mut u64, draws: &mut u64| {
        *draws ^= *draws << 13;
        *draws ^= *draws >> 7;
        *draws ^= *draws << 17;
        *walk = (*walk + *draws % 3).saturating_sub(1).min(100);
        *walk as f64
    };
    let series = |(index, walk): (usize, &mut u64)| {
        let (host, field) = (index / 20, index % 20);
        let labelled = Series::gauge(format!("cpu_{field}")).label("hostname", format!("host_{host}"));
        labelled.label("region", host % 7).label("service", host % 19).sample(now, step(walk, draws))
    };
    walks.iter_mut().enumerate().map(series).collect()
}

/// What a wide load leaves in metrics.db, three ways on the same samples: a
/// file v0.1.0 made under v0.1.0's passes; the same file under this
/// release's, which vacuums it once; a file this release made. A tick every
/// 10 s for `TINYSTORE_TICKS` ticks, 2,520 unless it says, seven hours; an
/// ingest a tick, the ways in turns, and their passes each minute. Then the
/// load stops and passes follow a minute apart for half an hour. Run by hand
/// with `--release -- --ignored --nocapture`; on Linux it also says the
/// bytes each way wrote.
#[test]
#[ignore = "a measurement, run by hand"]
fn what_a_wide_load_leaves_in_its_file() {
    let ticks: u32 = std::env::var("TINYSTORE_TICKS").ok().and_then(|ticks| ticks.parse().ok()).unwrap_or(2520);
    let clock = Arc::new(TestClock::new(std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let mut ways = [
        Way::open("0.1.0", true, passes_of_0_1_0, &clock),
        Way::open("0.1.0's file, passes now", true, passes_of_now, &clock),
        Way::open("now", false, passes_of_now, &clock),
    ];
    let (mut walks, mut draws) = (vec![50_u64; 2020], 0x9e37_79b9_7f4a_7c15_u64);
    for at in 0..ticks {
        let series = tick(&mut walks, &mut draws, clock.now());
        let turn = at as usize % ways.len();
        ways.rotate_left(turn);
        ways.iter_mut().for_each(|way| way.ingest(&series));
        if (at + 1) % 6 == 0 {
            ways.iter_mut().for_each(Way::pass);
        }
        ways.rotate_right(turn);
        clock.set(clock.now() + 10 * SECOND).unwrap();
    }
    ways.iter().for_each(|way| way.division().print(&format!("{} — live, after its last pass", way.name), 10));
    let live: Vec<(usize, u64)> = ways.iter().map(|way| (way.passes.len(), way.written)).collect();
    for _ in 0..30 {
        clock.set(clock.now() + Duration::from_mins(1)).unwrap();
        ways.iter_mut().for_each(Way::pass);
    }
    let stopped =
        |way: &Way| way.division().print(&format!("{} — stopped, half an hour of passes after", way.name), 10);
    ways.iter().for_each(stopped);
    ways.iter().zip(live).for_each(|(way, live)| print_costs(way, live, f64::from(ticks) * 2020.0));
}

/// What a load written after the fact leaves in metrics.db, the three ways
/// of the measurement above on the same samples: its seven hours ingested at
/// once, four ticks a call, in the order of their times, before any pass, as
/// a bulk load of the TSBS corpus is; then passes until none is left to
/// pack, and passes a minute apart for half an hour. Run by hand with
/// `--release -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run by hand"]
fn what_a_bulk_load_leaves_in_its_file() {
    let ticks: u32 = std::env::var("TINYSTORE_TICKS").ok().and_then(|ticks| ticks.parse().ok()).unwrap_or(2520);
    let start = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let clock = Arc::new(TestClock::new(start + 10 * SECOND * ticks));
    let mut ways = [
        Way::open("0.1.0", true, passes_of_0_1_0, &clock),
        Way::open("0.1.0's file, passes now", true, passes_of_now, &clock),
        Way::open("now", false, passes_of_now, &clock),
    ];
    let (mut walks, mut draws) = (vec![50_u64; 2020], 0x9e37_79b9_7f4a_7c15_u64);
    for first in (0..ticks).step_by(4) {
        let series: Vec<Series> = (first..ticks.min(first + 4))
            .flat_map(|at| tick(&mut walks, &mut draws, start + 10 * SECOND * at))
            .collect();
        let turn = first as usize % ways.len();
        ways.rotate_left(turn);
        ways.iter_mut().for_each(|way| way.ingest(&series));
        ways.rotate_right(turn);
    }
    ways.iter().for_each(|way| way.division().print(&format!("{} — ingested, before a pass", way.name), 10));
    ways.iter_mut().for_each(Way::pass);
    ways.iter().for_each(|way| way.division().print(&format!("{} — packed", way.name), 10));
    let live: Vec<(usize, u64)> = ways.iter().map(|way| (way.passes.len(), way.written)).collect();
    for _ in 0..30 {
        clock.set(clock.now() + Duration::from_mins(1)).unwrap();
        ways.iter_mut().for_each(Way::pass);
    }
    let stopped = |way: &Way| way.division().print(&format!("{} — half an hour of passes after", way.name), 10);
    ways.iter().for_each(stopped);
    ways.iter().zip(live).for_each(|(way, live)| print_costs(way, live, f64::from(ticks) * 2020.0));
}

/// The packing of what a bulk load grew, timed: 600 of the load's gauges,
/// seven hours of them ingested sixteen ticks a call into a fresh store,
/// then its passes until none is left to pack, its shrunk heads rewritten in
/// place or out and in.
fn pack_of_a_bulk_load(made_before: bool, in_place: bool) -> Duration {
    let start = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let clock = Arc::new(TestClock::new(start + 10 * SECOND * 2520));
    let way = Way::open("", made_before, passes_of_now, &clock);
    let (mut walks, mut draws) = (vec![50_u64; 600], 0x9e37_79b9_7f4a_7c15_u64);
    for first in (0..2520).step_by(16) {
        let series: Vec<Series> =
            (first..first + 16).flat_map(|at| tick(&mut walks, &mut draws, start + 10 * SECOND * at)).collect();
        way.store.metrics().unwrap().ingest(&series).unwrap();
    }
    let engine = Engine::of(&way.store).unwrap();
    crate::metrics::publish::IN_PLACE.set(in_place);
    let started = Instant::now();
    while engine.expire_and_pack().unwrap().1 {}
    let took = started.elapsed();
    crate::metrics::publish::IN_PLACE.set(false);
    took
}

/// What packing the heads a bulk load grew costs, its shrunk heads rewritten
/// in place against out and in, on a file 0.1.0 made and on one made now:
/// `TINYSTORE_ROUNDS` rounds, 5 unless it says, of a fresh store each way,
/// the ways in turns. Run by hand with `--release -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run by hand"]
fn what_packing_grown_heads_costs() {
    let rounds: usize = std::env::var("TINYSTORE_ROUNDS").ok().and_then(|rounds| rounds.parse().ok()).unwrap_or(5);
    let ways =
        [("0.1.0's file, in place", true, true), ("0.1.0's file, out and in", true, false), ("now", false, false)];
    let mut took = vec![Vec::new(); ways.len()];
    for round in 0..rounds {
        for turn in 0..ways.len() {
            let index = (round + turn) % ways.len();
            let (_, made_before, in_place) = ways[index];
            took[index].push(pack_of_a_bulk_load(made_before, in_place));
        }
    }
    for ((name, ..), took) in ways.iter().zip(&mut took) {
        took.sort();
        let (median, fastest, slowest) = (took[took.len() / 2], took[0], took[took.len() - 1]);
        println!("{name}: packing {median:?} at the median of {rounds}, {fastest:?} to {slowest:?}");
    }
}

/// What a way cost: its ingests, its passes, the bytes it wrote under the
/// load and after, which `live` says of the load, and its file once closed.
fn print_costs(way: &Way, (passes, written): (usize, u64), samples: f64) {
    println!(
        "{}: ingests {:?} in all; {}",
        way.name,
        way.ingests,
        crate::sqlite::division::passes(&way.passes, passes)
    );
    if way.written > 0 {
        println!("  written: {} KiB under the load, {} KiB after", written / 1024, (way.written - written) / 1024);
    }
    way.store.close().unwrap();
    let size = std::fs::metadata(way.dir.path().join("metrics.db")).unwrap().len() as f64;
    println!("  closed: metrics.db {:.0} KiB, {:.3} bytes a sample", size / 1024.0, size / samples);
}
