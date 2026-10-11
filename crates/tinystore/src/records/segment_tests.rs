use serde_json::json;

use super::super::event::{Level, NewEvent};
use super::*;
use crate::ErrorKind;

/// An event at `at` whose fields are the object `fields` spells, keys written
/// twice and their order kept.
fn spelled(name: &str, at: i64, fields: &str) -> Kept {
    let mut event = NewEvent::new(name).kept(at);
    event.fields = Fields::read(fields).unwrap();
    event
}

#[test]
fn a_segment_keeps_each_name_context_and_shape_once() {
    let request = json!({ "request": "9f86" });
    let line = |message: &str, level: Level, ms: u32| {
        let line = NewEvent::new("log").level(level).message(message).context(&request);
        line.fields(&json!({ "route": "/notes", "ms": ms }))
    };
    let events = [
        line("served", Level::INFO, 12).kept(1),
        line("slow", Level::WARN, 1200).kept(2),
        NewEvent::new("click").fields(&json!({ "x": 812 })).kept(3),
        NewEvent::new("click").kept(4),
    ];
    let (schema, ids) = Schema::of(&events);
    assert_eq!(schema.names, ["log", "click"]);
    assert_eq!(schema.contexts, [Fields::of(&request).unwrap()]);
    assert_eq!((schema.shapes.len(), schema.columns), (3, 3));
    assert_eq!((ids.names, ids.shapes, ids.contexts), (vec![0, 0, 1, 1], vec![0, 0, 1, 2], vec![0, 0, 0, 0]));
    let lines = &schema.shapes[0];
    assert_eq!(lines.presence, HAS_LEVEL | HAS_MESSAGE | HAS_CONTEXT);
    assert_eq!((lines.keys.clone(), lines.columns.clone()), (vec!["ms".to_owned(), "route".to_owned()], vec![0, 1]));
    assert_eq!((schema.shapes[1].columns.clone(), schema.shapes[2].presence), (vec![2], 0));
}

#[test]
fn a_key_written_twice_has_a_column_each_time_which_another_shape_shares_by_its_place() {
    let events = [spelled("a", 1, r#"{"user": 1, "tag": 2, "tag": 3}"#), spelled("b", 2, r#"{"tag": 4, "user": 5}"#)];
    let (schema, _) = Schema::of(&events);
    assert_eq!(schema.columns, 3);
    assert_eq!(schema.shapes[0].columns, [0, 1, 2]);
    // its one tag is the first shape's first, its user the first shape's
    assert_eq!(schema.shapes[1].columns, [1, 0]);
}

#[test]
fn a_segment_gives_columns_to_its_first_shapes_and_keeps_the_fields_of_the_rest_whole() {
    let events: Vec<Kept> = (0..70).map(|n| spelled("event", n, &format!(r#"{{"key{n}": {n}}}"#))).collect();
    let (schema, ids) = Schema::of(&events);
    assert_eq!((schema.shapes.len(), schema.columns), (65, 64));
    assert_eq!(schema.shapes[64].presence, WHOLE_FIELDS);
    assert!(schema.shapes[64].keys.is_empty());
    assert_eq!(ids.shapes[63..], [63, 64, 64, 64, 64, 64, 64]);
    // one of the first shapes again is still that shape
    let (again, ids) = Schema::of(&[events.clone(), vec![spelled("event", 99, r#"{"key3": 0}"#)]].concat());
    assert_eq!((again.shapes.len(), ids.shapes[70]), (65, 3));
}

#[test]
fn a_segment_row_gives_back_its_schema_and_a_changed_or_missing_byte_is_refused() {
    let mut codec = Codec::default();
    let events = [
        spelled("a", 1, r#"{"user": 1, "tag": 2, "tag": 3}"#),
        NewEvent::new("log").level(Level::INFO).message("m").context(&json!({ "request": "9f86", "n": 1.50 })).kept(2),
    ];
    let (schema, _) = Schema::of(&events);
    let row = schema.encode(&mut codec).unwrap();
    assert_eq!(Schema::parse(&row, &mut codec).unwrap(), schema);
    for at in 0..row.len() {
        let mut changed = row.clone();
        changed[at] ^= 0x10;
        assert_eq!(Schema::parse(&changed, &mut codec).unwrap_err().kind(), ErrorKind::Corrupt, "byte {at}");
        assert_eq!(Schema::parse(&row[..at], &mut codec).unwrap_err().kind(), ErrorKind::Corrupt, "cut at {at}");
    }
}
