//! A database's streams: an event appended in a transaction or a batch of
//! the database reaches its stream exactly when the rows commit.

use std::time::Duration;

use serde::Serialize;

use super::super::engine::KEEP;
use super::super::event::Event;
use super::super::fixture::{DAY, Fixture, fixture, names};
use super::super::maintain::maintain;
use crate::sql::Database;
use crate::{Error, ErrorKind, Result, sql};

const ROLES: &str = "create table roles (user_id integer primary key, role text not null) strict";

#[derive(Serialize)]
struct RoleChanged {
    user_id: i64,
    role: &'static str,
}

const ADMIN: RoleChanged = RoleChanged { user_id: 42, role: "admin" };

fn app(fixture: &Fixture) -> Database {
    fixture.store.database("app").migrations([("0001_roles.sql", ROLES)]).open().unwrap()
}

fn roles(db: &Database) -> i64 {
    db.scalar(sql!("select count(*) from roles")).unwrap()
}

/// The events the outbox of `db` holds, not moved yet.
fn kept(db: &Database) -> i64 {
    db.scalar(sql!("select count(*) from _tinystore_records_outbox")).unwrap()
}

/// The stream `audit` as the store reads it, opened from the store.
fn streamed(fixture: &Fixture) -> Vec<Event> {
    fixture.store.events("audit").open().unwrap().list().since(DAY).page().unwrap().events
}

/// Keeps an event in the outbox as a transaction would, by hand: what a
/// crash between the commit and the move leaves.
fn keep_by_hand(db: &Database, id: &str, at: i64) {
    let row = sql!(
        "insert into _tinystore_records_outbox (stream, id, at, name, context, fields)
            values ('audit', ?, ?, 'role.changed', '{}', '{\"user_id\":42}')",
        id,
        at
    );
    db.exec(row).unwrap();
}

fn nanos(fixture: &Fixture) -> i64 {
    crate::unix_nanos(fixture.now())
}

// contract: REC-13
#[test]
fn an_event_appended_in_a_transaction_that_rolls_back_is_never_written() {
    let fixture = fixture();
    let db = app(&fixture);
    let audit = db.events("audit").open().unwrap();
    let attempt: Result<()> = db.tx(|tx| {
        tx.exec(sql!("insert into roles (user_id, role) values (?, ?)", 42, "admin"))?;
        tx.with(&audit).append("role.changed", &ADMIN)?;
        tx.exec(sql!("insert into roles (user_id, role) values (?, ?)", 42, "admin"))?;
        Ok(())
    });
    assert_eq!(attempt.unwrap_err().kind(), ErrorKind::Conflict);
    assert_eq!((roles(&db), kept(&db)), (0, 0));
    assert!(audit.list().since(DAY).page().unwrap().events.is_empty());
    maintain(&fixture.store).unwrap();
    assert!(streamed(&fixture).is_empty(), "nor moved later");
}

// contract: REC-13
#[test]
fn an_event_appended_in_a_transaction_is_in_its_stream_when_the_transaction_returns() {
    let fixture = fixture();
    let db = app(&fixture);
    let audit = db.events("audit").open().unwrap();
    let at = fixture.now() - Duration::from_secs(5);
    let written = db.tx(|tx| {
        tx.exec(sql!("insert into roles (user_id, role) values (?, ?)", 42, "admin"))?;
        tx.with(&audit).append("role.changed", &ADMIN)?;
        tx.with(&audit).event("role.seen").at(at).message("by ann").append(&())
    });
    assert!(written.unwrap());
    let read = streamed(&fixture);
    assert_eq!(names(&read), ["role.seen", "role.changed"], "by their times");
    assert_eq!(read[1].fields.json("role"), Some(r#""admin""#));
    assert_eq!((read[0].at, read[0].message.as_deref()), (at, Some("by ann")));
    assert!(read.iter().all(|event| event.id.as_ref().is_some_and(|id| id.len() == 36)), "{read:?}");
    assert_eq!((roles(&db), kept(&db)), (1, 0));
    let every = fixture.store.all_events().unwrap().list().since(DAY).page().unwrap().events;
    assert_eq!(every.len(), 2, "every stream's read sees it");
    assert_eq!(audit.list().since(DAY).page().unwrap().events, read, "and the database's handle reads the stream");
}

// contract: REC-13
#[test]
fn an_event_a_crash_left_in_the_outbox_is_moved_once_at_the_next_open() {
    let fixture = fixture();
    let db = app(&fixture);
    let audit = db.events("audit").open().unwrap();
    db.tx(|tx| tx.with(&audit).event("role.changed").id("change-1").append(&ADMIN)).unwrap();
    // the move's append committed, its removal from the outbox did not
    let at = streamed(&fixture)[0].at;
    keep_by_hand(&db, "change-1", crate::unix_nanos(at));
    // the commit did, the move did not
    keep_by_hand(&db, "change-2", nanos(&fixture));
    drop((audit, db));

    let fixture = fixture.reopen();
    let db = app(&fixture);
    assert_eq!(kept(&db), 2);
    db.events("audit").open().unwrap();
    assert_eq!(kept(&db), 0);
    let read = streamed(&fixture);
    let ids: Vec<Option<&str>> = read.iter().map(|event| event.id.as_deref()).collect();
    assert_eq!(ids, [Some("change-1"), Some("change-2")], "each once");
    assert_eq!(read[0].fields.json("role"), Some(r#""admin""#), "the first of an id stands");
}

// contract: REC-13
#[test]
fn maintenance_moves_what_the_outbox_holds_once() {
    let fixture = fixture();
    let db = app(&fixture);
    let _audit = db.events("audit").open().unwrap();
    keep_by_hand(&db, "change-1", nanos(&fixture));
    keep_by_hand(&db, "change-1", nanos(&fixture));
    maintain(&fixture.store).unwrap();
    assert_eq!((kept(&db), streamed(&fixture).len()), (0, 1));
    keep_by_hand(&db, "change-1", nanos(&fixture));
    maintain(&fixture.store).unwrap();
    assert_eq!((kept(&db), streamed(&fixture).len()), (0, 1), "a move made again writes nothing");
}

// contract: REC-13
#[test]
fn a_batch_appends_its_events_with_its_rows_or_not_at_all() {
    let fixture = fixture();
    let db = app(&fixture);
    let audit = db.events("audit").open().unwrap();
    let done = db
        .batch([
            sql!("insert into roles (user_id, role) values (?, ?)", 42, "admin").into(),
            audit.appending("role.changed", &ADMIN),
            audit.event("role.changed").id("change-7").appending(&ADMIN),
        ])
        .unwrap();
    assert_eq!(done.iter().map(|done| done.changes).collect::<Vec<_>>(), [1, 1, 1]);
    assert_eq!((streamed(&fixture).len(), kept(&db)), (2, 0), "written when the batch returns");

    let refused = db.batch([
        audit.appending("role.changed", &ADMIN),
        sql!("insert into roles (user_id, role) values (?, ?)", 42, "admin").into(),
    ]);
    assert_eq!(refused.unwrap_err().kind(), ErrorKind::Conflict);
    let again = db.batch([audit.event("role.changed").id("change-7").appending(&ADMIN)]).unwrap();
    assert_eq!(again[0].changes, 0, "its stream holds the id");
    assert_eq!((streamed(&fixture).len(), kept(&db), roles(&db)), (2, 0, 1));
}

#[test]
fn an_id_its_stream_or_the_transaction_holds_is_appended_as_nothing() {
    let fixture = fixture();
    let db = app(&fixture);
    let audit = db.events("audit").open().unwrap();
    assert!(audit.event("push").id("d-1").append(&()).unwrap(), "outside a transaction, to the stream at once");
    assert_eq!(kept(&db), 0);
    let (held, twice) = db
        .tx(|tx| {
            let held = tx.with(&audit).event("push").id("d-1").append(&())?;
            assert!(tx.with(&audit).event("push").id("d-2").append(&())?);
            Ok::<_, Error>((held, tx.with(&audit).event("push").id("d-2").append(&())?))
        })
        .unwrap();
    assert!(!held && !twice);
    let ids: Vec<Option<String>> = streamed(&fixture).into_iter().map(|event| event.id).collect();
    assert_eq!(ids, [Some("d-1".to_owned()), Some("d-2".to_owned())]);
}

#[test]
fn a_stream_kept_elsewhere_is_refused_in_a_transaction_and_a_batch() {
    let fixture = fixture();
    let (db, other) = (app(&fixture), fixture.store.database("other").open().unwrap());
    let (store_audit, other_audit) =
        (fixture.store.events("audit").open().unwrap(), other.events("audit").open().unwrap());
    for refused in [
        db.tx(|tx| tx.with(&store_audit).append("role.changed", &ADMIN)).unwrap_err(),
        db.tx(|tx| tx.with(&other_audit).append("role.changed", &ADMIN)).unwrap_err(),
        db.batch([store_audit.appending("role.changed", &ADMIN)]).unwrap_err(),
        db.batch([other_audit.appending("role.changed", &ADMIN)]).unwrap_err(),
    ] {
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
        assert!(refused.to_string().contains("events audit"), "{refused}");
    }
    let refused = db.tx(|tx| tx.with(&other_audit).event("").append(&())).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    assert!(streamed(&fixture).is_empty());
}

// contract: REC-13
#[test]
fn an_append_around_the_databases_transaction_from_inside_it_is_refused() {
    let fixture = fixture();
    let db = app(&fixture);
    let audit = db.events("audit").open().unwrap();
    let attempt: Result<()> = db.tx(|tx| {
        tx.exec(sql!("insert into roles (user_id, role) values (?, ?)", 42, "admin"))?;
        audit.append("role.changed", &ADMIN)?;
        Ok(())
    });
    let refused = attempt.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    assert!(refused.to_string().contains("tx.with(&events)"), "{refused}");
    assert_eq!((roles(&db), streamed(&fixture).len()), (0, 0));
    let store_audit = fixture.store.events("audit").open().unwrap();
    db.tx(|_| store_audit.append("beside", &())).unwrap();
    assert_eq!(names(&streamed(&fixture)), ["beside"], "a stream opened from the store is another file's");
}

#[test]
fn an_event_its_stream_keeps_no_longer_by_its_move_is_dropped_and_one_ahead_waits() {
    let fixture = fixture();
    let db = app(&fixture);
    let _audit = db.events("audit").open().unwrap();
    keep_by_hand(&db, "gone", nanos(&fixture) - KEEP.as_nanos() as i64 - 1);
    keep_by_hand(&db, "ahead", nanos(&fixture) + 3_600_000_000_000);
    keep_by_hand(&db, "now", nanos(&fixture));
    maintain(&fixture.store).unwrap();
    let ids: Vec<Option<String>> = streamed(&fixture).into_iter().map(|event| event.id).collect();
    assert_eq!((ids, kept(&db)), (vec![Some("now".to_owned())], 1), "the one ahead stays");
    fixture.pass(Duration::from_secs(3_600));
    maintain(&fixture.store).unwrap();
    assert_eq!((streamed(&fixture).len(), kept(&db)), (2, 0));
}
