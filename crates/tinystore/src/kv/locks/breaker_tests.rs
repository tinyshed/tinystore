use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use crate::kv::fixture::*;
use crate::testing::{PATIENCE, holds_within};
use crate::{Clock, unix_millis};

const PAUSE: Duration = Duration::from_secs(30);
#[cfg(feature = "wire")]
const RULE: Rule = Rule { failures: 2, pause: PAUSE };

/// A semaphore of one place whose key goes down for 30 s after two failed
/// holds in a row.
fn lane(f: &Fixture) -> Semaphore {
    f.store.semaphore("cabinet").limit(1).breaker(2, PAUSE).open().unwrap()
}

fn bad_gateway() -> Error {
    Error::new(ErrorKind::Io, "the cabinet answered 502")
}

fn fail(lane: &Semaphore) -> Error {
    lane.hold("requests", |_| Err::<(), _>(bad_gateway())).unwrap_err()
}

fn succeed(lane: &Semaphore) -> Result<&'static str> {
    lane.hold("requests", |_| Ok("answered"))
}

fn locks(store: &Store) -> Arc<Locks> {
    Locks::of(store).unwrap()
}

fn move_clock(f: &Fixture, by: Duration) {
    f.clock.advance(by);
    f.store.clock_moved();
}

/// A hold on a thread of its own that holds its place until told, then
/// answers what it is told.
fn held_elsewhere(lane: &Semaphore) -> (mpsc::Sender<Result<()>>, thread::JoinHandle<Result<()>>) {
    let (holds, held) = mpsc::channel();
    let (go, told) = mpsc::channel::<Result<()>>();
    let lane = lane.clone();
    let holder = thread::spawn(move || {
        lane.hold("requests", |_| {
            holds.send(()).unwrap();
            told.recv_timeout(PATIENCE).unwrap()
        })
    });
    held.recv_timeout(PATIENCE).unwrap();
    (go, holder)
}

// contract: LOCK-12
#[test]
fn failed_holds_in_a_row_put_the_key_down_and_every_ask_waiting_or_new_is_refused_at_once() {
    let f = fixture();
    let lane = lane(&f);
    fail(&lane);
    let once = lane.state("requests").unwrap();
    assert!(once.up, "one failure of two");
    assert_eq!(once.last_failure, Some(f.clock.now()));

    let (go, holder) = held_elsewhere(&lane);
    let waiter = {
        let lane = lane.clone();
        thread::spawn(move || lane.key("requests").wait(MINUTE).hold(|_| Ok::<_, Error>(())))
    };
    let locks = locks(&f.store);
    holds_within(PATIENCE, "the waiter in line", || locks.waiting() == 1);
    assert_eq!(lane.state("requests").unwrap().waiting, 1);
    go.send(Err(bad_gateway())).unwrap();
    assert_eq!(holder.join().unwrap().unwrap_err().kind(), ErrorKind::Io, "the work's own error");
    let waited = waiter.join().unwrap().unwrap_err();
    assert_eq!(waited.kind(), ErrorKind::Unavailable, "the waiter is refused, not given the key: {waited}");

    let refused = succeed(&lane).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Unavailable, "{refused}");
    let said = refused.to_string();
    let since = crate::clock::utc(unix_millis(f.clock.now()));
    assert!(said.starts_with("semaphore cabinet: key \"requests\": down since "), "{said}");
    assert!(said.contains(&format!("down since {since}, after 2 failed holds in a row")), "{said}");
    let down = lane.state("requests").unwrap();
    assert!(!down.up);
    assert_eq!(down.down_since, Some(f.clock.now()));
    assert_eq!((down.waiting, down.holding), (0, 0));
    assert!(lane.try_hold("other", |_| Ok::<_, Error>(())).unwrap().is_some(), "a breaker a key");
}

// contract: LOCK-15
#[test]
fn a_keys_state_counts_its_failed_holds_in_a_row_as_its_breaker_does() {
    let f = fixture();
    let lane = f.store.semaphore("cabinet").limit(1).breaker(3, PAUSE).open().unwrap();
    let failures = || lane.state("requests").unwrap().failures;
    assert_eq!(failures(), 0, "none yet");
    fail(&lane);
    fail(&lane);
    assert_eq!(failures(), 2, "2 of 3");
    succeed(&lane).unwrap();
    assert_eq!(failures(), 0, "a success resets the count");

    fail(&lane);
    move_clock(&f, PAUSE);
    assert_eq!(failures(), 0, "a pause with no failure resets it too");
    fail(&lane);
    fail(&lane);
    fail(&lane);
    let down = lane.state("requests").unwrap();
    assert!(!down.up);
    assert_eq!(down.failures, 3, "while down, the failures that put it down");
    move_clock(&f, PAUSE);
    fail(&lane);
    assert_eq!(failures(), 3, "a probe that failed keeps it down, not counted past what tripped it");
    move_clock(&f, PAUSE);
    succeed(&lane).unwrap();
    assert_eq!((lane.state("requests").unwrap().up, failures()), (true, 0), "up again, counting from none");

    let plain = f.store.semaphore("plain").limit(1).open().unwrap();
    plain.hold("requests", |_| Err::<(), _>(bad_gateway())).unwrap_err();
    assert_eq!(plain.state("requests").unwrap().failures, 0, "no breaker counts nothing");
}

// contract: LOCK-13
#[test]
fn once_the_pause_is_over_one_ask_goes_through_and_its_success_puts_the_key_up() {
    let f = fixture();
    let lane = lane(&f);
    fail(&lane);
    fail(&lane);
    move_clock(&f, PAUSE - Duration::from_millis(1));
    assert_eq!(succeed(&lane).unwrap_err().kind(), ErrorKind::Unavailable, "a millisecond short of the pause");
    move_clock(&f, Duration::from_millis(1));

    let (go, probe) = held_elsewhere(&lane);
    let other = lane.key("requests").wait(MINUTE).hold(|_| Ok::<_, Error>(())).unwrap_err();
    assert_eq!(other.kind(), ErrorKind::Unavailable, "one ask goes through at a time");
    assert!(other.to_string().contains("one hold tries it now"), "{other}");
    assert!(!lane.state("requests").unwrap().up, "down until the probe says");
    go.send(Ok(())).unwrap();
    probe.join().unwrap().unwrap();

    let up = lane.state("requests").unwrap();
    assert!(up.up && up.down_since.is_none(), "{up:?}");
    assert_eq!(succeed(&lane).unwrap(), "answered");
    fail(&lane);
    assert!(lane.state("requests").unwrap().up, "the count started again from zero");
}

// contract: LOCK-13
#[test]
fn a_probe_that_fails_pauses_the_key_again_from_its_failure() {
    let f = fixture();
    let lane = lane(&f);
    fail(&lane);
    fail(&lane);
    let down_since = f.clock.now();
    move_clock(&f, PAUSE);
    fail(&lane);
    move_clock(&f, PAUSE - Duration::from_millis(1));
    assert_eq!(succeed(&lane).unwrap_err().kind(), ErrorKind::Unavailable, "paused again from the probe's failure");
    let down = lane.state("requests").unwrap();
    assert_eq!(down.down_since, Some(down_since), "down since the first failure that put it down");
    move_clock(&f, Duration::from_millis(1));
    assert_eq!(succeed(&lane).unwrap(), "answered");
}

// contract: LOCK-12
#[test]
fn a_success_resets_the_count_and_a_hold_that_said_its_service_is_healthy_is_no_failure() {
    let f = fixture();
    let lane = lane(&f);
    fail(&lane);
    succeed(&lane).unwrap();
    fail(&lane);
    assert!(lane.state("requests").unwrap().up, "not two in a row");
    for _ in 0..2 {
        let not_found = lane.hold("requests", |permit| {
            permit.healthy();
            Err::<(), _>(Error::not_found("no such student"))
        });
        assert_eq!(not_found.unwrap_err().kind(), ErrorKind::NotFound);
    }
    assert!(lane.state("requests").unwrap().up, "the caller's errors, not the service's");
    fail(&lane);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lane.hold("requests", |_| -> Result<()> { panic!("the holder's function panicked") })
    }));
    assert!(panicked.is_err());
    assert!(!lane.state("requests").unwrap().up, "a panic is a failed hold");
}

// contract: LOCK-12
#[test]
fn a_failure_a_pause_after_the_last_counts_as_first_and_old_failures_are_forgotten() {
    let f = fixture();
    let lane = lane(&f);
    fail(&lane);
    move_clock(&f, PAUSE);
    fail(&lane);
    let standing = lane.state("requests").unwrap();
    assert!(standing.up, "a pause apart: not in a row");
    assert_eq!(standing.last_failure, Some(f.clock.now()));
    move_clock(&f, PAUSE);
    assert_eq!(lane.state("requests").unwrap().last_failure, None, "forgotten a pause after");

    // keys that failed once and are never asked again: the memory they keep
    // stays within twice the keys that failed within a pause
    let locks = locks(&f.store);
    for round in 0..4 {
        for key in 0..1000 {
            lane.hold(format!("{round}/{key}").as_str(), |_| Err::<(), _>(bad_gateway())).unwrap_err();
        }
        move_clock(&f, PAUSE);
    }
    assert!(locks.breakers() <= 2000, "{} breakers kept for 4000 keys failed once", locks.breakers());
}

/// What a holder through a connection asks of the breaker's key.
#[cfg(feature = "wire")]
fn remote(max_hold: Option<Duration>, request: u8) -> Ask {
    Ask {
        place: Place::new(Kind::Semaphore, Arc::from("cabinet"), "requests".to_owned()),
        limit: 1,
        priority: Priority::Urgent,
        background: None,
        ttl: Some(10 * SECOND),
        max_hold,
        wait: Duration::ZERO,
        request: Some([request; 16]),
        breaker: Some(RULE),
        probe: false,
    }
}

#[cfg(feature = "wire")]
fn ask(locks: &Locks, ask: Ask) -> Result<Answer> {
    let (answered, answer) = mpsc::channel();
    locks.ask(ask, Box::new(move |answer| drop(answered.send(answer))));
    answer.recv_timeout(PATIENCE).unwrap()
}

#[cfg(feature = "wire")]
fn granted(answer: Result<Answer>) -> Granted {
    match answer {
        Ok(Answer::Granted(granted)) => granted,
        Ok(Answer::Busy) => panic!("busy where a grant was due"),
        Err(error) => panic!("refused where a grant was due: {error}"),
    }
}

// contract: LOCK-12, LOCK-13
#[cfg(feature = "wire")]
#[test]
fn a_hold_past_its_timeout_is_a_failure_and_one_whose_client_went_is_none() {
    let f = fixture();
    let lane = lane(&f);
    let locks = locks(&f.store);
    for request in 1..=2 {
        let gone = granted(ask(&locks, remote(None, request)));
        move_clock(&f, 10 * SECOND);
        holds_within(PATIENCE, "the lease ran out", || gone.watch.ending() == lease::Ending::Lost);
    }
    assert!(lane.state("requests").unwrap().up, "two clients went: the service did not fail");

    for request in 3..=4 {
        let hung = granted(ask(&locks, remote(Some(SECOND), request)));
        move_clock(&f, SECOND);
        holds_within(PATIENCE, "revoked at its timeout", || hung.watch.ending() == lease::Ending::Lost);
    }
    assert!(!lane.state("requests").unwrap().up, "two holds ran past their time");

    move_clock(&f, PAUSE);
    let probe = granted(ask(&locks, remote(None, 5)));
    assert_eq!(ask(&locks, remote(None, 6)).err().map(|error| error.kind()), Some(ErrorKind::Unavailable));
    move_clock(&f, 10 * SECOND);
    holds_within(PATIENCE, "the probe's lease ran out", || probe.watch.ending() == lease::Ending::Lost);
    let next = granted(ask(&locks, remote(None, 7)));
    assert!(locks.release(&next.lease, Outcome::Succeeded), "its client went: the next ask probes");
    assert!(lane.state("requests").unwrap().up);
}

// contract: LOCK-12, LOCK-13
#[cfg(feature = "wire")]
#[test]
fn an_ask_hears_what_the_holds_that_ended_before_it_said_though_no_timer_settled_them() {
    let f = fixture();
    let lane = lane(&f);
    let locks = locks(&f.store);
    // the timer sleeps a minute at most and so settles none of these before the next ask
    let hung = |request| granted(ask(&locks, Ask { limit: 2, ttl: Some(2 * HOUR), ..remote(Some(HOUR), request) }));
    let hung = [hung(1), hung(2)];
    f.clock.advance(HOUR);
    let refused = ask(&locks, remote(None, 3)).err().map(|error| error.kind());
    assert_eq!(refused, Some(ErrorKind::Unavailable), "the holds past their time put the key down before this ask");
    assert!(hung.iter().all(|hung| hung.watch.ending() == lease::Ending::Lost));

    f.clock.advance(PAUSE);
    let probe = granted(ask(&locks, Ask { ttl: Some(HOUR), ..remote(None, 4) }));
    f.clock.advance(HOUR);
    let next = granted(ask(&locks, remote(None, 5)));
    assert_eq!(probe.watch.ending(), lease::Ending::Lost, "the probe's lease ran out");
    assert!(locks.release(&next.lease, Outcome::Succeeded), "its client went: this ask probed");
    assert!(lane.state("requests").unwrap().up);
}

// contract: LOCK-12
#[cfg(feature = "wire")]
#[test]
fn a_hold_whose_lease_ran_out_before_its_time_is_no_failure_however_late_it_is_settled() {
    let f = fixture();
    let lane = lane(&f);
    let locks = locks(&f.store);
    let gone: Vec<_> =
        (1..=2).map(|request| granted(ask(&locks, Ask { limit: 2, ..remote(Some(HOUR), request) }))).collect();
    // past the ttl and the max_hold both, with no tick between
    f.clock.advance(HOUR);
    let standing = lane.state("requests").unwrap();
    assert!(gone.iter().all(|gone| gone.watch.ending() == lease::Ending::Lost));
    assert!(standing.up && standing.last_failure.is_none(), "their clients went first: {standing:?}");
}

// contract: LOCK-12
#[cfg(feature = "wire")]
#[test]
fn a_hold_asked_without_the_breaker_resets_no_count_as_an_older_client_s_release_says_no_failure() {
    let f = fixture();
    let lane = lane(&f);
    let locks = locks(&f.store);
    fail(&lane);
    let older = granted(ask(&locks, Ask { breaker: None, ..remote(None, 1) }));
    assert!(locks.release(&older.lease, Outcome::Succeeded));
    fail(&lane);
    assert!(!lane.state("requests").unwrap().up, "two failures in a row, the older client's hold not between");
}

// contract: LOCK-14
#[test]
fn a_key_without_a_breaker_is_up_and_says_how_many_wait_and_hold() {
    let f = fixture();
    let plain = f.store.semaphore("plain").limit(1).open().unwrap();
    let (go, holder) = {
        let (holds, held) = mpsc::channel();
        let (go, told) = mpsc::channel::<()>();
        let plain = plain.clone();
        let holder = thread::spawn(move || {
            plain.hold("requests", |_| {
                holds.send(()).unwrap();
                told.recv_timeout(PATIENCE).unwrap();
                Err::<(), _>(bad_gateway())
            })
        });
        held.recv_timeout(PATIENCE).unwrap();
        (go, holder)
    };
    let standing = plain.state("requests").unwrap();
    assert!(standing.up && standing.down_since.is_none() && standing.last_failure.is_none());
    assert_eq!((standing.waiting, standing.holding), (0, 1));
    go.send(()).unwrap();
    holder.join().unwrap().unwrap_err();
    assert!(plain.state("requests").unwrap().last_failure.is_none(), "no breaker keeps no failure");
}

#[test]
fn a_breaker_is_checked_and_kept_by_its_name() {
    let f = fixture();
    let open = |failures: u32, pause: Duration| f.store.semaphore("cabinet").limit(1).breaker(failures, pause).open();
    assert_eq!(open(0, PAUSE).unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(open(2, Duration::ZERO).unwrap_err().kind(), ErrorKind::Invalid);
    open(2, PAUSE).unwrap();
    let other = open(3, PAUSE).unwrap_err();
    assert!(other.to_string().contains("with a breaker of 2 failures and 30s, not 3 failures and 30s"), "{other}");
    let none = f.store.semaphore("cabinet").limit(1).open().unwrap_err();
    assert!(none.to_string().contains("not none"), "{none}");
}

/// Uncontended holds of a plain semaphore against one with a background and
/// a breaker, in turns in one run, the median of seven passes each:
/// `cargo test --release -p tinystore --lib --features testing
/// hold_and_release_with_and_without -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn hold_and_release_with_and_without_a_background_and_a_breaker() {
    const PAIRS: u32 = 100_000;
    let f = fixture();
    let plain = f.store.semaphore("plain").limit(8).open().unwrap();
    let shaped = f.store.semaphore("shaped").limit(8).background(4).breaker(2, PAUSE).open().unwrap();
    let pass = |lane: &Semaphore| {
        let started = std::time::Instant::now();
        for _ in 0..PAIRS {
            lane.hold("requests", |_| Ok::<_, Error>(())).unwrap();
        }
        started.elapsed()
    };
    let (mut plains, mut shapeds) = (Vec::new(), Vec::new());
    for _ in 0..7 {
        plains.push(pass(&plain));
        shapeds.push(pass(&shaped));
    }
    let median = |passes: &mut Vec<Duration>| {
        passes.sort();
        passes[passes.len() / 2] / PAIRS
    };
    println!("plain: {:?} a pair, background and breaker: {:?} a pair", median(&mut plains), median(&mut shapeds));
}
