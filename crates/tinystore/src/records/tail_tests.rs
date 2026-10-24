use std::time::Duration;

use serde_json::json;

use super::super::fixture::{SECOND, fixture};
use super::*;
use crate::records::{Level, NewEvent};

fn names(batch: &TailBatch) -> Vec<&str> {
    batch.events.iter().map(|event| event.name.as_str()).collect()
}

// contract: REC-08
#[test]
fn a_tail_gives_the_events_appended_after_it_opened_a_batch_as_they_come() {
    let fixture = fixture();
    let api = fixture.events("api");
    api.append("before", &()).unwrap();
    let mut tail = api.list().tail().unwrap();
    assert_eq!(tail.next_within(Duration::ZERO).unwrap(), None);
    api.append_all([NewEvent::new("first"), NewEvent::new("second")]).unwrap();
    let batch = tail.next_within(SECOND).unwrap().unwrap();
    assert_eq!((names(&batch), batch.missed), (vec!["first", "second"], 0));
    assert_eq!(batch.events[0].stream, "api");
    assert_eq!(tail.next_within(Duration::ZERO).unwrap(), None);
}

// contract: REC-08
#[test]
fn a_tail_takes_a_reads_conditions_and_no_range_order_or_limit() {
    let fixture = fixture();
    let (api, web) = (fixture.events("api"), fixture.events("web"));
    let mut loud = api.list().min_level(Level::WARN).tail().unwrap();
    let all = fixture.store.all_events().unwrap();
    let mut clicks = all.list().streams(["web"]).tail().unwrap();
    api.append_all([NewEvent::new("quiet").level(Level::INFO), NewEvent::new("loud").level(Level::ERROR)]).unwrap();
    web.append("click", &json!({ "x": 1 })).unwrap();
    assert_eq!(names(&loud.next_within(SECOND).unwrap().unwrap()), ["loud"]);
    assert_eq!(names(&clicks.next_within(SECOND).unwrap().unwrap()), ["click"]);
    assert_eq!(loud.next_within(Duration::ZERO).unwrap(), None);
    for refused in [api.list().since(SECOND), api.list().newest(), api.list().limit(5), api.list().after("1~2")] {
        assert_eq!(refused.tail().unwrap_err().kind(), crate::ErrorKind::Invalid);
    }
}

// contract: REC-08
#[test]
fn a_tail_slower_than_its_writers_says_how_many_left_before_it_read_them() {
    let fixture = fixture();
    let api = fixture.events("api");
    let mut tail = api.list().tail().unwrap();
    // two megabytes of events, where the store holds one for its tails
    let big = "x".repeat(10_000);
    for _ in 0..200 {
        api.append_all([NewEvent::new("big").message(&big)]).unwrap();
    }
    let batch = tail.next_within(SECOND).unwrap().unwrap();
    assert!(batch.missed > 50 && batch.missed + batch.events.len() as u64 == 200, "{}", batch.missed);
    api.append("after", &()).unwrap();
    let after = tail.next_within(SECOND).unwrap().unwrap();
    assert_eq!((names(&after), after.missed), (vec!["after"], 0));
}

// contract: REC-04, REC-08
#[test]
fn what_a_tail_gave_of_one_time_a_read_gives_in_its_order_once_it_is_packed() {
    let fixture = fixture();
    let all = fixture.store.all_events().unwrap();
    let mut tail = all.list().tail().unwrap();
    let at = fixture.now() - Duration::from_mins(1);
    // web's late events, behind one twenty seconds after them, among api's on time
    let appended = [("api", "a", at), ("web", "ahead", at + SECOND * 20), ("web", "b", at), ("api", "c", at)];
    for (stream, name, time) in appended {
        fixture.events(stream).append_all([NewEvent::new(name).at(time)]).unwrap();
    }
    let mut tailed = Vec::new();
    while let Some(batch) = tail.next_within(Duration::ZERO).unwrap() {
        tailed.extend(batch.events.into_iter().filter(|event| event.at == at).map(|event| event.name));
    }
    assert_eq!(tailed, ["a", "b", "c"]);
    fixture.pass(Duration::from_hours(1));
    crate::records::maintain(&fixture.store).unwrap();
    let read = all.list().to(at + SECOND).page().unwrap().events;
    assert_eq!(read.into_iter().map(|event| event.name).collect::<Vec<_>>(), tailed);
}

// contract: REC-08
#[test]
fn a_tail_ends_when_the_store_closes() {
    let fixture = fixture();
    let tail = fixture.events("api").list().tail().unwrap();
    let walking = std::thread::spawn(move || tail.count());
    fixture.store.close().unwrap();
    assert_eq!(walking.join().unwrap(), 0);
}
