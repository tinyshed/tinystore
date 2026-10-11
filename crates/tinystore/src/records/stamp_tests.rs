use super::*;

/// The nanoseconds since 1970 of a day and a time of it, as if UTC.
fn at(date: (i64, i64, i64), clock: (i64, i64, i64), nanos: i64) -> i64 {
    let seconds = days_from_civil(date.0, date.1, date.2) * SECONDS_IN_DAY + clock.0 * 3600 + clock.1 * 60 + clock.2;
    seconds * 1_000_000_000 + nanos
}

/// The event's time every example here is written at.
fn written() -> i64 {
    at((2026, 9, 23), (0, 47, 32), 101_187_376)
}

/// Writes `values` at `times` as stamped and reads them back, when their
/// stamps pay; the bytes they took.
fn stamped(values: &[&str], times: &[i64]) -> Option<Vec<u8>> {
    let mut codec = Codec::default();
    let mut out = Vec::new();
    if !put_stamped(&mut out, values, times, &mut codec).unwrap() {
        assert!(out.is_empty());
        return None;
    }
    let mut reader = Reader::new(&out);
    let read = read_stamped(&mut reader, times, &mut Decoder { codec: &mut codec, left: 1 << 20 });
    reader.finish().unwrap();
    assert_eq!(read, values);
    Some(out)
}

fn found(text: &str, at: i64) -> Vec<Stamp> {
    let mut stamps = Vec::new();
    find_stamps(&mut stamps, text.as_bytes(), at);
    stamps
}

#[test]
fn every_layout_spells_its_text_back() {
    let lines = [
        "2026-09-23 00:47:32,100 GET /notes",
        "2026-09-23T00:47:32.101187376Z served",
        "2026/09/23 00:47:32 listening",
        "I20260923 00:47:32.100929   141 raft_server.h:60] Peer refresh succeeded!",
        "E0923 00:47:32.000001  1 main.go:12] failed",
        "1:M 23 Sep 2026 00:47:32.500 # WARNING overcommit",
        "Sep 23 00:47:32 host sshd[1]: session opened",
        r#"10.0.0.1 - - [23/Sep/2026:00:47:32 +0000] "GET / HTTP/1.1" 200 12"#,
    ];
    for line in lines {
        assert_eq!(found(line, written()).len(), 1, "{line}");
        let column = stamped(&[line; 8], &[written(); 8]).expect(line);
        // the time's digits are gone from what is written
        assert!(!column.windows(8).any(|window| window == b"00:47:32"), "{line}");
    }
    let padded = at((2026, 9, 5), (0, 47, 32), 0);
    stamped(&["Sep  5 00:47:32 host cron[7]: run"; 8], &[padded; 8]).unwrap();
}

#[test]
fn a_stamp_is_how_far_behind_its_event_it_lies_whatever_its_zone() {
    let glog = found("I20260923 00:47:32.100929   141 raft_server.h:60]", written());
    assert_eq!((glog[0].start, glog[0].end, glog[0].digits), (1, 25, 6));
    assert_eq!(behind(written(), glog[0].wall, 6), Some(258));
    // a program three hours east of UTC is behind by minus three hours, every line
    let local = found("2026-09-23 03:47:32,100 GET /notes", written());
    assert_eq!(behind(written(), local[0].wall, 3), Some(1 - 3 * 3_600_000));
    let lines: Vec<String> = (0..16).map(|n| format!("2026-09-23 03:47:{:02},100 GET /notes/{n}", 32 + n)).collect();
    let times: Vec<i64> = (0..16).map(|n| written() + n * 1_000_000_000).collect();
    stamped(&lines.iter().map(String::as_str).collect::<Vec<_>>(), &times).unwrap();
}

#[test]
fn a_time_no_calendar_or_clock_shows_stays_text() {
    let never = [
        "2026-13-01 00:00:00 a month too many",
        "2026-02-30 00:00:00 a day February has not",
        "2025-02-29 00:00:00 no leap year",
        "2026-09-23 24:00:00 an hour too many",
        "2026-09-23 23:59:60 a leap second",
        "2026-09-00 00:00:00 no day",
        "Sep 05 00:47:32 a day syslog pads with a space",
        "20260923T004732 too tight",
        "12:00:01 a clock alone",
    ];
    for text in never {
        assert!(found(text, written()).is_empty(), "{text}");
    }
    assert_eq!(found("2024-02-29 00:00:00 a leap year", written()).len(), 1);
    // without a year a stamp takes its event's: the 29th of February is one in 2024 and none in 2026
    assert_eq!(found("0229 00:00:00 glog", at((2024, 3, 1), (0, 0, 0), 0)).len(), 1);
    assert!(found("0229 00:00:00 glog", written()).is_empty());
}

#[test]
fn a_fraction_is_one_to_nine_digits_and_more_stay_text() {
    for digits in 1..=12 {
        let line = format!("2026-09-23 00:47:32.{} tail", "123456789012"[..digits].to_owned());
        let stamp = found(&line, written())[0];
        assert_eq!(usize::from(stamp.digits), digits.min(9), "{line}");
        stamped(&[line.as_str(); 8], &[written(); 8]).unwrap();
    }
    assert_eq!(found("2026-09-23 00:47:32. a stop", written())[0].digits, 0);
    stamped(&["2026-09-23 00:47:32. a stop"; 8], &[written(); 8]).unwrap();
}

#[test]
fn a_value_keeps_up_to_four_stamps_each_within_reach_of_the_one_before() {
    let two = "from 2026-09-23 00:47:30 to 2026-09-23 00:47:32.5 done";
    assert_eq!(found(two, written()).len(), 2);
    stamped(&[two; 8], &[written(); 8]).unwrap();
    let five = "2026-09-23 00:47:31 ".repeat(5);
    assert_eq!(found(&five, written()).len(), 4);
    stamped(&[five.as_str(); 8], &[written(); 8]).unwrap();
    let far = format!("{} 2026-09-23 00:47:32", "x".repeat(70));
    assert!(found(&far, written()).is_empty());
    let unicode = "caf\u{e9} \u{2014} 2026-09-23 00:47:32,100 \u{2014} caf\u{e9}";
    stamped(&[unicode; 8], &[written(); 8]).unwrap();
}

#[test]
fn stamps_are_cut_only_where_they_pay_for_their_columns() {
    let line = "2026-09-23 00:47:32,100 GET /notes";
    assert!(stamped(&[line; 7], &[written(); 7]).is_none());
    let mut mostly: Vec<&str> = vec!["no time here"; 64];
    mostly[..8].fill(line);
    assert!(stamped(&mostly, &[written(); 64]).is_some());
    mostly.push("no time here");
    assert!(stamped(&mostly, &[written(); 65]).is_none());
}

#[test]
fn stamps_read_with_other_times_or_from_changed_bytes_are_other_text_or_refused_and_never_more() {
    let lines: Vec<String> = (0..12).map(|n| format!("2026-09-23 00:47:{:02},{n:03} GET /notes/{n}", 20 + n)).collect();
    let values: Vec<&str> = lines.iter().map(String::as_str).collect();
    let column = stamped(&values, &[written(); 12]).unwrap();
    let mut codec = Codec::default();
    // an event's time is part of what spells its stamp back
    let mut reader = Reader::new(&column);
    let moved =
        read_stamped(&mut reader, &[written() + 1_000_000_000; 12], &mut Decoder { codec: &mut codec, left: 1 << 20 });
    assert!(moved[0].starts_with("2026-09-23 00:47:21,000"), "{}", moved[0]);
    for at in 0..column.len() {
        let mut changed = column.clone();
        changed[at] ^= 0x11;
        for bytes in [&changed[..], &column[..at]] {
            let mut reader = Reader::new(bytes);
            read_stamped(&mut reader, &[written(); 12], &mut Decoder { codec: &mut codec, left: 1 << 20 });
            let _ = reader.finish();
        }
    }
    let mut reader = Reader::new(&column);
    read_stamped(&mut reader, &[written(); 12], &mut Decoder { codec: &mut codec, left: 100 });
    assert!(reader.finish().is_err());
}
