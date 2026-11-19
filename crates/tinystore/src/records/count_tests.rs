use std::time::{Duration, SystemTime};

use serde_json::{Map as JsonMap, json};

use super::super::engine::KEEP;
use super::super::events::Events;
use super::super::fixture::{DAY, Fixture, SECOND, fixture};
use super::super::maintain::maintain;
use super::*;
use crate::records::{Event, Level, NewEvent};
use crate::{Calendar, ErrorKind};

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

// contract: REC-06
#[test]
fn a_count_finds_what_a_list_finds_packed_and_waiting() {
    let fixture = fixture();
    let (api, _) = api(&fixture);
    assert_eq!(api.list().count().unwrap(), 61);
    assert_eq!(api.list().min_level(Level::ERROR).count().unwrap(), 7);
    assert_eq!(api.list().filter("route", "/notes").count().unwrap(), 21);
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

// contract: REC-19
#[test]
fn largest_gives_the_events_of_a_keys_largest_numbers_first_those_of_one_number_in_read_order() {
    let fixture = fixture();
    let (api, _) = api(&fixture);
    let ms = |events: Vec<Event>| -> Vec<String> {
        events.iter().map(|event| event.fields.json("ms").unwrap().to_owned()).collect()
    };
    assert_eq!(
        ms(api.list().largest("ms").unwrap()),
        ["590", "580", "570", "560", "550", "540", "530", "520", "510", "500"]
    );
    // packed and waiting alike: the event that waits holds 5
    assert_eq!(ms(api.list().min_level(Level::ERROR).limit(3).largest("ms").unwrap()), ["590", "490", "390"]);
    assert_eq!(ms(api.list().filter("route", "/notes").limit(1000).largest("ms").unwrap()).last().unwrap(), "0");
    // a key with no number in any event finds none; a limit past 1000, newest and after are refused
    assert!(api.list().largest("route").unwrap().is_empty());
    for refused in [api.list().limit(1001), api.list().limit(0), api.list().newest(), api.list().after("1~2")] {
        assert_eq!(refused.largest("ms").unwrap_err().kind(), ErrorKind::Invalid);
    }

    let pages = fixture.events("pages");
    let at = fixture.now() - MINUTE;
    let open = |n: u32, ms: serde_json::Value| {
        NewEvent::new("page-open").at(at + SECOND * n).fields(&json!({ "n": n, "ms": ms }))
    };
    pages
        .append_all([
            open(0, json!(9_007_199_254_740_992.0)),
            open(1, json!(9_007_199_254_740_993_u64)),
            open(2, json!(300)),
            open(3, json!("9000")),
            open(4, json!(300)),
            open(5, json!(300.5)),
            open(6, json!(-1)),
        ])
        .unwrap();
    pages.append_all([NewEvent::new("page-open").at(at).context(&json!({ "ms": 301 }))]).unwrap();
    let order: Vec<String> = pages
        .list()
        .limit(6)
        .largest("ms")
        .unwrap()
        .iter()
        .map(|event| event.fields.json("n").unwrap_or("context").to_owned())
        .collect();
    // an integer past 2⁵³ above the float below it; a context's number counts; text is passed over; ties as read
    assert_eq!(order, ["1", "0", "context", "5", "2", "4"]);
}

#[test]
fn an_integer_and_a_float_compare_exactly() {
    let (int, float) = (Number::Integer, Number::Float);
    assert_eq!(compare(int(9_007_199_254_740_993), float(9_007_199_254_740_992.0)), Ordering::Greater);
    assert_eq!(compare(int(3), float(3.0)), Ordering::Equal);
    assert_eq!(compare(int(-3), float(-2.5)), Ordering::Less);
    assert_eq!(compare(float(-3.5), int(-3)), Ordering::Less);
    assert_eq!(compare(int(i128::MAX), float(1e39)), Ordering::Less);
    assert_eq!(compare(int(i128::MIN), float(-1e39)), Ordering::Greater);
}

#[test]
fn top_and_by_read_message_as_the_events_own_and_a_field_of_that_name_without_one() {
    let fixture = fixture();
    let errors = fixture.events("client-errors");
    errors
        .append_all([
            NewEvent::new("error").message("TypeError: x is undefined"),
            NewEvent::new("error").message("TypeError: x is undefined").fields(&json!({ "message": "other" })),
            NewEvent::new("error").fields(&json!({ "message": "copied" })),
        ])
        .unwrap();
    let top = |value: &str, count: u64| Top { value: json!(value), count };
    assert_eq!(errors.list().top("message").unwrap(), [top("TypeError: x is undefined", 2), top("copied", 1)]);
    let rows = errors.list().aggregate(&Aggregate::new().by(["message"]).distinct(["message"])).unwrap();
    assert_eq!(
        rows.iter().map(|row| (row.by["message"].clone(), row.count)).collect::<Vec<_>>(),
        [(json!("TypeError: x is undefined"), 2), (json!("copied"), 1)]
    );
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

// contract: REC-06
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

// contract: ERR-05
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
fn streams_are_those_with_events_with_what_they_keep_where_it_begins_and_their_newest() {
    let fixture = fixture();
    let (_, base) = api(&fixture);
    fixture.store.events("quiet").keep(DAY).open().unwrap();
    let all = fixture.store.all_events().unwrap();
    let streams = all.streams().unwrap();
    let api = StreamInfo { name: "api".to_owned(), keep: KEEP, retained_from: base, newest: fixture.now() };
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
fn field_names_are_the_first_keys_of_every_segment_and_head_in_their_order() {
    let fixture = fixture();
    let api = fixture.events("api");
    let event = |keys: &[&str]| {
        NewEvent::new("e").fields(
            &keys.iter().map(|key| ((*key).to_owned(), json!(1))).collect::<JsonMap<String, serde_json::Value>>(),
        )
    };
    for keys in [["b1", "d1", "f1"], ["a2", "c2", "e2"], ["b3", "c3", "g3"]] {
        api.append_all([event(&keys)]).unwrap();
        fixture.pass(HOUR);
        maintain(&fixture.store).unwrap();
    }
    api.append_all([event(&["a4", "h4"])]).unwrap();
    assert_eq!(fixture.rows("segments"), 3);
    assert_eq!(api.list().limit(4).field_names().unwrap(), ["a2", "a4", "b1", "b3"]);
    assert_eq!(api.list().field_names_prefix("c").unwrap(), ["c2", "c3"]);
    assert_eq!(api.list().limit(1).field_names_prefix("b").unwrap(), ["b1"]);
    assert_eq!(api.list().field_names_prefix("h").unwrap(), ["h4"]);
    assert!(api.list().field_names_prefix("z").unwrap().is_empty());
}

// contract: ERR-05
#[test]
fn field_names_hold_the_heads_they_read_of_the_stores_memory() {
    let dir = tempfile::tempdir().unwrap();
    let options = crate::Options { memory: Some(1 << 20), background: false, ..crate::Options::default() };
    let store = crate::Store::open(dir.path(), options).unwrap();
    let api = store.events("api").open().unwrap();
    for _ in 0..11 {
        api.append_all((0..100).map(|n| NewEvent::new("e").fields(&json!({ "n": n })))).unwrap();
    }
    // 1,100 events waiting, which a read holds a KiB each of while it parses them
    let error = api.list().field_names().unwrap_err();
    assert_eq!(error.fact("limit"), Some(crate::memory::STORE_MEMORY), "{error}");
    assert_eq!(api.list().since(MINUTE).limit(1).field_names_prefix("n").unwrap_err().kind(), ErrorKind::Limit);
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

// contract: REC-16, REC-17
#[test]
fn fill_gives_a_row_for_every_span_of_the_range_and_a_calendars_rows_their_local_date() {
    let fixture = fixture();
    // 2027-01-12 00:00 in Moscow, a Tuesday three days before the fixture's clock
    let midnight = std::time::UNIX_EPOCH + Duration::from_secs(1_799_701_200);
    let orders = fixture.events("orders");
    orders.append_all((0..48).map(|hour| NewEvent::new("order").at(midnight + HOUR * hour))).unwrap();
    let rows = |list: List, asked: Aggregate| {
        let rows = list.aggregate(&asked).unwrap();
        rows.iter().map(|row| (row.at, row.count, row.date.map(|date| date.to_string()))).collect::<Vec<_>>()
    };
    let around = || orders.list().from(midnight - DAY).to(midnight + 3 * DAY);
    let day = |n: u32, count: u64, date: &str| (midnight - DAY + DAY * n, count, Some(date.to_owned()));
    let days = rows(around(), Aggregate::new().calendar(Calendar::Day, "Europe/Moscow").fill());
    assert_eq!(
        days,
        [day(0, 0, "2027-01-11"), day(1, 24, "2027-01-12"), day(2, 24, "2027-01-13"), day(3, 0, "2027-01-14")]
    );
    // without fill a span without events has no row, and a calendar's row its date all the same
    assert_eq!(rows(around(), Aggregate::new().calendar(Calendar::Day, "Europe/Moscow")).len(), 2);
    let weeks = rows(around(), Aggregate::new().calendar(Calendar::Week, "Europe/Moscow").fill());
    assert_eq!(weeks, [(midnight - DAY, 48, Some("2027-01-11".to_owned()))], "a week from its Monday");
    let months = rows(around(), Aggregate::new().calendar(Calendar::Month, "Europe/Moscow"));
    assert_eq!(months[0].2.as_deref(), Some("2027-01-01"));
    // every has no date, a UTC span needing no zone
    assert!(rows(around(), Aggregate::new().every(DAY).fill()).iter().all(|(_, _, date)| date.is_none()));
    // a by fills every value the answer has; the whole range is one row, empty or not
    let hours = orders.list().from(midnight - 2 * HOUR).to(midnight + HOUR);
    let by = hours.clone().aggregate(&Aggregate::new().every(HOUR).by(["name"]).stats(["n"]).distinct(["n"]).fill());
    let by = by.unwrap();
    assert_eq!(by.iter().map(|row| row.count).collect::<Vec<_>>(), [0, 0, 1]);
    assert!(by[0].stats.is_empty() && by[0].distinct["n"] == 0 && by[0].by["name"] == json!("order"));
    let before = orders.list().from(midnight - 2 * HOUR).to(midnight);
    assert_eq!(rows(before.clone(), Aggregate::new().fill()), [(midnight - 2 * HOUR, 0, None)]);
    assert!(rows(before, Aggregate::new().by(["name"]).fill()).is_empty(), "no value of by to fill");
}

// contract: REC-16
#[test]
fn fill_needs_a_start_keeps_to_what_the_stream_keeps_and_is_bounded_before_the_read() {
    let fixture = fixture();
    let short = fixture.store.events("short").keep(DAY).open().unwrap();
    short.append_all([NewEvent::new("call")]).unwrap();
    let refused = short.list().aggregate(&Aggregate::new().every(HOUR).fill()).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    // a range from three days back fills from the day the stream keeps, up to now
    let rows = short.list().since(3 * DAY).aggregate(&Aggregate::new().every(HOUR).fill()).unwrap();
    assert!((24..=25).contains(&rows.len()), "{} rows", rows.len());
    assert_eq!(rows.iter().map(|row| row.count).sum::<u64>(), 1);
    let seconds = short.list().since(DAY).aggregate(&Aggregate::new().every(SECOND).fill()).unwrap_err();
    assert_eq!(seconds.kind(), ErrorKind::Limit, "{seconds}");
}

#[test]
fn calendar_rows_begin_at_local_midnight_and_every_and_a_calendar_together_are_refused() {
    let fixture = fixture();
    // 2027-01-12 00:00 in Moscow, three days before the fixture's clock: an order an hour for two days
    let midnight = std::time::UNIX_EPOCH + Duration::from_secs(1_799_701_200);
    let orders = fixture.events("orders");
    let placed = (0..48).map(|hour| NewEvent::new("order").at(midnight + HOUR * hour));
    orders.append_all(placed).unwrap();
    let days = |asked: Aggregate| {
        let rows = orders.list().from(midnight).to(midnight + 2 * DAY).aggregate(&asked).unwrap();
        rows.iter().map(|row| (row.at, row.count)).collect::<Vec<_>>()
    };
    let local = days(Aggregate::new().calendar(Calendar::Day, "Europe/Moscow"));
    assert_eq!(local, [(midnight, 24), (midnight + DAY, 24)]);
    let utc: Vec<u64> = days(Aggregate::new().every(DAY)).into_iter().map(|(_, count)| count).collect();
    assert_eq!(utc, [3, 24, 21], "days of UTC cut at 03:00 in Moscow");
    let both = Aggregate::new().every(DAY).calendar(Calendar::Day, "UTC");
    let refused = orders.list().from(midnight).aggregate(&both).unwrap_err();
    assert!(refused.to_string().contains("`every` or `calendar`, not both"), "{refused}");
}
