//! Adding, setting, updating, cancelling and finding jobs.

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::fixture::{DAY, HOUR, MINUTE, SECOND, fixture};
use super::{Concurrency, Filter, Queue, State};
use crate::{ErrorKind, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Note {
    text: String,
}

fn note(text: &str) -> Note {
    Note { text: text.to_owned() }
}

/// Runs what is due and answers the texts of the jobs that ran, in order.
fn ran(queue: &Queue<Note>) -> Vec<String> {
    let seen = Mutex::new(Vec::new());
    queue
        .concurrency(1)
        .run_due(|note: Note, _run| -> Result<()> {
            seen.lock().unwrap().push(note.text);
            Ok(())
        })
        .unwrap();
    seen.into_inner().unwrap()
}

// contract: JOB-01
#[test]
fn a_job_runs_at_its_time_and_not_before() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    assert!(notes.delay(HOUR).add(&note("later")).unwrap());
    assert!(notes.add(&note("now")).unwrap());
    assert_eq!(ran(&notes), ["now"]);
    f.clock.advance(HOUR - SECOND);
    assert!(ran(&notes).is_empty());
    f.clock.advance(SECOND);
    assert_eq!(ran(&notes), ["later"]);
    assert!(ran(&notes).is_empty(), "a done job leaves the queue");
}

// contract: DUR-01
#[test]
fn an_added_job_survives_a_reopen() {
    let mut f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    notes.add(&note("kept")).unwrap();
    f.reopen();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    assert_eq!(ran(&notes), ["kept"]);
}

// contract: JOB-01
#[test]
fn jobs_run_in_the_order_of_their_time_equal_times_in_the_order_added() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    notes.delay(2 * SECOND).add(&note("third")).unwrap();
    notes.delay(SECOND).add(&note("first")).unwrap();
    notes.delay(SECOND).add(&note("second")).unwrap();
    f.clock.advance(2 * SECOND);
    assert_eq!(ran(&notes), ["first", "second", "third"]);
}

// contract: JOB-03
#[test]
fn add_adds_only_to_an_id_that_is_free() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").attempts(1).open().unwrap();
    assert!(notes.id("a").delay(HOUR).add(&note("first")).unwrap());
    assert!(!notes.id("a").add(&note("second")).unwrap(), "a scheduled job keeps its id");
    assert_eq!(notes.get("a").unwrap().unwrap().value, note("first"), "and its value");
    f.clock.advance(HOUR);
    assert_eq!(ran(&notes), ["first"]);
    assert!(notes.id("a").add(&note("again")).unwrap(), "a done job leaves its id free");

    let failing = notes.concurrency(1).run_due(|_: Note, _run| -> Result<(), String> { Err("down".into()) });
    assert_eq!(failing.unwrap(), 1);
    assert_eq!(notes.get("a").unwrap().unwrap().state, State::Failed);
    assert!(notes.id("a").add(&note("after the failure")).unwrap(), "a failed job holds no id");
    let job = notes.get("a").unwrap().unwrap();
    assert_eq!((job.state, job.value, job.attempt), (State::Waiting, note("after the failure"), 0));
    assert_eq!(job.error.as_deref(), Some("down"), "it says why the last one failed until its own run ends");
    assert!(!notes.id("a").add(&note("third")).unwrap(), "the new job holds the id again");
    assert_eq!(ran(&notes), ["after the failure"]);
    assert!(notes.get("a").unwrap().is_none(), "the failed record went with the add");
}

// contract: JOB-03
#[test]
fn add_in_the_place_of_a_failed_job_keeps_its_group_unless_it_names_one() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").attempts(1).open().unwrap();
    notes.id("a").group("acme").add(&note("first")).unwrap();
    notes.concurrency(1).run_due(|_: Note, _run| -> Result<(), String> { Err("down".into()) }).unwrap();
    assert!(notes.id("a").add(&note("again")).unwrap());
    assert_eq!(notes.get("a").unwrap().unwrap().group.as_deref(), Some("acme"));
    assert_eq!(notes.counts().unwrap().failed, 0);
}

// contract: JOB-03
#[test]
fn add_of_an_id_whose_job_runs_adds_nothing() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    notes.id("a").add(&note("running")).unwrap();
    let added = Mutex::new(None);
    notes
        .run_due(|_: Note, _run| -> Result<()> {
            *added.lock().unwrap() = Some(notes.id("a").add(&note("while it runs"))?);
            Ok(())
        })
        .unwrap();
    assert_eq!(*added.lock().unwrap(), Some(false));
    assert!(ran(&notes).is_empty());
}

#[test]
fn dedupe_keeps_a_done_id_taken_until_its_span_passes() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").dedupe(HOUR).open().unwrap();
    pushes.id("m1:u1").add(&note("push")).unwrap();
    assert_eq!(ran(&pushes), ["push"]);
    assert!(!pushes.id("m1:u1").add(&note("push")).unwrap(), "an id runs once in the hour");
    let done = pushes.get("m1:u1").unwrap().unwrap();
    assert_eq!((done.state, done.value), (State::Done, note("push")));
    assert!(done.last_run.is_some());

    f.clock.advance(HOUR);
    assert_eq!(super::maintain(&f.store).unwrap().done, 1);
    assert!(pushes.id("m1:u1").add(&note("push")).unwrap());
}

// contract: JOB-17
#[test]
fn upsert_adds_a_job_or_brings_the_one_there_to_the_call_its_time_kept() {
    let f = fixture();
    let polls = f.store.queue::<Note>("polls").open().unwrap();
    assert!(polls.id("group:1").every(3 * MINUTE).upsert(&note("241-327")).unwrap(), "added");
    let added = polls.get("group:1").unwrap().unwrap();
    assert!(!polls.id("group:1").every(3 * MINUTE).upsert(&note("241-327")).unwrap(), "it already was so");
    assert_eq!(polls.get("group:1").unwrap().unwrap(), added, "and nothing was written");

    assert!(polls.id("group:1").every(3 * MINUTE).upsert(&note("renamed")).unwrap());
    let renamed = polls.get("group:1").unwrap().unwrap();
    assert_eq!((renamed.value.text.as_str(), renamed.at), ("renamed", added.at), "a new value, its time kept");
    assert!(polls.id("group:1").delay(HOUR).every(3 * MINUTE).upsert(&note("renamed")).is_ok_and(|changed| !changed));
    assert_eq!(polls.get("group:1").unwrap().unwrap().at, added.at, "a delay counts only when it adds");

    assert!(polls.id("group:1").every(HOUR).upsert(&note("renamed")).unwrap());
    let slower = polls.get("group:1").unwrap().unwrap();
    assert!(slower.repeat.as_deref().is_some_and(|repeat| repeat.starts_with("@every 1h")), "{slower:?}");
    assert!(slower.at > f.now() && slower.at != added.at, "the new repeat's next time");

    f.clock.advance(HOUR);
    assert!(polls.id("group:1").every(2 * HOUR).upsert(&note("renamed")).unwrap());
    let due = polls.get("group:1").unwrap().unwrap();
    assert_eq!((due.state, due.at), (State::Waiting, slower.at), "a due job stays due");
    let error = polls.upsert("", &note("x")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
}

// contract: JOB-17
#[test]
fn upsert_keeps_a_retrys_attempts_and_a_done_job_its_dedupe_holds() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").attempts(3).dedupe(HOUR).open().unwrap();
    pushes.id("retry").add(&note("first")).unwrap();
    fail_due(&pushes);
    let waiting = pushes.get("retry").unwrap().unwrap();
    assert!(pushes.upsert("retry", &note("second")).unwrap());
    let changed = pushes.get("retry").unwrap().unwrap();
    assert_eq!((changed.attempt, changed.at, changed.error), (1, waiting.at, waiting.error), "attempts and time kept");

    pushes.id("done").add(&note("done")).unwrap();
    assert_eq!(ran(&pushes), ["done"]);
    assert!(!pushes.upsert("done", &note("again")).unwrap(), "a done job its dedupe holds is left alone");
    assert_eq!(pushes.get("done").unwrap().unwrap().value, note("done"));
}

// contract: JOB-17
#[test]
fn upsert_of_a_running_job_gives_its_next_run_what_differs_and_one_that_ends_with_its_run_nothing() {
    let f = fixture();
    let polls = f.store.queue::<Note>("polls").open().unwrap();
    polls.id("repeat").every(MINUTE).add(&note("old")).unwrap();
    polls.id("once").add(&note("old")).unwrap();
    f.clock.advance(MINUTE);
    let answers = Mutex::new(Vec::new());
    polls
        .concurrency(1)
        .run_due(|value: Note, run| -> Result<()> {
            let id = run.id().unwrap_or_default().to_owned();
            if value.text == "old" {
                answers.lock().unwrap().push((id.clone(), polls.upsert(&id, &note("new"))?));
            }
            Ok(())
        })
        .unwrap();
    let answers = answers.into_inner().unwrap();
    assert!(
        answers.contains(&("repeat".to_owned(), true)) && answers.contains(&("once".to_owned(), false)),
        "{answers:?}"
    );
    let next = polls.get("repeat").unwrap().unwrap();
    assert_eq!((next.state, next.value), (State::Scheduled, note("new")), "its next run takes the new value");
    assert!(polls.get("once").unwrap().is_none(), "a job that ended with its run took nothing");
}

// contract: JOB-19
#[test]
fn a_ttl_ends_a_repeat_unless_an_upsert_renews_it() {
    let f = fixture();
    let polls = f.store.queue::<Note>("polls").open().unwrap();
    assert!(polls.id("group:1").every(MINUTE).ttl(HOUR).upsert(&note("241-327")).unwrap());
    let term = f.now() + HOUR;
    assert_eq!(polls.get("group:1").unwrap().unwrap().expires_at, Some(term));
    f.clock.advance(MINUTE);
    assert!(
        !polls.id("group:1").every(MINUTE).ttl(HOUR).upsert(&note("241-327")).unwrap(),
        "a renewal changes nothing"
    );
    assert_eq!(
        polls.get("group:1").unwrap().unwrap().expires_at,
        Some(term),
        "nor is written within a thirtieth of it"
    );
    f.clock.advance(30 * MINUTE);
    assert_eq!(ran(&polls), ["241-327"]);
    assert!(!polls.id("group:1").every(MINUTE).ttl(HOUR).upsert(&note("241-327")).unwrap());
    let renewed = f.now() + HOUR;
    assert_eq!(polls.get("group:1").unwrap().unwrap().expires_at, Some(renewed), "a sighting renews the term");

    f.clock.advance(HOUR);
    assert!(polls.get("group:1").unwrap().is_none(), "past its term it is gone");
    assert!(ran(&polls).is_empty(), "and never starts");
    assert_eq!(super::maintain(&f.store).unwrap().expired, 0, "the claim that met it took it");
    assert!(polls.id("group:1").add(&note("again")).unwrap(), "its id is free");
}

// contract: JOB-19
#[test]
fn a_repeat_ends_at_its_term_and_its_watcher_sees_it_cancelled() {
    let f = fixture();
    let reports = f.store.queue::<Note>("reports").dedupe(DAY).open().unwrap();
    reports.id("report:term").every(HOUR).add(&note("report")).unwrap();
    let due = reports.get("report:term").unwrap().unwrap().at;
    reports.id("report:term").expires_at(due + MINUTE).upsert(&note("report")).unwrap();
    assert_eq!(reports.get("report:term").unwrap().unwrap().at, due, "a term of its own, its time kept");
    let mut watched = reports.watch("report:term");
    assert_eq!(watched.next().unwrap().unwrap().state, State::Scheduled);
    f.clock.advance(due.duration_since(f.now()).unwrap());
    assert_eq!(ran(&reports), ["report"], "a run before the term runs");
    let last = watched.last().unwrap().unwrap();
    assert_eq!(last.state, State::Cancelled, "its next time was past the term");
    assert!(reports.get("report:term").unwrap().is_none());
    assert!(reports.id("report:term").add(&note("again")).unwrap(), "its id is free, dedupe or not");
}

// contract: JOB-19
#[test]
fn a_run_under_way_at_its_term_finishes_and_a_retry_past_it_does_not_run() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").attempts(3).backoff(HOUR, HOUR).timeout(2 * HOUR).open().unwrap();
    pushes.id("push").ttl(30 * MINUTE).add(&note("push")).unwrap();
    pushes.id("late").ttl(30 * MINUTE).add(&note("late")).unwrap();
    let finished = Mutex::new(Vec::new());
    pushes
        .concurrency(1)
        .run_due(|value: Note, _run| -> Result<(), String> {
            if value.text == "late" {
                f.clock.advance(HOUR); // the term comes while it runs
                finished.lock().unwrap().push(value.text);
                return Ok(());
            }
            Err("push service 503".into())
        })
        .unwrap();
    assert_eq!(*finished.lock().unwrap(), ["late"], "the run under way finished");
    assert!(pushes.get("push").unwrap().is_none(), "its retry an hour on was past the term");
    assert_eq!(pushes.counts().unwrap(), super::Counts::default());
}

// contract: JOB-19
#[test]
fn a_term_past_is_gone_at_once_one_scheduled_past_it_goes_with_maintenance_and_a_wrong_one_is_refused() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").open().unwrap();
    assert!(pushes.id("past").expires_at(f.now() - SECOND).add(&note("x")).unwrap());
    assert!(pushes.get("past").unwrap().is_none());
    assert!(pushes.id("past").add(&note("x")).unwrap(), "its id is free");
    pushes.delay(2 * HOUR).ttl(HOUR).add(&note("never")).unwrap();
    f.clock.advance(HOUR);
    assert_eq!(super::maintain(&f.store).unwrap().expired, 1);
    assert_eq!(pushes.counts().unwrap().scheduled, 0);

    let zero = pushes.id("z").ttl(Duration::ZERO).add(&note("x")).unwrap_err();
    assert_eq!(zero.kind(), ErrorKind::Invalid, "{zero}");
    let both = pushes.id("z").ttl(HOUR).expires_at(f.now()).add(&note("x")).unwrap_err();
    assert!(both.to_string().contains("not both"), "{both}");
}

/// Fails every job due, as a handler that always fails does.
fn fail_due(queue: &Queue<Note>) {
    queue.concurrency(1).run_due(|_: Note, _run| -> Result<(), String> { Err("push service 503".into()) }).unwrap();
}

// contract: JOB-18
#[test]
fn a_jobs_own_dedupe_holds_its_id_beside_its_queues() {
    let f = fixture();
    let crawl = f.store.queue::<Note>("crawl").open().unwrap();
    assert!(crawl.id("now:1").dedupe(10 * MINUTE).add(&note("crawl")).unwrap());
    assert!(crawl.id("plain").add(&note("crawl")).unwrap());
    assert_eq!(ran(&crawl), ["crawl", "crawl"]);
    assert!(!crawl.id("now:1").add(&note("again")).unwrap(), "its own dedupe holds the id");
    assert_eq!(crawl.get("now:1").unwrap().unwrap().state, State::Done);
    assert!(crawl.id("plain").add(&note("again")).unwrap(), "a queue without dedupe holds nothing");
    f.clock.advance(10 * MINUTE);
    assert!(crawl.id("now:1").add(&note("again")).unwrap());

    let zero = crawl.id("zero").dedupe(Duration::ZERO).add(&note("x")).unwrap_err();
    assert_eq!(zero.kind(), ErrorKind::Invalid, "{zero}");
    let nowhere = crawl.id("z").dedupe(super::Dedupe::calendar(crate::Calendar::Day, "Mars/Olympus"));
    assert!(nowhere.add(&note("x")).unwrap_err().to_string().contains("no such zone"));
}

// contract: JOB-18
#[test]
fn a_calendar_dedupe_holds_a_done_id_until_the_local_day_it_was_done_in_ends() {
    let f = fixture(); // 12:00 UTC, 15:00 in Moscow
    let day = super::Dedupe::calendar(crate::Calendar::Day, "Europe/Moscow");
    let crawl = f.store.queue::<Note>("schedule-fetch").dedupe(day).open().unwrap();
    crawl.id("crawl:241-327").add(&note("crawl")).unwrap();
    assert_eq!(ran(&crawl), ["crawl"]);
    f.clock.advance(8 * HOUR + 59 * MINUTE); // 23:59 in Moscow
    assert!(!crawl.id("crawl:241-327").add(&note("again")).unwrap(), "once a Moscow day");
    f.clock.advance(MINUTE);
    assert!(crawl.id("crawl:241-327").add(&note("the next day")).unwrap());
    assert!(crawl.id("now:1").dedupe(MINUTE).add(&note("a job's own")).unwrap());
}

// contract: JOB-20
#[test]
fn a_failed_job_with_a_keep_of_zero_is_gone_at_once_and_its_watcher_sees_it_failed() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").attempts(1).open().unwrap();
    pushes.id("p1").keep(Duration::ZERO).add(&note("the text of a private message")).unwrap();
    pushes.keep(Duration::ZERO).add(&note("one without an id")).unwrap();
    let mut watched = pushes.watch("p1");
    assert_eq!(watched.next().unwrap().unwrap().state, State::Waiting);
    fail_due(&pushes);
    let last = watched.next().unwrap().unwrap();
    assert_eq!((last.state, last.error.as_deref()), (State::Failed, Some("push service 503")));
    assert!(watched.next().is_none());
    assert!(pushes.get("p1").unwrap().is_none(), "nothing keeps its value");
    assert_eq!(pushes.counts().unwrap().failed, 0);
}

// contract: JOB-20
#[test]
fn a_jobs_own_keep_wins_over_its_queues_and_a_queues_of_zero_keeps_nothing() {
    let f = fixture();
    let reports = f.store.queue::<Note>("reports").attempts(1).keep(HOUR).open().unwrap();
    reports.id("long").keep(30 * DAY).add(&note("kept a month")).unwrap();
    reports.id("short").keep(MINUTE).add(&note("kept a minute")).unwrap();
    reports.id("queue's").add(&note("kept an hour")).unwrap();
    fail_due(&reports);
    assert_eq!(reports.counts().unwrap().failed, 3);
    f.clock.advance(2 * MINUTE);
    assert!(reports.get("short").unwrap().is_none(), "gone once its keep passed, before maintenance ran");
    assert_eq!(reports.counts().unwrap().failed, 2);
    f.clock.advance(2 * HOUR);
    assert_eq!(super::maintain(&f.store).unwrap().failed, 2, "the queue's keep, and the minute's");
    assert_eq!(reports.get("long").unwrap().unwrap().state, State::Failed);
    f.clock.advance(30 * DAY);
    assert_eq!(super::maintain(&f.store).unwrap().failed, 1);

    let pushes = f.store.queue::<Note>("pushes").attempts(1).keep(Duration::ZERO).open().unwrap();
    pushes.id("p1").add(&note("x")).unwrap();
    fail_due(&pushes);
    assert!(pushes.get("p1").unwrap().is_none(), "a queue's keep of zero keeps nothing either");
}

// contract: STORE-11
#[test]
fn a_jobs_file_the_release_before_wrote_opens_and_its_jobs_run_by_their_queues_options() {
    let mut f = fixture();
    let notes = f.store.queue::<Note>("notes").attempts(1).dedupe(HOUR).keep(HOUR).open().unwrap();
    notes.id("failing").add(&note("fails")).unwrap();
    notes.id("done").add(&note("done")).unwrap();
    notes.id("later").delay(DAY).add(&note("later")).unwrap();
    notes
        .concurrency(1)
        .run_due(|note: Note, _run| -> Result<(), String> {
            if note.text == "fails" { Err("down".into()) } else { Ok(()) }
        })
        .unwrap();
    drop(notes);
    let path = f.dir.path().join("jobs.db");
    f.store.close().unwrap();
    // as 0.10 left it: the tables of 0001 alone
    let back = "drop index _tinystore_jobs_expiring; drop index _tinystore_jobs_failed_by_until;
        alter table _tinystore_jobs drop column dedupe; alter table _tinystore_jobs drop column keep;
        alter table _tinystore_jobs drop column expires; alter table _tinystore_jobs_failed drop column until;
        alter table _tinystore_jobs drop column latest; alter table _tinystore_jobs drop column taken;
        alter table _tinystore_jobs drop column quiet;
        delete from _tinystore_migrations where history = 'jobs' and version >= 2";
    let file = crate::sqlite::File::open(&path, crate::sqlite::Config::default()).unwrap();
    let failed = |error| crate::sqlite::sql_error("a test", error);
    file.transaction(|tx| tx.execute_batch(back).map_err(failed)).unwrap();
    // and 0.10, which knows its first step alone, refuses a file this one migrated
    file.migrate("jobs", super::engine::MIGRATIONS).unwrap();
    let refused = file.migrate("jobs", &super::engine::MIGRATIONS[..1]).unwrap_err();
    assert!(refused.to_string().contains("a newer program migrated it"), "{refused}");
    file.transaction(|tx| tx.execute_batch(back).map_err(failed)).unwrap();
    file.close().unwrap();

    f.reopen();
    let notes = f.store.queue::<Note>("notes").attempts(1).dedupe(HOUR).keep(HOUR).open().unwrap();
    assert_eq!(notes.get("failing").unwrap().unwrap().state, State::Failed);
    assert_eq!(notes.get("done").unwrap().unwrap().state, State::Done);
    assert!(!notes.id("done").add(&note("again")).unwrap(), "its queue's dedupe holds it");
    assert_eq!(notes.get("later").unwrap().unwrap().state, State::Scheduled);
    f.clock.advance(DAY);
    assert_eq!(ran(&notes), ["later"]);
    assert_eq!(super::maintain(&f.store).unwrap().failed, 1, "a failed job of 0.10 goes by its queue's keep");
}

// contract: JOB-03
#[test]
fn set_makes_the_ids_job_this_value_at_this_time_whatever_it_was() {
    let f = fixture();
    let checks = f.store.queue::<Note>("checks").attempts(1).open().unwrap();
    checks.id("backup").delay(25 * HOUR).set(&note("ping 1")).unwrap();
    f.clock.advance(24 * HOUR);
    checks.id("backup").delay(25 * HOUR).set(&note("ping 2")).unwrap();
    f.clock.advance(24 * HOUR);
    assert!(ran(&checks).is_empty(), "each ping moved the alarm on");
    f.clock.advance(HOUR);
    assert_eq!(ran(&checks), ["ping 2"]);

    checks.set("failing", &note("one")).unwrap();
    checks.run_due(|_: Note, _run| -> Result<(), &str> { Err("down") }).unwrap();
    assert_eq!(checks.get("failing").unwrap().unwrap().state, State::Failed);
    checks.set("failing", &note("two")).unwrap();
    let again = checks.get("failing").unwrap().unwrap();
    assert_eq!(
        (again.state, again.attempt, again.value),
        (State::Waiting, 0, note("two")),
        "a failed job starts again"
    );
}

#[test]
fn set_of_a_running_job_runs_it_once_more_with_its_value() {
    let f = fixture();
    let indexing = f.store.queue::<Note>("indexing").open().unwrap();
    indexing.set("note:7", &note("draft")).unwrap();
    let seen = Mutex::new(Vec::new());
    let index = |note: Note, _run: &super::Run| -> Result<()> {
        if note.text == "draft" {
            indexing.set("note:7", &note_of("edited"))?;
            indexing.set("note:7", &note_of("edited twice"))?;
        }
        seen.lock().unwrap().push(note.text);
        Ok(())
    };
    indexing.run_due(index).unwrap();
    assert_eq!(*seen.lock().unwrap(), ["draft", "edited twice"], "two sets while it ran ask one run more");
}

fn note_of(text: &str) -> Note {
    note(text)
}

// contract: JOB-04
#[test]
fn update_changes_only_a_job_that_has_not_started() {
    let f = fixture();
    let later = f.store.queue::<Note>("send-later").open().unwrap();
    assert!(!later.update("missing", &note("x")).unwrap(), "there was no job");
    later.id("m1").delay(HOUR).add(&note("hello")).unwrap();
    assert!(later.id("m1").delay(2 * HOUR).update(&note("hello, edited")).unwrap());
    let job = later.get("m1").unwrap().unwrap();
    assert_eq!((job.value.text.as_str(), job.at), ("hello, edited", f.now() + 2 * HOUR));
    assert!(later.update("m1", &note("text alone")).unwrap(), "an update without a time keeps it");
    assert_eq!(later.get("m1").unwrap().unwrap().at, f.now() + 2 * HOUR);

    f.clock.advance(2 * HOUR);
    let too_late = Mutex::new(None);
    later
        .run_due(|_: Note, _run| -> Result<()> {
            *too_late.lock().unwrap() = Some(later.update("m1", &note("while it is sent"))?);
            Ok(())
        })
        .unwrap();
    assert_eq!(*too_late.lock().unwrap(), Some(false));
    assert!(!later.update("m1", &note("after it was sent")).unwrap());
}

// contract: JOB-04, JOB-06
#[test]
fn an_update_of_a_job_waiting_for_its_retry_drops_the_steps_kept_for_its_old_value() {
    let f = fixture();
    let orders = f.store.queue::<Note>("orders").attempts(3).open().unwrap();
    orders.id("order:7").add(&note("10")).unwrap();
    let charged = Mutex::new(Vec::new());
    // it borrows `charged` alone, so that it is `Copy` and runs both times by value
    let charge = |order: Note, run: &super::Run| -> Result<()> {
        let kept: String = run.step("charge", || -> Result<String> { Ok(order.text.clone()) })?;
        charged.lock().unwrap().push(kept);
        match order.text.as_str() {
            "10" => Err(crate::Error::invalid("the receipt did not save")),
            _ => Ok(()),
        }
    };
    orders.run_due(charge).unwrap();
    assert!(orders.id("order:7").update(&note("20")).unwrap(), "a job waiting for its retry is not running");
    f.clock.advance(HOUR);
    orders.run_due(charge).unwrap();
    assert_eq!(*charged.lock().unwrap(), ["10", "20"], "the step ran again for the new value");
    assert!(orders.get("order:7").unwrap().is_none(), "and the job is done");
}

// contract: JOB-04
#[test]
fn cancel_says_whether_there_was_a_job_and_tells_a_running_handler_to_stop() {
    let f = fixture();
    let later = f.store.queue::<Note>("send-later").attempts(1).open().unwrap();
    assert!(!later.cancel("missing").unwrap());
    later.id("m1").delay(HOUR).add(&note("scheduled")).unwrap();
    assert!(later.cancel("m1").unwrap());
    assert!(later.get("m1").unwrap().is_none());

    later.id("m2").add(&note("running")).unwrap();
    let stopped = Mutex::new(false);
    later
        .run_due(|_: Note, run| -> Result<()> {
            assert!(later.cancel("m2")?);
            *stopped.lock().unwrap() = run.stopped();
            Err(crate::Error::invalid("what a cancelled handler returns settles nothing"))
        })
        .unwrap();
    assert!(*stopped.lock().unwrap());
    assert!(later.get("m2").unwrap().is_none(), "the cancel took it, not a retry");

    later.id("m3").add(&note("failing")).unwrap();
    later.run_due(|_: Note, _run| -> Result<(), &str> { Err("down") }).unwrap();
    assert!(later.cancel("m3").unwrap(), "a failed job is cancelled too");
    assert!(later.get("m3").unwrap().is_none());
}

/// The texts of the queue's jobs, as a walk of every job meets them.
fn texts_of(queue: &Queue<Note>) -> Vec<String> {
    queue.all(Filter::default()).map(|job| job.unwrap().value.text).collect()
}

/// The job a walk of `filter` meets whose text is `text`.
fn listed(queue: &Queue<Note>, filter: Filter, text: &str) -> super::Job<Note> {
    queue.all(filter).map(Result::unwrap).find(|job| job.value.text == text).unwrap()
}

// contract: JOB-15
#[test]
fn cancel_takes_a_listed_job_without_an_id_by_its_row_and_no_other() {
    let f = fixture();
    let emails = f.store.queue::<Note>("emails").attempts(1).open().unwrap();
    for text in ["typo", "same time", "sent"] {
        emails.delay(HOUR).add(&note(text)).unwrap();
    }
    let typo = listed(&emails, Filter::default().state(State::Scheduled), "typo");
    assert_eq!(typo.id, None);
    assert!(emails.cancel(&typo).unwrap());
    assert!(!emails.cancel(&typo).unwrap(), "cancelled already");
    assert_eq!(texts_of(&emails), ["same time", "sent"], "a job due at the same time stays");

    emails.add(&note("bounced")).unwrap();
    emails.run_due(|_: Note, _run| -> Result<(), &str> { Err("no such mailbox") }).unwrap();
    let bounced = listed(&emails, Filter::default().state(State::Failed), "bounced");
    assert!(emails.cancel(&bounced).unwrap(), "a failed job by the row it failed into");
    assert_eq!(texts_of(&emails), ["same time", "sent"]);

    emails.id("welcome").delay(HOUR).add(&note("welcome")).unwrap();
    let welcome = listed(&emails, Filter::default(), "welcome");
    assert!(emails.cancel(&welcome).unwrap(), "a job with an id by its id");
    assert!(emails.get("welcome").unwrap().is_none());
}

// contract: JOB-15
#[test]
fn a_listed_job_that_moved_since_is_in_no_row_and_its_cancel_takes_nothing() {
    let f = fixture();
    let emails = f.store.queue::<Note>("emails").attempts(3).open().unwrap();
    emails.add(&note("flaky")).unwrap();
    let flaky = listed(&emails, Filter::default(), "flaky");
    emails.run_due(|_: Note, _run| -> Result<(), &str> { Err("timeout") }).unwrap();
    assert!(!emails.cancel(&flaky).unwrap(), "retried into another row");
    assert_eq!(texts_of(&emails), ["flaky"], "and still there");

    let retried = listed(&emails, Filter::default(), "flaky");
    assert_eq!(retried.attempt, 1);
    assert!(emails.cancel(&retried).unwrap(), "the row its retry lies in");
    assert!(texts_of(&emails).is_empty());
}

// contract: JOB-15
#[test]
fn a_cancel_by_row_stops_a_running_job_and_takes_a_parked_one() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").concurrency(Concurrency::total(2).group(1)).open().unwrap();
    pushes.group("ada").add(&note("first")).unwrap();
    pushes.group("ada").add(&note("second")).unwrap();
    let (stopped, parked) = (Mutex::new(false), Mutex::new(None));
    pushes
        .run_due(|note: Note, run| -> Result<()> {
            if note.text == "first" {
                let second = listed(&pushes, Filter::default().state(State::Waiting), "second");
                *parked.lock().unwrap() = Some(pushes.cancel(&second)?);
                let first = listed(&pushes, Filter::default().state(State::Running), "first");
                assert!(pushes.cancel(&first)?);
                *stopped.lock().unwrap() = run.stopped();
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(*parked.lock().unwrap(), Some(true), "parked while its group runs, found at its time");
    assert!(*stopped.lock().unwrap(), "the running one's handler is told to stop");
    assert!(texts_of(&pushes).is_empty());
}

// contract: JOB-15
#[test]
fn a_transaction_cancels_a_listed_job_once_it_commits() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").open().unwrap();
    pushes.delay(HOUR).add(&note("kept")).unwrap();
    let kept = listed(&pushes, Filter::default(), "kept");
    let rolled_back = f.store.tx(|tx| -> Result<()> {
        assert!(tx.with(&pushes).cancel(&kept)?);
        Err(crate::Error::invalid("rolled back"))
    });
    assert!(rolled_back.is_err());
    assert_eq!(texts_of(&pushes), ["kept"], "a cancel that did not commit took nothing");
    assert!(f.store.tx(|tx| tx.with(&pushes).cancel(&kept)).unwrap());
    assert!(texts_of(&pushes).is_empty());
}

// contract: JOB-16
#[test]
fn counts_say_how_many_jobs_are_in_each_state() {
    let f = fixture();
    let emails = f.store.queue::<Note>("emails").attempts(1).open().unwrap();
    assert_eq!(emails.counts().unwrap(), super::Counts::default());
    emails.add(&note("bounced")).unwrap();
    emails.run_due(|_: Note, _run| -> Result<(), &str> { Err("no such mailbox") }).unwrap();
    for text in ["running", "waiting"] {
        emails.add(&note(text)).unwrap();
    }
    emails.id("later").delay(HOUR).add(&note("later")).unwrap();
    emails.delay(DAY).add(&note("tomorrow")).unwrap();
    let counted = Mutex::new(None);
    emails
        .concurrency(1)
        .run_due(|note: Note, _run| -> Result<()> {
            if note.text == "running" {
                *counted.lock().unwrap() = Some(emails.counts()?);
            }
            Ok(())
        })
        .unwrap();
    let counts = counted.into_inner().unwrap().unwrap();
    assert_eq!(counts, super::Counts { scheduled: 2, waiting: 1, running: 1, failed: 1 });
    let counts = emails.counts().unwrap();
    assert_eq!(counts, super::Counts { scheduled: 2, waiting: 0, running: 0, failed: 1 }, "a done job left");
    f.clock.advance(HOUR);
    assert_eq!(emails.counts().unwrap(), super::Counts { scheduled: 1, waiting: 1, running: 0, failed: 1 });
}

#[test]
fn get_says_where_a_job_is_and_how_many_run_before_it() {
    let f = fixture();
    let videos = f.store.queue::<Note>("videos").attempts(1).open().unwrap();
    for at in 0..3 {
        videos.add(&note(&format!("before {at}"))).unwrap();
    }
    videos.id("mine").add(&note("mine")).unwrap();
    videos.id("later").delay(HOUR).add(&note("later")).unwrap();
    let mine = videos.get("mine").unwrap().unwrap();
    assert_eq!((mine.state, mine.ahead), (State::Waiting, 3));
    assert_eq!(videos.get("later").unwrap().unwrap().state, State::Scheduled);

    let running = Mutex::new(None);
    videos
        .concurrency(1)
        .run_due(|note: Note, run| -> Result<()> {
            if note.text == "mine" {
                run.set_progress(&0.4)?;
                *running.lock().unwrap() = videos.get("mine")?;
                return Err(crate::Error::invalid("transcoding failed"));
            }
            Ok(())
        })
        .unwrap();
    let running = running.into_inner().unwrap().unwrap();
    assert_eq!((running.state, running.attempt, running.progress), (State::Running, 1, Some(serde_json::json!(0.4))));
    let failed = videos.get("mine").unwrap().unwrap();
    assert_eq!(failed.state, State::Failed);
    assert!(failed.error.unwrap().contains("transcoding failed"));
    assert!(failed.last_run.is_some());
}

#[test]
fn list_reads_the_ids_under_a_prefix_a_page_at_a_time() {
    let f = fixture();
    let later = f.store.queue::<Note>("send-later").open().unwrap();
    for id in ["chat:42:1", "chat:42:2", "chat:42:3", "chat:420:1", "chat:7:1"] {
        later.id(id).delay(HOUR).add(&note(id)).unwrap();
    }
    let first = later.list(&Filter::prefix("chat:42:").limit(2)).unwrap();
    let ids: Vec<_> = first.jobs.iter().map(|job| job.id.clone().unwrap()).collect();
    assert_eq!(ids, ["chat:42:1", "chat:42:2"]);
    let rest = later.list(&Filter::prefix("chat:42:").limit(2).after(first.next.unwrap())).unwrap();
    assert_eq!(rest.jobs.len(), 1);
    assert_eq!(rest.next, None);
    let all: Vec<_> = later.all(Filter::prefix("chat:4")).map(|job| job.unwrap().id.unwrap()).collect();
    assert_eq!(all.len(), 4, "a prefix is text: chat:4 meets chat 420 too");
}

// contract: JOB-13
#[test]
fn a_page_with_no_prefix_lists_the_jobs_without_ids_after_those_with() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").attempts(1).open().unwrap();
    pushes.add(&note("bounced")).unwrap();
    pushes.id("gone").add(&note("gone")).unwrap();
    pushes.run_due(|_: Note, _run| -> Result<(), &str> { Err("no device") }).unwrap();
    f.clock.advance(SECOND);
    pushes.add(&note("first")).unwrap();
    pushes.id("b").add(&note("b")).unwrap();
    pushes.add(&note("second")).unwrap();
    pushes.id("a").delay(HOUR).add(&note("a")).unwrap();
    pushes.delay(HOUR).add(&note("later")).unwrap();

    let texts = |filter: Filter| -> Vec<String> { pushes.all(filter).map(|job| job.unwrap().value.text).collect() };
    let every = ["a", "b", "gone", "first", "second", "later", "bounced"];
    assert_eq!(texts(Filter::default()), every, "ids first, then the rest as they run, then the failed");
    assert_eq!(texts(Filter::default().limit(1)), every, "a page at a time across the walks");
    assert_eq!(texts(Filter::default().state(State::Waiting)), ["b", "first", "second"]);
    assert_eq!(texts(Filter::default().state(State::Scheduled)), ["a", "later"]);
    assert_eq!(texts(Filter::default().state(State::Failed)), ["gone", "bounced"], "the last failed first");
    assert_eq!(texts(Filter::prefix("g")), ["gone"], "a job without an id starts with no prefix");

    let first = pushes.list(&Filter::default().limit(4)).unwrap();
    assert_eq!(first.jobs.last().unwrap().id, None);
    let rest = pushes.list(&Filter::default().after(first.next.clone().unwrap())).unwrap();
    let rest: Vec<_> = rest.jobs.iter().map(|job| job.value.text.as_str()).collect();
    assert_eq!(rest, ["second", "later", "bounced"]);
    let error = pushes.list(&Filter::prefix("chat:").after(first.next.unwrap())).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "a prefix walks ids alone: {error}");
    let error = pushes.list(&Filter::default().after("chat:42")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "an id is no cursor: {error}");
}

// contract: JOB-14, JOB-13
#[test]
fn a_page_of_one_state_reads_on_past_the_others_until_it_holds_its_limit() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    let waiting = f.store.tx(|tx| -> Result<()> {
        for at in 0..150 {
            tx.with(&notes).id(format!("now:{at:03}")).add(&note("now"))?;
            tx.with(&notes).add(&note("now"))?;
        }
        Ok(())
    });
    waiting.unwrap();
    notes.id("~later").delay(HOUR).add(&note("later 0")).unwrap();
    for at in 1..4 {
        notes.delay(HOUR).add(&note(&format!("later {at}"))).unwrap();
    }
    let scheduled = Filter::default().state(State::Scheduled).limit(3);
    let texts =
        |page: &super::Page<Note>| -> Vec<String> { page.jobs.iter().map(|job| job.value.text.clone()).collect() };

    let first = notes.list(&scheduled).unwrap();
    assert_eq!(texts(&first), ["later 0", "later 1", "later 2"], "past 300 waiting jobs, a page holds its limit");
    let rest = notes.list(&scheduled.clone().after(first.next.expect("a full page has a next"))).unwrap();
    assert_eq!(texts(&rest), ["later 3"]);
    assert_eq!(rest.next, None, "the last page has no next");
    let walked: Vec<String> = notes.all(scheduled.limit(1)).map(|job| job.unwrap().value.text).collect();
    assert_eq!(walked, ["later 0", "later 1", "later 2", "later 3"], "each met once");
}

#[test]
fn the_failed_jobs_list_the_last_failed_first() {
    let f = fixture();
    let emails = f.store.queue::<Note>("emails").attempts(1).open().unwrap();
    for text in ["a", "b", "c"] {
        emails.add(&note(text)).unwrap();
        emails.run_due(|_: Note, _run| -> Result<(), &str> { Err("bounced") }).unwrap();
        f.clock.advance(SECOND);
    }
    let failed = emails.list(&Filter::default().state(State::Failed)).unwrap();
    let texts: Vec<_> = failed.jobs.iter().map(|job| job.value.text.as_str()).collect();
    assert_eq!(texts, ["c", "b", "a"]);
}

// contract: JOB-12
#[test]
fn a_page_that_meets_a_value_that_no_longer_reads_names_its_job() {
    #[derive(Debug, Serialize, Deserialize)]
    struct Renamed {
        body: String,
    }
    let f = fixture();
    f.store.queue::<Note>("notes").attempts(1).open().unwrap().id("old").add(&note("from before")).unwrap();
    let renamed = f.store.queue::<Renamed>("notes").attempts(1).open().unwrap();
    renamed.run_due(|_: Renamed, _run| -> Result<()> { Ok(()) }).unwrap();
    let error = renamed.list(&Filter::default().state(State::Failed)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt);
    assert!(error.to_string().contains("id \"old\""), "the id a program cancels, or reads as JSON: {error}");
    let raw = f.store.queue::<serde_json::Value>("notes").attempts(1).open().unwrap();
    let failed = raw.list(&Filter::default().state(State::Failed)).unwrap();
    assert_eq!(failed.jobs[0].value, serde_json::json!({ "text": "from before" }), "kept as it was");
}

// contract: ERR-02, ERR-07, JOB-12
#[test]
fn a_walk_meets_a_job_whose_value_no_longer_reads_as_its_error_and_goes_on() {
    #[derive(Debug, Serialize, Deserialize)]
    struct Renamed {
        body: String,
    }
    let f = fixture();
    let raw = f.store.queue::<serde_json::Value>("notes").open().unwrap();
    raw.id("a").delay(HOUR).add(&serde_json::json!({ "body": "first" })).unwrap();
    raw.delay(HOUR).add(&serde_json::json!({ "text": "an older shape" })).unwrap();
    raw.delay(HOUR).add(&serde_json::json!({ "body": "last" })).unwrap();
    let renamed = f.store.queue::<Renamed>("notes").open().unwrap();

    let walked: Vec<Result<String>> = renamed.all(Filter::default()).map(|job| job.map(|job| job.value.body)).collect();
    assert_eq!(walked.len(), 3, "the walk goes on past it: {walked:?}");
    assert_eq!((walked[0].as_deref().ok(), walked[2].as_deref().ok()), (Some("first"), Some("last")));
    let error = walked[1].as_ref().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt);
    assert!(error.to_string().contains("without an id, due at unix ms"), "named by where it lies: {error}");
    let failed = renamed.list(&Filter::default()).unwrap_err();
    assert!(failed.to_string().contains("without an id, due at unix ms"), "a page names it the same: {failed}");
}

#[test]
fn a_value_comes_back_as_the_json_it_went_in() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    let long = note(&"x".repeat(2000));
    notes.id("long").add(&long).unwrap();
    assert_eq!(notes.get("long").unwrap().unwrap().value, long, "a value past 512 bytes lives in a row of its own");
    assert_eq!(ran(&notes), [long.text]);

    let raw = f.store.queue::<serde_json::Value>("notes").open().unwrap();
    raw.id("plain").add(&serde_json::json!({ "text": "from another language" })).unwrap();
    assert_eq!(notes.get("plain").unwrap().unwrap().value, note("from another language"));
}

// contract: JOB-12
#[test]
fn a_value_that_no_longer_reads_fails_its_job_not_its_queue() {
    let f = fixture();
    let raw = f.store.queue::<serde_json::Value>("notes").open().unwrap();
    raw.id("old").add(&serde_json::json!({ "body": "an older shape" })).unwrap();
    raw.add(&serde_json::json!({ "text": "fine" })).unwrap();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    assert_eq!(ran(&notes), ["fine"]);
    let failed = raw.get("old").unwrap().unwrap();
    assert_eq!(failed.state, State::Failed);
    assert!(failed.error.unwrap().contains("no longer reads"));
}

// contract: JOB-10
#[test]
fn a_queue_past_max_waiting_refuses_the_next_job() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").max_waiting(2).open().unwrap();
    notes.add(&note("1")).unwrap();
    notes.add(&note("2")).unwrap();
    let error = notes.add(&note("3")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert!(error.to_string().starts_with("jobs queue notes"), "{error}");
}

#[test]
fn a_call_that_cannot_be_kept_is_invalid_and_says_why() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    let cases = [
        notes.id("").add(&note("x")),
        notes.at(f.now()).delay(HOUR).add(&note("x")),
        notes.delay(SECOND).every(MINUTE).add(&note("x")),
        notes.id("a").cron("0 9 * * *", "").add(&note("x")),
        notes.id("a").cron("0 9 * * *", "Mars/Olympus").add(&note("x")),
    ];
    for case in cases {
        assert_eq!(case.unwrap_err().kind(), ErrorKind::Invalid);
    }
    let error = notes.id("a").cron("0 9 * * *", "").add(&note("x")).unwrap_err();
    assert_eq!(
        error.to_string(),
        "jobs queue notes: id \"a\": cron \"0 9 * * *\": a cron needs a time zone, \"UTC\" among them: invalid"
    );
}

#[test]
fn a_name_keeps_its_kind_and_one_process_its_options() {
    let f = fixture();
    f.store.queue::<Note>("notes").open().unwrap();
    let error = f.store.queue::<Note>("notes").attempts(3).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    let error = f.store.schedule("notes").every(HOUR).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    let error = f.store.queue::<Note>("Notes!").open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
}

#[test]
fn a_failed_job_is_kept_then_removed() {
    let f = fixture();
    let emails = f.store.queue::<Note>("emails").attempts(1).keep(DAY).open().unwrap();
    emails.id("welcome").add(&note("hi")).unwrap();
    emails.run_due(|_: Note, _run| -> Result<(), &str> { Err("bounced") }).unwrap();
    assert_eq!(super::maintain(&f.store).unwrap().failed, 0);
    f.clock.advance(DAY);
    assert_eq!(super::maintain(&f.store).unwrap().failed, 1);
    assert!(emails.get("welcome").unwrap().is_none());
}

#[test]
fn an_id_a_done_job_left_behind_names_nothing_and_maintenance_drops_it() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    notes.id("a").add(&note("a")).unwrap();
    assert_eq!(ran(&notes), ["a"]);
    assert!(notes.get("a").unwrap().is_none());
    assert_eq!(super::maintain(&f.store).unwrap().ids, 1);
    assert_eq!(super::maintain(&f.store).unwrap().ids, 0);
}

// contract: JOB-08
#[test]
fn a_repeating_job_neither_overlaps_nor_piles_up() {
    let f = fixture();
    let probes = f.store.queue::<Note>("probes").open().unwrap();
    probes.id("probe:7").every(HOUR).set(&note("probe")).unwrap();
    let first = probes.get("probe:7").unwrap().unwrap();
    assert!(first.repeat.unwrap().starts_with("@every 1h +"));
    assert!(first.at > f.now() && first.at <= f.now() + HOUR, "its first run within the next interval");

    f.clock.advance(5 * HOUR);
    assert_eq!(ran(&probes), ["probe"], "a night down runs it once, not once a missed time");
    let next = probes.get("probe:7").unwrap().unwrap();
    assert!(next.at > f.now() && next.at <= f.now() + HOUR);
    assert_eq!((next.state, next.attempt), (State::Scheduled, 0));
    assert!(probes.cancel("probe:7").unwrap(), "only an id stops it");
}

#[test]
fn ids_repeating_together_run_each_at_a_phase_of_its_own() {
    let f = fixture();
    let probes = f.store.queue::<Note>("probes").open().unwrap();
    let mut times = Vec::new();
    for site in 0..20 {
        let id = format!("probe:{site}");
        probes.id(&id).every(Duration::from_secs(30)).set(&note(&id)).unwrap();
        times.push(probes.get(&id).unwrap().unwrap().at);
    }
    times.sort();
    times.dedup();
    assert!(times.len() > 10, "{} instants for 20 ids", times.len());
}

// contract: JOB-08
#[test]
fn a_cron_runs_on_its_zones_wall_clock() {
    let f = fixture();
    let digests = f.store.queue::<Note>("digests").open().unwrap();
    digests.id("user:42").cron("0 20 * * *", "Europe/Berlin").set(&note("digest")).unwrap();
    let digest = digests.get("user:42").unwrap().unwrap();
    // 2026-10-09 20:00 in Berlin, two hours ahead of UTC in October
    assert_eq!(crate::unix_millis(digest.at), 1_791_568_800_000);
    assert_eq!(digest.repeat.as_deref(), Some("0 20 * * * Europe/Berlin"));
}

#[test]
fn a_schedule_keeps_the_codes_repeat_each_time_it_opens() {
    let mut f = fixture();
    let cleanup = f.store.schedule("cleanup").cron("10 3 * * *", "UTC").open().unwrap();
    let first = cleanup.get().unwrap().unwrap();
    assert_eq!(first.repeat.as_deref(), Some("10 3 * * * UTC"));

    f.reopen();
    let cleanup = f.store.schedule("cleanup").cron("10 3 * * *", "UTC").open().unwrap();
    assert_eq!(cleanup.get().unwrap().unwrap().at, first.at, "the same repeat keeps its time");

    f.reopen();
    let cleanup = f.store.schedule("cleanup").cron("30 4 * * *", "UTC").open().unwrap();
    let changed = cleanup.get().unwrap().unwrap();
    assert_eq!(changed.repeat.as_deref(), Some("30 4 * * * UTC"), "the code's repeat replaces the kept one");
    assert_ne!(changed.at, first.at);

    f.clock.advance(DAY);
    let runs = Mutex::new(Vec::new());
    let ran = cleanup.run_due(|run| -> Result<()> {
        runs.lock().unwrap().push(run.at());
        Ok(())
    });
    assert_eq!(ran.unwrap(), 1);
    assert_eq!(*runs.lock().unwrap(), [changed.at], "a run knows when it was due");
}

// contract: JOB-21
#[test]
fn run_now_runs_a_schedule_once_and_leaves_its_later_times_as_they_were() {
    let f = fixture();
    let nightly = f.store.schedule("nightly").every(HOUR).open().unwrap();
    let due = nightly.get().unwrap().unwrap().at;
    nightly.run_now().unwrap();
    let job = nightly.get().unwrap().unwrap();
    assert_eq!((job.state, job.at), (State::Waiting, f.now()));
    let runs = Mutex::new(Vec::new());
    let ran = nightly.run_due(|run| -> Result<()> {
        runs.lock().unwrap().push(run.at());
        Ok(())
    });
    assert_eq!(ran.unwrap(), 1);
    assert_eq!(*runs.lock().unwrap(), [f.now()], "the run is for when it was asked");
    let after = nightly.get().unwrap().unwrap();
    assert_eq!((after.state, after.at), (State::Scheduled, due), "its next time is the one it had");
    assert!(after.last_run.is_some());
}

// contract: JOB-21
#[test]
fn run_now_of_a_running_schedule_runs_once_more_right_after_the_run_under_way() {
    let f = fixture();
    let nightly = f.store.schedule("nightly").every(HOUR).open().unwrap();
    let due = nightly.get().unwrap().unwrap().at;
    nightly.run_now().unwrap();
    let (runs, running) = (Mutex::new(0), Mutex::new(false));
    let ran = nightly.run_due(|_run| -> Result<()> {
        assert!(!std::mem::replace(&mut *running.lock().unwrap(), true), "never beside a run");
        let mut runs = runs.lock().unwrap();
        *runs += 1;
        if *runs == 1 {
            nightly.run_now()?;
        }
        *running.lock().unwrap() = false;
        Ok(())
    });
    assert_eq!(ran.unwrap(), 2, "one run more, after the one under way");
    assert_eq!(nightly.get().unwrap().unwrap().at, due);
}

#[test]
fn run_now_of_a_schedule_a_cancel_took_is_not_found() {
    let f = fixture();
    let nightly = f.store.schedule("nightly").every(HOUR).open().unwrap();
    assert!(nightly.cancel().unwrap());
    let error = nightly.run_now().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
    assert!(error.to_string().contains("jobs schedule nightly"), "{error}");
}

// contract: JOB-08
#[test]
fn a_run_missed_while_the_program_was_down_runs_once_though_the_deploy_changed_its_repeat() {
    let mut f = fixture();
    let nightly = f.store.schedule("nightly").every(HOUR).open().unwrap();
    let due = nightly.get().unwrap().unwrap().at;
    drop(nightly);
    f.advance(2 * HOUR);
    f.reopen();
    let nightly = f.store.schedule("nightly").every(2 * HOUR).open().unwrap();
    let missed = nightly.get().unwrap().unwrap();
    assert_eq!((missed.state, missed.at), (State::Waiting, due), "the missed run stays due");
    assert!(missed.repeat.as_deref().is_some_and(|repeat| repeat.starts_with("@every 2h")), "{missed:?}");
    assert_eq!(nightly.run_due(|_run| -> Result<()> { Ok(()) }).unwrap(), 1, "once, not once a missed time");
    assert!(nightly.get().unwrap().unwrap().at > f.now(), "then by the new repeat");
}

// contract: JOB-03
#[test]
fn a_schedule_whose_job_failed_for_good_starts_again_under_its_one_id() {
    let mut f = fixture();
    let nightly = f.store.schedule("nightly").every(HOUR).open().unwrap();
    // as v0.1.0 failed a schedule whose handler killed its process each attempt
    let fail = "insert into _tinystore_jobs_failed (queue, id, key, at, attempt, failed, error, value, spill, grp)
            select queue, id, key, at, attempt, 0, 'killed', value, spill, grp from _tinystore_jobs where key = 'nightly';
        delete from _tinystore_jobs where key = 'nightly'";
    let jobs = super::engine::Jobs::of(&f.store).unwrap();
    let failed = |error| crate::sqlite::sql_error("a test", error);
    jobs.file().transaction(|tx| tx.execute_batch(fail).map_err(failed)).unwrap();
    assert_eq!(nightly.get().unwrap().unwrap().state, State::Failed);
    drop((nightly, jobs));

    f.reopen();
    let nightly = f.store.schedule("nightly").every(HOUR).open().unwrap();
    assert_eq!(nightly.get().unwrap().unwrap().state, State::Scheduled, "the schedule starts again");
    let jobs = super::engine::Jobs::of(&f.store).unwrap();
    let count = "select count(*) from _tinystore_jobs_failed";
    let kept: i64 = jobs.file().read(|c| c.query_row(count, [], |row| row.get(0)).map_err(failed)).unwrap();
    assert_eq!(kept, 0, "one job under the schedule's id, not a failed one beside it");
}

#[test]
fn a_time_outside_the_years_a_job_keeps_is_refused() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    let year_10000 = std::time::UNIX_EPOCH + Duration::from_secs(253_402_300_800);
    assert_eq!(notes.at(year_10000).add(&note("x")).unwrap_err().kind(), ErrorKind::Invalid);
}

// contract: ERR-03, ERR-06
#[test]
fn a_span_longer_than_the_years_a_job_keeps_is_refused_where_it_is_given() {
    let f = fixture();
    let notes = f.store.queue::<Note>("notes").open().unwrap();
    let error = notes.id("a").every(Duration::MAX).add(&note("x")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert!(notes.get("a").unwrap().is_none(), "the call added nothing");
    let builders = [
        f.store.queue::<Note>("rated").rate(1, Duration::MAX),
        f.store.queue::<Note>("grouped").group_rate(1, Duration::MAX),
        f.store.queue::<Note>("patient").timeout(Duration::MAX),
    ];
    for builder in builders {
        let error = builder.open().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    }
    assert!(f.store.queue::<Note>("rated").rate(1, SECOND).open().is_ok(), "the refused open kept nothing");
}

#[test]
fn a_progress_past_4_kib_is_refused() {
    let f = fixture();
    let videos = f.store.queue::<Note>("videos").open().unwrap();
    videos.add(&note("v")).unwrap();
    let refused = Mutex::new(None);
    videos
        .run_due(|_: Note, run| -> Result<()> {
            *refused.lock().unwrap() = run.set_progress(&"x".repeat(5000)).err().map(|error| error.kind());
            Ok(())
        })
        .unwrap();
    assert_eq!(*refused.lock().unwrap(), Some(ErrorKind::Limit));
}

#[test]
fn a_transaction_of_the_store_adds_its_jobs_together_or_not_at_all() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").open().unwrap();
    f.store
        .tx(|tx| -> Result<()> {
            for member in ["ann", "bob", "eve"] {
                assert!(tx.with(&pushes).id(member).add(&note("the build is green"))?);
            }
            assert!(tx.with(&pushes).get("ann")?.is_some(), "a read inside sees what the transaction added");
            Ok(())
        })
        .unwrap();
    assert_eq!(ran(&pushes).len(), 3);

    pushes.id("kept").add(&note("stays")).unwrap();
    let refused = f.store.tx(|tx| -> Result<()> {
        tx.with(&pushes).id("zed").add(&note("never sent"))?;
        assert!(tx.with(&pushes).cancel("kept")?);
        Err(crate::Error::invalid("changed its mind"))
    });
    assert_eq!(refused.unwrap_err().kind(), ErrorKind::Invalid);
    assert!(pushes.get("zed").unwrap().is_none(), "the add rolled back");
    assert!(pushes.get("kept").unwrap().is_some(), "and the cancel");

    let around = f.store.tx(|_| pushes.add(&note("outside the transaction"))).unwrap_err();
    assert_eq!(around.kind(), ErrorKind::Invalid, "a call around the transaction is refused: {around}");
}

#[cfg(feature = "kv")]
#[test]
fn a_transaction_of_the_store_is_of_one_file() {
    let f = fixture();
    let pushes = f.store.queue::<Note>("pushes").open().unwrap();
    let seen = f.store.bucket::<String>("seen").open().unwrap();
    let error = f
        .store
        .tx(|tx| -> Result<()> {
            tx.with(&pushes).add(&note("first"))?;
            tx.with(&seen).set("ann", &"yes".to_owned())
        })
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert!(error.to_string().contains("kept in kv.db, outside this transaction of jobs.db"), "{error}");
    assert!(ran(&pushes).is_empty(), "the job rolled back with the transaction");

    let nothing: Result<()> = f.store.tx(|_| Ok(()));
    nothing.unwrap();
}

/// SQLite's `synchronous` on a file's writer: 2 syncs each commit, 1 leaves
/// the sync to checkpoints.
fn synchronous(file: &crate::sqlite::File) -> i64 {
    file.transaction(|tx| -> crate::Result<i64> {
        tx.pragma_query_value(None, "synchronous", |row| row.get(0))
            .map_err(|error| crate::sqlite::sql_error("synchronous", error))
    })
    .unwrap()
}

// contract: DUR-01
#[test]
fn jobs_db_commits_as_far_as_its_store_says_and_syncs_each_commit_unless_told() {
    for (durability, level) in [(None, 2), (Some(crate::Durability::Os), 1)] {
        let dir = tempfile::tempdir().unwrap();
        let options = crate::Options { durability, background: false, ..crate::Options::default() };
        let store = crate::Store::open(dir.path(), options).unwrap();
        let mail = store.queue::<i64>("mail").open().unwrap();
        mail.add(&1).unwrap();
        assert_eq!(synchronous(super::engine::Jobs::of(&store).unwrap().file()), level, "{durability:?}");
        store.close().unwrap();
    }
}

/// What a job costs from its add to its end, one call at a time, on a store
/// whose commits are not synced, so that each call's SQL shows rather than
/// the disk.
///
/// `cargo test -p tinystore --release --lib timing_a_job -- --ignored --nocapture`
#[test]
#[ignore = "a timing, run by hand"]
fn timing_a_job_from_its_add_to_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let options = crate::Options { background: false, durability: Some(crate::Durability::Os), ..Default::default() };
    let store = crate::Store::open(dir.path(), options).unwrap();
    let notes = store.queue::<Note>("notes").open().unwrap();
    for round in 0..3 {
        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            notes.add(&note("a note")).unwrap();
        }
        let added = started.elapsed();
        let started = std::time::Instant::now();
        while let Some(claimed) = notes.claim().unwrap() {
            claimed.settle(()).unwrap();
        }
        let ran = started.elapsed();
        let each = |spent: Duration| spent.as_secs_f64() * 100.0;
        println!("round {round}: an add {:.1} µs, a claim and its settle {:.1} µs", each(added), each(ran));
        let started = std::time::Instant::now();
        std::thread::scope(|callers| {
            for _ in 0..16 {
                callers.spawn(|| {
                    for _ in 0..2_000 {
                        notes.add(&note("a note")).unwrap();
                    }
                });
            }
        });
        println!("round {round}: 16 callers at once, {:.0} adds a second", 32_000.0 / started.elapsed().as_secs_f64());
        while let Some(claimed) = notes.claim().unwrap() {
            claimed.settle(()).unwrap();
        }
    }
}
