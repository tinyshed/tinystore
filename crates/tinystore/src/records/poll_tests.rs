use std::time::Duration;

use super::super::fixture::{DAY, Fixture, SECOND, fixture};
use super::super::maintain::maintain;
use super::*;
use crate::ErrorKind;
use crate::records::NewEvent;

const HOUR: Duration = Duration::from_secs(60 * 60);

/// Packs `events` of a stream as one segment, an hour on.
fn segment(fixture: &Fixture, stream: &str, round: usize, events: usize) {
    fixture.events(stream).append_all((0..events).map(|n| NewEvent::new(&format!("{round}-{n}")))).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
}

fn names(batch: &PollBatch) -> Vec<String> {
    batch.events.iter().map(|event| event.name.clone()).collect()
}

/// Every event polls from `cursor` give, `limit` a batch, and the cursor
/// after the last.
fn drain(all: &AllEvents, mut cursor: Option<String>, limit: usize) -> (Vec<String>, String) {
    let mut given = Vec::new();
    loop {
        let batch = all.poll_limit(cursor.as_deref(), limit).unwrap();
        if batch.events.is_empty() {
            return (given, batch.cursor);
        }
        given.extend(names(&batch));
        cursor = Some(batch.cursor);
    }
}

fn spelled(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

// contract: REC-07
#[test]
fn a_poll_gives_every_packed_event_once_a_segment_of_one_stream_at_a_time() {
    let fixture = fixture();
    segment(&fixture, "api", 0, 3);
    segment(&fixture, "web", 1, 2);
    // what waits in a head is polled once it is packed
    fixture.events("api").append("waiting", &()).unwrap();
    let all = fixture.store.all_events().unwrap();
    let first = all.poll(None).unwrap();
    assert_eq!((names(&first), first.cursor.as_str(), first.expired), (spelled(&["0-0", "0-1", "0-2"]), "2:0", 0));
    assert_eq!(first.events[0].stream, "api");
    let second = all.poll(Some(&first.cursor)).unwrap();
    assert_eq!(
        (names(&second), second.events[0].stream.as_str(), second.cursor.as_str()),
        (spelled(&["1-0", "1-1"]), "web", "3:0")
    );
    // nothing past the cursor: an empty batch, and the cursor given
    let third = all.poll(Some(&second.cursor)).unwrap();
    assert_eq!((third.events.len(), third.cursor.as_str()), (0, "3:0"));
    // a batch with a limit ends inside a segment, and the next goes on from there
    assert_eq!(all.poll_limit(None, 2).unwrap().cursor, "1:2");
    assert_eq!(drain(&all, None, 2), (spelled(&["0-0", "0-1", "0-2", "1-0", "1-1"]), "3:0".to_owned()));
    for (cursor, limit) in [(Some("x"), 10), (Some("1:-1"), 10), (None, 0), (None, 10_001)] {
        assert_eq!(all.poll_limit(cursor, limit).unwrap_err().kind(), ErrorKind::Invalid, "{cursor:?} {limit}");
    }
}

// contract: REC-07
#[test]
fn a_cursor_keeps_its_place_across_merges_the_middle_of_a_merged_segment_included() {
    let fixture = fixture();
    for round in 0..3 {
        segment(&fixture, "api", round, 4);
    }
    let all = fixture.store.all_events().unwrap();
    let (mut given, mut cursor) = (Vec::new(), None);
    for _ in 0..3 {
        let batch = all.poll_limit(cursor.as_deref(), 2).unwrap();
        given.extend(names(&batch));
        cursor = Some(batch.cursor);
    }
    // two events into the second segment, which a fourth now merges with the others into the first
    assert_eq!(cursor.as_deref(), Some("2:2"));
    segment(&fixture, "api", 3, 4);
    let held: i64 = fixture
        .records()
        .file
        .read(|connection| {
            Ok(connection.query_row("select held from segments where id = 1", [], |row| row.get(0)).unwrap())
        })
        .unwrap();
    assert_eq!(held, 16);
    let (rest, last) = drain(&all, cursor, 3);
    given.extend(rest);
    let every: Vec<String> = (0..4).flat_map(|round| (0..4).map(move |n| format!("{round}-{n}"))).collect();
    assert_eq!((given, last.as_str()), (every, "5:0"));
}

// contract: REC-07
#[test]
fn a_poll_counts_the_events_keep_removed_before_its_cursor_reached_them() {
    let fixture = fixture();
    fixture.store.events("api").keep(DAY).open().unwrap();
    segment(&fixture, "api", 0, 3);
    segment(&fixture, "api", 1, 5);
    segment(&fixture, "web", 2, 2);
    let all = fixture.store.all_events().unwrap();
    let first = all.poll_limit(None, 1).unwrap();
    assert_eq!(first.cursor, "1:1");
    // api keeps a day: its two segments go, two events of the first and five of the second unread
    fixture.pass(DAY);
    maintain(&fixture.store).unwrap();
    let next = all.poll(Some(&first.cursor)).unwrap();
    assert_eq!((names(&next), next.expired), (spelled(&["2-0", "2-1"]), 7));
    assert_eq!(all.poll(None).unwrap().expired, 0);
}

// contract: REC-07
#[test]
fn a_poll_counts_the_events_keep_removed_while_they_waited_in_a_head() {
    let fixture = fixture();
    let audit = fixture.store.events("audit").keep(2 * HOUR).open().unwrap();
    segment(&fixture, "audit", 0, 1);
    let all = fixture.store.all_events().unwrap();
    let first = all.poll(None).unwrap();
    assert_eq!((names(&first), first.cursor.as_str()), (spelled(&["0-0"]), "2:0"));
    // a backfilled event near what the stream keeps waits in its late head, and goes before it is packed
    audit.append_all([NewEvent::new("backfilled").at(fixture.now() - 2 * HOUR + 60 * SECOND)]).unwrap();
    fixture.pass(2 * 60 * SECOND);
    assert_eq!(maintain(&fixture.store).unwrap().expired, 1);
    segment(&fixture, "audit", 1, 1);
    let next = all.poll(Some(&first.cursor)).unwrap();
    assert_eq!((names(&next), next.expired), (spelled(&["1-0"]), 1));
    assert_eq!(all.poll(Some(&next.cursor)).unwrap().expired, 0, "a cursor after it passed none of it");
}

// contract: REC-07
#[test]
fn a_first_poll_that_finds_nothing_counts_nothing_keep_removed_before_it() {
    let fixture = fixture();
    fixture.store.events("api").keep(2 * HOUR).open().unwrap();
    segment(&fixture, "api", 0, 3);
    fixture.pass(2 * HOUR);
    assert_eq!(maintain(&fixture.store).unwrap().expired, 3);
    let all = fixture.store.all_events().unwrap();
    let first = all.poll(None).unwrap();
    assert_eq!((first.events.len(), first.cursor.as_str(), first.expired), (0, "2:0", 0));
    segment(&fixture, "api", 1, 2);
    let next = all.poll(Some(&first.cursor)).unwrap();
    assert_eq!((names(&next), next.expired), (spelled(&["1-0", "1-1"]), 0));
}

// contract: REC-07
#[test]
fn a_stream_that_keeps_less_than_an_hour_packs_its_events_before_keep_takes_them() {
    let fixture = fixture();
    let audit = fixture.store.events("audit").keep(30 * 60 * SECOND).open().unwrap();
    audit.append("signed in", &()).unwrap();
    fixture.pass(15 * 60 * SECOND);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1, "packed at half what its stream keeps");
    let all = fixture.store.all_events().unwrap();
    assert_eq!(names(&all.poll(None).unwrap()), spelled(&["signed in"]));
}
