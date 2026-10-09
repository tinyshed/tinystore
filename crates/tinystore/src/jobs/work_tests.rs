//! Workers: handlers and their answers, retries, leases, limits and steps.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::fixture::{HOUR, MINUTE, SECOND, fixture};
use super::{Concurrency, Run, State};
use crate::{ErrorKind, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Push {
    user: i64,
}

fn eventually(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "the condition never held");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_handler_settles_its_job_by_what_it_returns() {
    let f = fixture();
    let pushes = f.store.queue::<Push>("pushes").attempts(5).open().unwrap();
    for (id, user) in [("done", 1), ("error", 2), ("retry", 3), ("snooze", 4), ("fail", 5)] {
        pushes.id(id).add(&Push { user }).unwrap();
    }
    pushes
        .run_due(|push: Push, run: &Run| -> Result<super::Outcome, String> {
            match push.user {
                1 => Ok(().into()),
                2 => Err("the provider is down".into()),
                3 => Ok(run.retry(10 * MINUTE)),
                4 => Ok(run.snooze(10 * MINUTE)),
                _ => Ok(run.fail("the provider no longer knows the token")),
            }
        })
        .unwrap();

    assert!(pushes.get("done").unwrap().is_none());
    let error = pushes.get("error").unwrap().unwrap();
    assert_eq!((error.state, error.attempt), (State::Scheduled, 1));
    assert_eq!(error.error.as_deref(), Some("the provider is down"));
    let retry = pushes.get("retry").unwrap().unwrap();
    assert_eq!((retry.at, retry.attempt), (f.now() + 10 * MINUTE, 1), "a retry counts its run");
    let snooze = pushes.get("snooze").unwrap().unwrap();
    assert_eq!((snooze.at, snooze.attempt), (f.now() + 10 * MINUTE, 0), "a snooze does not");
    let fail = pushes.get("fail").unwrap().unwrap();
    assert_eq!((fail.state, fail.error.as_deref()), (State::Failed, Some("the provider no longer knows the token")));
}

#[test]
fn a_retry_waits_longer_each_time_then_fails_for_good() {
    let f = fixture();
    let emails = f.store.queue::<Push>("emails").attempts(4).backoff(SECOND, Duration::from_secs(4)).open().unwrap();
    emails.id("welcome").add(&Push { user: 1 }).unwrap();
    let mut waits = Vec::new();
    for _ in 0..4 {
        emails.run_due(|_: Push, _run| -> Result<(), &str> { Err("bounced") }).unwrap();
        let job = emails.get("welcome").unwrap().unwrap();
        if job.state == State::Failed {
            break;
        }
        waits.push(job.at.duration_since(f.now()).unwrap());
        f.clock.advance(job.at.duration_since(f.now()).unwrap());
    }
    let millis: Vec<u128> = waits.iter().map(Duration::as_millis).collect();
    for (wait, expected) in millis.iter().zip([1000u128, 2000, 4000]) {
        assert!((expected * 9 / 10..=expected * 11 / 10 + 1).contains(wait), "{millis:?}");
    }
    let failed = emails.get("welcome").unwrap().unwrap();
    assert_eq!((failed.state, failed.attempt), (State::Failed, 4));
}

#[test]
fn a_panic_is_an_error_with_its_message() {
    let f = fixture();
    let pushes = f.store.queue::<Push>("pushes").attempts(1).open().unwrap();
    pushes.id("p").add(&Push { user: 1 }).unwrap();
    pushes.run_due(|_: Push, _run| -> Result<()> { panic!("a bug") }).unwrap();
    let failed = pushes.get("p").unwrap().unwrap();
    assert_eq!(failed.error.as_deref(), Some("the handler panicked: a bug"));
}

#[test]
fn a_job_that_kills_its_process_fails_after_its_attempts() {
    let mut f = fixture();
    for _ in 0..3 {
        let pushes = f.store.queue::<Push>("pushes").attempts(3).open().unwrap();
        if pushes.get("deadly").unwrap().is_none() {
            pushes.id("deadly").add(&Push { user: 1 }).unwrap();
        }
        let claimed = pushes.claim().unwrap().expect("the job is due again at once");
        std::mem::forget(claimed); // the process dies with the job in hand
        f.reopen();
    }
    let pushes = f.store.queue::<Push>("pushes").attempts(3).open().unwrap();
    assert!(pushes.claim().unwrap().is_none(), "a fourth attempt is past its three");
    let failed = pushes.get("deadly").unwrap().unwrap();
    assert_eq!(failed.state, State::Failed);
    assert!(failed.error.unwrap().contains("each of its 3 attempts ended without a settlement"));
}

#[test]
fn a_job_whose_lease_ended_runs_again_and_its_stale_lease_settles_nothing() {
    let f = fixture();
    let pushes = f.store.queue::<Push>("pushes").timeout(MINUTE).open().unwrap();
    pushes.id("p").add(&Push { user: 1 }).unwrap();
    let first = pushes.claim().unwrap().unwrap();
    assert!(pushes.claim().unwrap().is_none(), "a live lease holds it");
    f.clock.advance(MINUTE);
    let second = pushes.claim().unwrap().unwrap();
    assert_eq!(second.run().attempt(), 2, "the attempt whose lease ended counts");
    assert_eq!(first.settle(()).unwrap_err().kind(), ErrorKind::Conflict);
    second.settle(()).unwrap();
    assert!(pushes.get("p").unwrap().is_none());
}

#[test]
fn a_worker_runs_jobs_on_the_stores_threads_until_it_stops() {
    let f = fixture();
    let pushes = f.store.queue::<Push>("pushes").concurrency(4).open().unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&seen);
    let worker = pushes
        .work(move |_: Push, _run| -> Result<()> {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    for user in 0..50 {
        pushes.add(&Push { user }).unwrap();
    }
    eventually(|| seen.load(Ordering::SeqCst) == 50);
    worker.stop();
    pushes.add(&Push { user: 50 }).unwrap();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(seen.load(Ordering::SeqCst), 50, "a stopped worker takes nothing more");
}

#[test]
fn a_stopping_worker_waits_for_its_handlers_and_gives_back_what_it_had_not_started() {
    let f = fixture();
    let pushes = f.store.queue::<Push>("pushes").open().unwrap();
    pushes.id("first").add(&Push { user: 1 }).unwrap();
    pushes.id("second").add(&Push { user: 2 }).unwrap();
    let (started, wait) = (mpsc::channel(), mpsc::channel::<()>());
    let (started_tx, started_rx) = started;
    let (release_tx, release_rx) = wait;
    let release_rx = Mutex::new(release_rx);
    let worker = pushes
        .work(move |push: Push, _run| -> Result<()> {
            started_tx.send(push.user).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(started_rx.recv().unwrap(), 1, "one handler, the second job held for it");
    eventually(|| pushes.get("second").unwrap().is_some_and(|job| job.state == State::Waiting));
    let stopping = thread::spawn(move || worker.stop());
    thread::sleep(Duration::from_millis(20));
    release_tx.send(()).unwrap();
    stopping.join().unwrap();
    assert!(pushes.get("first").unwrap().is_none(), "the handler under way finished");
    let second = pushes.get("second").unwrap().unwrap();
    assert_eq!((second.state, second.attempt), (State::Waiting, 0), "given back, its attempt not counted");
}

#[test]
fn a_worker_runs_a_job_its_moved_clock_made_due_at_once() {
    let f = fixture();
    let pushes = f.store.queue::<Push>("pushes").open().unwrap();
    let (ran_tx, ran_rx) = mpsc::channel();
    let worker = pushes
        .work(move |push: Push, _run| -> Result<()> {
            ran_tx.send(push.user).unwrap();
            Ok(())
        })
        .unwrap();
    pushes.delay(HOUR).add(&Push { user: 7 }).unwrap();
    assert!(ran_rx.recv_timeout(Duration::from_millis(100)).is_err(), "not before its time");
    f.advance(HOUR);
    let ran = ran_rx.recv_timeout(Duration::from_secs(5));
    assert_eq!(ran, Ok(7), "run once the clock moved, not when the worker's minute of sleep ends");
    worker.stop();
}

#[test]
fn a_stopping_worker_writes_each_answer_as_its_handler_gives_it() {
    let f = fixture();
    let pushes = f.store.queue::<Push>("pushes").open().unwrap();
    pushes.id("fast").add(&Push { user: 1 }).unwrap();
    pushes.id("slow").add(&Push { user: 2 }).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let ((fast_tx, fast_rx), (slow_tx, slow_rx)) = (mpsc::channel::<()>(), mpsc::channel::<()>());
    let (fast_rx, slow_rx) = (Mutex::new(fast_rx), Mutex::new(slow_rx));
    let worker = pushes
        .concurrency(2)
        .work(move |push: Push, _run| -> Result<()> {
            started_tx.send(push.user).unwrap();
            let release = if push.user == 1 { &fast_rx } else { &slow_rx };
            release.lock().unwrap().recv().unwrap();
            Ok(())
        })
        .unwrap();
    let mut started = [started_rx.recv().unwrap(), started_rx.recv().unwrap()];
    started.sort_unstable();
    assert_eq!(started, [1, 2], "both run at once");

    let stopping = thread::spawn(move || worker.stop());
    thread::sleep(Duration::from_millis(20));
    fast_tx.send(()).unwrap();
    eventually(|| pushes.get("fast").unwrap().is_none());
    assert!(!stopping.is_finished(), "the slow handler still holds the stop");
    assert_eq!(pushes.get("slow").unwrap().map(|job| job.state), Some(State::Running));
    slow_tx.send(()).unwrap();
    stopping.join().unwrap();
    assert!(pushes.get("slow").unwrap().is_none());
}

#[test]
fn jobs_due_together_are_claimed_and_settled_in_batches() {
    let f = fixture();
    let pushes = f.store.queue::<Push>("pushes").open().unwrap();
    for user in 0..200 {
        pushes.add(&Push { user }).unwrap();
    }
    let before = pushes.jobs.file().commits();
    let ran = pushes.concurrency(8).run_due(|_: Push, _run| -> Result<()> { Ok(()) }).unwrap();
    let commits = pushes.jobs.file().commits() - before;
    assert_eq!(ran, 200);
    assert!(commits < 100, "{commits} commits for 200 jobs");
}

#[test]
fn a_total_holds_a_queue_to_its_places_across_claims() {
    let f = fixture();
    let videos = f.store.queue::<Push>("videos").concurrency(2).open().unwrap();
    for user in 0..3 {
        videos.add(&Push { user }).unwrap();
    }
    let first = videos.claim().unwrap().unwrap();
    let _second = videos.claim().unwrap().unwrap();
    assert!(videos.claim().unwrap().is_none(), "two places, both taken");
    first.settle(()).unwrap();
    assert!(videos.claim().unwrap().is_some(), "a settlement gives its place back");
}

#[test]
fn a_group_bounds_its_running_jobs_and_holds_back_no_other_group() {
    let f = fixture();
    let refreshes = f.store.queue::<Push>("refreshes").concurrency(Concurrency::total(10).group(1)).open().unwrap();
    refreshes.group("db:1").add(&Push { user: 1 }).unwrap();
    refreshes.group("db:1").add(&Push { user: 2 }).unwrap();
    refreshes.group("db:2").add(&Push { user: 3 }).unwrap();
    let first = refreshes.claim().unwrap().unwrap();
    let other = refreshes.claim().unwrap().unwrap();
    assert_eq!((first.value.user, other.value.user), (1, 3), "db:1's second waits; db:2 runs");
    assert!(refreshes.claim().unwrap().is_none());
    first.settle(()).unwrap();
    assert_eq!(refreshes.claim().unwrap().unwrap().value.user, 2, "db:1's place went to its next job");
}

#[test]
fn a_rate_lets_no_more_than_its_count_start_in_a_span() {
    let f = fixture();
    let telegram = f.store.queue::<Push>("telegram").rate(2, SECOND).open().unwrap();
    for user in 0..3 {
        telegram.add(&Push { user }).unwrap();
    }
    let ran = telegram.run_due(|_: Push, _run| -> Result<()> { Ok(()) }).unwrap();
    assert_eq!(ran, 2);
    f.clock.advance(SECOND);
    assert_eq!(telegram.run_due(|_: Push, _run| -> Result<()> { Ok(()) }).unwrap(), 1);
}

#[test]
fn a_step_runs_once_across_the_attempts_of_a_run() {
    let f = fixture();
    let agents = f.store.queue::<Push>("agents").backoff(SECOND, SECOND * 2).open().unwrap();
    agents.id("question").add(&Push { user: 1 }).unwrap();
    let searches = AtomicUsize::new(0);
    let answer = |_: Push, run: &Run| -> Result<()> {
        let found: Vec<String> = run.step("search", || -> Result<Vec<String>> {
            searches.fetch_add(1, Ordering::SeqCst);
            Ok(vec!["a hit".to_owned()])
        })?;
        assert_eq!(found, ["a hit"]);
        if run.attempt() == 1 {
            return Err(crate::Error::invalid("the model timed out"));
        }
        Ok(())
    };
    agents.run_due(answer).unwrap();
    f.clock.advance(HOUR);
    agents.run_due(answer).unwrap();
    assert_eq!(searches.load(Ordering::SeqCst), 1, "the retry got the search back");
    assert!(agents.get("question").unwrap().is_none());
}

#[test]
fn a_job_cancelled_while_it_runs_gives_its_groups_place_to_the_next() {
    let f = fixture();
    let refreshes = f.store.queue::<Push>("refreshes").concurrency(Concurrency::default().group(1)).open().unwrap();
    refreshes.id("a").group("db:1").add(&Push { user: 1 }).unwrap();
    refreshes.id("b").group("db:1").add(&Push { user: 2 }).unwrap();
    let first = refreshes.claim().unwrap().unwrap();
    assert!(refreshes.claim().unwrap().is_none(), "b is parked behind a");
    assert!(refreshes.cancel("a").unwrap());
    assert_eq!(first.settle(()).unwrap_err().kind(), ErrorKind::Conflict, "a cancelled job settles nothing");
    assert_eq!(refreshes.claim().unwrap().unwrap().value.user, 2);
}

#[test]
fn a_run_that_ends_takes_its_steps_along_and_a_repeats_next_run_starts_without_them() {
    let f = fixture();
    let digests = f.store.queue::<Push>("digests").open().unwrap();
    digests.id("user:1").every(HOUR).set(&Push { user: 1 }).unwrap();
    let steps = AtomicUsize::new(0);
    let digest = |_: Push, run: &Run| -> Result<()> {
        run.step("gather", || -> Result<()> {
            steps.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    };
    for _ in 0..2 {
        f.clock.advance(HOUR);
        assert_eq!(digests.run_due(digest).unwrap(), 1);
    }
    assert_eq!(steps.load(Ordering::SeqCst), 2, "each run of the repeat ran its step");
}

#[test]
fn an_update_of_a_parked_job_gives_it_back_its_time() {
    let f = fixture();
    let refreshes = f.store.queue::<Push>("refreshes").concurrency(Concurrency::default().group(1)).open().unwrap();
    refreshes.id("a").group("db:1").add(&Push { user: 1 }).unwrap();
    refreshes.id("b").group("db:1").add(&Push { user: 2 }).unwrap();
    let _running = refreshes.claim().unwrap().unwrap();
    assert!(refreshes.claim().unwrap().is_none(), "b is parked behind a");
    assert!(refreshes.update("b", &Push { user: 3 }).unwrap());
    let b = refreshes.get("b").unwrap().unwrap();
    assert_eq!((b.at, b.state, b.value.user), (f.now(), State::Waiting, 3));
}

#[test]
fn a_worker_its_program_let_go_of_still_stops_when_the_store_closes() {
    let f = fixture();
    let pushes = f.store.queue::<Push>("pushes").open().unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&seen);
    let worker = pushes.work(move |_: Push, _run| -> Result<()> {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    drop(worker.unwrap());
    pushes.add(&Push { user: 1 }).unwrap();
    eventually(|| seen.load(Ordering::SeqCst) == 1);
    let closing = Instant::now();
    f.store.close().unwrap();
    assert!(closing.elapsed() < Duration::from_secs(5), "{:?}", closing.elapsed());
}
