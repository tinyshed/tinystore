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

    f.advance(15 * HOUR);
    let ran = Mutex::new(Vec::new());
    let run = |value: String, _run: &super::Run| -> Result<()> {
        ran.lock().unwrap().push(value);
        Ok(())
    };
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
