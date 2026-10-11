use super::super::fixture::{DAY, SECOND, fixture, names};
use super::*;
use crate::records::NewEvent;

#[test]
fn an_event_older_than_its_stream_keeps_is_never_read_and_maintenance_removes_its_row() {
    let fixture = fixture();
    let api = fixture.store.events("api").keep(DAY).open().unwrap();
    let audit = fixture.store.events("audit").keep(30 * DAY).open().unwrap();
    api.append("request", &()).unwrap();
    audit.append("role.changed", &()).unwrap();
    fixture.pass(DAY + SECOND);
    api.append("fresh", &()).unwrap();

    // gone from every read at once, though its row is still in the file
    assert_eq!(names(&api.list().page().unwrap().events), ["fresh"]);
    let all = fixture.store.all_events().unwrap();
    assert_eq!(names(&all.list().page().unwrap().events), ["role.changed", "fresh"]);
    assert_eq!(fixture.rows("heads"), 3);

    // the same pass packs audit's event, which has waited its hour
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance { expired: 1, packed: 1, segments: 1 });
    assert_eq!((fixture.rows("heads"), fixture.rows("segments")), (1, 1));
    assert_eq!(names(&all.list().page().unwrap().events), ["role.changed", "fresh"]);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance::default());
}

#[test]
fn a_row_goes_once_its_newest_event_is_past_what_its_stream_keeps() {
    let fixture = fixture();
    let api = fixture.store.events("api").keep(DAY).open().unwrap();
    let early = fixture.now() - DAY + SECOND;
    api.append_all([NewEvent::new("early").at(early), NewEvent::new("late")]).unwrap();
    fixture.pass(SECOND * 2);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance::default());
    assert_eq!(names(&api.list().page().unwrap().events), ["late"]);
    fixture.pass(DAY);
    assert_eq!(maintain(&fixture.store).unwrap(), Maintenance { expired: 2, ..Maintenance::default() });
    assert_eq!(fixture.rows("heads"), 0);
}

#[test]
fn maintenance_with_nothing_to_remove_commits_nothing() {
    let fixture = fixture();
    fixture.events("api").append("request", &()).unwrap();
    let commits = fixture.records().file.commits();
    maintain(&fixture.store).unwrap();
    assert_eq!(fixture.records().file.commits(), commits);
}
