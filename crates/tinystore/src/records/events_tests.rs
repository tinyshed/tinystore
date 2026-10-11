use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::super::fixture::{DAY, SECOND, fixture, names};
use super::*;

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
    assert_eq!(refused.to_string(), "events api: head row 1 of api: a head row's checksum: corrupt");
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
