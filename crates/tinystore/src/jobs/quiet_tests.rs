//! Quiet hours: a time the store gives a job inside them is their end.

use std::sync::Mutex;
use std::time::SystemTime;

use super::fixture::{DAY, HOUR, MINUTE, SECOND, fixture};
use super::{Gather, Queue, Quiet, State};
use crate::{ErrorKind, Result};

/// The fixture's clock starts at 2026-10-09 12:00 UTC.
fn night() -> Quiet {
    Quiet::hours("23:00", "07:00", "UTC")
}

fn at_of(queue: &Queue<String>, id: &str) -> SystemTime {
    queue.get(id).unwrap().unwrap().at
}

// contract: JOB-24
#[test]
fn a_job_due_inside_its_queues_quiet_hours_is_due_at_their_end_and_one_outside_as_it_was() {
    let f = fixture();
    let pushes = f.store.queue::<String>("pushes").quiet(night()).open().unwrap();
    pushes.id("evening").delay(10 * HOUR + 59 * MINUTE).add(&"22:59".into()).unwrap();
    pushes.id("night").delay(11 * HOUR).add(&"23:00".into()).unwrap();
    pushes.id("small hours").delay(15 * HOUR).add(&"03:00".into()).unwrap();
    pushes.id("at nine").at(f.now() + 21 * HOUR).add(&"09:00".into()).unwrap();
    let seven = f.now() + 19 * HOUR;
    assert_eq!(at_of(&pushes, "evening"), f.now() + 10 * HOUR + 59 * MINUTE, "outside, as it was");
    assert_eq!(at_of(&pushes, "night"), seven, "inside, wrapping midnight: the end the next morning");
    assert_eq!(at_of(&pushes, "small hours"), seven, "inside, after midnight: the end that morning");
    assert_eq!(at_of(&pushes, "at nine"), f.now() + 21 * HOUR, "an explicit time outside, as it was");
    pushes.id("explicit").at(f.now() + 12 * HOUR).add(&"midnight".into()).unwrap();
    assert_eq!(at_of(&pushes, "explicit"), seven, "an explicit time inside is moved too");

    f.advance(10 * HOUR + 59 * MINUTE);
    let ran = Mutex::new(Vec::new());
    let run = |value: String, _run: &super::Run| -> Result<()> {
        ran.lock().unwrap().push(value);
        Ok(())
    };
    pushes.run_due(run).unwrap();
    assert_eq!(*ran.lock().unwrap(), ["22:59"]);
    f.advance(4 * HOUR + MINUTE);
    pushes.run_due(run).unwrap();
    assert_eq!(*ran.lock().unwrap(), ["22:59"], "nothing of the night ran in it");
    f.advance(4 * HOUR);
    pushes.run_due(run).unwrap();
    assert_eq!(ran.lock().unwrap().len(), 4, "the night's jobs ran at seven");
}

// contract: JOB-24
#[test]
fn a_calls_own_window_wins_over_its_queues_and_none_holds_its_job_by_none() {
    let f = fixture();
    let pushes = f.store.queue::<String>("pushes").quiet(night()).open().unwrap();
    let dinner = Quiet::hours("22:00", "23:00", "UTC");
    pushes.id("dinner").delay(10 * HOUR + 30 * MINUTE).quiet(dinner).add(&"22:30".into()).unwrap();
    assert_eq!(at_of(&pushes, "dinner"), f.now() + 11 * HOUR, "its own end, which its queue's would hold on");
    pushes.id("urgent").delay(12 * HOUR).quiet(Quiet::none()).add(&"midnight".into()).unwrap();
    assert_eq!(at_of(&pushes, "urgent"), f.now() + 12 * HOUR, "no hours hold it");
    let same = pushes.id("x").quiet(Quiet::hours("23:00", "23:00", "UTC")).add(&"x".into()).unwrap_err();
    assert_eq!(same.kind(), ErrorKind::Invalid);
    let refused = f.store.queue::<String>("loud").quiet(Quiet::none()).open().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "a queue's quiet hours are hours");
}

/// Runs what is due and answers the values that ran.
fn ran(queue: &Queue<String>) -> Vec<String> {
    let seen = Mutex::new(Vec::new());
    queue
        .run_due(|value: String, _run| -> Result<()> {
            seen.lock().unwrap().push(value);
            Ok(())
        })
        .unwrap();
    seen.into_inner().unwrap()
}

// contract: JOB-24
#[test]
fn a_job_due_before_the_hours_that_a_worker_reaches_inside_them_runs_at_their_end() {
    let mut f = fixture();
    let pushes = f.store.queue::<String>("pushes").quiet(night()).open().unwrap();
    pushes.id("backlog").delay(10 * HOUR + 59 * MINUTE).add(&"22:59".into()).unwrap();
    drop(pushes);
    let open = |store: &crate::Store, quiet: bool| {
        let builder = store.queue::<String>("pushes");
        if quiet { builder.quiet(night()).open() } else { builder.open() }.unwrap()
    };
    f.reopen();
    open(&f.store, false).id("older").delay(12 * HOUR).add(&"midnight".into()).unwrap();
    f.reopen();
    let pushes = open(&f.store, true);
    f.advance(11 * HOUR + 20 * MINUTE);
    assert!(ran(&pushes).is_empty(), "at 23:20 nothing runs, the backlog's 22:59 neither");
    f.advance(HOUR);
    assert!(ran(&pushes).is_empty(), "nor a job added before the queue had hours");
    let seven = f.now() + 6 * HOUR + 40 * MINUTE;
    assert_eq!((at_of(&pushes, "backlog"), at_of(&pushes, "older")), (seven, seven), "the claim moved both");
    f.advance(6 * HOUR + 40 * MINUTE);
    assert_eq!(ran(&pushes).len(), 2);
}

// contract: JOB-24
#[test]
fn a_snooze_into_the_hours_is_due_at_their_end() {
    let f = fixture();
    let pushes = f.store.queue::<String>("pushes").quiet(night()).open().unwrap();
    pushes.id("later").delay(10 * HOUR).add(&"22:00".into()).unwrap();
    f.advance(10 * HOUR);
    pushes.run_due(|_: String, run: &super::Run| -> Result<super::Outcome> { Ok(run.snooze(2 * HOUR)) }).unwrap();
    assert_eq!(at_of(&pushes, "later"), f.now() + 9 * HOUR, "midnight held to seven");
}

// contract: JOB-24
#[test]
fn a_set_an_update_or_an_upsert_that_gives_no_hours_keeps_the_jobs_own() {
    let f = fixture();
    let pushes = f.store.queue::<String>("pushes").quiet(night()).open().unwrap();
    pushes.id("urgent").delay(HOUR).quiet(Quiet::none()).add(&"a".into()).unwrap();
    pushes.id("lunch").delay(HOUR).quiet(Quiet::hours("12:00", "13:00", "UTC")).add(&"a".into()).unwrap();
    f.advance(11 * HOUR + 30 * MINUTE);
    pushes.set("urgent", &"b".into()).unwrap();
    assert_eq!(at_of(&pushes, "urgent"), f.now(), "23:30, which its own none holds by nothing");
    pushes.id("urgent").delay(10 * MINUTE).update(&"c".into()).unwrap();
    assert_eq!(at_of(&pushes, "urgent"), f.now() + 10 * MINUTE);
    pushes.id("urgent").cron("45 23 * * *", "UTC").upsert(&"d".into()).unwrap();
    assert_eq!(at_of(&pushes, "urgent"), f.now() + 15 * MINUTE, "a changed repeat's 23:45");
    f.advance(13 * HOUR);
    pushes.set("lunch", &"b".into()).unwrap();
    assert_eq!(at_of(&pushes, "lunch"), f.now() + 30 * MINUTE, "12:30 held by its own lunch, not by the night");
}

// contract: JOB-24
#[test]
fn a_retry_that_would_land_inside_the_hours_waits_for_their_end() {
    let f = fixture();
    let lunch = Quiet::hours("12:01", "13:00", "UTC");
    let pushes = f.store.queue::<String>("pushes").backoff(2 * MINUTE, HOUR).quiet(lunch).open().unwrap();
    pushes.id("failing").add(&"x".into()).unwrap();
    pushes.run_due(|_: String, _run| -> Result<(), &str> { Err("push service 503") }).unwrap();
    let job = pushes.get("failing").unwrap().unwrap();
    assert_eq!((job.attempt, job.at), (1, f.now() + HOUR), "its backoff of two minutes landed in lunch");
}

// contract: JOB-24
#[test]
fn a_jobs_own_window_holds_its_retries_and_its_repeats_across_a_reopen() {
    let mut f = fixture();
    let checks = f.store.queue::<String>("checks").backoff(2 * MINUTE, HOUR).open().unwrap();
    let lunch = Quiet::hours("12:01", "13:00", "UTC");
    checks.id("daily").cron("45 12 * * *", "UTC").quiet(lunch.clone()).add(&"daily".into()).unwrap();
    assert_eq!(at_of(&checks, "daily"), f.now() + HOUR, "its first time, 12:45, held to 13:00");
    checks.id("failing").quiet(lunch).add(&"x".into()).unwrap();
    drop(checks);
    f.reopen();
    let checks = f.store.queue::<String>("checks").backoff(2 * MINUTE, HOUR).open().unwrap();
    checks.run_due(|_: String, _run| -> Result<(), &str> { Err("down") }).unwrap();
    assert_eq!(at_of(&checks, "failing"), f.now() + HOUR, "its retry, held by its own window");
    f.advance(HOUR);
    checks.run_due(|_: String, _run| -> Result<()> { Ok(()) }).unwrap();
    assert_eq!(at_of(&checks, "daily"), f.now() + DAY, "the repeat's next 12:45, held to 13:00 again");
}

// contract: JOB-24
#[test]
fn a_gathers_settle_inside_the_hours_is_their_end() {
    let f = fixture();
    let night = Quiet::hours("12:03", "12:30", "UTC");
    let batch = f.store.queue::<Vec<i64>>("notify-batch").quiet(night).open().unwrap();
    let first = batch.gather("grades:7", &vec![1], Gather::settle(MINUTE)).unwrap();
    assert_eq!(first.at, f.now() + MINUTE, "outside");
    f.advance(2 * MINUTE + 30 * SECOND);
    let second = batch.gather("grades:7", &vec![2], Gather::settle(MINUTE)).unwrap();
    assert_eq!(second.at, f.now() - 2 * MINUTE - 30 * SECOND + 30 * MINUTE, "12:03:30 held to 12:30");
    assert_eq!(batch.get("grades:7").unwrap().unwrap().state, State::Scheduled);
}
