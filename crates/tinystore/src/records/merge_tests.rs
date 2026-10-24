use std::time::Duration;

use rusqlite::OptionalExtension;

use super::super::event::Kept;
use super::super::fixture::{DAY, Fixture, fixture, names};
use super::super::maintain::{Maintenance, maintain};
use super::super::pack::made_of;
use super::*;
use crate::records::NewEvent;

const HOUR: Duration = Duration::from_secs(60 * 60);

fn small(id: i64, first: i64, last: i64, held: usize) -> Small {
    Small { id, first, last, held, weight: held * 100 }
}

fn ids(run: Option<Vec<Small>>) -> Vec<i64> {
    run.unwrap_or_default().iter().map(|segment| segment.id).collect()
}

/// Each segment's row: its id, holder, start, count and held.
fn rows(fixture: &Fixture) -> Vec<(i64, Option<i64>, i64, i64, i64)> {
    let sql = "select id, holder, start, count, held from segments order by id";
    let read = |connection: &rusqlite::Connection| {
        let mut select = connection.prepare(sql).unwrap();
        let rows = select.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)));
        Ok(rows.unwrap().map(Result::unwrap).collect())
    };
    fixture.records().file.read(read).unwrap()
}

/// Appends `events` named `{round}-{n}`, an hour on packs them as one
/// segment, and says what that maintenance did.
fn segment_of(fixture: &Fixture, round: usize, events: usize) -> Maintenance {
    let api = fixture.events("api");
    api.append_all((0..events).map(|n| NewEvent::new(&format!("{round}-{n}")))).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap()
}

#[test]
fn small_segments_merge_four_of_a_size_in_the_order_of_their_times() {
    assert_eq!([1, 3, 4, 15, 16, 63, 143, 593].map(size_of), [1, 1, 2, 2, 3, 3, 4, 5]);
    // three of the smallest size wait for a fourth; four of the next merge, earliest first
    let segments = vec![
        small(1, 300, 310, 30),
        small(2, 100, 110, 40),
        small(3, 400, 410, 35),
        small(4, 200, 210, 38),
        small(5, 0, 5, 2),
        small(6, 10, 15, 3),
        small(7, 20, 25, 1),
    ];
    assert_eq!(ids(pick(segments)), [2, 4, 1, 3]);
    // four of the smallest size go before four of a larger
    let both = vec![small(1, 300, 310, 30), small(2, 100, 110, 40), small(3, 400, 410, 35), small(4, 200, 210, 38)];
    let smaller = vec![small(5, 0, 5, 2), small(6, 10, 15, 3), small(7, 20, 25, 1), small(8, 30, 35, 2)];
    assert_eq!(ids(pick(both.into_iter().chain(smaller).collect())), [5, 6, 7, 8]);
    let fewer = vec![small(1, 300, 310, 30), small(2, 100, 110, 40), small(3, 400, 410, 35), small(4, 200, 210, 5)];
    assert_eq!(pick(fewer), None);
}

#[test]
fn a_merge_leaves_overlapping_segments_out() {
    let segments = vec![
        small(1, 100, 200, 30),
        small(2, 150, 160, 30),
        small(3, 200, 300, 30),
        small(4, 300, 400, 30),
        small(5, 400, 500, 30),
    ];
    // the second begins before the first ends; the third begins where it ends
    assert_eq!(ids(pick(segments)), [1, 3, 4, 5]);
    let three = vec![small(1, 100, 200, 30), small(2, 150, 160, 30), small(3, 155, 300, 30), small(4, 300, 400, 30)];
    assert_eq!(pick(three), None);
    // one that begins where the one before ends is taken whichever was packed first
    let earlier = vec![small(1, 100, 100, 1), small(2, 95, 100, 2), small(3, 200, 210, 1), small(4, 300, 310, 1)];
    assert_eq!(ids(pick(earlier)), [2, 1, 3, 4]);
}

// contract: REC-04
#[test]
fn a_merge_after_a_member_packed_later_that_ends_at_its_time_keeps_the_order_they_were_appended() {
    let fixture = fixture();
    let api = fixture.events("api");
    let start = fixture.now();
    let at = |name: &str, millis: u64| NewEvent::new(name).at(start + Duration::from_millis(millis));
    let rounds = [vec![at("a", 100)], vec![at("e", 95), at("b", 100)], vec![at("c", 200)], vec![at("d", 300)]];
    let mut merged = 0;
    for (round, events) in rounds.into_iter().enumerate() {
        api.append_all(events).unwrap();
        fixture.pass(HOUR);
        merged += maintain(&fixture.store).unwrap().merged;
        if round == 1 {
            assert_eq!(names(&api.list().page().unwrap().events), ["e", "a", "b"]);
        }
    }
    // e and b's segment, then a's, which begins where it ends: b lies before a in the merged blocks
    assert_eq!((merged, rows(&fixture)[0]), (3, (1, None, 2, 1, 5)));
    assert_eq!(names(&api.list().page().unwrap().events), ["e", "a", "b", "c", "d"]);
}

// contract: REC-04, REC-07
#[test]
fn a_late_segment_merges_after_one_that_ends_at_its_time_and_a_poll_past_it_finds_each_event_once() {
    let fixture = fixture();
    let (api, all) = (fixture.events("api"), fixture.store.all_events().unwrap());
    let at = fixture.now() - Duration::from_mins(1);
    let half = Duration::from_mins(30);
    let append = |name: &str, time| api.append_all([NewEvent::new(name).at(time)]).unwrap();
    append("z", at + Duration::from_secs(30));
    fixture.pass(half);
    // thirty seconds behind z, which waits on time: late, and in its late head for an hour
    append("y", at);
    fixture.pass(half);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1, "z alone");
    // nothing waits on time now, so x comes on time at y's time
    api.append_all([NewEvent::new("w").at(at - Duration::from_secs(5)), NewEvent::new("x").at(at)]).unwrap();
    fixture.pass(half);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1, "y's late head");
    fixture.pass(half);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1, "w and x");
    let read = || names(&api.list().page().unwrap().events).into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(read(), ["w", "y", "x", "z"]);
    // a consumer polls z's segment and y's, and holds the cursor of w and x's
    let polled = [all.poll(None).unwrap(), all.poll(Some("2:0")).unwrap()];
    let polled: Vec<(&str, Vec<&str>)> =
        polled.iter().map(|batch| (batch.cursor.as_str(), names(&batch.events))).collect();
    assert_eq!(polled, [("2:0", vec!["z"]), ("3:0", vec!["y"])]);
    append("v", at + Duration::from_secs(60));
    fixture.pass(HOUR);
    // w and x's segment, y's that begins where it ends, z's and v's, in the order of their times
    assert_eq!(maintain(&fixture.store).unwrap().merged, 3);
    assert_eq!(rows(&fixture)[..3], [(1, None, 3, 1, 5), (2, Some(1), 2, 1, 0), (3, Some(1), 0, 2, 0)]);
    assert_eq!(read(), ["w", "y", "x", "z", "v"]);
    let after = [all.poll(Some("3:0")).unwrap(), all.poll(Some("4:0")).unwrap()];
    let after: Vec<(&str, Vec<&str>)> =
        after.iter().map(|batch| (batch.cursor.as_str(), names(&batch.events))).collect();
    assert_eq!(after, [("4:0", vec!["w", "x"]), ("5:0", vec!["v"])]);
}

/// Events of api to append, a round at a time: each a name and its time in
/// milliseconds from `start`.
type Rounds<'a> = &'a [&'a [(&'a str, u64)]];

/// Appends each round's events and packs them an hour on as a segment of
/// their own, and says how many segments merged.
fn pack_rounds(fixture: &Fixture, start: std::time::SystemTime, rounds: Rounds<'_>) -> u64 {
    let api = fixture.events("api");
    let mut merged = 0;
    for events in rounds {
        let at = |(name, millis): &(&str, u64)| NewEvent::new(name).at(start + Duration::from_millis(*millis));
        api.append_all(events.iter().map(at)).unwrap();
        fixture.pass(HOUR);
        merged += maintain(&fixture.store).unwrap().merged;
    }
    merged
}

/// What a read of api gives once every round is packed, and how many
/// segments merged.
fn read_after(rounds: Rounds<'_>) -> (Vec<String>, u64) {
    let fixture = fixture();
    let merged = pack_rounds(&fixture, fixture.now(), rounds);
    let read = fixture.events("api").list().page().unwrap();
    (names(&read.events).into_iter().map(str::to_owned).collect(), merged)
}

// contract: REC-04
#[test]
fn a_merge_written_after_a_larger_segment_that_holds_one_of_its_times_keeps_the_order_they_were_appended() {
    // x's segment, of the next size, is written after a's and holds its time
    let larger: &[(&str, u64)] = &[("x", 100), ("x2", 101), ("x3", 102), ("x4", 103)];
    let rounds = [&[("a", 100)][..], larger, &[("c", 200)], &[("d", 300)], &[("f", 400)]];
    assert_eq!(read_after(&rounds), (["a", "x", "x2", "x3", "x4", "c", "d", "f"].map(String::from).to_vec(), 3));
}

// contract: REC-04
#[test]
fn a_segment_a_merge_leaves_out_for_overlapping_keeps_its_place_among_events_of_one_time() {
    // x's segment begins before a's ends, so a merge in time order leaves it
    // out, and a's merges with the others
    let rounds =
        [&[("a", 100), ("a2", 110)][..], &[("x", 100), ("x2", 105)], &[("c", 200)], &[("d", 300)], &[("f", 400)]];
    assert_eq!(read_after(&rounds), (["a", "x", "x2", "a2", "c", "d", "f"].map(String::from).to_vec(), 3));
}

#[test]
fn a_merge_reads_its_members_alone_so_a_segment_beside_them_that_no_longer_reads_stops_nothing() {
    // x's segment, written after a's, spans a's time and holds none of it
    let larger: &[(&str, u64)] = &[("x", 99), ("x2", 101), ("x3", 102), ("x4", 103)];
    let rounds = [&[("a", 100)][..], larger, &[("c", 200)], &[("d", 300)]];
    let fixture = fixture();
    let start = fixture.now();
    pack_rounds(&fixture, start, &rounds);
    let broken = |tx: &rusqlite::Connection| execute(tx, "update segments set body = x'00' where id = 2", [], "break");
    fixture.records().file.write(0, move |tx| broken(tx).map(drop)).unwrap();
    assert_eq!(pack_rounds(&fixture, start, &[&[("f", 400)]]), 3);
    assert!(fixture.store.all_events().unwrap().damaged().unwrap().is_empty());
    let read = fixture.events("api").list().page().unwrap_err();
    assert_eq!(read.kind(), crate::ErrorKind::Corrupt, "{read}");
}

// contract: DUR-08
#[test]
fn four_small_segments_merge_into_the_one_of_lowest_id_and_keep_their_places() {
    let fixture = fixture();
    for (round, events) in [4, 6, 5].into_iter().enumerate() {
        assert_eq!(segment_of(&fixture, round, events).merged, 0);
    }
    let merged = segment_of(&fixture, 3, 7);
    assert_eq!(merged, Maintenance { packed: 7, segments: 1, merged: 3, ..Maintenance::default() });
    assert_eq!(
        rows(&fixture),
        [(1, None, 0, 4, 22), (2, Some(1), 4, 6, 0), (3, Some(1), 10, 5, 0), (4, Some(1), 15, 7, 0)]
    );
    assert_eq!(fixture.rows("blocks"), 1);
    let read = fixture.events("api").list().page().unwrap();
    let expected: Vec<String> = [4, 6, 5, 7]
        .iter()
        .enumerate()
        .flat_map(|(round, events)| (0..*events).map(move |n| format!("{round}-{n}")))
        .collect();
    assert_eq!(names(&read.events), expected);
}

// contract: REC-07
#[test]
fn a_merged_segment_merges_again_and_each_place_still_names_its_own_events() {
    let fixture = fixture();
    let mut merged = 0;
    for round in 0..16 {
        merged += segment_of(&fixture, round, 5).merged;
    }
    // four merges of four, then one of the four segments they made
    assert_eq!(merged, 15);
    let rows = rows(&fixture);
    assert_eq!(rows[0], (1, None, 0, 5, 80));
    let read = fixture.events("api").list().limit(100).page().unwrap();
    for (id, holder, start, count, held) in &rows[1..] {
        assert_eq!((holder, held), (&Some(1), &0));
        let at = usize::try_from(*start).unwrap();
        let named = names(&read.events[at..at + usize::try_from(*count).unwrap()]);
        assert!(named.iter().all(|name| name.starts_with(&format!("{}-", id - 1))), "segment {id}: {named:?}");
    }
}

#[test]
fn a_merged_segment_expires_whole_with_its_places() {
    let fixture = fixture();
    fixture.store.events("api").keep(DAY).open().unwrap();
    for round in 0..4 {
        segment_of(&fixture, round, 4);
    }
    assert_eq!(rows(&fixture).len(), 4);
    fixture.pass(DAY);
    let expired = maintain(&fixture.store).unwrap();
    assert_eq!(expired, Maintenance { expired: 16, ..Maintenance::default() });
    assert_eq!((fixture.rows("segments"), fixture.rows("blocks")), (0, 0));
}

// contract: DUR-08
#[test]
fn a_merge_that_meets_a_changed_segment_leaves_everything_as_it_was() {
    let fixture = fixture();
    for round in 0..4 {
        segment_of(&fixture, round, 4);
    }
    let before = (rows(&fixture), fixture.rows("blocks"));
    let records = fixture.records();
    let mut codec = Codec::default();
    let events: Vec<Kept> = (0..8).map(|n| NewEvent::new("again").kept(1_800_000_000_000_000_000 + n)).collect();
    let made = made_of(&events, &(0..8).collect::<Vec<_>>(), &mut codec).unwrap();
    // segment 2 is a place now, which a merge takes for a segment holding its own events
    let run = [small(1, 0, 1, 16), small(2, 2, 3, 4)];
    let failed = records.publish_merge(1, &run, &made).unwrap_err();
    assert!(failed.to_string().contains("segment 2 changed while it was merged"), "{failed}");
    assert_eq!((rows(&fixture), fixture.rows("blocks")), before);
    let place: Option<Vec<u8>> = records
        .file
        .read(|connection| {
            Ok(connection.query_row("select body from segments where id = 2", [], |row| row.get(0)).optional().unwrap())
        })
        .unwrap();
    assert_eq!(place, Some(Vec::new()));
    assert_eq!(fixture.events("api").list().page().unwrap().events.len(), 16);
}

// contract: REC-07
#[test]
fn places_move_by_where_their_holders_events_begin_when_a_holder_is_not_the_earliest() {
    let fixture = fixture();
    // three of one size and one of the next, which make no merge of their own
    for (round, events) in [4, 5, 6, 16].into_iter().enumerate() {
        segment_of(&fixture, round, events);
    }
    let records = fixture.records();
    let mut codec = Codec::default();
    let mut made = |count: i64| {
        let events: Vec<Kept> =
            (0..count).map(|n| NewEvent::new("merged").kept(1_800_000_000_000_000_000 + n)).collect();
        made_of(&events, &(0..count).collect::<Vec<_>>(), &mut codec).unwrap()
    };
    // segment 3 taken as earlier than 2, as a late segment is: 2 holds both, its own after 3's
    records.publish_merge(1, &[small(3, 0, 0, 6), small(2, 0, 0, 5)], &made(11)).unwrap();
    assert_eq!(rows(&fixture)[1..3], [(2, None, 6, 5, 11), (3, Some(2), 0, 6, 0)]);
    // 4 earlier than 2: 2's own events and its place move past 4's
    records.publish_merge(1, &[small(4, 0, 0, 16), small(2, 0, 0, 11)], &made(27)).unwrap();
    assert_eq!(
        rows(&fixture),
        [(1, None, 0, 4, 4), (2, None, 22, 5, 27), (3, Some(2), 16, 6, 0), (4, Some(2), 0, 16, 0)]
    );
    // 2 earlier than 1: 1 holds all, and 2's places keep where they begin among its events
    records.publish_merge(1, &[small(2, 0, 0, 27), small(1, 0, 0, 4)], &made(31)).unwrap();
    assert_eq!(
        rows(&fixture),
        [(1, None, 27, 4, 31), (2, Some(1), 22, 5, 0), (3, Some(1), 16, 6, 0), (4, Some(1), 0, 16, 0)]
    );
}

#[test]
fn a_merge_goes_past_a_segment_that_no_longer_reads_and_the_other_streams_merge() {
    let fixture = fixture();
    let (api, web) = (fixture.events("api"), fixture.events("web"));
    let round = |round: usize| {
        api.append_all((0..4).map(|n| NewEvent::new(&format!("{round}-{n}")))).unwrap();
        web.append_all((0..4).map(|n| NewEvent::new(&format!("{round}-{n}")))).unwrap();
        fixture.pass(HOUR);
    };
    for number in 0..3 {
        round(number);
        maintain(&fixture.store).unwrap();
    }
    let broken = |tx: &rusqlite::Connection| execute(tx, "update segments set body = x'00' where id = 1", [], "break");
    fixture.records().file.write(0, move |tx| broken(tx).map(drop)).unwrap();
    round(3);
    maintain(&fixture.store).unwrap();
    let damaged = fixture.store.all_events().unwrap().damaged().unwrap();
    assert_eq!(damaged.iter().map(ToString::to_string).collect::<Vec<_>>(), ["segment 1 of api"]);
    let rows = rows(&fixture);
    // api's segments 1, 3, 5 and 7 stay as they were, three that read too few
    // to merge; web's 2, 4, 6 and 8 merged into 2
    assert_eq!(
        rows.iter().filter(|(_, holder, ..)| holder.is_none()).map(|(id, ..)| *id).collect::<Vec<_>>(),
        [1, 2, 3, 5, 7]
    );
    assert_eq!(rows[1], (2, None, 0, 4, 16));
}

#[test]
fn a_holders_places_are_found_through_an_index() {
    let fixture = fixture();
    let plan = |connection: &rusqlite::Connection| {
        let mut explain = connection.prepare(&format!("explain query plan {MOVE_PLACES}")).unwrap();
        let details = explain.query_map(rusqlite::params![1, 0, 1], |row| row.get::<_, String>(3)).unwrap();
        Ok(details.map(Result::unwrap).collect::<Vec<_>>().join("; "))
    };
    let plan = fixture.records().file.read(plan).unwrap();
    assert!(plan.contains("USING INDEX segments_by_holder"), "{plan}");
}
