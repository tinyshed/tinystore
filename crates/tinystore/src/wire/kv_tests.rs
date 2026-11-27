use std::time::Duration;

use crate::kv::fixture::{Fixture, fixture};
use crate::pipe::Capability;
use crate::pipe::fixture::Client;
use crate::wire::codec::{Message, Row};
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{
    Empty, Handle, KvAnswer, KvBranch, KvBucketOpen, KvCall, KvCount, KvOnceOpen, KvResealed, KvWritten, method,
};

// contract: KV-15
#[test]
fn a_count_over_the_wire_is_the_branchs_own_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let open = KvBucketOpen { name: "sessions".to_owned(), ..KvBucketOpen::default() };
    let handle = client.call::<Handle>(method::KV_BUCKET_OPEN, &open).unwrap().handle;
    for (under, key) in [(vec![], "a"), (vec!["42".to_owned()], "phone"), (vec!["42".to_owned()], "laptop")] {
        let set = KvCall { handle, under, key: key.to_owned(), ..KvCall::default() };
        client.call::<KvWritten>(method::KV_SET, &set).unwrap();
    }
    let mut count = |under: Vec<String>| client.call::<KvCount>(method::KV_COUNT, &KvBranch { handle, under }).unwrap();
    assert_eq!(count(vec![]).value, 1, "the root's own key, not its branch's");
    assert_eq!(count(vec!["42".to_owned()]).value, 2);
    assert_eq!(count(vec!["7".to_owned()]).value, 0);
}

/// A client of a store on the kv fixture's clock, and the once `books` it
/// opened as a client that reads age and refresh does, kept twelve hours.
fn books(f: &Fixture, fresh: Option<u64>) -> (Client, u64) {
    let mut client = Client::admitted(&f.store, Capability::Data);
    let open =
        KvOnceOpen { name: "books".to_owned(), keep: Some(12 * 3_600_000), fresh, aged: true, ..KvOnceOpen::default() };
    let handle = client.call::<Handle>(method::KV_ONCE_OPEN, &open).unwrap().handle;
    (client, handle)
}

fn book(n: i64) -> KvAnswer {
    KvAnswer { found: true, value: Some(Row::Int(n)), ..KvAnswer::default() }
}

/// Runs a key handed over to its end, the client answering `answer`.
fn built(client: &mut Client, call: &KvCall, answer: &KvAnswer) {
    let stream = client.run(call);
    assert_eq!(client.next_on(stream).flags, 0, "handed over");
    client.write(Frame::new(Kind::Data, stream, answer.encode()).ending());
    assert_eq!(client.next_on(stream).flags, frame::END);
}

// contract: KV-16, KV-17, KV-18
#[test]
fn a_stale_answer_comes_with_a_build_in_the_background_whose_failure_keeps_it() {
    let f = fixture();
    let (mut client, handle) = books(&f, Some(600_000));
    let call = KvCall { handle, key: "user-7".to_owned(), ..KvCall::default() };
    built(&mut client, &call, &book(1));
    f.clock.advance(Duration::from_mins(11));

    let stale = client.run(&call);
    let offered = client.next_on(stale);
    assert_eq!((offered.kind, offered.flags), (Kind::Response, 0), "the stream stays open for the new answer");
    let refresh = KvAnswer { age: Some(660_000), stale: true, refresh: true, ..book(1) };
    assert_eq!(KvAnswer::decode(&offered.body).unwrap(), refresh);

    let beside = client.run(&call);
    let answered = client.next_on(beside);
    assert_eq!(answered.flags, frame::END, "a stale answer beside the build waits for nothing");
    assert_eq!(KvAnswer::decode(&answered.body).unwrap(), KvAnswer { refresh: false, ..refresh.clone() });

    client.write(Frame::new(Kind::Data, stale, KvAnswer::default().encode()).ending());
    assert_eq!(client.next_on(stale).flags, frame::END, "the build failed");
    assert_eq!(client.call::<KvAnswer>(method::KV_ONCE_GET, &call).unwrap(), book(1), "the stale answer stays");
    let paused = client.run(&call);
    assert_eq!(client.next_on(paused).flags, frame::END, "no build within the pause");

    f.clock.advance(Duration::from_mins(1));
    let again = client.run(&call);
    assert_eq!(client.next_on(again).flags, 0, "the pause over, the next build");
    client.write(Frame::new(Kind::Data, again, book(2).encode()).ending());
    assert_eq!(client.next_on(again).flags, frame::END);
    let renewed = client.run(&call);
    let answered = client.next_on(renewed);
    assert_eq!(KvAnswer::decode(&answered.body).unwrap(), KvAnswer { age: Some(0), ..book(2) }, "both terms again");
}

// contract: KV-11
#[test]
fn a_handle_opened_as_an_older_client_does_is_never_handed_a_build_in_the_background() {
    let f = fixture();
    let mut client = Client::admitted(&f.store, Capability::Data);
    let open = KvOnceOpen { name: "books".to_owned(), ..KvOnceOpen::default() };
    let handle = client.call::<Handle>(method::KV_ONCE_OPEN, &open).unwrap().handle;
    let call = KvCall { handle, key: "user-7".to_owned(), ..KvCall::default() };
    built(&mut client, &call, &book(1));
    f.clock.advance(Duration::from_hours(23));
    let kept = client.run(&call);
    let answered = client.next_on(kept);
    assert_eq!(answered.flags, frame::END);
    assert_eq!(KvAnswer::decode(&answered.body).unwrap(), book(1), "neither age, stale nor refresh");
}

#[test]
fn a_fresh_as_long_as_keep_is_refused_over_the_wire() {
    let f = fixture();
    let mut client = Client::admitted(&f.store, Capability::Data);
    let open = KvOnceOpen {
        name: "books".to_owned(),
        keep: Some(60_000),
        fresh: Some(60_000),
        aged: true,
        ..KvOnceOpen::default()
    };
    let refused = client.call::<Handle>(method::KV_ONCE_OPEN, &open).unwrap_err();
    assert_eq!(refused.code, "invalid", "{refused:?}");
    assert!(refused.message.contains("kv once books"), "{refused:?}");
}

// contract: KV-19
#[test]
fn a_clear_over_the_wire_takes_once_answers() {
    let f = fixture();
    let (mut client, handle) = books(&f, None);
    let under = vec!["course-1".to_owned()];
    let call = KvCall { handle, under: under.clone(), key: "user-7".to_owned(), ..KvCall::default() };
    built(&mut client, &call, &book(1));
    client.call::<Empty>(method::KV_CLEAR, &KvBranch { handle, under }).unwrap();
    assert!(!client.call::<KvAnswer>(method::KV_ONCE_GET, &call).unwrap().found, "cleared");
}

// contract: KV-21, SEC-03
#[test]
fn once_opened_encrypted_over_the_wire_keeps_its_answers_sealed_and_a_read_connection_opens_none() {
    let f = fixture();
    let mut client = Client::admitted(&f.store, Capability::Data);
    let open = KvOnceOpen { name: "books".to_owned(), encrypted: true, ..KvOnceOpen::default() };
    let handle = client.call::<Handle>(method::KV_ONCE_OPEN, &open).unwrap().handle;
    built(&mut client, &KvCall { handle, key: "user-7".to_owned(), ..KvCall::default() }, &book(7));
    let sealed = f.store.once::<i64>("books").encrypted().open().unwrap();
    assert_eq!(sealed.get("user-7").unwrap(), Some(7), "the core's own handle opens what the client kept");
    let resealed: KvResealed = client.call(method::KV_RESEAL, &KvBranch { handle, under: vec![] }).unwrap();
    assert_eq!(resealed.count, 0, "sealed with the store's own key already");

    let plain = KvOnceOpen { name: "books".to_owned(), ..KvOnceOpen::default() };
    let refused = client.call::<Handle>(method::KV_ONCE_OPEN, &plain).unwrap_err();
    assert_eq!(refused.code, "invalid", "a name keeps whether it is encrypted: {refused:?}");
    let mut reader = Client::admitted(&f.store, Capability::Read);
    let refused = reader.call::<Handle>(method::KV_ONCE_OPEN, &open).unwrap_err();
    assert_eq!(refused.code, "permission", "{refused:?}");
}
