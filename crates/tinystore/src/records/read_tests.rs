use std::time::Duration;

use serde_json::json;

use super::super::fixture::{Fixture, SECOND, fixture, names};
use super::*;
use crate::ErrorKind;
use crate::records::{Events, NewEvent, SpanId};

/// `count` events a second apart, named by their place, the last one now.
fn timeline(fixture: &Fixture, events: &Events, count: u32) {
    let start = fixture.now() - SECOND * (count - 1);
    events.append_all((0..count).map(|n| NewEvent::new(&n.to_string()).at(start + SECOND * n))).unwrap();
}

/// Every page of a read in turn, as the names of its events.
fn pages(list: &List) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut next = None;
    loop {
        let asked = next.as_deref().map_or(list.clone(), |next| list.clone().after(next));
        let page = asked.page().unwrap();
        assert!(pages.len() < 50, "the pages do not end");
        pages.push(page.events.iter().map(|event| event.name.clone()).collect());
        match page.next {
            Some(after) => next = Some(after),
            None => return pages,
        }
    }
}

#[test]
fn events_come_in_the_order_of_their_times_and_those_of_one_time_as_they_were_written() {
    let fixture = fixture();
    let api = fixture.events("api");
    let now = fixture.now();
    let at = |name: &str, seconds: u64| NewEvent::new(name).at(now - Duration::from_secs(seconds));
    api.append_all([at("c", 1), at("a", 3), at("b1", 2)]).unwrap();
    api.append_all([at("b2", 2), at("d", 0)]).unwrap();
    assert_eq!(names(&api.list().page().unwrap().events), ["a", "b1", "b2", "c", "d"]);
    assert_eq!(names(&api.list().newest().page().unwrap().events), ["d", "c", "b2", "b1", "a"]);
}

#[test]
fn pages_go_on_without_losing_or_repeating_an_event_from_either_end() {
    let fixture = fixture();
    let api = fixture.events("api");
    timeline(&fixture, &api, 10);
    let forward = pages(&api.list().limit(4));
    assert_eq!(forward, [vec!["0", "1", "2", "3"], vec!["4", "5", "6", "7"], vec!["8", "9"]]);
    let backward = pages(&api.list().newest().limit(4));
    assert_eq!(backward, [vec!["9", "8", "7", "6"], vec!["5", "4", "3", "2"], vec!["1", "0"]]);
}

#[test]
fn a_page_never_splits_a_time_and_more_at_one_time_than_it_holds_is_limit() {
    let fixture = fixture();
    let api = fixture.events("api");
    let now = fixture.now();
    let at = |name: &str, seconds: u64| NewEvent::new(name).at(now - Duration::from_secs(seconds));
    api.append_all([at("a", 3), at("b1", 2), at("b2", 2), at("b3", 2), at("c", 1)]).unwrap();
    assert_eq!(pages(&api.list().limit(3)), [vec!["a"], vec!["b1", "b2", "b3"], vec!["c"]]);
    assert_eq!(pages(&api.list().newest().limit(3)), [vec!["c"], vec!["b3", "b2", "b1"], vec!["a"]]);
    let refused = api.list().limit(2).after(&api.list().limit(1).page().unwrap().next.unwrap()).page().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Limit);
}

#[test]
fn a_page_ends_at_the_rows_it_fetched_and_the_next_goes_on_from_there() {
    let fixture = fixture();
    let api = fixture.events("api");
    // a row a second, each of two events: a page of three takes two rows and is whole up to the third
    for second in 0..4_u32 {
        let name = |half: &str| NewEvent::new(&format!("{second}{half}"));
        api.append_all([name("a"), name("b").at(fixture.now() + Duration::from_millis(500))]).unwrap();
        fixture.pass(SECOND);
    }
    let read = pages(&api.list().limit(3));
    assert_eq!(read, [vec!["0a", "0b", "1a"], vec!["1b", "2a", "2b"], vec!["3a", "3b"]]);
    let warn = api.list().min_level(Level::WARN).limit(3);
    assert_eq!(pages(&warn), [Vec::<String>::new()]);
}

#[test]
fn rows_that_reach_over_a_ranges_end_are_all_read_so_that_the_next_page_begins_past_this_one() {
    let fixture = fixture();
    let api = fixture.events("api");
    let now = fixture.now();
    // three rows, each with an event before the range, one in it and one after it
    for n in 0..3_u32 {
        let outside = [NewEvent::new("before").at(now - SECOND * 60), NewEvent::new("after").at(now + SECOND * 60)];
        let inside = NewEvent::new(&n.to_string()).at(now - SECOND * (3 - n));
        api.append_all(outside.into_iter().chain([inside])).unwrap();
    }
    let ranged = api.list().from(now - SECOND * 10).to(now + SECOND * 10).limit(1);
    assert_eq!(pages(&ranged), [vec!["0"], vec!["1"], vec!["2"]]);
    assert_eq!(pages(&ranged.newest()), [vec!["2"], vec!["1"], vec!["0"]]);
}

#[test]
fn a_range_is_a_span_before_now_counted_once_or_a_start_in_it_and_an_end_not() {
    let fixture = fixture();
    let api = fixture.events("api");
    timeline(&fixture, &api, 10);
    let now = fixture.now();
    assert_eq!(names(&api.list().since(SECOND * 3).page().unwrap().events), ["6", "7", "8", "9"]);
    let ranged = api.list().from(now - SECOND * 5).to(now - SECOND * 2);
    assert_eq!(names(&ranged.page().unwrap().events), ["4", "5", "6"]);

    let first = api.list().since(SECOND * 5).limit(2).page().unwrap();
    assert_eq!(names(&first.events), ["4", "5"]);
    fixture.pass(SECOND * 3);
    let second = api.list().since(SECOND * 5).limit(2).after(&first.next.unwrap()).page().unwrap();
    assert_eq!(names(&second.events), ["6", "7"]);

    let both = api.list().since(SECOND).from(now).page().unwrap_err();
    let no_next = api.list().after("yesterday").page().unwrap_err();
    let no_page = [api.list().limit(0).page().unwrap_err(), api.list().limit(10_001).page().unwrap_err()];
    for refused in [both, no_next].iter().chain(&no_page) {
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    }
}

#[test]
fn all_walks_every_event_a_page_at_a_time() {
    let fixture = fixture();
    let api = fixture.events("api");
    timeline(&fixture, &api, 25);
    let walked: Vec<String> = api.list().limit(10).all().map(|event| event.unwrap().name).collect();
    assert_eq!(walked, (0..25).map(|n| n.to_string()).collect::<Vec<_>>());
    let stopped: Vec<String> = api.list().newest().limit(10).all().take(3).map(|event| event.unwrap().name).collect();
    assert_eq!(stopped, ["24", "23", "22"]);
}

/// A stream with a logger's lines and events of other names.
fn mixed(fixture: &Fixture) -> Events {
    let api = fixture.events("api");
    let line = |level: Level, message: &str| NewEvent::new("log").level(level).message(message);
    api.append_all([
        line(Level::INFO, "listening").fields(&json!({ "port": 3000 })),
        line(Level::WARN, "slow request").fields(&json!({ "route": "/notes", "ms": 1200 })),
        line(Level::ERROR, "Charge failed").fields(&json!({ "order": 7, "error": { "cause": ["ECONNRESET", 104] } })),
        NewEvent::new("signup").fields(&json!({ "plan": "pro", "user": 42 })),
        NewEvent::new("role.changed").fields(&json!({ "role": "admin" })).context(&json!({ "user": 42 })),
        NewEvent::new("click").fields(&json!({ "user": "42", "x": 42.0 })).trace(TraceId([5; 16]), SpanId([6; 8])),
    ])
    .unwrap();
    api
}

#[test]
fn a_name_a_level_and_a_trace_find_their_events() {
    let fixture = fixture();
    let api = mixed(&fixture);
    let found = |list: List| names(&list.page().unwrap().events).into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(found(api.list().names(["signup", "click"])), ["signup", "click"]);
    assert_eq!(found(api.list().names(["log"])).len(), 3);
    let grave = api.list().min_level(Level::WARN).page().unwrap().events;
    assert_eq!(
        grave.iter().map(|event| event.message.as_deref().unwrap()).collect::<Vec<_>>(),
        ["slow request", "Charge failed"]
    );
    // an event without a level is found by no level
    assert_eq!(api.list().min_level(Level::TRACE).page().unwrap().events.len(), 3);
    assert_eq!(found(api.list().trace_id(TraceId([5; 16]))), ["click"]);
    assert!(found(api.list().trace_id(TraceId([9; 16]))).is_empty());
}

#[test]
fn a_filter_finds_a_key_in_the_fields_or_the_context_by_the_json_of_its_value() {
    let fixture = fixture();
    let api = mixed(&fixture);
    let found = |list: List| names(&list.page().unwrap().events).into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(found(api.list().filter("user", &42)), ["signup", "role.changed"]);
    assert_eq!(found(api.list().filter("user", &"42")), ["click"]);
    assert_eq!(found(api.list().filter("x", &42.0)), ["click"]);
    assert!(found(api.list().filter("x", &42)).is_empty());
    assert_eq!(found(api.list().filter("user", &42).filter("plan", &"pro")), ["signup"]);
    // a key is the top's own: nothing looks into a value for it
    assert!(found(api.list().filter("cause", &["ECONNRESET"])).is_empty());
}

#[test]
fn a_search_finds_its_phrase_in_the_message_the_name_and_the_values_whatever_its_case() {
    let fixture = fixture();
    let api = mixed(&fixture);
    let found = |text: &str| {
        let events = api.list().search(text).page().unwrap().events;
        events.iter().map(|event| event.message.clone().unwrap_or_else(|| event.name.clone())).collect::<Vec<_>>()
    };
    assert_eq!(found("charge FAILED"), ["Charge failed"]);
    assert_eq!(found("econnreset"), ["Charge failed"]);
    assert_eq!(found("104"), ["Charge failed"]);
    assert_eq!(found("Role."), ["role.changed"]);
    assert_eq!(found("/NOTES"), ["slow request"]);
    assert_eq!(found("admin"), ["role.changed"]);
    // a key is no value, and a phrase is whole
    assert!(found("route").is_empty());
    assert!(found("cause").is_empty());
    assert!(found("failed charge").is_empty());
}

/// What three reads take over a corpus the Go engine wrote, packed: an exact
/// filter, a single trace and an absent context value. Run by hand with
/// `TINYSTORE_READS=<the lines>` and `--release -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run by hand"]
fn three_reads_of_a_packed_corpus() {
    #[derive(serde::Deserialize)]
    struct Line {
        at: u64,
        name: String,
        level: i32,
        body: String,
        fields: Vec<(String, String)>,
        context: Vec<(String, String)>,
        trace: Option<String>,
        span: Option<String>,
    }
    let Ok(path) = std::env::var("TINYSTORE_READS") else { return };
    let object = |pairs: &[(String, String)]| {
        let pairs: Vec<String> =
            pairs.iter().map(|(key, json)| format!("{}:{json}", serde_json::Value::from(key.as_str()))).collect();
        serde_json::value::RawValue::from_string(format!("{{{}}}", pairs.join(","))).unwrap()
    };
    let bytes = |text: &str| -> Vec<u8> {
        (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
    };
    let fixture = fixture();
    let api = fixture.events("api");
    let base = fixture.now() - Duration::from_secs(30 * 60);
    let text = std::fs::read_to_string(path).unwrap();
    let mut events = Vec::new();
    let mut trace = None;
    for (place, line) in text.lines().enumerate() {
        let line: Line = serde_json::from_str(line).unwrap();
        let mut event = NewEvent::new(&line.name).at(base + Duration::from_nanos(line.at)).level(Level::of(line.level));
        event = event.message(&line.body).fields(&object(&line.fields)).context(&object(&line.context));
        if let (Some(trace_id), Some(span)) = (&line.trace, &line.span) {
            let (trace_id, span) =
                (TraceId(bytes(trace_id).try_into().unwrap()), SpanId(bytes(span).try_into().unwrap()));
            if place == 64 * 500 {
                trace = Some(trace_id);
            }
            event = event.trace(trace_id, span);
        }
        events.push(event);
    }
    while !events.is_empty() {
        let rest = events.split_off(events.len().min(1000));
        api.append_all(std::mem::replace(&mut events, rest)).unwrap();
    }
    fixture.pass(Duration::from_secs(2 * 60 * 60));
    let done = super::super::maintain::maintain(&fixture.store).unwrap();
    println!("rust packed {} events into {} segments", done.packed, done.segments);
    let read = |name: &str| match name {
        "exact" => api.list().names(["request"]).filter("route", &"/route/7").filter("status", &500),
        "trace" => api.list().trace_id(trace.unwrap()),
        _ => api.list().filter("host", &"nowhere"),
    };
    for name in ["exact", "trace", "absent"] {
        let run = || read(name).limit(10_000).all().map(Result::unwrap).count();
        let found = (0..3).fold(0, |_, _| run());
        let mut runs: Vec<Duration> = (0..15)
            .map(|_| {
                let started = std::time::Instant::now();
                (0..10).for_each(|_| assert_eq!(run(), found));
                started.elapsed() / 10
            })
            .collect();
        runs.sort();
        let ms = |span: Duration| span.as_secs_f64() * 1000.0;
        println!("rust read={name:<7} found={found:<5} median={:.3}ms min={:.3}ms", ms(runs[7]), ms(runs[0]));
    }
}

#[test]
fn a_read_passes_a_block_whose_segment_or_filters_say_it_holds_nothing_asked() {
    let fixture = fixture();
    let api = fixture.events("api");
    let traced = TraceId([7; 16]);
    let request = |n: usize| {
        let event =
            NewEvent::new("request").fields(&json!({ "request": format!("r-{n:03}"), "user": format!("u-{n:03}") }));
        let event = event.context(&json!({ "host": "web", "user": "from-context" }));
        if n == 5 { event.trace(traced, SpanId([1; 8])) } else { event }
    };
    api.append_all((0..32).map(request)).unwrap();
    fixture.pass(Duration::from_secs(60 * 60));
    super::super::maintain::maintain(&fixture.store).unwrap();
    let records = fixture.records();
    let block = |connection: &Connection| {
        let row = connection.query_row("select id, segment from blocks", [], |row| Ok((row.get(0)?, row.get(1)?)));
        Ok(row.unwrap())
    };
    let (id, segment) = records.file.read(block).unwrap();
    let picked = Picked { kind: Kind::Block, id, segment, stream: 1, first_at: 0, last_at: 0, events: 32, size: 0 };
    let may_find = |list: List| {
        let query = &list.query;
        records
            .file
            .read(|connection| Passing { query, connection, segments: HashMap::new() }.may_find(&picked))
            .unwrap()
    };
    assert!(may_find(api.list().trace_id(traced)));
    assert!(!may_find(api.list().trace_id(TraceId([8; 16]))));
    assert!(may_find(api.list().filter("request", &"r-005")));
    assert!(!may_find(api.list().filter("request", &"r-999")));
    assert!(!may_find(api.list().names(["missing"])));
    assert!(!may_find(api.list().filter("tenant", &7)));
    // a context holds no filter of its own, so a value it may hold passes no block
    assert!(may_find(api.list().filter("host", &"nowhere")));
    // nor does a field whose key a context has too
    assert!(may_find(api.list().filter("user", &"from-context")));
    assert!(may_find(api.list().names(["missing", "request"]).filter("request", &"r-031")));
}
