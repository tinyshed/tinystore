use std::time::Duration;

use crate::pipe::Capability;
use crate::pipe::fixture::{Client, answered};
use crate::wire::frame::Kind;
use crate::wire::protocol::{
    Empty, Failure, LocksHeld, LocksHold, LocksLease, LocksRenew, LocksState, LocksStateAsked, method,
};
use crate::{Options, Store};

fn hold(key: &str, wait: u64, request: u8) -> LocksHold {
    LocksHold {
        kind: "lock".to_owned(),
        name: "invoices".to_owned(),
        key: key.to_owned(),
        limit: 1,
        ttl: 10_000,
        max_hold: None,
        wait,
        request: vec![request; 16],
        ..Default::default()
    }
}

fn renew(client: &mut Client, lease: &[u8]) -> Result<Empty, Failure> {
    client.call(method::LOCKS_RENEW, &LocksRenew { lease: lease.to_vec(), ttl: 10_000 })
}

fn release(client: &mut Client, lease: &[u8]) {
    client.call::<Empty>(method::LOCKS_RELEASE, &LocksLease { lease: lease.to_vec(), failed: false }).unwrap();
}

#[test]
fn a_hold_answers_once_the_key_is_given_and_a_hold_that_would_not_wait_says_it_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let first: LocksHeld = client.call(method::LOCKS_HOLD, &hold("42", 0, 1)).unwrap();
    assert!(first.acquired);
    assert_eq!(first.lease.len(), 16);
    let fence: u128 = first.fence.parse().unwrap();
    assert!(fence >> 64 > 0, "the epoch in the high bits: {fence}");

    let busy: LocksHeld = client.call(method::LOCKS_HOLD, &hold("42", 0, 2)).unwrap();
    assert!(!busy.acquired && busy.lease.is_empty(), "held, and the hold would not wait");

    let waiting = client.start(method::LOCKS_HOLD, &hold("42", 60_000, 3));
    assert!(client.silent_on(waiting, Duration::from_millis(50)), "in line while the key is held");
    renew(&mut client, &first.lease).unwrap();
    release(&mut client, &first.lease);
    let second: LocksHeld = answered(&client.next_on(waiting)).unwrap();
    assert!(second.acquired);
    assert!(second.fence.parse::<u128>().unwrap() > fence);
    assert_eq!(renew(&mut client, &first.lease).unwrap_err().code, "lost", "the first lease is gone");
    release(&mut client, &first.lease);
    assert!(!client.call::<LocksHeld>(method::LOCKS_HOLD, &hold("42", 0, 4)).unwrap().acquired, "nor let go by it");
}

// contract: LOCK-06
#[test]
fn a_cancelled_hold_leaves_the_line_and_is_answered_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let first: LocksHeld = client.call(method::LOCKS_HOLD, &hold("42", 0, 1)).unwrap();
    let waiting = client.start(method::LOCKS_HOLD, &hold("42", 60_000, 2));
    assert!(client.silent_on(waiting, Duration::from_millis(50)));
    client.write(crate::wire::frame::Frame::new(Kind::Cancel, waiting, Vec::new()));
    assert_eq!(answered::<LocksHeld>(&client.next_on(waiting)).unwrap_err().code, "cancelled");
    release(&mut client, &first.lease);
    let free: LocksHeld = client.call(method::LOCKS_HOLD, &hold("42", 0, 3)).unwrap();
    assert!(free.acquired, "the cancelled hold took nothing");
}

#[test]
fn a_lease_outlives_its_connection_and_its_request_asked_again_answers_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let mut first = Client::admitted(&store, Capability::Data);
    let held: LocksHeld = first.call(method::LOCKS_HOLD, &hold("42", 0, 9)).unwrap();
    drop(first);

    let mut again = Client::admitted(&store, Capability::Data);
    renew(&mut again, &held.lease).expect("renewed by its id from another connection");
    let asked_again: LocksHeld = again.call(method::LOCKS_HOLD, &hold("42", 0, 9)).unwrap();
    assert_eq!((asked_again.lease, asked_again.fence), (held.lease.clone(), held.fence), "the same lease");
    release(&mut again, &held.lease);
    assert_eq!(renew(&mut again, &[0; 16]).unwrap_err().code, "lost", "a lease nobody was given");

    let mut reading = Client::admitted(&store, Capability::Read);
    let refused = reading.call::<LocksHeld>(method::LOCKS_HOLD, &hold("42", 0, 5)).unwrap_err();
    assert_eq!(refused.code, "permission", "holding a key writes");
}

#[test]
fn a_semaphore_held_over_the_wire_has_no_fence_and_names_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let uploads = LocksHold { kind: "semaphore".to_owned(), name: "uploads".to_owned(), limit: 2, ..hold("ann", 0, 1) };
    let first: LocksHeld = client.call(method::LOCKS_HOLD, &uploads).unwrap();
    let second: LocksHeld =
        client.call(method::LOCKS_HOLD, &LocksHold { request: vec![2; 16], ..uploads.clone() }).unwrap();
    let third: LocksHeld = client.call(method::LOCKS_HOLD, &LocksHold { request: vec![3; 16], ..uploads }).unwrap();
    assert_eq!((first.acquired, second.acquired, third.acquired), (true, true, false));
    assert!(first.fence.is_empty(), "two holders at once share no order");

    let refused =
        |client: &mut Client, hold: &LocksHold| client.call::<LocksHeld>(method::LOCKS_HOLD, hold).unwrap_err().code;
    assert_eq!(refused(&mut client, &LocksHold { limit: 2, ..hold("42", 0, 7) }), "invalid", "a lock of limit 2");
    assert_eq!(refused(&mut client, &LocksHold { ttl: 500, ..hold("42", 0, 7) }), "invalid", "a ttl under 1 s");
    assert_eq!(refused(&mut client, &LocksHold { name: "Invoices".to_owned(), ..hold("42", 0, 7) }), "invalid");
    assert_eq!(refused(&mut client, &LocksHold { request: vec![1; 3], ..hold("42", 0, 7) }), "invalid");
}

/// A hold of a semaphore of two places, one of them for background holds.
fn cabinet(request: u8, priority: &str) -> LocksHold {
    LocksHold {
        kind: "semaphore".to_owned(),
        name: "cabinet".to_owned(),
        limit: 2,
        background: 1,
        priority: priority.to_owned(),
        ..hold("requests", 60_000, request)
    }
}

// contract: LOCK-09
// contract: LOCK-10
#[test]
fn an_urgent_hold_over_the_wire_passes_the_background_ones_which_keep_to_their_places() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let first: LocksHeld = client.call(method::LOCKS_HOLD, &cabinet(1, "background")).unwrap();
    assert!(first.acquired);
    let second = client.start(method::LOCKS_HOLD, &cabinet(2, "background"));
    assert!(client.silent_on(second, Duration::from_millis(50)), "background holds have one place of two");
    let urgent: LocksHeld = client.call(method::LOCKS_HOLD, &cabinet(3, "")).unwrap();
    assert!(urgent.acquired, "an urgent hold takes the place left, past the background one waiting");
    let later = client.start(method::LOCKS_HOLD, &cabinet(4, "urgent"));
    release(&mut client, &first.lease);
    let given: LocksHeld = answered(&client.next_on(later)).unwrap();
    assert!(given.acquired, "the place freed goes to the urgent hold that came after the background one");
    assert!(client.silent_on(second, Duration::from_millis(50)));
    release(&mut client, &urgent.lease);
    let background: LocksHeld = answered(&client.next_on(second)).unwrap();
    assert!(background.acquired);

    let refused =
        |client: &mut Client, hold: &LocksHold| client.call::<LocksHeld>(method::LOCKS_HOLD, hold).unwrap_err().code;
    let wider = LocksHold { background: 3, request: vec![8; 16], ..cabinet(8, "") };
    assert_eq!(refused(&mut client, &wider), "invalid", "a background past the limit");
    assert_eq!(refused(&mut client, &cabinet(9, "later")), "invalid", "a priority of neither");
}

/// A hold of a semaphore of one place, down 30 s after two failed holds.
fn breaking(request: u8) -> LocksHold {
    LocksHold { failures: 2, pause: Some(30_000), ..cabinet(request, "") }
}

fn state(client: &mut Client, kind: &str) -> Result<LocksState, Failure> {
    let asked = LocksStateAsked { kind: kind.to_owned(), name: "cabinet".to_owned(), key: "requests".to_owned() };
    client.call(method::LOCKS_STATE, &asked)
}

// contract: LOCK-12, LOCK-14, LOCK-15
#[test]
fn releases_that_say_their_holds_failed_put_the_key_down_and_its_state_says_since_when() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let up = state(&mut client, "semaphore").unwrap();
    assert!(up.up && up.down_since.is_none() && up.waiting == 0 && up.holding == 0, "{up:?}");
    for request in 1..=2 {
        let held: LocksHeld = client.call(method::LOCKS_HOLD, &breaking(request)).unwrap();
        let standing = state(&mut client, "semaphore").unwrap();
        assert_eq!(standing.holding, 1);
        let failed = LocksLease { lease: held.lease, failed: true };
        client.call::<Empty>(method::LOCKS_RELEASE, &failed).unwrap();
        assert_eq!(state(&mut client, "semaphore").unwrap().failures, u64::from(request), "failures in a row");
    }
    let refused = client.call::<LocksHeld>(method::LOCKS_HOLD, &breaking(3)).unwrap_err();
    assert_eq!(refused.code, "unavailable", "{refused:?}");
    let down = state(&mut client, "semaphore").unwrap();
    assert!(!down.up, "{down:?}");
    assert!(down.down_since.is_some() && down.down_since == down.last_failure, "{down:?}");

    let hold = |client: &mut Client, asked: &LocksHold| client.call::<LocksHeld>(method::LOCKS_HOLD, asked);
    let alone = LocksHold { pause: None, ..breaking(4) };
    assert_eq!(hold(&mut client, &alone).unwrap_err().code, "invalid", "failures without a pause");
    assert_eq!(state(&mut client, "mutex").unwrap_err().code, "invalid", "neither a lock nor a semaphore");
}
