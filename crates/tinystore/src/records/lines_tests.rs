use std::io::Write;

use super::super::event::{Event, Level};
use super::super::fixture::{Fixture, SECOND, fixture};
use super::super::fuzzing::line_of;
use super::*;
use crate::ErrorKind;

/// Writes `text` to a writer of lines in one write, drops it, writes what
/// the store holds, and reads back the stream's events.
fn lines_of(f: &Fixture, stream: &str, text: &[u8]) -> Vec<Event> {
    written_in_pieces(f, stream, text, text.len().max(1))
}

/// `lines_of`, the text written `piece` bytes at a time.
fn written_in_pieces(f: &Fixture, stream: &str, text: &[u8], piece: usize) -> Vec<Event> {
    let events = f.events(stream);
    let mut out = events.lines().unwrap();
    text.chunks(piece).for_each(|bytes| out.write_all(bytes).unwrap());
    drop(out);
    f.records().flush_held(false).unwrap();
    events.list().limit(10_000).page().unwrap().events
}

fn messages(events: &[Event]) -> Vec<&str> {
    events.iter().filter_map(|event| event.message.as_deref()).collect()
}

#[test]
fn the_lines_of_a_stack_trace_a_traceback_or_a_json_value_make_one_event() {
    let f = fixture();
    let cases: [(&str, &str, &[&str]); 4] = [
        (
            "python",
            "2026-09-26 12:00:01,500 ERROR request failed\nTraceback (most recent call last):\n  File \"app.py\", line 3, in handle\nValueError: bad input\n2026-09-26 12:00:02,000 INFO next\n",
            &[
                "2026-09-26 12:00:01,500 ERROR request failed\nTraceback (most recent call last):\n  File \"app.py\", line 3, in handle\nValueError: bad input",
                "2026-09-26 12:00:02,000 INFO next",
            ],
        ),
        (
            "bot",
            "{\n\"status\": 200\n}\n[2026-09-26 12:00:03] info done\n",
            &["{\n\"status\": 200\n}", "[2026-09-26 12:00:03] info done"],
        ),
        (
            "colors",
            "[2026-09-26 12:00:03] \x1b[32minfo\x1b[39m (app\n[2026-09-26 12:00:04] next\n",
            &["[2026-09-26 12:00:03] \x1b[32minfo\x1b[39m (app", "[2026-09-26 12:00:04] next"],
        ),
        (
            "java",
            "Exception in thread \"main\" java.lang.IllegalStateException: boom\n\tat com.example.App.run(App.java:12)\nCaused by: java.io.IOException: disk\n\t... 2 more\nplain\n",
            &[
                "Exception in thread \"main\" java.lang.IllegalStateException: boom\n\tat com.example.App.run(App.java:12)\nCaused by: java.io.IOException: disk\n\t... 2 more",
                "plain",
            ],
        ),
    ];
    for (stream, text, expected) in cases {
        assert_eq!(messages(&lines_of(&f, stream, text.as_bytes())), expected, "{stream}");
    }
}

#[test]
fn a_lines_text_comes_back_however_it_was_cut_into_writes() {
    let f = fixture();
    let lines = ["plain", "with a carriage return\r", "", "a\ttab", "\0 and é", r#"{"a":1,"b":"x"}"#, r#"{ "a": 1 }"#];
    let events = written_in_pieces(&f, "bytes", format!("{}\n", lines.join("\n")).as_bytes(), 3);
    assert_eq!(events.iter().map(line_of).collect::<Vec<_>>(), lines);
    assert_eq!((events[5].name.as_str(), events[6].name.as_str()), (JSON, TEXT), "an exact object is its fields");
}

#[test]
fn a_line_that_is_no_utf8_keeps_a_replacement_for_each_byte_that_spells_nothing() {
    let f = fixture();
    assert_eq!(messages(&lines_of(&f, "bytes", b"bad \xff byte\n")), ["bad \u{fffd} byte"]);
}

#[test]
fn an_event_that_may_still_grow_waits_for_a_flush_it_did_not_grow_through() {
    let f = fixture();
    let events = f.events("app");
    let mut out = events.lines().unwrap();
    let started = f.now();
    out.write_all(b"2026-09-26 12:00:01,500 ERROR failed\n  at frame one\n").unwrap();
    f.pass(SECOND);
    f.records().flush_held(false).unwrap();
    assert!(events.list().page().unwrap().events.is_empty(), "still growing at the first flush");
    f.records().flush_held(false).unwrap();
    let found = events.list().page().unwrap().events;
    assert_eq!((found.len(), found[0].at, found[0].level), (1, started, Some(Level::ERROR)));
    out.write_all(b"no newline yet").unwrap();
    drop(out);
    f.records().flush_held(false).unwrap();
    assert_eq!(messages(&events.list().page().unwrap().events)[1], "no newline yet");
}

#[test]
fn a_flush_writes_every_line_ended_and_keeps_one_without_its_newline() {
    let f = fixture();
    let events = f.events("app");
    let mut out = events.lines().unwrap();
    out.write_all(b"done\nhalf a li").unwrap();
    out.flush().unwrap();
    assert_eq!(messages(&events.list().page().unwrap().events), ["done"]);
    out.write_all(b"ne\n").unwrap();
    drop(out);
    f.records().flush_held(false).unwrap();
    assert_eq!(messages(&events.list().page().unwrap().events), ["done", "half a line"]);
}

#[test]
fn a_writer_never_waits_drops_what_its_buffer_has_no_room_for_and_closes_with_its_store() {
    let f = fixture();
    let events = f.events("busy");
    let mut out = events.lines().unwrap();
    // each line hands the one before it over, and the last waits for what may join it
    for line in 0..BUFFER + 6 {
        writeln!(out, "line {line}").unwrap();
    }
    assert_eq!(out.dropped(), 5);
    let f = f.reopen();
    assert_eq!(
        f.events("busy").list().limit(10_000).page().unwrap().events.len(),
        BUFFER + 1,
        "the store's close wrote the last"
    );
    assert_eq!(
        out.write(b"after the close\n").unwrap_err().get_ref().unwrap().to_string(),
        "lines of events busy: closed"
    );
    assert_eq!(events.lines().unwrap_err().kind(), ErrorKind::Closed, "a new writer after the close");
}

#[test]
fn a_line_longer_than_an_event_holds_is_dropped_whole_and_counted() {
    let f = fixture();
    let events = f.events("large");
    let mut out = events.lines().unwrap();
    let room = out.writer.room;
    // a line that grows a little at a time up to an event's room, then past it
    for _ in 0..room / 1000 {
        out.write_all(&[b'x'; 1000]).unwrap();
    }
    assert!(out.writer.lock().partial.capacity() <= room, "what a line holds never passes an event's room");
    out.write_all(&vec![b'x'; 4 * MAX_EVENT]).unwrap();
    assert_eq!(out.writer.lock().partial.capacity(), 0, "a line dropped lets go of what it held");
    out.write_all(b"\nsmall\n").unwrap();
    assert_eq!(out.dropped(), 1);
    drop(out);
    f.records().flush_held(false).unwrap();
    assert_eq!(messages(&events.list().page().unwrap().events), ["small"]);
}

#[test]
fn however_a_text_is_cut_its_lines_come_back_but_those_longer_than_an_event_holds() {
    let f = fixture();
    let room = MAX_EVENT - weight(TEXT, None, &[], &[]);
    let texts = [
        "2026-09-26 12:00:01,500 ERROR failed\n  at frame\nnext\n{\n\"a\": [1,\n2]}\n".to_owned(),
        "{\"level\":30,\"msg\":\"ok\"}\nplain\r\n\n".to_owned(),
        "level=info msg=\"slow \\\"request\\\"\" ms=1200 ok=true\nlevel=warn  msg=x\na= b=\n".to_owned(),
        format!("before\n  at frame\n{}\n  at frame\nafter", "x".repeat(MAX_EVENT)),
    ];
    for (place, text) in texts.iter().enumerate() {
        let lines = text.strip_suffix('\n').unwrap_or(text);
        let kept: Vec<&str> = lines.split('\n').filter(|line| line.len() <= room).collect();
        let longer = lines.split('\n').count() - kept.len();
        for piece in [1, 2, 3, 7, 256] {
            let stream = format!("cut-{place}-{piece}");
            let events = f.events(&stream);
            let mut out = events.lines().unwrap();
            text.as_bytes().chunks(piece).for_each(|bytes| out.write_all(bytes).unwrap());
            let dropped = out.dropped();
            drop(out);
            f.records().flush_held(false).unwrap();
            let found = events.list().limit(10_000).page().unwrap().events;
            let spelled: Vec<String> = found.iter().map(line_of).collect();
            assert_eq!((spelled.join("\n"), dropped as usize), (kept.join("\n"), longer), "{stream}");
        }
    }
}

#[test]
fn a_stack_trace_goes_on_with_a_cause_a_traceback_and_an_exceptions_name() {
    let lines = [
        ("ValueError: bad input", true),
        ("java.io.IOException: disk", true),
        ("KeyError", true),
        ("Caused by: java.io.IOException", true),
        ("Traceback (most recent call last):", true),
        ("goroutine 1 [running]:", true),
        ("INFO started", false),
        ("level=info msg=ok", false),
        (r#"Exception in thread "main" java.lang.IllegalStateException: boom"#, false),
        ("error: no such file", false),
    ];
    for (line, goes_on) in lines {
        assert_eq!(continues(line.as_bytes()), goes_on, "{line}");
    }
}

#[test]
fn a_logfmt_line_becomes_its_pairs_and_a_json_line_its_fields_which_a_read_finds() {
    let f = fixture();
    let text = "level=warn msg=\"slow request\" ms=1200\n{\"level\":30,\"msg\":\"ok\"}\nplain\n";
    let events = lines_of(&f, "mixed", text.as_bytes());
    let names: Vec<&str> = events.iter().map(|event| event.name.as_str()).collect();
    assert_eq!(names, [LOGFMT, JSON, TEXT]);
    assert_eq!((events[0].level, events[1].level), (Some(Level::WARN), Some(Level::INFO)));
    let pairs: Vec<(&str, &str)> = events[0].fields.iter().collect();
    assert_eq!(pairs, [("level", r#""warn""#), ("msg", r#""slow request""#), ("ms", "1200")]);
    let found = f.events("mixed").list().filter("ms", &1200).page().unwrap().events;
    assert_eq!(found.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(), [LOGFMT]);
}

#[test]
fn a_json_line_whose_fields_weigh_past_an_event_is_kept_as_its_text() {
    let f = fixture();
    let room = MAX_EVENT - weight(TEXT, None, &[], &[]);
    // 128 fields, each a few bytes heavier as a field than as text, a line just within the room
    let value = "x".repeat((room - 50 - 129) / 128 - 9);
    let fields: Vec<String> = (0..128).map(|key| format!("\"k{key:03}\":\"{value}\"")).collect();
    let line = format!("{{{}}}", fields.join(","));
    assert!(line.len() <= room && exact_fields(&line).is_some());
    let events = lines_of(&f, "heavy", format!("{line}\n").as_bytes());
    assert_eq!((events[0].name.as_str(), events[0].message.as_deref()), (TEXT, Some(line.as_str())));
}
