use std::time::Duration;

use serde::Serialize;
use serde_json::json;

use super::super::event::Event;
use super::super::fixture::{DAY, Fixture, SECOND, carried_by, fixture};
use super::super::maintain::maintain;
use super::*;
use crate::records::NewEvent;

const HOUR: Duration = Duration::from_secs(60 * 60);

#[derive(Serialize)]
struct Order {
    order_id: i64,
    amount: f64,
    currency: &'static str,
    paid: bool,
}

#[derive(Serialize)]
struct Deploy {
    service: &'static str,
    version: &'static str,
    took_ms: i64,
}

/// The issue's store: a hundred orders paid and one deploy.
fn orders_and_a_deploy(fixture: &Fixture) {
    let (orders, deploys) = (fixture.events("orders"), fixture.events("deploys"));
    for order_id in 0..100 {
        let order = Order { order_id, amount: 12.5, currency: "EUR", paid: order_id % 3 != 0 };
        orders.append("order.paid", &order).unwrap();
    }
    deploys.append("deploy.done", &Deploy { service: "api", version: "1.4.2", took_ms: 5200 }).unwrap();
}

fn field(name: &str, kinds: &[Kind], count: u64) -> FieldInfo {
    FieldInfo { name: name.to_owned(), kinds: kinds.to_vec(), count }
}

fn named(name: &str, count: u64) -> NameCount {
    NameCount { name: name.to_owned(), count }
}

/// What a stream's events carry, as a program counts it reading every one.
fn counted_from(fixture: &Fixture, stream: &str) -> StreamFields {
    carried_by(&fixture.events(stream).list().all().map(Result::unwrap).collect::<Vec<_>>())
}

fn fields_of(fixture: &Fixture, stream: &str) -> StreamFields {
    fixture.store.all_events().unwrap().fields(stream).unwrap()
}

#[test]
fn a_kind_is_read_from_how_its_json_is_spelled() {
    let kinds = ["12.5", "1e3", "-2.0E-3", "42", "-7", "\"7\"", "true", "false", "{\"a\":1}", "[1]", "null"];
    let expected = [
        Kind::Float,
        Kind::Float,
        Kind::Float,
        Kind::Integer,
        Kind::Integer,
        Kind::Text,
        Kind::Bool,
        Kind::Bool,
        Kind::Object,
        Kind::List,
        Kind::Null,
    ];
    assert_eq!(kinds.map(Kind::of), expected);
    let names: Vec<&str> = Kind::ALL.iter().map(|kind| kind.name()).collect();
    assert_eq!(names, ["float", "integer", "text", "bool", "object", "list", "null"]);
    let bits: Vec<u8> = Kind::ALL.iter().map(|kind| kind.bit()).collect();
    assert_eq!(bits, [1, 2, 4, 8, 16, 32, 64], "the file keeps them");
}

// contract: REC-14
#[test]
fn a_stream_says_which_names_and_fields_its_events_carry_before_and_after_they_are_packed() {
    let fixture = fixture();
    orders_and_a_deploy(&fixture);
    let orders = StreamFields {
        event_names: vec![named("order.paid", 100)],
        fields: vec![
            field("amount", &[Kind::Float], 100),
            field("currency", &[Kind::Text], 100),
            field("order_id", &[Kind::Integer], 100),
            field("paid", &[Kind::Bool], 100),
        ],
        context: Vec::new(),
    };
    let deploys = StreamFields {
        event_names: vec![named("deploy.done", 1)],
        fields: vec![
            field("service", &[Kind::Text], 1),
            field("took_ms", &[Kind::Integer], 1),
            field("version", &[Kind::Text], 1),
        ],
        context: Vec::new(),
    };
    assert_eq!((fields_of(&fixture, "orders"), fields_of(&fixture, "deploys")), (orders.clone(), deploys.clone()));
    fixture.pass(HOUR);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 2);
    assert_eq!(fixture.rows("heads"), 0);
    assert_eq!((fields_of(&fixture, "orders"), fields_of(&fixture, "deploys")), (orders, deploys));
    assert_eq!(fields_of(&fixture, "nothing"), StreamFields::default());
}

// contract: REC-14
#[test]
fn a_key_counts_once_an_event_with_every_kind_its_values_have_and_a_context_apart() {
    let fixture = fixture();
    let api = fixture.events("api");
    let twice = Fields::read(r#"{"tag": "a", "tag": 7, "ms": 12}"#).unwrap();
    let mut first = NewEvent::new("request").context(&json!({ "host": "web", "tag": [1] }));
    first.fields = twice;
    let second = NewEvent::new("request").fields(&json!({ "ms": 12.5, "error": null, "user": { "id": 1 } }));
    api.append_all([first, second]).unwrap();
    let expected = StreamFields {
        event_names: vec![named("request", 2)],
        fields: vec![
            field("error", &[Kind::Null], 1),
            field("ms", &[Kind::Float, Kind::Integer], 2),
            field("tag", &[Kind::Integer, Kind::Text], 1),
            field("user", &[Kind::Object], 1),
        ],
        context: vec![field("host", &[Kind::Text], 1), field("tag", &[Kind::List], 1)],
    };
    assert_eq!(fields_of(&fixture, "api"), expected);
    assert_eq!(counted_from(&fixture, "api"), expected);
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    assert_eq!(fields_of(&fixture, "api"), expected);
}

/// Appends `event` to api, and an hour on packs it as a segment of its own.
fn segment_of(fixture: &Fixture, event: NewEvent) -> crate::records::Maintenance {
    fixture.events("api").append_all([event]).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap()
}

// contract: REC-14
#[test]
fn a_merge_sums_what_its_segments_carried_and_what_waits_is_added_to_it() {
    let fixture = fixture();
    let amounts = [json!({ "amount": 12 }), json!({ "amount": 12.5 }), json!({ "amount": "12" })];
    for amount in amounts {
        segment_of(&fixture, NewEvent::new("paid").fields(&amount));
    }
    let three = fields_of(&fixture, "api");
    assert_eq!(three.fields, [field("amount", &[Kind::Float, Kind::Integer, Kind::Text], 3)]);
    let merged = segment_of(&fixture, NewEvent::new("refund").fields(&json!({ "note": null })));
    assert_eq!(merged.merged, 3, "four segments of one event merge into one");
    fixture.events("api").append_all([NewEvent::new("paid").fields(&json!({ "amount": true }))]).unwrap();
    let expected = StreamFields {
        event_names: vec![named("paid", 4), named("refund", 1)],
        fields: vec![
            field("amount", &[Kind::Float, Kind::Integer, Kind::Text, Kind::Bool], 4),
            field("note", &[Kind::Null], 1),
        ],
        context: Vec::new(),
    };
    assert_eq!(fields_of(&fixture, "api"), expected);
    assert_eq!(counted_from(&fixture, "api"), expected);
}

// contract: REC-14
#[test]
fn what_a_stream_keeps_no_longer_leaves_its_counts_before_expiry_removes_it_and_after() {
    let fixture = fixture();
    let api = fixture.store.events("api").keep(DAY).open().unwrap();
    api.append("old", &json!({ "gone": 1 })).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    fixture.pass(12 * HOUR);
    api.append("new", &json!({ "kept": 1 })).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    assert_eq!(fixture.rows("segments"), 2);
    let both = fields_of(&fixture, "api");
    assert_eq!(both.event_names, [named("new", 1), named("old", 1)]);

    fixture.pass(11 * HOUR + SECOND);
    let kept = StreamFields {
        event_names: vec![named("new", 1)],
        fields: vec![field("kept", &[Kind::Integer], 1)],
        context: Vec::new(),
    };
    assert_eq!(fields_of(&fixture, "api"), kept, "the old segment is past what the stream keeps");
    assert_eq!(maintain(&fixture.store).unwrap().expired, 1);
    assert_eq!(fixture.rows("segments"), 1);
    assert_eq!(fields_of(&fixture, "api"), kept);
}

/// Takes the counts from every key of the file, as a release before them
/// wrote its segments' keys.
fn without_counts(fixture: &Fixture) {
    let forgotten = fixture.records().file.write(0, |tx| {
        tx.execute("update segment_keys set events = null, kinds = null", []).unwrap();
        Ok(())
    });
    forgotten.unwrap();
}

// contract: REC-14, STORE-11
#[test]
fn segments_an_earlier_release_packed_are_counted_by_upkeep_and_left_out_till_then() {
    let fixture = fixture();
    orders_and_a_deploy(&fixture);
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    without_counts(&fixture);
    fixture.events("orders").append("order.refunded", &json!({ "order_id": 3 })).unwrap();
    let waiting = StreamFields {
        event_names: vec![named("order.refunded", 1)],
        fields: vec![field("order_id", &[Kind::Integer], 1)],
        context: Vec::new(),
    };
    assert_eq!(fields_of(&fixture, "orders"), waiting, "a segment without counts is left out");

    maintain(&fixture.store).unwrap();
    let expected = StreamFields {
        event_names: vec![named("order.paid", 100), named("order.refunded", 1)],
        fields: vec![
            field("amount", &[Kind::Float], 100),
            field("currency", &[Kind::Text], 100),
            field("order_id", &[Kind::Integer], 101),
            field("paid", &[Kind::Bool], 100),
        ],
        context: Vec::new(),
    };
    assert_eq!(fields_of(&fixture, "orders"), expected);
    assert_eq!(counted_from(&fixture, "orders"), expected);
    assert_eq!(fixture.rows("segment_keys where events is null"), 0);
}

#[test]
fn a_pass_of_upkeep_counts_sixteen_segments_at_most_and_a_gone_one_keeps_no_keys() {
    let fixture = fixture();
    let api = fixture.events("api");
    for n in 0..20 {
        api.append_all([NewEvent::new(&format!("round-{n}"))]).unwrap();
        fixture.pass(HOUR);
        let records = fixture.records();
        let packing = records.pack(records.now()).unwrap();
        assert_eq!(packing.segments, 1, "packed alone, so that none merges");
    }
    without_counts(&fixture);
    // keys whose segment left the file without them, which no release writes
    let orphaned = fixture.records().file.write(0, |tx| {
        tx.execute("insert into segment_keys (segment, kind, key) values (0, 0, 'orphan')", []).unwrap();
        Ok(())
    });
    orphaned.unwrap();
    let records = fixture.records();
    assert_eq!(records.count_uncounted().unwrap(), 15, "the orphan's keys go and fifteen are counted");
    assert_eq!(fixture.rows("segment_keys"), 20);
    assert_eq!(fields_of(&fixture, "api").event_names.len(), 15);
    assert_eq!(records.count_uncounted().unwrap(), 5);
    assert_eq!(records.count_uncounted().unwrap(), 0);
    assert_eq!(fields_of(&fixture, "api"), counted_from(&fixture, "api"));
}

#[test]
fn upkeep_passes_over_a_segment_that_no_longer_reads_and_counts_the_others() {
    let fixture = fixture();
    let api = fixture.events("api");
    for n in 0..3 {
        api.append_all([NewEvent::new(&format!("round-{n}"))]).unwrap();
        fixture.pass(HOUR);
        maintain(&fixture.store).unwrap();
    }
    without_counts(&fixture);
    let broken = fixture.records().file.write(0, |tx| {
        tx.execute("update segments set body = x'00' where id = 1", []).unwrap();
        Ok(())
    });
    broken.unwrap();
    let records = fixture.records();
    assert_eq!(records.count_uncounted().unwrap(), 2);
    let damaged = fixture.store.all_events().unwrap().damaged().unwrap();
    assert_eq!(damaged.iter().map(|damage| (damage.part, damage.id)).collect::<Vec<_>>(), [(Part::Segment, 1)]);
    assert_eq!(records.count_uncounted().unwrap(), 0, "and leaves it to a drop");
    let names: Vec<String> = fields_of(&fixture, "api").event_names.into_iter().map(|name| name.name).collect();
    assert_eq!(names, ["round-1", "round-2"]);
}

/// Appends `events` access-log lines to api from line `from` on, a thousand
/// an append: four names, five fields and a context.
fn access_log(fixture: &Fixture, from: u64, events: u64) {
    let line = |n: u64| {
        let name = ["request", "login", "search", "error"][(n % 4) as usize];
        let fields =
            json!({ "route": "/notes", "ms": n % 900, "user": n % 5000, "ok": !n.is_multiple_of(7), "size": 1.5 });
        let at = fixture.now() - HOUR + Duration::from_micros(n);
        NewEvent::new(name).at(at).context(&json!({ "host": "web-1" })).fields(&fields)
    };
    let api = fixture.events("api");
    for start in (from..from + events).step_by(1000) {
        api.append_all((start..(start + 1000).min(from + events)).map(line)).unwrap();
    }
}

/// The median of `rounds` runs of `call`, printed beside `what`.
fn time(what: &str, rounds: usize, call: &dyn Fn()) {
    let mut took: Vec<Duration> = (0..rounds)
        .map(|_| {
            let started = std::time::Instant::now();
            call();
            started.elapsed()
        })
        .collect();
    took.sort();
    println!("{what:<64} {:>9.3} ms", took[rounds / 2].as_secs_f64() * 1000.0);
}

/// What a listing of a stream's fields costs against `streams`, with a
/// million events packed and with more waiting; what upkeep costs a segment;
/// and what packing's counts cost against the set of keys it made before
/// them, on the same events. Run by hand with `--release -- --ignored
/// --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn what_listing_counting_and_upkeep_cost() {
    let fixture = fixture();
    let all = fixture.store.all_events().unwrap();
    let list = |what: &str| {
        time(&format!("streams, {what}"), 21, &|| drop(all.streams().unwrap()));
        time(&format!("fields, {what}"), 21, &|| drop(all.fields("api").unwrap()));
    };
    access_log(&fixture, 0, 1_000_000);
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    let (segments, keys) = (fixture.rows("segments where holder is null"), fixture.rows("segment_keys"));
    list(&format!("1,000,000 events in {segments} segments, {keys} keys"));
    let mut appended = 1_000_000;
    for waiting in [1_000, 16_384, 100_000] {
        access_log(&fixture, appended, waiting - (appended - 1_000_000));
        appended = 1_000_000 + waiting;
        list(&format!("and {waiting} waiting"));
    }
    let names = all.fields("api").unwrap().event_names;
    assert_eq!(names.iter().map(|name| name.count).sum::<u64>(), appended);

    let records = fixture.records();
    keys_division(&records, "segment_keys with counts");
    without_counts(&fixture);
    keys_division(&records, "segment_keys without, as 0.5.1 wrote them");
    let started = std::time::Instant::now();
    let counted = records.count_uncounted().unwrap();
    let each = started.elapsed().as_secs_f64() * 1000.0 / counted as f64;
    println!("upkeep: {each:.1} ms a segment, of {counted}");

    let read = all.list().limit(10_000).all().take(16_384).map(Result::unwrap);
    let kept =
        |event: Event| Kept { fields: event.fields, context: event.context, ..NewEvent::new(&event.name).kept(0) };
    let events: Vec<Kept> = read.map(kept).collect();
    let (schema, _) = Schema::of(&events);
    time("keys of 16,384 events, as a set (before)", 21, &|| drop(key_set(&schema, &events)));
    time("keys of 16,384 events, counted (now)", 21, &|| drop(super::super::pack::keys_of(&events)));
    let arrivals: Vec<i64> = (0..events.len() as i64).collect();
    time("a segment made of them, its counts among it", 5, &|| {
        drop(super::super::pack::made_of(&events, &arrivals, &mut Codec::default()).unwrap());
    });
}

/// Prints the b-trees of segment_keys and its index of keys not counted:
/// their pages, and the bytes of their rows a key.
fn keys_division(records: &Records, what: &str) {
    let division = crate::sqlite::division::Division::of(&records.file);
    let keys = division.trees.iter().filter(|tree| tree.name.starts_with("segment_keys"));
    for tree in keys {
        let rows: i64 = records
            .file
            .read(|connection| {
                Ok(connection.query_row("select count(*) from segment_keys", [], |row| row.get(0)).unwrap())
            })
            .unwrap();
        let each = tree.payload as f64 / rows as f64;
        println!("{what}: {} {} pages of 1 KiB, {} B of rows, {each:.1} B a key", tree.name, tree.pages, tree.payload);
    }
}

/// The set of keys packing made of a segment before it counted them.
fn key_set(schema: &Schema, events: &[Kept]) -> std::collections::BTreeSet<(u8, String)> {
    let names = schema.names.iter().map(|name| (NAME_KEY, name.clone()));
    let contexts =
        schema.contexts.iter().flat_map(|context| context.iter().map(|(key, _)| (CONTEXT_KEY, key.to_owned())));
    let fields = events.iter().flat_map(|event| event.fields.iter().map(|(key, _)| (FIELD_KEY, key.to_owned())));
    names.chain(contexts).chain(fields).collect()
}
