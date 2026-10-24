use serde_json::json;

use super::super::event::NewEvent;
use super::super::head;
use super::*;
use crate::ErrorKind;

/// An event at `at` whose fields are the object `fields` spells.
fn spelled(name: &str, at: i64, fields: &str) -> Kept {
    let mut event = NewEvent::new(name).kept(at);
    event.fields = Fields::read(fields).unwrap();
    event
}

/// Events of every kind a block holds, in the order of their times.
fn mixed() -> Vec<Kept> {
    let request = json!({ "request": "9f86d081", "tenant": 7 });
    let line = |n: i64| {
        let line = NewEvent::new("log").level(Level::of(4 * (n % 3) as i32)).message(&format!("request {n}\nserved"));
        line.context(&request).fields(&json!({ "route": "/notes", "ms": 1200 + n, "ok": n % 2 == 0 })).kept(1000 + n)
    };
    let mut events: Vec<Kept> = (0..20).map(line).collect();
    events.push(NewEvent::new("click").trace(TraceId([7; 16]), SpanId([9; 8])).kept(2000));
    events.push(NewEvent::new("bare").kept(2000));
    events.push(spelled("spelled", 2001, r#"{"n": 1.2300, "big": 9007199254740993, "n": {"x": [1, "y\n"]}, "e": ""}"#));
    events.push(spelled("spelled", 2002, r#"{"n": -0, "big": 12, "n": null, "e": "café"}"#));
    events.push(NewEvent::new("log").level(Level::ERROR).message("").kept(2003));
    events
}

/// The block of `events`, whose arrivals are their places.
fn block_of(events: &[Kept], codec: &mut Codec) -> (Schema, Row) {
    let (schema, ids) = Schema::of(events);
    let arrivals: Vec<i64> = (0..events.len() as i64).collect();
    let row = encode(&schema, (events, &arrivals), [&ids.names, &ids.shapes, &ids.contexts], codec).unwrap();
    (schema, row)
}

/// The events of a block of id 1, without their arrivals.
fn events_of(schema: &Schema, body: &[u8], codec: &mut Codec) -> Result<Vec<Kept>> {
    Ok(parse(schema, (1, body), codec)?.into_iter().map(|(_, event)| event).collect())
}

// contract: REC-01
#[test]
fn a_block_gives_back_its_events_each_value_as_it_was_spelled() {
    let mut codec = Codec::default();
    let events = mixed();
    let (schema, row) = block_of(&events, &mut codec);
    assert_eq!(events_of(&schema, &row.body, &mut codec).unwrap(), events);
    assert_eq!((row.first, row.last, row.count), (1000, 2003, events.len()));
    assert_eq!(row.levels, Level::INFO.bit() | Level::WARN.bit() | Level::ERROR.bit());
    assert_eq!(row.weight, events.iter().map(Kept::weight).sum::<usize>());
}

// contract: REC-04
#[test]
fn a_block_gives_back_each_events_arrival_with_it_in_the_order_it_holds_them() {
    let mut codec = Codec::default();
    let events = mixed();
    let (schema, ids) = Schema::of(&events);
    // a merge's: the second member's arrived before the first's, and the last row's out of their order
    let arrivals: Vec<i64> = (0..events.len() as i64)
        .map(|place| if place < 20 { 5 << 10 | place } else { 3 << 10 | (30 - place) })
        .collect();
    let row = encode(&schema, (&events, &arrivals), [&ids.names, &ids.shapes, &ids.contexts], &mut codec).unwrap();
    let read = parse(&schema, (9, &row.body), &mut codec).unwrap();
    assert_eq!(read, arrivals.into_iter().zip(events).collect::<Vec<_>>());
}

#[test]
fn arrivals_appended_in_the_order_of_their_times_take_a_few_bytes_a_block() {
    let mut codec = Codec::default();
    let events: Vec<Kept> = (0..1024).map(|n| NewEvent::new("log").kept(1000 + n)).collect();
    let (schema, ids) = Schema::of(&events);
    // head rows of a thousand events each, 7 and 8
    let arrivals: Vec<i64> =
        (0..1024).map(|place| super::head::arrival(7 + place / 1000, place as usize % 1000)).collect();
    let row = encode(&schema, (&events, &arrivals), [&ids.names, &ids.shapes, &ids.contexts], &mut codec).unwrap();
    let written = columns_of(&schema, &row.body).unwrap()[ARRIVALS].len();
    assert!(written <= 24, "{written} bytes");
    let read: Vec<i64> = parse(&schema, (1, &row.body), &mut codec).unwrap().into_iter().map(|(at, _)| at).collect();
    assert_eq!(read, arrivals);
}

/// A block of two events, `request` and `signup` at 1 and 2 ns, and the row
/// of its segment, as v0.1.0 (f11772d3) wrote them.
const VERSION_2_BLOCK: &str = "02020906060000000000000000020100010200000100010255fb2be8";
const VERSION_2_SEGMENT: &str = "010014020772657175657374067369676e757000010000d42743b0";

#[test]
fn a_block_of_version_2_gives_its_events_the_arrivals_of_where_they_lie_below_every_head_rows() {
    let events = [NewEvent::new("request").kept(1), NewEvent::new("signup").kept(2)];
    let mut codec = Codec::default();
    let schema = Schema::parse(&hex(VERSION_2_SEGMENT), &mut codec).unwrap();
    let read = parse(&schema, (9, &hex(VERSION_2_BLOCK)), &mut codec).unwrap();
    assert_eq!(read, [(9 * 1024 - (1 << 62), events[0].clone()), (9 * 1024 + 1 - (1 << 62), events[1].clone())]);
    assert!(read.iter().all(|(arrival, _)| *arrival < super::head::arrival(1, 0)));
    // the same events written now are of version 3, which keeps their arrivals
    let (_, ids) = Schema::of(&events);
    let row = encode(&schema, (&events, &[7, 3]), [&ids.names, &ids.shapes, &ids.contexts], &mut codec).unwrap();
    assert_eq!(row.body[0], VERSION);
    assert_eq!(parse(&schema, (9, &row.body), &mut codec).unwrap()[1].0, 3);
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
}

#[test]
fn a_segment_of_one_name_and_one_shape_writes_no_column_of_them() {
    let mut codec = Codec::default();
    let line = |n: i64| NewEvent::new("log").message(&format!("line {n}")).kept(1000 + n);
    let events: Vec<Kept> = (0..64).map(line).collect();
    let (schema, row) = block_of(&events, &mut codec);
    let columns = columns_of(&schema, &row.body).unwrap();
    assert!(columns[NAMES].is_empty() && columns[SHAPES].is_empty());
    assert_eq!(events_of(&schema, &row.body, &mut codec).unwrap(), events);
    let (schema, row) = block_of(&mixed(), &mut codec);
    let columns = columns_of(&schema, &row.body).unwrap();
    assert!(!columns[NAMES].is_empty() && !columns[SHAPES].is_empty());
}

#[test]
fn the_fields_of_a_shape_past_the_segments_columns_come_back_whole() {
    let mut codec = Codec::default();
    let events: Vec<Kept> =
        (0..70).map(|n| spelled("event", n, &format!(r#"{{"key{n}": {n}, "s": "a\"b"}}"#))).collect();
    let (schema, row) = block_of(&events, &mut codec);
    assert_eq!(schema.shapes.len(), 65);
    assert_eq!(events_of(&schema, &row.body, &mut codec).unwrap(), events);
}

#[test]
fn a_changed_or_missing_byte_is_refused() {
    let mut codec = Codec::default();
    let (schema, row) = block_of(&mixed(), &mut codec);
    let refused = |body: &[u8], codec: &mut Codec| events_of(&schema, body, codec).unwrap_err().kind();
    for at in 0..row.body.len() {
        let mut changed = row.body.clone();
        changed[at] ^= 0x04;
        assert_eq!(refused(&changed, &mut codec), ErrorKind::Corrupt, "byte {at}");
        assert_eq!(refused(&row.body[..at], &mut codec), ErrorKind::Corrupt, "cut at {at}");
    }
}

#[test]
fn a_block_read_with_another_segments_row_is_refused_and_never_more() {
    let mut codec = Codec::default();
    let (_, row) = block_of(&mixed(), &mut codec);
    let (other, _) = Schema::of(&[NewEvent::new("other").kept(1)]);
    assert_eq!(events_of(&other, &row.body, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt);
    let (wider, _) = Schema::of(&(0..40).map(|n| spelled("e", n, &format!(r#"{{"k{n}": 1}}"#))).collect::<Vec<_>>());
    assert_eq!(events_of(&wider, &row.body, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt);
}

/// What a block takes of a thousand lines of one logger against the head row
/// of the same lines: run by hand with `--release -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run by hand"]
fn a_thousand_lines_in_a_block_and_in_a_head_row() {
    let routes = ["/notes", "/login", "/buy", "/search"];
    let line = |n: i64| {
        let fields = json!({ "route": routes[(n * 7 % 4) as usize], "ms": 20 + n * 37 % 400, "user": 1000 + n % 50 });
        let line = NewEvent::new("log").level(Level::INFO).message("request served").fields(&fields);
        line.context(&json!({ "service": "api" })).kept(1_800_000_000_000_000_000 + n * 3_000_000)
    };
    let mut codec = Codec::default();
    let events: Vec<Kept> = (0..1024).map(line).collect();
    let (schema, row) = block_of(&events, &mut codec);
    let shared = schema.encode(&mut codec).unwrap();
    let head = head::encode(&events, &mut codec).unwrap();
    let each = |bytes: usize| bytes as f64 / 1024.0;
    println!(
        "a block {:.2} B/event, its segment's row {} B, a head row {:.2} B/event",
        each(row.body.len()),
        shared.len(),
        each(head.body.len())
    );
}

/// What this engine's columns take of a corpus the Go engine dumped, a file a
/// container of `{"at", "name", "level", "body", "fields"}` lines, all but the
/// time optional, cut into segments and blocks as packing cuts them: run by
/// hand with `TINYSTORE_DUMP=<dir>` and `--release -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run by hand"]
fn a_corpus_by_its_columns() {
    #[derive(serde::Deserialize)]
    struct Line {
        at: i64,
        name: Option<String>,
        level: Option<i32>,
        body: Option<String>,
        fields: Option<Vec<(String, String)>>,
    }
    let Ok(dir) = std::env::var("TINYSTORE_DUMP") else { return };
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().path()).collect();
    files.sort();
    let mut codec = Codec::default();
    let (mut records, mut rows, mut blocks, mut block_count) = (0, 0, 0, 0);
    let mut slots = [0; FIXED + 1];
    let mut kinds = std::collections::BTreeMap::<u8, (usize, usize)>::new();
    let started = std::time::Instant::now();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let event = |line: &str| {
            let line: Line = serde_json::from_str(line).unwrap();
            let mut event = NewEvent::new(line.name.as_deref().unwrap_or("log")).kept(line.at);
            (event.level, event.message) = (line.level.map(Level::of), line.body);
            event.fields = Fields::from_pairs(line.fields.unwrap_or_default());
            event
        };
        let events: Vec<Kept> = text.lines().map(event).collect();
        for segment in events.chunks(16_384) {
            // each arriving as it lies in the dump, sixty-four a head row
            let arrival = |place: usize| super::super::head::arrival(1 + (place / 64) as i64, place % 64);
            let mut arrived: Vec<(i64, Kept)> =
                segment.iter().cloned().enumerate().map(|(n, e)| (arrival(n), e)).collect();
            arrived.sort_by_key(|(arrival, event)| (event.at, *arrival));
            let (arrivals, segment): (Vec<i64>, Vec<Kept>) = arrived.into_iter().unzip();
            let (schema, ids) = Schema::of(&segment);
            rows += schema.encode(&mut codec).unwrap().len();
            let mut from = 0;
            for events in super::super::pack::cut(&segment) {
                let to = from + events.len();
                let ids = [&ids.names[from..to], &ids.shapes[from..to], &ids.contexts[from..to]];
                let row = encode(&schema, (events, &arrivals[from..to]), ids, &mut codec).unwrap();
                for (slot, column) in columns_of(&schema, &row.body).unwrap().into_iter().enumerate() {
                    slots[slot.min(FIXED)] += column.len();
                    if let (true, Some(kind)) = (slot >= FIXED, column.first()) {
                        let seen = kinds.entry(*kind).or_default();
                        *seen = (seen.0 + 1, seen.1 + column.len());
                    }
                }
                (blocks, block_count, from) = (blocks + row.body.len(), block_count + 1, to);
            }
            records += segment.len();
        }
    }
    let per = |bytes: usize| bytes as f64 / records as f64;
    let took = started.elapsed();
    println!(
        "rust records={records} blocks={block_count} took={took:?}\nrust blocks={:.3} segment_rows={:.3} whole={:.3}",
        per(blocks),
        per(rows),
        per(blocks + rows)
    );
    let names = [
        "times", "arrivals", "names", "shapes", "contexts", "levels", "messages", "traces", "spans", "whole", "fields",
    ];
    for (name, bytes) in names.iter().zip(slots) {
        println!("rust slot {name:<8} {:.3}", per(bytes));
    }
    for (kind, (columns, bytes)) in kinds {
        let name = super::super::values::kind_of(kind);
        println!("rust field {name:<22} columns={columns:<6} bytes={:.4}", per(bytes));
    }
}

#[test]
fn a_read_rebuilds_only_the_events_its_columns_leave_and_no_other_column_when_none_is_left() {
    let mut codec = Codec::default();
    let events = mixed();
    let (schema, row) = block_of(&events, &mut codec);
    let pair = |key: &str, json: &str| (key.to_owned(), json.to_owned());
    let wanted = |names: &[String], filters: &[(String, String)]| {
        let kept = Wanted { first: 0, last: i64::MAX, names, min_level: None, trace: None, filters };
        parse_wanted(&schema, (1, &row.body), &mut Codec::default(), &kept).unwrap()
    };
    // the block's arrivals are its places
    let places = |kept: Vec<(i64, Kept)>| kept.into_iter().map(|(place, _)| place).collect::<Vec<_>>();
    let exact = wanted(&[], &[pair("route", "\"/notes\""), pair("ms", "1203")]);
    assert_eq!(exact.iter().map(|(place, event)| (*place, event)).collect::<Vec<_>>(), [(3, &events[3])]);
    assert_eq!(places(wanted(&["click".to_owned(), "bare".to_owned()], &[])), [20, 21]);
    // a value its context holds, and one of a key written twice, are narrowed by as well
    assert_eq!(places(wanted(&[], &[pair("tenant", "7"), pair("ok", "true")])), [0, 2, 4, 6, 8, 10, 12, 14, 16, 18]);
    assert_eq!(places(wanted(&[], &[pair("e", "\"caf\u{e9}\"")])), [23]);
    let grave =
        Wanted { first: 0, last: i64::MAX, names: &[], min_level: Some(Level::ERROR), trace: None, filters: &[] };
    assert_eq!(places(parse_wanted(&schema, (1, &row.body), &mut codec, &grave).unwrap()), [2, 5, 8, 11, 14, 17, 24]);

    // a message column no read can parse, its checksum made again: a read that leaves no event never reads it
    let mut broken = row.body.clone();
    let messages = columns_of(&schema, &row.body).unwrap()[MESSAGES].as_ptr() as usize - row.body.as_ptr() as usize;
    broken[messages] = 0x7f;
    let framed = broken.len() - 4;
    let sum = crate::compress::checksum::crc32(&broken[..framed]);
    broken[framed..].copy_from_slice(&sum.to_le_bytes());
    assert_eq!(events_of(&schema, &broken, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt);
    let nowhere = [pair("tenant", "8")];
    let none = Wanted { first: 0, last: i64::MAX, names: &[], min_level: None, trace: None, filters: &nowhere };
    assert!(parse_wanted(&schema, (1, &broken), &mut codec, &none).unwrap().is_empty());
}
