use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::super::fixture::{DAY, Fixture, fixture, fixture_of};
use super::MIGRATIONS;
use crate::ErrorKind;
use crate::records::{AllEvents, Event, NewEvent, maintain};
use crate::sqlite::{Config, File};

/// records.db as v0.1.0 (f11772d3) wrote and closed it, every block of
/// version 2. Four rounds an hour apart from 1_800_000_000 s, each packed an
/// hour on: api appends `api-r-0` at the round before's time, from round 1
/// on, and `api-r-a`, web `web-r`, then api `api-r-c`, at the round's time;
/// round 0 adds api's `late-0`, a minute behind. api's first four segments
/// merged into segment 1, and web's into 3. At the close `wait-api`,
/// `wait-web` and the late `wait-late` wait in their heads.
const V0_1_0: &[u8] = include_bytes!("../../../../testdata/records/v0.1.0.db");
/// Where v0.1.0's clock stood when it closed the file: four hours on.
const V0_1_0_CLOSED: Duration = Duration::from_secs(1_800_014_400);
/// The time of the fourth round, which wait-api and wait-web share with it.
const ROUND_3: Duration = Duration::from_secs(1_800_010_800);

/// Every event of every stream as v0.1.0 read the file, in its order: those
/// of one time by where they lay, its blocks' before its heads'.
const READ_BY_V0_1_0: [&str; 19] = [
    "api/late-0",
    "api/api-0-a",
    "api/api-0-c",
    "api/api-1-0",
    "web/web-0",
    "api/api-1-a",
    "api/api-1-c",
    "api/api-2-0",
    "web/web-1",
    "api/api-2-a",
    "api/api-2-c",
    "api/api-3-0",
    "web/web-2",
    "api/wait-late",
    "api/api-3-a",
    "api/api-3-c",
    "web/web-3",
    "api/wait-api",
    "web/wait-web",
];

/// Each batch of a poll from no cursor as v0.1.0 gave them, with the cursor after it.
const POLLED_BY_V0_1_0: [(&[&str], &str); 9] = [
    (&["api-0-a", "api-0-c"], "2:0"),
    (&["late-0"], "3:0"),
    (&["web-0"], "4:0"),
    (&["api-1-0", "api-1-a", "api-1-c"], "5:0"),
    (&["web-1"], "6:0"),
    (&["api-2-0", "api-2-a", "api-2-c"], "7:0"),
    (&["web-2"], "8:0"),
    (&["api-3-0", "api-3-a", "api-3-c"], "9:0"),
    (&["web-3"], "10:0"),
];

fn released() -> Fixture {
    fixture_of(V0_1_0, UNIX_EPOCH + V0_1_0_CLOSED)
}

/// Each event as `stream/name`.
fn shown(events: &[Event]) -> Vec<String> {
    events.iter().map(|event| format!("{}/{}", event.stream, event.name)).collect()
}

/// What reads of every stream, of each stream and newest first give.
fn read_every_way(fixture: &Fixture, expected: &[String], when: &str) {
    let all = fixture.store.all_events().unwrap();
    assert_eq!(shown(&all.list().page().unwrap().events), expected, "{when}");
    let backwards: Vec<String> = expected.iter().rev().cloned().collect();
    assert_eq!(shown(&all.list().newest().page().unwrap().events), backwards, "newest, {when}");
    let walked: Vec<String> = all.list().limit(20).all().map(|event| shown(&[event.unwrap()]).remove(0)).collect();
    assert_eq!(walked, expected, "all, {when}");
    for stream in ["api", "web"] {
        let own: Vec<String> =
            expected.iter().filter(|name| name.starts_with(&format!("{stream}/"))).cloned().collect();
        assert_eq!(shown(&fixture.events(stream).list().page().unwrap().events), own, "{stream}, {when}");
    }
}

/// Every batch a poll gives from no cursor, with the cursor after each.
fn polled(all: &AllEvents) -> Vec<(Vec<String>, String)> {
    let (mut batches, mut cursor) = (Vec::new(), None);
    loop {
        let batch = all.poll(cursor.as_deref()).unwrap();
        if batch.events.is_empty() {
            return batches;
        }
        batches.push((batch.events.iter().map(|event| event.name.clone()).collect(), batch.cursor.clone()));
        cursor = Some(batch.cursor);
    }
}

// contract: REC-04, REC-07
#[test]
fn a_file_v0_1_0_wrote_reads_and_polls_as_v0_1_0_read_it_and_v0_1_0_refuses_it_once_opened() {
    let fixture = released();
    let expected: Vec<String> = READ_BY_V0_1_0.map(String::from).to_vec();
    read_every_way(&fixture, &expected, "opened");
    let polls: Vec<(Vec<String>, String)> = POLLED_BY_V0_1_0
        .iter()
        .map(|(names, cursor)| (names.iter().map(|name| (*name).to_owned()).collect(), (*cursor).to_owned()))
        .collect();
    assert_eq!(polled(&fixture.store.all_events().unwrap()), polls);
    let path = fixture.dir.path().join("records.db");
    fixture.store.close().unwrap();
    // v0.1.0 knows its first step alone, and refuses a file a later one migrated
    let file = File::open(&path, Config { page_size: 1024, ..Config::default() }).unwrap();
    let refused = file.migrate("records", &MIGRATIONS[..1]).unwrap_err();
    assert!(refused.to_string().contains("a newer program migrated it"), "{refused}");
    file.close().unwrap();
}

// contract: REC-04, DUR-09
#[test]
fn events_appended_after_v0_1_0_come_after_its_own_of_one_time_in_the_order_appended() {
    let fixture = released();
    let round_3 = UNIX_EPOCH + ROUND_3;
    let mut expected: Vec<String> = READ_BY_V0_1_0.map(String::from).to_vec();
    let append = |expected: &mut Vec<String>, stream: &str, name: &str, at: SystemTime| {
        fixture.events(stream).append_all([NewEvent::new(name).at(at)]).unwrap();
        expected.push(format!("{stream}/{name}"));
    };
    for (stream, name) in [("api", "new-1"), ("web", "new-2"), ("api", "new-3"), ("web", "new-4")] {
        append(&mut expected, stream, name, round_3);
    }
    read_every_way(&fixture, &expected, "appended");
    // what waits packed, then two more of api's segments at round 3's time
    // and after it: four of a size that merge into v0.1.0's segment 8
    let mut merged = 0;
    for (round, at) in [None, Some(round_3), Some(round_3 + Duration::from_hours(2))].into_iter().enumerate() {
        if let Some(at) = at {
            append(&mut expected, "api", &format!("more-{round}"), at);
        }
        fixture.pass(Duration::from_hours(1));
        merged += maintain(&fixture.store).unwrap().merged;
        read_every_way(&fixture, &expected, &format!("packed {round}"));
    }
    let holds = |connection: &rusqlite::Connection| {
        let sql = "select holder is null, held from segments where id = 8";
        Ok(connection.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap())
    };
    let holds: (bool, i64) = fixture.records().file.read(holds).unwrap();
    assert_eq!((merged, holds), (3, (true, 8)), "v0.1.0's segment 8 holds the merge");
    assert_eq!(fixture.rows("heads"), 0);
    let fixture = fixture.reopen();
    read_every_way(&fixture, &expected, "reopened");
}

#[test]
fn a_stream_nobody_gave_a_keep_keeps_fourteen_days() {
    let fixture = fixture();
    fixture.events("api");
    assert_eq!(fixture.records().stream("api").unwrap().keeps(), 14 * DAY);
}

// contract: REC-09
#[test]
fn a_stream_keeps_what_an_open_gave_it_across_runs_and_an_open_that_gives_none_changes_nothing() {
    let fixture = fixture();
    fixture.store.events("audit").keep(365 * DAY).open().unwrap();
    fixture.events("audit");
    assert_eq!(fixture.records().stream("audit").unwrap().keeps(), 365 * DAY);

    let fixture = fixture.reopen();
    fixture.events("audit");
    assert_eq!(fixture.records().stream("audit").unwrap().keeps(), 365 * DAY);
    // a new run may say another: the code changed
    fixture.store.events("audit").keep(30 * DAY).open().unwrap();
    assert_eq!(fixture.reopen().records().stream("audit").unwrap().keeps(), 30 * DAY);
}

// contract: REC-09
#[test]
fn another_keep_while_the_store_is_open_is_invalid_and_names_both() {
    let fixture = fixture();
    fixture.store.events("api").keep(30 * DAY).open().unwrap();
    fixture.store.events("api").keep(30 * DAY).open().unwrap();
    let refused = fixture.store.events("api").keep(365 * DAY).open().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert_eq!(refused.to_string(), "events api: keep 365d, where the stream is open with keep 30d: invalid");
    assert_eq!(fixture.records().stream("api").unwrap().keeps(), 30 * DAY);
}

#[test]
fn a_stream_has_a_name_as_a_bucket_has_and_keeps_for_more_than_no_time() {
    let fixture = fixture();
    for name in ["", "Api", "a b", "-a", &"a".repeat(65)] {
        assert_eq!(fixture.store.events(name).open().unwrap_err().kind(), ErrorKind::Invalid, "{name:?}");
    }
    assert_eq!(fixture.store.events("api").keep(Duration::ZERO).open().unwrap_err().kind(), ErrorKind::Invalid);
    let under_a_millisecond = fixture.store.events("api").keep(Duration::from_micros(500)).open().unwrap_err();
    assert_eq!(under_a_millisecond.kind(), ErrorKind::Invalid, "it would be kept as none");
    fixture.store.events("a-1_b").open().unwrap();
    assert_eq!(fixture.rows("streams"), 1);
}
