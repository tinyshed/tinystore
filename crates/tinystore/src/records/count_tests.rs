use std::time::{Duration, SystemTime};

use serde_json::json;

use super::super::engine::KEEP;
use super::super::events::Events;
use super::super::fixture::{DAY, Fixture, SECOND, fixture};
use super::super::maintain::maintain;
use super::*;
use crate::ErrorKind;
use crate::records::{Level, NewEvent};

const HOUR: Duration = Duration::from_secs(60 * 60);
const MINUTE: Duration = Duration::from_secs(60);

/// Sixty events of api three minutes apart from three hours ago, packed an
/// hour on, and one more that waits: an error on `/notes`. Gives the stream
/// and when its first event was.
fn api(fixture: &Fixture) -> (Events, SystemTime) {
    let api = fixture.events("api");
    let base = fixture.now() - 3 * HOUR;
    let event = |n: u32| {
        let level = if n % 10 == 9 { Level::ERROR } else { Level::INFO };
        let name = if n.is_multiple_of(5) { "cache" } else { "request" };
        let route = ["/notes", "/buy", "/login"][n as usize % 3];
        let fields = json!({ "batch": 1, "ms": n * 10, "route": route });
        NewEvent::new(name).at(base + 3 * MINUTE * n).level(level).fields(&fields)
    };
    api.append_all((0..60).map(event)).unwrap();
    fixture.pass(HOUR);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1);
    let waiting = NewEvent::new("request").level(Level::ERROR).fields(&json!({ "ms": 5, "route": "/notes" }));
    api.append_all([waiting]).unwrap();
    (api, base)
}

#[test]
fn a_count_finds_what_a_list_finds_packed_and_waiting() {
    let fixture = fixture();
    let (api, _) = api(&fixture);
    assert_eq!(api.list().count().unwrap(), 61);
    assert_eq!(api.list().min_level(Level::ERROR).count().unwrap(), 7);
    assert_eq!(api.list().filter("route", &"/notes").count().unwrap(), 21);
    assert_eq!(api.list().since(MINUTE).count().unwrap(), 1);
    for refused in [api.list().newest(), api.list().limit(5), api.list().after("1~2")] {
        let refused = refused.count().unwrap_err();
        assert_eq!((refused.kind(), refused.to_string().contains("count takes no")), (ErrorKind::Invalid, true));
    }
}

#[test]
fn top_gives_a_keys_most_frequent_values_first() {
    let fixture = fixture();
    let (api, _) = api(&fixture);
    let top = |value: serde_json::Value, count: u64| Top { value, count };
    let routes = [top(json!("/notes"), 21), top(json!("/buy"), 20), top(json!("/login"), 20)];
    assert_eq!(api.list().top("route").unwrap(), routes);
    assert_eq!(api.list().limit(1).top("name").unwrap(), [top(json!("request"), 49)]);
    assert_eq!(api.list().top("level").unwrap(), [top(json!("info"), 54), top(json!("error"), 7)]);
    // values as frequent as each other come in the order of their JSON
    let ms: Vec<serde_json::Value> =
        api.list().min_level(Level::ERROR).top("ms").unwrap().into_iter().map(|top| top.value).collect();
    assert_eq!(ms, [json!(190), json!(290), json!(390), json!(490), json!(5), json!(590), json!(90)]);
    assert!(api.list().top("missing").unwrap().is_empty());
    let all = fixture.store.all_events().unwrap();
    assert_eq!(all.list().top("stream").unwrap(), [top(json!("api"), 61)]);
    assert_eq!(api.list().limit(1001).top("route").unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(api.list().newest().top("route").unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn an_aggregate_gives_a_row_a_span_and_a_value_of_its_keys() {
    let fixture = fixture();
    let (api, base) = api(&fixture);
    let asked = Aggregate::new().every(HOUR).by(["level"]).stats(["ms"]).distinct(["route"]);
    let rows = api.list().aggregate(&asked).unwrap();
    // three hours of 2 errors and 18 infos, then the one that waits; an hour without events has no row
    let counts: Vec<(SystemTime, u64)> = rows.iter().map(|row| (row.at, row.count)).collect();
    let hour = |n: u32| base + HOUR * n;
    assert_eq!(
        counts,
        [(hour(0), 2), (hour(0), 18), (hour(1), 2), (hour(1), 18), (hour(2), 2), (hour(2), 18), (hour(4), 1)]
    );
    let first = &rows[0];
    assert_eq!(first.by, BTreeMap::from([("level".to_owned(), json!("error"))]));
    let ms = Stats {
        samples: 2,
        sum: Number::Integer(280),
        min: Number::Integer(90),
        max: Number::Integer(190),
        avg: 140.0,
    };
    assert_eq!(first.stats, BTreeMap::from([("ms".to_owned(), ms)]));
    assert_eq!(first.distinct, BTreeMap::from([("route".to_owned(), 2)]));
    // without every the range is one span, and without by a span one row
    let whole = api.list().since(4 * HOUR).aggregate(&Aggregate::new()).unwrap();
    assert_eq!(whole.iter().map(|row| (row.at, row.count)).collect::<Vec<_>>(), [(fixture.now() - 4 * HOUR, 61)]);
    assert_eq!(api.list().aggregate(&Aggregate::new().every(Duration::ZERO)).unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn a_sum_of_integers_is_exact_and_a_fraction_makes_it_a_floats() {
    let fixture = fixture();
    let big = fixture.events("big");
    let huge = 4_611_686_018_427_387_904_i64;
    big.append_all((0..3).map(|_| NewEvent::new("n").fields(&json!({ "n": huge })))).unwrap();
    let sum = |list: crate::records::List| list.aggregate(&Aggregate::new().stats(["n"])).unwrap()[0].stats["n"].sum;
    assert_eq!(sum(big.list()), Number::Integer(3 * i128::from(huge)));
    big.append_all([NewEvent::new("n").fields(&json!({ "n": 1.5 })), NewEvent::new("n").fields(&json!({ "n": "7" }))])
        .unwrap();
    let stats = big.list().aggregate(&Aggregate::new().stats(["n"])).unwrap()[0].stats["n"];
    // the string is no number, and a field called count meets nothing of the row's own
    assert_eq!((stats.samples, stats.min, stats.max), (4, Number::Float(1.5), Number::Integer(i128::from(huge))));
    assert_eq!(stats.sum, Number::Float(3.0 * huge as f64 + 1.5));
}

#[test]
fn an_aggregate_past_ten_thousand_rows_is_limit_and_never_a_smaller_answer() {
    let fixture = fixture();
    let ticks = fixture.events("ticks");
    let start = fixture.now() - HOUR;
    for batch in 0..11_u32 {
        let tick = |n: u32| NewEvent::new("tick").at(start + Duration::from_millis(u64::from(batch * 1000 + n)));
        ticks.append_all((0..1000).map(tick)).unwrap();
    }
    let every = Aggregate::new().every(Duration::from_millis(1));
    assert_eq!(ticks.list().aggregate(&every).unwrap_err().kind(), ErrorKind::Limit);
    assert_eq!(ticks.list().aggregate(&Aggregate::new().every(SECOND)).unwrap().len(), 11);
}

#[test]
fn streams_are_those_with_events_with_what_they_keep_and_their_oldest_and_newest() {
    let fixture = fixture();
    let (_, base) = api(&fixture);
    fixture.store.events("quiet").keep(DAY).open().unwrap();
    let all = fixture.store.all_events().unwrap();
    let streams = all.streams().unwrap();
    let api = StreamInfo { name: "api".to_owned(), keep: KEEP, oldest: base, newest: fixture.now() };
    assert_eq!(streams, [api]);
}

#[test]
fn field_names_are_the_keys_of_fields_and_contexts_sorted() {
    let fixture = fixture();
    let (api, _) = api(&fixture);
    api.append_all([NewEvent::new("request").context(&json!({ "host": "web" })).fields(&json!({ "user": 7 }))])
        .unwrap();
    assert_eq!(api.list().field_names().unwrap(), ["batch", "host", "ms", "route", "user"]);
    assert_eq!(api.list().field_names_prefix("r").unwrap(), ["route"]);
    assert_eq!(api.list().limit(2).field_names().unwrap(), ["batch", "host"]);
    // the packed segment lies wholly before the last minute, and gives nothing to it
    assert_eq!(api.list().since(MINUTE).field_names().unwrap(), ["host", "ms", "route", "user"]);
    let all = fixture.store.all_events().unwrap();
    assert_eq!(all.list().field_names_prefix("u").unwrap(), ["user"]);
}

#[test]
fn what_a_count_holds_of_different_values_is_bounded_in_bytes_and_a_sum_past_128_bits_is_limit() {
    let mut held = Held { bytes: 0, most: 200 };
    let value = "\"0123456789012345678901234567890123456789\"";
    // a value it holds already weighs nothing more; a new one its JSON and its map's
    held.add(value, true).unwrap();
    held.add(value, false).unwrap();
    held.add("\"another\"", true).unwrap();
    assert_eq!(held.add("\"a third\"", true).unwrap_err().kind(), ErrorKind::Limit);
    let mut summing = Summing::default();
    summing.add(Number::Integer(i128::MAX)).unwrap();
    assert_eq!(summing.add(Number::Integer(1)).unwrap_err().kind(), ErrorKind::Limit);
}
