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
    assert_eq!(
        maintain(&fixture.store).unwrap(),
        Maintenance { expired: 1, packed: 1, segments: 1, ..Maintenance::default() }
    );
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

/// The records a process killed now would leave: the file and its log
/// copied while the store is open, as a crash leaves them on disk.
fn crash_image(fixture: &super::super::fixture::Fixture) -> tempfile::TempDir {
    let image = tempfile::tempdir().unwrap();
    for name in ["records.db", "records.db-wal"] {
        let from = fixture.dir.path().join(name);
        if from.exists() {
            std::fs::copy(from, image.path().join(name)).unwrap();
        }
    }
    image
}

fn names_after_crash(fixture: &super::super::fixture::Fixture, image: &tempfile::TempDir) -> Vec<String> {
    let options = crate::Options { clock: Some(fixture.clock.clone()), background: false, ..crate::Options::default() };
    let store = Store::open(image.path(), options).unwrap();
    let events = store.all_events().unwrap().list().page().unwrap().events;
    events.into_iter().map(|event| event.name).collect()
}

#[test]
fn an_abrupt_exit_loses_no_appended_event_before_or_after_a_pack() {
    let fixture = fixture();
    let audit = fixture.events("audit");
    audit.append_all([NewEvent::new("a"), NewEvent::new("b")]).unwrap();
    let before = crash_image(&fixture);
    fixture.pass(Duration::from_secs(3601));
    assert_eq!(maintain(&fixture.store).unwrap().segments, 1);
    audit.append("c", &()).unwrap();
    let after = crash_image(&fixture);
    assert_eq!(names_after_crash(&fixture, &before), ["a", "b"]);
    assert_eq!(names_after_crash(&fixture, &after), ["a", "b", "c"], "packed and waiting alike");
}

#[test]
fn a_reader_finds_each_event_once_while_heads_pack_and_segments_merge() {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let fixture = fixture();
    let api = fixture.events("api");
    let all = fixture.store.all_events().unwrap();
    let (done, reads) = (AtomicBool::new(false), AtomicUsize::new(0));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                let events = all.list().limit(1000).page().unwrap().events;
                let names: BTreeSet<&str> = events.iter().map(|event| event.name.as_str()).collect();
                assert_eq!(names.len(), events.len(), "an event read twice");
                assert_eq!(events.len() % 50, 0, "a round of 50 read in part: {}", events.len());
                reads.fetch_add(1, Ordering::Relaxed);
            }
        });
        let mut merged = 0;
        for round in 0..8 {
            api.append_all((0..50).map(|n| NewEvent::new(&format!("{round}-{n}")))).unwrap();
            fixture.pass(Duration::from_secs(3601));
            merged += maintain(&fixture.store).unwrap().merged;
        }
        done.store(true, Ordering::Release);
        assert!(merged > 0, "segments merged meanwhile");
    });
    assert!(reads.load(Ordering::Relaxed) > 0);
}

#[test]
fn a_blocks_id_is_never_given_twice_though_its_block_left() {
    let fixture = fixture();
    let api = fixture.store.events("api").keep(DAY).open().unwrap();
    api.append("first", &()).unwrap();
    fixture.pass(Duration::from_secs(3601));
    maintain(&fixture.store).unwrap();
    fixture.pass(DAY);
    maintain(&fixture.store).unwrap();
    assert_eq!(fixture.rows("blocks"), 0, "expired");
    api.append("second", &()).unwrap();
    fixture.pass(Duration::from_secs(3601));
    maintain(&fixture.store).unwrap();
    let ids = fixture.records().file.read(|connection| {
        Ok(connection
            .query_row("select min(id), max(id) from blocks", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap())
    });
    assert_eq!(ids.unwrap(), (2_i64, 2_i64));
}
