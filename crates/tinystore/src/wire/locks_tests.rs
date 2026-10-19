use std::time::Duration;

use crate::pipe::Capability;
use crate::pipe::fixture::{Client, answered};
use crate::wire::frame::Kind;
use crate::wire::protocol::{Empty, Failure, LocksHeld, LocksHold, LocksLease, LocksRenew, method};
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
    }
}

fn renew(client: &mut Client, lease: &[u8]) -> Result<Empty, Failure> {
    client.call(method::LOCKS_RENEW, &LocksRenew { lease: lease.to_vec(), ttl: 10_000 })
}

fn release(client: &mut Client, lease: &[u8]) {
    client.call::<Empty>(method::LOCKS_RELEASE, &LocksLease { lease: lease.to_vec() }).unwrap();
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
