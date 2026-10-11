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

fn block_of(events: &[Kept], codec: &mut Codec) -> (Schema, Row) {
    let (schema, ids) = Schema::of(events);
    let row = encode(&schema, events, [&ids.names, &ids.shapes, &ids.contexts], codec).unwrap();
    (schema, row)
}

#[test]
fn a_block_gives_back_its_events_each_value_as_it_was_spelled() {
    let mut codec = Codec::default();
    let events = mixed();
    let (schema, row) = block_of(&events, &mut codec);
    assert_eq!(parse(&schema, &row.body, &mut codec).unwrap(), events);
    assert_eq!((row.first, row.last, row.count), (1000, 2003, events.len()));
    assert_eq!(row.levels, Level::INFO.bit() | Level::WARN.bit() | Level::ERROR.bit());
    assert_eq!(row.weight, events.iter().map(Kept::weight).sum::<usize>());
}

#[test]
fn the_fields_of_a_shape_past_the_segments_columns_come_back_whole() {
    let mut codec = Codec::default();
    let events: Vec<Kept> =
        (0..70).map(|n| spelled("event", n, &format!(r#"{{"key{n}": {n}, "s": "a\"b"}}"#))).collect();
    let (schema, row) = block_of(&events, &mut codec);
    assert_eq!(schema.shapes.len(), 65);
    assert_eq!(parse(&schema, &row.body, &mut codec).unwrap(), events);
}

#[test]
fn a_changed_or_missing_byte_is_refused() {
    let mut codec = Codec::default();
    let (schema, row) = block_of(&mixed(), &mut codec);
    for at in 0..row.body.len() {
        let mut changed = row.body.clone();
        changed[at] ^= 0x04;
        assert_eq!(parse(&schema, &changed, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt, "byte {at}");
        assert_eq!(parse(&schema, &row.body[..at], &mut codec).unwrap_err().kind(), ErrorKind::Corrupt, "cut at {at}");
    }
}

#[test]
fn a_block_read_with_another_segments_row_is_refused_and_never_more() {
    let mut codec = Codec::default();
    let (_, row) = block_of(&mixed(), &mut codec);
    let (other, _) = Schema::of(&[NewEvent::new("other").kept(1)]);
    assert_eq!(parse(&other, &row.body, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt);
    let (wider, _) = Schema::of(&(0..40).map(|n| spelled("e", n, &format!(r#"{{"k{n}": 1}}"#))).collect::<Vec<_>>());
    assert_eq!(parse(&wider, &row.body, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt);
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

/// What this engine's columns take of a corpus the Go engine dumped, a file
/// a container of `{"at", "body"}` lines, cut into segments and blocks as
/// packing cuts them: run by hand with `TINYSTORE_DUMP=<dir>` and
/// `--release -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run by hand"]
fn a_corpus_by_its_columns() {
    #[derive(serde::Deserialize)]
    struct Line {
        at: i64,
        body: String,
    }
    let Ok(dir) = std::env::var("TINYSTORE_DUMP") else { return };
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().path()).collect();
    files.sort();
    let mut codec = Codec::default();
    let (mut records, mut plain, mut rows, mut blocks) = (0, 0, 0, 0);
    let (mut times, mut bodies) = (0, 0);
    let started = std::time::Instant::now();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let line = |line: &str| {
            let line: Line = serde_json::from_str(line).unwrap();
            NewEvent::new("log").message(&line.body).kept(line.at)
        };
        let events: Vec<Kept> = text.lines().map(line).collect();
        for segment in events.chunks(16_384) {
            let mut segment = segment.to_vec();
            segment.sort_by_key(|event| event.at);
            let (schema, ids) = Schema::of(&segment);
            rows += schema.encode(&mut codec).unwrap().len();
            let mut from = 0;
            for events in super::super::pack::cut(&segment) {
                let to = from + events.len();
                let ids = [&ids.names[from..to], &ids.shapes[from..to], &ids.contexts[from..to]];
                let row = encode(&schema, events, ids, &mut codec).unwrap();
                let lengths = column_lengths(&schema, &row.body).unwrap();
                (times, bodies, blocks) = (times + lengths[TIMES], bodies + lengths[MESSAGES], blocks + row.body.len());
                from = to;
            }
            records += segment.len();
            plain += segment.iter().map(|event| event.message.as_ref().map_or(0, String::len)).sum::<usize>();
        }
    }
    let per = |bytes: usize| bytes as f64 / records as f64;
    println!(
        "rust records={records} plain={:.2} times={:.3} bodies={:.3} blocks={:.3} segment_rows={:.3} whole={:.3} took={:?}",
        per(plain),
        per(times),
        per(bodies),
        per(blocks),
        per(rows),
        per(blocks + rows),
        started.elapsed()
    );
}
