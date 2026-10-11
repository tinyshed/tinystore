use super::super::fixture::{Fixture, fixture, names};
use super::*;
use crate::records::{Maintenance, NewEvent, maintain};

/// What waits in a head is packed once its oldest row has waited this long.
const HOUR: Duration = Duration::from_secs(3601);

/// Every body of a table zeroed, as a disk that lost them would leave them.
fn zero(fixture: &Fixture, table: &str) {
    let sql = format!("update {table} set body = zeroblob(length(body))");
    fixture.records().file.write(0, move |tx| execute(tx, &sql, [], "zero the bodies").map(drop)).unwrap();
}

#[test]
fn a_changed_head_row_is_named_as_a_drop_names_it_and_once_dropped_reads_go_past_it() {
    let fixture = fixture();
    let audit = fixture.events("audit");
    audit.append("signup", &()).unwrap();
    zero(&fixture, "heads");
    let failed = audit.list().page().unwrap_err();
    assert_eq!(failed.kind(), ErrorKind::Corrupt);

    let all = fixture.store.all_events().unwrap();
    let damaged = all.damaged().unwrap();
    let [damage] = damaged.as_slice() else { panic!("{damaged:?}") };
    assert!(failed.to_string().contains(&damage.to_string()), "{failed} names {damage}");
    assert_eq!((damage.part, damage.stream.as_str(), damage.first), (Part::HeadRow, "audit", fixture.now()));

    all.drop(damage).unwrap();
    assert!(audit.list().page().unwrap().events.is_empty());
    assert!(all.damaged().unwrap().is_empty());
    audit.append("again", &()).unwrap();
    assert_eq!(names(&audit.list().page().unwrap().events), ["again"]);
}

#[test]
fn only_what_no_longer_reads_is_dropped_a_segment_whole_and_a_poll_behind_it_told() {
    let fixture = fixture();
    let audit = fixture.events("audit");
    audit.append_all([NewEvent::new("a"), NewEvent::new("b")]).unwrap();
    let all = fixture.store.all_events().unwrap();
    let cursor = all.poll(None).unwrap().cursor;
    let now = fixture.now();
    let reads =
        Damage { part: Part::HeadRow, id: 1, stream: "audit".to_owned(), first: now, last: now, error: String::new() };
    assert_eq!(all.drop(&reads).unwrap_err().kind(), ErrorKind::Invalid, "it still reads");

    fixture.pass(HOUR);
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1);
    zero(&fixture, "blocks");
    let failed = audit.list().page().unwrap_err();
    let damaged = all.damaged().unwrap();
    let [damage] = damaged.as_slice() else { panic!("{damaged:?}") };
    assert_eq!(damage.part, Part::Segment);
    assert!(failed.to_string().contains(&format!("{damage}: block ")), "{failed} names {damage}");

    all.drop(damage).unwrap();
    assert!(audit.list().page().unwrap().events.is_empty());
    assert_eq!((fixture.rows("segments"), fixture.rows("blocks")), (0, 0));
    // a poll says what it passed with the segment after
    audit.append("c", &()).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    let behind = all.poll(Some(&cursor)).unwrap();
    assert_eq!((names(&behind.events), behind.expired), (vec!["c"], 2), "its events counted as expired");
}

#[test]
fn a_damaged_head_row_is_left_for_a_drop_and_the_rows_around_it_still_pack() {
    let fixture = fixture();
    let (api, audit) = (fixture.events("api"), fixture.events("audit"));
    audit.append("signup", &()).unwrap();
    zero(&fixture, "heads");
    api.append("request", &()).unwrap();
    audit.append("later", &()).unwrap();
    fixture.pass(HOUR);

    let done = maintain(&fixture.store).unwrap();
    assert_eq!((done.packed, done.segments), (2, 2), "api's row and audit's later one");
    let damaged = fixture.store.all_events().unwrap().damaged().unwrap();
    let parts: Vec<(Part, &str)> = damaged.iter().map(|damage| (damage.part, damage.stream.as_str())).collect();
    assert_eq!(parts, [(Part::HeadRow, "audit")]);
    assert_eq!(fixture.rows("heads"), 1, "it waits for its drop");
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance::default(), "and is not read again and again");
}
