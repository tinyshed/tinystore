use std::time::Duration;

use super::super::fixture::{DAY, Fixture, SECOND, fixture, names};
use super::super::maintain::{Maintenance, maintain};
use super::*;
use crate::records::{Level, NewEvent};

const HOUR: Duration = Duration::from_secs(60 * 60);

/// The rows of the three tables events lie in.
fn lie(fixture: &Fixture) -> (i64, i64, i64) {
    (fixture.rows("heads"), fixture.rows("segments"), fixture.rows("blocks"))
}

#[test]
fn a_streams_events_are_packed_once_its_oldest_row_has_waited_an_hour() {
    let fixture = fixture();
    let api = fixture.events("api");
    api.append("first", &()).unwrap();
    fixture.pass(HOUR - SECOND);
    api.append("second", &()).unwrap();
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance::default());
    assert_eq!(lie(&fixture), (2, 0, 0));

    fixture.pass(SECOND);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance { packed: 2, segments: 1, ..Maintenance::default() });
    assert_eq!(lie(&fixture), (0, 1, 1));
    assert_eq!(names(&api.list().page().unwrap().events), ["first", "second"]);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance::default());
}

#[test]
fn a_segments_worth_is_packed_at_once_and_what_is_left_waits() {
    let fixture = fixture();
    let api = fixture.events("api");
    for batch in 0..17 {
        api.append_all((0..1000).map(|n| NewEvent::new(&format!("{batch}-{n}")))).unwrap();
        fixture.pass(Duration::from_millis(1));
    }
    assert_eq!(
        maintain(&fixture.store).unwrap(),
        Maintenance { packed: 16_000, segments: 1, ..Maintenance::default() }
    );
    // sixteen rows of a thousand fit the 16,384 a segment holds, a seventeenth does not
    assert_eq!(lie(&fixture), (1, 1, 16));
    let read = api.list().limit(10_000).all().map(Result::unwrap).count();
    assert_eq!(read, 17_000);
}

#[test]
fn a_segment_holds_its_events_in_the_order_of_their_times_those_of_one_time_as_they_were_written() {
    let fixture = fixture();
    let api = fixture.events("api");
    let now = fixture.now();
    let at = |name: &str, millis: u64| NewEvent::new(name).at(now + Duration::from_millis(millis));
    api.append_all([at("buy", 300), at("menu", 100)]).unwrap();
    api.append_all([at("save", 200), at("menu again", 100)]).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    assert_eq!(lie(&fixture), (0, 1, 1));
    assert_eq!(names(&api.list().page().unwrap().events), ["menu", "menu again", "save", "buy"]);
    assert_eq!(names(&api.list().newest().page().unwrap().events), ["buy", "save", "menu again", "menu"]);
    let times: (i64, i64) = fixture
        .records()
        .file
        .read(|connection| {
            Ok(connection
                .query_row("select first_at, last_at from segments", [], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap())
        })
        .unwrap();
    assert_eq!(times.1 - times.0, 200_000_000);
}

#[test]
fn a_read_finds_each_event_once_in_what_is_packed_and_what_waits() {
    let fixture = fixture();
    let api = fixture.events("api");
    let start = fixture.now();
    let at = |name: &str, seconds: u64| NewEvent::new(name).at(start + SECOND * seconds as u32);
    api.append_all([at("1", 1), at("3", 3), at("5", 5), at("same packed", 4)]).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    // written after the packing, with times among the packed ones
    api.append_all([at("2", 2), at("same fresh", 4), at("6", 6)]).unwrap();
    assert_eq!(lie(&fixture), (1, 1, 1));

    let whole = ["1", "2", "3", "same packed", "same fresh", "5", "6"];
    assert_eq!(names(&api.list().page().unwrap().events), whole);
    let backwards: Vec<&str> = whole.iter().rev().copied().collect();
    assert_eq!(names(&api.list().newest().page().unwrap().events), backwards);

    let mut paged = Vec::new();
    let mut list = api.list().limit(2);
    loop {
        let page = list.page().unwrap();
        paged.extend(page.events.into_iter().map(|event| event.name));
        match page.next {
            Some(next) => list = api.list().limit(2).after(&next),
            None => break,
        }
    }
    assert_eq!(paged, whole);
    let found = api.list().from(start + SECOND * 2).to(start + SECOND * 5).page().unwrap();
    assert_eq!(names(&found.events), ["2", "3", "same packed", "same fresh"]);
}

#[test]
fn what_a_read_looks_for_is_found_in_a_block_as_in_a_head() {
    let fixture = fixture();
    let api = fixture.events("api");
    let warn = NewEvent::new("log").level(Level::WARN).message("slow request");
    api.append_all([warn.fields(&serde_json::json!({ "route": "/notes" })), NewEvent::new("click")]).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    assert_eq!(lie(&fixture), (0, 1, 1));
    assert_eq!(names(&api.list().min_level(Level::WARN).page().unwrap().events), ["log"]);
    assert_eq!(names(&api.list().names(["click"]).page().unwrap().events), ["click"]);
    assert_eq!(names(&api.list().filter("route", &"/notes").page().unwrap().events), ["log"]);
    assert_eq!(names(&api.list().search("SLOW").page().unwrap().events), ["log"]);
    assert!(api.list().min_level(Level::ERROR).page().unwrap().events.is_empty());
}

#[test]
fn a_segment_is_cut_into_blocks_of_the_events_and_the_bytes_a_block_holds() {
    let fixture = fixture();
    let api = fixture.events("api");
    api.append_all((0..2500).map(|n| NewEvent::new(&n.to_string()))).unwrap();
    api.append_all((0..3).map(|_| NewEvent::new("large").message(&"x".repeat(100 << 10)))).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    // 2503 events of one time: two blocks of 1024, and the rest cut where the large ones pass a block's bytes
    assert_eq!(lie(&fixture), (0, 1, 4));
    let read = api.list().limit(10_000).page().unwrap().events;
    assert_eq!(read.len(), 2503);
    assert_eq!((read[0].name.as_str(), read[2499].name.as_str(), read[2502].name.as_str()), ("0", "2499", "large"));
}

#[test]
fn a_segment_goes_whole_once_its_newest_event_is_past_what_its_stream_keeps() {
    let fixture = fixture();
    let api = fixture.store.events("api").keep(DAY).open().unwrap();
    api.append("early", &()).unwrap();
    fixture.pass(HOUR);
    api.append("late", &()).unwrap();
    maintain(&fixture.store).unwrap();
    assert_eq!(lie(&fixture), (0, 1, 1));

    fixture.pass(DAY - HOUR + SECOND);
    // the early event is past a day and read no more, and its segment stays for the late one
    assert_eq!(names(&api.list().page().unwrap().events), ["late"]);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance::default());
    assert_eq!(lie(&fixture), (0, 1, 1));

    fixture.pass(HOUR);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance { expired: 2, ..Maintenance::default() });
    assert_eq!(lie(&fixture), (0, 0, 0));
}

#[test]
fn each_stream_is_packed_by_itself() {
    let fixture = fixture();
    fixture.events("api").append("request", &()).unwrap();
    fixture.pass(HOUR);
    fixture.events("web").append("click", &()).unwrap();
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance { packed: 1, segments: 1, ..Maintenance::default() });
    assert_eq!(lie(&fixture), (1, 1, 1));
    let all = fixture.store.all_events().unwrap();
    assert_eq!(names(&all.list().page().unwrap().events), ["request", "click"]);
}

#[test]
fn a_head_row_that_left_while_it_was_packed_fails_the_whole_and_leaves_nothing_written() {
    let fixture = fixture();
    fixture.events("api").append("request", &()).unwrap();
    let records = fixture.records();
    let stream = records.stream("api").unwrap().id;
    let packed = Packed { rows: vec![1, 99], first: 0, last: 0, events: 1, weight: 1, blocks: Vec::new() };
    assert_eq!(records.publish(stream, &packed).unwrap_err().kind(), crate::ErrorKind::Internal);
    assert_eq!(lie(&fixture), (1, 0, 0));
}
