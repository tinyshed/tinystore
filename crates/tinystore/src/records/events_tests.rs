use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::super::fixture::{DAY, Fixture, SECOND, fixture, names};
use super::super::maintain::{Maintenance, maintain};
use super::*;

const HOUR: Duration = Duration::from_secs(60 * 60);

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct RoleChanged {
    user: u64,
    role: String,
}

/// A struct writes its fields in its own order, `route` before `ms`.
#[derive(Serialize)]
struct Slow {
    route: &'static str,
    ms: u32,
}

// contract: REC-03
#[test]
fn an_appended_event_is_read_at_once_with_what_it_carries() {
    let fixture = fixture();
    let audit = fixture.events("audit");
    let change = RoleChanged { user: 42, role: "admin".to_owned() };
    audit.append("role.changed", &change).unwrap();
    let line = audit.event("log").level(Level::WARN).message("slow request");
    let traced = line.context(&serde_json::json!({ "request": "9f86d0" })).trace(TraceId([1; 16]), SpanId([2; 8]));
    traced.append(&Slow { route: "/notes", ms: 1200 }).unwrap();

    let read = audit.list().page().unwrap();
    assert_eq!(read.next, None);
    let (changed, line) = (&read.events[0], &read.events[1]);
    assert_eq!((changed.stream.as_str(), changed.name.as_str(), changed.at), ("audit", "role.changed", fixture.now()));
    assert_eq!((changed.level, changed.message.as_deref(), changed.trace_id), (None, None, None));
    assert_eq!(changed.fields.to::<RoleChanged>().unwrap(), change);
    assert!(changed.context.is_empty());
    assert_eq!((line.level, line.message.as_deref()), (Some(Level::WARN), Some("slow request")));
    assert_eq!(line.fields.iter().collect::<Vec<_>>(), [("route", r#""/notes""#), ("ms", "1200")]);
    assert_eq!(line.context.json("request"), Some(r#""9f86d0""#));
    assert_eq!((line.trace_id, line.span_id), (Some(TraceId([1; 16])), Some(SpanId([2; 8]))));
}

#[test]
fn events_are_there_after_the_store_opens_again() {
    let fixture = fixture();
    fixture.events("audit").append("first", &()).unwrap();
    let fixture = fixture.reopen();
    fixture.events("audit").append("second", &()).unwrap();
    assert_eq!(names(&fixture.events("audit").list().page().unwrap().events), ["first", "second"]);
}

// contract: REC-02
#[test]
fn a_list_is_written_whole_or_not_at_all_and_a_refused_event_names_itself() {
    let fixture = fixture();
    let audit = fixture.events("audit");
    let late = fixture.now() - 15 * DAY;
    let refused = audit.append_all([NewEvent::new("kept"), NewEvent::new("stale").at(late)]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::TooOld);
    assert_eq!(refused.to_string(), r#"events audit: event 1 "stale": older than its stream keeps: too old"#);
    assert_eq!(fixture.rows("heads"), 0);

    audit.append_all([NewEvent::new("first"), NewEvent::new("second")]).unwrap();
    assert_eq!((fixture.rows("heads"), names(&audit.list().page().unwrap().events)), (1, vec!["first", "second"]));
    audit.append_all([]).unwrap();
    assert_eq!(fixture.rows("heads"), 1);
}

// contract: REC-09, STORE-08
#[test]
fn an_events_time_is_from_as_long_ago_as_its_stream_keeps_to_ten_minutes_ahead() {
    let fixture = fixture();
    let api = fixture.store.events("api").keep(DAY).open().unwrap();
    let now = fixture.now();
    api.event("oldest").at(now - DAY).append(&()).unwrap();
    api.event("newest").at(now + Duration::from_secs(600)).append(&()).unwrap();
    let older = api.event("older").at(now - DAY - SECOND).append(&()).unwrap_err();
    let newer = api.event("newer").at(now + Duration::from_secs(601)).append(&()).unwrap_err();
    assert_eq!((older.kind(), newer.kind()), (ErrorKind::TooOld, ErrorKind::TooNew));
}

#[test]
fn an_event_is_refused_without_a_name_past_its_keys_or_with_fields_that_are_no_object() {
    let fixture = fixture();
    let api = fixture.events("api");
    let many: Vec<(String, u8)> = (0..129).map(|key| (key.to_string(), 0)).collect();
    let many: std::collections::BTreeMap<String, u8> = many.into_iter().collect();
    let refused = [
        api.append("", &()).unwrap_err(),
        api.append("wide", &many).unwrap_err(),
        api.event("wide").context(&many).append(&()).unwrap_err(),
        api.append("list", &[1, 2]).unwrap_err(),
        api.event("text").context(&"request").append(&()).unwrap_err(),
    ];
    for error in &refused {
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    }
    assert_eq!(fixture.rows("heads"), 0);
}

#[test]
fn an_event_and_an_append_have_the_bytes_they_carry_at_most() {
    let fixture = fixture();
    let api = fixture.events("api");
    let heavy = api.event("heavy").message(&"x".repeat(MAX_EVENT)).append(&()).unwrap_err();
    assert_eq!(heavy.kind(), ErrorKind::Limit);

    let event = || NewEvent::new("fits").message(&"x".repeat(200 << 10));
    api.append_all((0..20).map(|_| event())).unwrap();
    let too_much = api.append_all((0..21).map(|_| event())).unwrap_err();
    assert_eq!(too_much.kind(), ErrorKind::Limit);
    assert_eq!(api.list().page().unwrap().events.len(), 20);
}

#[test]
fn an_append_is_cut_into_rows_of_the_events_and_the_bytes_a_row_holds() {
    let fixture = fixture();
    let api = fixture.events("api");
    api.append_all((0..2500).map(|n| NewEvent::new("small").fields(&serde_json::json!({ "n": n })))).unwrap();
    assert_eq!(fixture.rows("heads"), 3);
    api.append_all((0..3).map(|_| NewEvent::new("large").message(&"x".repeat(100 << 10)))).unwrap();
    assert_eq!(fixture.rows("heads"), 5);
    let read = api.list().limit(10_000).page().unwrap().events;
    assert_eq!(read.len(), 2503);
    let numbers: Vec<u32> = read[..2500].iter().map(|event| event.fields.get("n").unwrap().unwrap()).collect();
    assert_eq!(numbers, (0..2500).collect::<Vec<u32>>());
}

#[test]
fn a_row_whose_bytes_changed_is_corrupt_and_names_itself() {
    let fixture = fixture();
    fixture.events("api").append("click", &()).unwrap();
    let changed = fixture.records().file.write(0, |tx| {
        let body: Vec<u8> = tx.query_row("select body from heads", [], |row| row.get(0)).unwrap();
        let flipped: Vec<u8> =
            body.iter().enumerate().map(|(at, byte)| if at == 3 { byte ^ 1 } else { *byte }).collect();
        tx.execute("update heads set body = ?1", [flipped]).unwrap();
        Ok(())
    });
    changed.unwrap();
    let refused = fixture.events("api").list().page().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Corrupt);
    assert_eq!(refused.to_string(), "events api: head row 1 of api: a row's checksum: corrupt");
}

#[test]
fn every_stream_is_read_together_and_a_stream_reads_its_own() {
    let fixture = fixture();
    fixture.events("api").append("request", &()).unwrap();
    fixture.pass(SECOND);
    fixture.events("audit").append("role.changed", &()).unwrap();
    fixture.pass(SECOND);
    fixture.events("web").append("click", &()).unwrap();

    let all = fixture.store.all_events().unwrap();
    let read = all.list().page().unwrap().events;
    assert_eq!(names(&read), ["request", "role.changed", "click"]);
    assert_eq!(read.iter().map(|event| event.stream.as_str()).collect::<Vec<_>>(), ["api", "audit", "web"]);
    assert_eq!(names(&all.list().streams(["web", "api", "none"]).page().unwrap().events), ["request", "click"]);
    assert!(all.list().streams(["none"]).page().unwrap().events.is_empty());
    assert_eq!(names(&fixture.events("audit").list().page().unwrap().events), ["role.changed"]);
    let refused = fixture.events("audit").list().streams(["api"]).page().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
}

/// An event `seconds` from the store's clock, behind it when negative.
fn at(fixture: &Fixture, name: &str, seconds: i32) -> NewEvent {
    let span = SECOND * seconds.unsigned_abs();
    NewEvent::new(name).at(if seconds < 0 { fixture.now() - span } else { fixture.now() + span })
}

/// Each head row's head, late or on time, and its events, in the order the
/// rows were written.
fn heads(fixture: &Fixture) -> Vec<(bool, i64)> {
    let read = |connection: &rusqlite::Connection| {
        let mut select = connection.prepare("select late, count from heads order by id").unwrap();
        Ok(select.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect())
    };
    fixture.records().file.read(read).unwrap()
}

#[test]
fn an_event_more_than_ten_seconds_behind_its_streams_newest_is_late() {
    let fixture = fixture();
    let web = fixture.events("web");
    // the clock at 12:01, web waiting until 12:00:40
    web.append_all([at(&fixture, "12:00:40", -20)]).unwrap();
    let batch = [at(&fixture, "12:00:35", -25), at(&fixture, "12:00:41", -19), at(&fixture, "11:50:02", -658)];
    web.append_all(batch.into_iter().chain([at(&fixture, "12:00:45", -15)])).unwrap();
    assert_eq!(heads(&fixture), [(false, 1), (false, 3), (true, 1)]);
    // a batch in the order of its times makes none of its own late, however long it spans
    let api = fixture.events("api");
    api.append_all([at(&fixture, "11:58:30", -150), at(&fixture, "11:59:10", -110), at(&fixture, "12:00:45", -15)])
        .unwrap();
    assert_eq!(heads(&fixture)[3..], [(false, 3)]);
    // and one that comes fifteen seconds behind the one before it in the batch is
    let db = fixture.events("db");
    db.append_all([at(&fixture, "12:00:50", -10), at(&fixture, "12:00:35", -25)]).unwrap();
    assert_eq!(heads(&fixture)[4..], [(false, 1), (true, 1)]);
}

#[test]
fn an_event_appended_alone_can_be_late_and_a_producer_ahead_of_the_clock_makes_no_one_late() {
    let fixture = fixture();
    let web = fixture.events("web");
    web.append_all([at(&fixture, "12:00:40", -20)]).unwrap();
    web.append_all([at(&fixture, "12:00:20", -40)]).unwrap();
    // five minutes ahead of the clock: an event five seconds behind the clock is on time
    let ahead = fixture.events("ahead");
    ahead.append_all([at(&fixture, "12:06:00", 300)]).unwrap();
    ahead.append_all([at(&fixture, "12:00:55", -5)]).unwrap();
    assert_eq!(heads(&fixture), [(false, 1), (true, 1), (false, 1), (false, 1)]);
}

#[test]
fn late_events_are_packed_from_their_own_head() {
    let fixture = fixture();
    let web = fixture.events("web");
    web.append_all([at(&fixture, "waiting", -20)]).unwrap();
    web.append_all([at(&fixture, "on time", -15), at(&fixture, "late", -658)]).unwrap();
    fixture.pass(HOUR);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance { packed: 3, segments: 2, ..Maintenance::default() });
    // the segment of those on time spans five seconds, and not eleven minutes
    let spans = |connection: &rusqlite::Connection| {
        let mut select = connection.prepare("select last_at - first_at from segments order by id").unwrap();
        Ok(select.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect::<Vec<i64>>())
    };
    assert_eq!(fixture.records().file.read(spans).unwrap(), [5_000_000_000, 0]);
    assert_eq!(names(&web.list().page().unwrap().events), ["late", "waiting", "on time"]);
}

#[test]
fn a_reopened_store_knows_what_its_heads_hold() {
    let fixture = fixture();
    fixture.events("web").append_all([at(&fixture, "waiting", 0)]).unwrap();
    let fixture = fixture.reopen();
    fixture.events("web").append_all([at(&fixture, "behind", -30)]).unwrap();
    assert_eq!(heads(&fixture), [(false, 1), (true, 1)]);
}

#[test]
fn a_head_packed_empty_forgets_the_newest_event_it_held() {
    let fixture = fixture();
    let web = fixture.events("web");
    web.append("waiting", &()).unwrap();
    fixture.pass(HOUR);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1);
    // an hour behind the clock, and thirty seconds behind what was packed: nothing waits to be behind
    let behind = fixture.now() - HOUR - SECOND * 30;
    web.append_all([NewEvent::new("behind").at(behind)]).unwrap();
    assert_eq!(heads(&fixture), [(false, 1)]);
}

#[test]
fn a_head_packed_in_part_keeps_its_newest_event_until_the_store_opens_again() {
    let fixture = fixture();
    let api = fixture.events("api");
    // sixteen rows of a thousand at the newest time make a segment; a seventeenth, five seconds behind, waits
    let newest = fixture.now() - Duration::from_mins(1);
    for row in 0..17 {
        let at = if row < 16 { newest } else { newest - SECOND * 5 };
        api.append_all((0..1000).map(|n| NewEvent::new(&format!("{row}-{n}")).at(at))).unwrap();
    }
    assert_eq!(maintain(&fixture.store).unwrap().packed, 16_000);
    // twelve seconds behind what was packed, and seven behind what waits
    let behind = || NewEvent::new("behind").at(newest - SECOND * 12);
    api.append_all([behind()]).unwrap();
    assert_eq!(heads(&fixture), [(false, 1000), (true, 1)]);
    // an open reads the newest of what waits, and forgets what was packed
    drop(api);
    let fixture = fixture.reopen();
    fixture.events("api").append_all([behind()]).unwrap();
    assert_eq!(heads(&fixture), [(false, 1000), (true, 1), (false, 1)]);
}

#[test]
fn records_work_holds_the_stores_memory_waits_for_it_and_one_past_it_is_limit() {
    use std::sync::Arc;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    use serde_json::value::RawValue;

    use crate::records::maintain;
    use crate::{Options, TestClock};

    let dir = tempfile::tempdir().unwrap();
    let budget = 1 << 20;
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let options = Options { clock: Some(clock.clone()), memory: Some(budget), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    let audit = store.events("audit").open().unwrap();

    let pad = RawValue::from_string(format!(r#"{{"pad":"{}"}}"#, "x".repeat(200_000))).unwrap();
    let past = audit.append_all((0..8).map(|_| NewEvent::new("big").fields(&pad))).unwrap_err();
    assert_eq!(past.kind(), ErrorKind::Limit, "{past}");

    // the test holds all but ten bytes for a fifth of a second: each kind of work waits for them
    let waits = |what: &str, work: &dyn Fn()| {
        let held = store.memory().try_reserve(budget - 10, "the test").unwrap();
        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                drop(held);
            });
            work();
        });
        assert!(started.elapsed() >= Duration::from_millis(150), "{what} waited for the store's memory");
    };
    waits("an append", &|| assert!(audit.append("signup", &()).unwrap()));
    waits("a read", &|| assert_eq!(audit.list().page().unwrap().events.len(), 1));
    clock.set(store.now() + Duration::from_secs(3601)).unwrap();
    waits("a packing", &|| assert_eq!(maintain(&store).unwrap().segments, 1));
    waits("a poll", &|| assert_eq!(store.all_events().unwrap().poll(None).unwrap().events.len(), 1));
}
