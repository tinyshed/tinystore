use std::sync::Barrier;
use std::time::Duration;

use serde::Serialize;

use super::super::event::Event;
use super::super::fixture::{DAY, SECOND, fixture, names};
use super::super::maintain::maintain;
use super::*;
use crate::ErrorKind;

const HOUR: Duration = Duration::from_secs(60 * 60);
const DELIVERY: &str = "72d3162e-cc78-11e3-81ab-4c9367dc0958";

#[derive(Serialize)]
struct Push {
    sha: &'static str,
}

/// The ids of a read's events, in its order.
fn ids(events: &[Event]) -> Vec<Option<&str>> {
    events.iter().map(|event| event.id.as_deref()).collect()
}

// contract: REC-11, REC-01
#[test]
fn an_event_whose_id_its_stream_holds_is_written_as_nothing_packed_or_not() {
    let fixture = fixture();
    let deploys = fixture.events("deploys");
    let at = fixture.now();
    assert!(deploys.event("push").id(DELIVERY).at(at).append(&Push { sha: "26a0865" }).unwrap());
    // the step's next attempt, and one that carries something else: the first event of an id stands
    assert!(!deploys.event("push").id(DELIVERY).at(at).append(&Push { sha: "26a0865" }).unwrap());
    assert!(!deploys.event("rebase").id(DELIVERY).append(&Push { sha: "f00ba47" }).unwrap());
    let read = deploys.list().page().unwrap().events;
    assert_eq!((names(&read), ids(&read)), (vec!["push"], vec![Some(DELIVERY)]));
    assert_eq!(read[0].fields.json("sha"), Some(r#""26a0865""#));
    // an event without an id is written each time
    assert!(deploys.append("tick", &()).unwrap());
    assert!(deploys.append("tick", &()).unwrap());

    fixture.pass(HOUR + SECOND);
    assert_eq!(maintain(&fixture.store).unwrap().packed, 3);
    assert!(!deploys.event("push").id(DELIVERY).append(&()).unwrap(), "held once packed");
    let fixture = fixture.reopen();
    let deploys = fixture.events("deploys");
    assert!(!deploys.event("push").id(DELIVERY).append(&()).unwrap(), "held once the store opens again");
    let read = deploys.list().since(DAY).page().unwrap().events;
    assert_eq!(ids(&read), [Some(DELIVERY), None, None], "the id read back from a block");
    let polled = fixture.store.all_events().unwrap().poll(None).unwrap().events;
    assert_eq!(ids(&polled), [Some(DELIVERY), None, None], "and from a poll");
}

// contract: REC-11, REC-02
#[test]
fn of_a_list_the_first_event_of_an_id_is_written_and_the_rest_of_it_whole() {
    let fixture = fixture();
    let deploys = fixture.events("deploys");
    let event = |name: &str, id: Option<&str>| {
        let event = NewEvent::new(name);
        id.map_or(event.clone(), |id| event.id(id))
    };
    let first = [event("a", Some("x")), event("b", None), event("c", Some("x")), event("d", Some("y"))];
    assert_eq!(deploys.append_all(first).unwrap(), 3);
    assert_eq!(deploys.append_all([event("e", Some("y")), event("f", Some("z"))]).unwrap(), 1);
    // a refused event refuses the list, and holds none of its ids
    let refused = deploys.append_all([event("g", Some("w")), NewEvent::new("")]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    assert_eq!(deploys.append_all([event("h", Some("w"))]).unwrap(), 1);
    let read = deploys.list().page().unwrap().events;
    assert_eq!(names(&read), ["a", "b", "d", "f", "h"]);
    assert_eq!(fixture.rows("event_ids"), 4);
}

// contract: REC-11
#[test]
fn an_event_on_time_is_written_beside_a_late_one_its_stream_holds() {
    let fixture = fixture();
    let deploys = fixture.events("deploys");
    let late = fixture.now() - HOUR;
    deploys.append("now", &()).unwrap();
    assert!(deploys.event("old").id("b").at(late).append(&()).unwrap());
    let both = [NewEvent::new("again").id("b").at(late), NewEvent::new("next").id("a")];
    assert_eq!(deploys.append_all(both).unwrap(), 1);
    assert_eq!(names(&deploys.list().page().unwrap().events), ["old", "now", "next"]);
}

// contract: REC-11
#[test]
fn an_id_is_its_streams_own() {
    let fixture = fixture();
    assert!(fixture.events("deploys").event("push").id(DELIVERY).append(&()).unwrap());
    assert!(fixture.events("audit").event("push").id(DELIVERY).append(&()).unwrap());
}

// contract: REC-11, REC-08
#[test]
fn appends_of_one_id_at_once_write_it_once_and_a_tail_sees_it_once() {
    let fixture = fixture();
    let deploys = fixture.events("deploys");
    let mut tail = deploys.list().tail().unwrap();
    let start = Barrier::new(8);
    let written = std::thread::scope(|scope| {
        let appends: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    deploys.event("push").id(DELIVERY).append(&()).unwrap()
                })
            })
            .collect();
        appends.into_iter().map(|append| append.join().unwrap()).filter(|wrote| *wrote).count()
    });
    assert_eq!(written, 1);
    assert_eq!(deploys.list().page().unwrap().events.len(), 1);
    let batch = tail.next_within(SECOND).unwrap().unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(tail.next_within(Duration::ZERO).unwrap(), None);
}

#[test]
fn an_id_is_1_to_128_bytes() {
    let fixture = fixture();
    let deploys = fixture.events("deploys");
    for refused in [String::new(), "x".repeat(129)] {
        let error = deploys.event("push").id(&refused).append(&()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
        assert!(error.to_string().contains("not 1 to 128"), "{error}");
    }
    assert!(deploys.event("push").id("x".repeat(128)).append(&()).unwrap());
}

// contract: REC-11, REC-09
#[test]
fn an_id_is_held_while_its_stream_holds_its_event() {
    let fixture = fixture();
    let deploys = fixture.store.events("deploys").keep(DAY).open().unwrap();
    deploys.append("boot", &()).unwrap();
    fixture.pass(HOUR + SECOND);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1);
    assert!(deploys.event("push").id(DELIVERY).append(&()).unwrap());
    fixture.pass(HOUR / 2);
    deploys.append("tick", &()).unwrap();
    fixture.pass(HOUR);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1, "the push and the tick in one segment");
    // the boot's segment goes; the push is older than the stream keeps, and its segment, which a
    // poll reads whole, is not
    fixture.pass(DAY - HOUR - HOUR / 4);
    assert_eq!(maintain(&fixture.store).unwrap().expired, 1);
    assert_eq!(names(&deploys.list().since(2 * DAY).page().unwrap().events), ["tick"]);
    assert!(!deploys.event("push").id(DELIVERY).append(&()).unwrap());
    let polled = fixture.store.all_events().unwrap().poll(None).unwrap().events;
    assert_eq!(ids(&polled), [Some(DELIVERY), None]);
    // once its segment goes, its id goes with it
    fixture.pass(DAY);
    assert_eq!(maintain(&fixture.store).unwrap().expired, 2);
    assert_eq!(fixture.rows("event_ids"), 0);
    assert!(deploys.event("push").id(DELIVERY).append(&()).unwrap());
}
