use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::*;
use crate::kv::fixture::*;
use crate::testing::{PATIENCE, holds_within};

/// The order holders of a key were given their places, by name.
type Served = Arc<Mutex<Vec<&'static str>>>;

/// A hold on a thread of its own, which keeps its place until told.
struct Holder {
    go: mpsc::Sender<()>,
    thread: thread::JoinHandle<Result<()>>,
}

impl Holder {
    fn let_go(self) {
        self.go.send(()).unwrap();
        self.thread.join().unwrap().unwrap();
    }
}

/// Asks for `key` on a thread, in the background or not, and writes `name`
/// into `served` once it holds a place.
fn ask(lane: &Semaphore, name: &'static str, background: bool, served: &Served) -> Holder {
    let (go, told) = mpsc::channel::<()>();
    let (lane, served) = (lane.clone(), Arc::clone(served));
    let thread = thread::spawn(move || {
        let key = lane.key("requests").wait(MINUTE);
        let key = if background { key.background() } else { key };
        key.hold(|_| {
            served.lock().unwrap().push(name);
            told.recv_timeout(PATIENCE).unwrap();
            Ok(())
        })
    });
    Holder { go, thread }
}

fn locks(store: &Store) -> Arc<Locks> {
    Locks::of(store).unwrap()
}

fn served_now(served: &Served) -> Vec<&'static str> {
    served.lock().unwrap().clone()
}

// contract: LOCK-09
#[test]
fn an_urgent_hold_takes_the_first_place_freed_before_background_holds_that_came_first() {
    let f = fixture();
    let lane = f.store.semaphore("cabinet").limit(2).open().unwrap();
    let locks = locks(&f.store);
    let served = Served::default();
    let first = ask(&lane, "b1", true, &served);
    holds_within(PATIENCE, "b1 holds", || served_now(&served).len() == 1);
    let second = ask(&lane, "b2", true, &served);
    holds_within(PATIENCE, "b2 holds", || served_now(&served).len() == 2);
    let third = ask(&lane, "b3", true, &served);
    holds_within(PATIENCE, "b3 in line", || locks.waiting() == 1);
    let fourth = ask(&lane, "b4", true, &served);
    holds_within(PATIENCE, "b4 in line", || locks.waiting() == 2);
    let urgent = ask(&lane, "urgent", false, &served);
    holds_within(PATIENCE, "the urgent hold in line", || locks.waiting() == 3);

    first.let_go();
    holds_within(PATIENCE, "a place given", || served_now(&served).len() == 3);
    assert_eq!(served_now(&served), ["b1", "b2", "urgent"], "the first place freed goes to the urgent hold");
    second.let_go();
    holds_within(PATIENCE, "b3 holds", || served_now(&served).len() == 4);
    urgent.let_go();
    holds_within(PATIENCE, "b4 holds", || served_now(&served).len() == 5);
    assert_eq!(served_now(&served), ["b1", "b2", "urgent", "b3", "b4"], "background holds in the order they came");
    third.let_go();
    fourth.let_go();
}

// contract: LOCK-10
#[test]
fn background_holds_take_no_more_than_their_places_and_an_urgent_hold_finds_the_rest_at_once() {
    let f = fixture();
    let lane = f.store.semaphore("cabinet").limit(2).background(1).open().unwrap();
    let locks = locks(&f.store);
    let served = Served::default();
    let first = ask(&lane, "b1", true, &served);
    holds_within(PATIENCE, "b1 holds", || served_now(&served).len() == 1);
    let waiting: Vec<Holder> = ["b2", "b3", "b4"]
        .into_iter()
        .enumerate()
        .map(|(at, name)| {
            let holder = ask(&lane, name, true, &served);
            holds_within(PATIENCE, "in line", || locks.waiting() == at + 1);
            holder
        })
        .collect();
    assert_eq!(served_now(&served), ["b1"], "one place of two is free, and background holds have theirs");

    let urgent = lane.key("requests").try_hold(|permit| Ok::<_, Error>(permit.waited())).unwrap();
    assert!(urgent.is_some_and(|waited| waited < PATIENCE), "the urgent hold is given the free place at once");
    let tried = lane.key("requests").background().try_hold(|_| Ok::<_, Error>(())).unwrap();
    assert!(tried.is_none(), "a background hold that would not wait finds the background's place held");

    first.let_go();
    holds_within(PATIENCE, "b2 holds", || served_now(&served).len() == 2);
    for holder in waiting {
        holder.let_go();
    }
    assert_eq!(served_now(&served), ["b1", "b2", "b3", "b4"]);
}

// contract: LOCK-01
#[test]
fn a_waiting_background_hold_never_holds_an_urgent_one_back_but_an_urgent_one_holds_it_back() {
    let f = fixture();
    let lane = f.store.semaphore("cabinet").limit(1).open().unwrap();
    let locks = locks(&f.store);
    let served = Served::default();
    let first = ask(&lane, "u1", false, &served);
    holds_within(PATIENCE, "u1 holds", || served_now(&served).len() == 1);
    let background = ask(&lane, "b1", true, &served);
    holds_within(PATIENCE, "b1 in line", || locks.waiting() == 1);
    let second = ask(&lane, "u2", false, &served);
    holds_within(PATIENCE, "u2 in line", || locks.waiting() == 2);
    let third = ask(&lane, "u3", false, &served);
    holds_within(PATIENCE, "u3 in line", || locks.waiting() == 3);
    first.let_go();
    holds_within(PATIENCE, "u2 holds", || served_now(&served).len() == 2);
    second.let_go();
    holds_within(PATIENCE, "u3 holds", || served_now(&served).len() == 3);
    third.let_go();
    holds_within(PATIENCE, "b1 holds", || served_now(&served).len() == 4);
    background.let_go();
    assert_eq!(served_now(&served), ["u1", "u2", "u3", "b1"], "urgent holds in their order, then the background");
}

// contract: LOCK-11
#[test]
fn a_holds_timeout_counts_from_its_grant_and_fails_it_lost() {
    let f = fixture();
    let lane = f.store.semaphore("cabinet").limit(1).open().unwrap();
    let locks = locks(&f.store);
    let served = Served::default();
    let first = ask(&lane, "first", false, &served);
    holds_within(PATIENCE, "the first holds", || served_now(&served).len() == 1);
    let (stuck, told) = mpsc::channel::<()>();
    let (ran, running) = mpsc::channel();
    let timed = {
        let lane = lane.clone();
        thread::spawn(move || {
            lane.key("requests").wait(MINUTE).timeout(10 * SECOND).hold(|permit| {
                ran.send(()).unwrap();
                told.recv_timeout(PATIENCE).unwrap();
                Ok::<_, Error>(permit.lost())
            })
        })
    };
    holds_within(PATIENCE, "the timed hold in line", || locks.waiting() == 1);
    f.clock.advance(30 * SECOND);
    f.store.clock_moved();
    first.let_go();
    running.recv_timeout(PATIENCE).unwrap();

    f.clock.advance(10 * SECOND - Duration::from_millis(1));
    f.store.clock_moved();
    thread::sleep(Duration::from_millis(20));
    let full = lane.try_hold("requests", |_| Ok::<_, Error>(())).unwrap();
    assert!(full.is_none(), "a millisecond short of its timeout from the grant, though it waited 30 s before");
    f.clock.advance(Duration::from_millis(1));
    f.store.clock_moved();
    let next = lane.key("requests").wait(MINUTE).hold(|_| Ok::<_, Error>("the place went to the next"));
    assert_eq!(next.unwrap(), "the place went to the next");
    stuck.send(()).unwrap();
    let error = timed.join().unwrap().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Lost, "{error}");
    assert!(error.to_string().contains("its timeout"), "{error}");
}

#[test]
fn a_background_and_a_timeout_are_checked() {
    let f = fixture();
    let open = |limit: u32, places: u32| f.store.semaphore("cabinet").limit(limit).background(places).open();
    assert_eq!(open(8, 0).unwrap_err().kind(), ErrorKind::Invalid);
    let wider = open(8, 9).unwrap_err();
    assert!(wider.to_string().contains("a background of 9, not 1 to its limit 8"), "{wider}");
    let lane = open(8, 4).unwrap();
    let other = open(8, 2).unwrap_err();
    assert_eq!(other.kind(), ErrorKind::Invalid, "{other}");
    assert!(other.to_string().contains("open in this program with a background of 4, not 2"), "{other}");
    let none = f.store.semaphore("cabinet").limit(8).open().unwrap_err();
    assert!(none.to_string().contains("a background of 4, not none"), "{none}");
    let zero = lane.key("requests").timeout(Duration::ZERO).hold(|_| Ok::<_, Error>(()));
    assert_eq!(zero.unwrap_err().kind(), ErrorKind::Invalid);
}
