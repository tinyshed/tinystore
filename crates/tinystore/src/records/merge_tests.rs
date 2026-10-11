use std::time::Duration;

use rusqlite::OptionalExtension;

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
}

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
    let made = made_of(&events, &mut codec).unwrap();
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
        made_of(&events, &mut codec).unwrap()
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
fn a_stream_whose_merge_fails_is_left_and_the_others_merge() {
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
    assert!(maintain(&fixture.store).is_err());
    let rows = rows(&fixture);
    // api's segments 1, 3, 5 and 7 stay as they were; web's 2, 4, 6 and 8 merged into 2
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
