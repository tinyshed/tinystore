use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use super::fixture::{Client, answered};
use super::*;
use crate::wire::codec::{Message, Row};
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{
    Empty, Failure as Failed, Handle, Hello, KvAllowance, KvAnswer, KvBranch, KvCall, KvCheck, KvCount, KvCountersOpen,
    KvEntry, KvList, KvOnceOpen, KvOp, KvPage, KvQuotaOpen, KvTx, KvTxResults, KvWindow, KvWritten, METHODS,
    ServerClock, method,
};

#[test]
fn a_client_sets_and_gets_a_key_through_the_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = client.bucket("sessions");
    let token = KvCall { handle, under: vec!["7".to_owned()], key: "token".to_owned(), ..KvCall::default() };

    let set = KvCall { value: Some(Row::Bin(b"hello".to_vec())), ..token.clone() };
    let written: KvWritten = client.call(method::KV_SET, &set).unwrap();
    assert!(written.written);

    let entry: KvEntry = client.call(method::KV_GET, &token).unwrap();
    assert_eq!((entry.found, entry.value), (true, Some(Row::Bin(b"hello".to_vec()))));
    assert_eq!(entry.version, Some(written.version), "the version the set gave");
}

#[test]
fn every_method_of_the_schema_is_answered() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    for &(name, called) in METHODS {
        let stream = client.start(called, &Empty {});
        let answer = client.next_on(stream);
        if let Err(failed) = answered::<Empty>(&answer) {
            assert_ne!(failed.code, "unimplemented", "{name}: {}", failed.message);
        }
    }
}

#[test]
fn a_method_the_core_does_not_have_is_unimplemented() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let failed = client.call::<Empty>(0x0601, &Empty {}).unwrap_err();
    assert_eq!(failed.code, "unimplemented");
}

#[test]
fn a_field_the_core_does_not_know_is_unimplemented_and_named() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = client.bucket("sessions");
    let mut body = KvCall { handle, key: "a".to_owned(), ..KvCall::default() }.encode();
    body[0] += 1; // one more pair in the map: field 99, a nil
    body.extend_from_slice(&[99, 0xc0]);
    client.next_stream += 1;
    client.write(Frame { method: method::KV_GET, ..Frame::new(Kind::Request, client.next_stream, body) }.ending());
    let failed = answered::<Empty>(&client.next_on(client.next_stream)).unwrap_err();
    assert_eq!(failed.code, "unimplemented");
    assert!(failed.message.contains("field 99 of kv.Call"), "{}", failed.message);
}

#[test]
fn a_client_of_protocol_one_is_refused_with_goaway() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::connect(dir.path());
    let refused = client.hello(1).unwrap_err();
    assert_eq!(refused.code, "protocol");
}

#[test]
fn a_frame_before_hello_ends_the_pipe_with_goaway() {
    let dir = tempfile::tempdir().unwrap();
    let pipe = Pipe::open(dir.path(), None).unwrap();
    let mut bytes = Vec::new();
    Frame::new(Kind::Ping, 0, vec![0; 8]).encode_into(&mut bytes);
    let mut reader = frame::Reader::default();
    reader.push(&pipe.send(&bytes));
    assert_eq!(reader.next(1 << 20).unwrap().unwrap().kind, Kind::GoAway);
}

#[test]
fn a_run_of_once_is_handed_to_the_client_and_its_answer_kept_for_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let open = KvOnceOpen { name: "charges".to_owned(), ..KvOnceOpen::default() };
    let handle = client.call::<Handle>(method::KV_ONCE_OPEN, &open).unwrap().handle;
    let charge = KvCall { handle, key: "order-1".to_owned(), ..KvCall::default() };

    let first = client.run(&charge);
    let handed = client.next_on(first);
    assert_eq!((handed.kind, handed.flags), (Kind::Response, 0), "the run is the client's, its stream open");
    assert!(!KvAnswer::decode(&handed.body).unwrap().found);
    let second = client.run(&charge);

    let charged = KvAnswer { found: true, value: Some(Row::Int(42)) };
    client.write(Frame::new(Kind::Data, first, charged.encode()).ending());
    let kept = client.next_on(first);
    assert_eq!((kept.kind, kept.flags), (Kind::Data, frame::END));

    let waited = client.next_on(second);
    assert_eq!((waited.kind, waited.flags), (Kind::Response, frame::END), "the second caller ran nothing");
    assert_eq!(KvAnswer::decode(&waited.body).unwrap(), charged);
}

#[test]
fn a_run_that_failed_or_was_cancelled_hands_the_key_to_the_next_caller() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let open = KvOnceOpen { name: "charges".to_owned(), ..KvOnceOpen::default() };
    let handle = client.call::<Handle>(method::KV_ONCE_OPEN, &open).unwrap().handle;
    let charge = KvCall { handle, key: "order-1".to_owned(), ..KvCall::default() };

    let failed = client.run(&charge);
    assert_eq!(client.next_on(failed).flags, 0);
    client.write(Frame::new(Kind::Data, failed, KvAnswer::default().encode()).ending());
    assert_eq!(client.next_on(failed).flags, frame::END, "nothing kept");

    let cancelled = client.run(&charge);
    assert_eq!(client.next_on(cancelled).flags, 0, "handed again after the failure");
    client.write(Frame::new(Kind::Cancel, cancelled, Vec::new()));
    let ended = client.next_on(cancelled);
    assert_eq!((ended.kind, ended.flags), (Kind::Data, frame::END | frame::ERROR));
    assert_eq!(Failed::decode(&ended.body).unwrap().code, "cancelled");

    let third = client.run(&charge);
    assert_eq!(client.next_on(third).flags, 0, "handed again after the cancel");
}

#[test]
fn a_run_cancelled_while_it_waits_is_never_handed_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let open = KvOnceOpen { name: "charges".to_owned(), ..KvOnceOpen::default() };
    let handle = client.call::<Handle>(method::KV_ONCE_OPEN, &open).unwrap().handle;
    let charge = KvCall { handle, key: "order-1".to_owned(), ..KvCall::default() };

    let first = client.run(&charge);
    assert_eq!(client.next_on(first).flags, 0);
    let waiting = client.run(&charge);
    client.write(Frame::new(Kind::Cancel, waiting, Vec::new()));
    let cancelled = client.next_on(waiting);
    assert_eq!((cancelled.kind, cancelled.flags), (Kind::Response, frame::END | frame::ERROR));

    client.write(Frame::new(Kind::Data, first, KvAnswer::default().encode()).ending());
    assert_eq!(client.next_on(first).flags, frame::END, "nothing kept");
    let next = client.run(&charge);
    assert_eq!(client.next_on(next).flags, 0, "the key is the next caller's, not the cancelled one's");
}

#[test]
fn a_transaction_applies_its_writes_or_names_the_read_that_changed() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = client.bucket("stock");
    let key = |key: &str| KvCall { handle, key: key.to_owned(), ..KvCall::default() };
    let set = |name: &str, n: i64| KvOp {
        method: method::KV_SET.into(),
        call: KvCall { value: Some(Row::Int(n)), ..key(name) },
    };
    let read: KvWritten = client.call(method::KV_SET, &KvCall { value: Some(Row::Int(5)), ..key("apples") }).unwrap();

    let check = KvCheck { handle, key: "apples".to_owned(), version: Some(read.version.clone()), ..KvCheck::default() };
    let tx = KvTx { checks: vec![check.clone()], writes: vec![set("apples", 4), set("baskets", 1)] };
    let results: KvTxResults = client.call(method::KV_TX, &tx).unwrap();
    assert!(results.outcomes.iter().all(|outcome| outcome.written));

    let absent = KvCheck { handle, key: "pears".to_owned(), ..KvCheck::default() };
    let stale = KvTx { checks: vec![absent, check], writes: vec![set("apples", 3)] };
    let failed = client.call::<KvTxResults>(method::KV_TX, &stale).unwrap_err();
    assert_eq!(failed.code, "conflict");
    assert_eq!(failed.what.unwrap().get("check").map(String::as_str), Some("1"), "the second read changed");
    let apples: KvEntry = client.call(method::KV_GET, &key("apples")).unwrap();
    assert_eq!(apples.value, Some(Row::Int(4)), "nothing of the failed transaction was written");
}

/// A server's connection to a store it holds: its stops counted, its clock
/// a test clock at 1,790,000,000,000 ms, its proof the challenge reversed.
fn served(store: &Store, stops: &Arc<AtomicUsize>) -> Pipe {
    let stops = Arc::clone(stops);
    let connect = Connect {
        stop: Some(Arc::new(move || {
            stops.fetch_add(1, Ordering::SeqCst);
        })),
        clock: Some(Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_millis(1_790_000_000_000)))),
        prove: Some(Arc::new(|challenge: &[u8]| challenge.iter().rev().copied().collect())),
        ..Connect::default()
    };
    Pipe::connect(store, connect).unwrap()
}

#[test]
fn a_store_the_program_holds_answers_its_servers_calls() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let stops = Arc::new(AtomicUsize::new(0));
    let mut client = Client::over(served(&store, &stops));
    let challenge: Vec<u8> = (0..16).collect();
    let welcome = client.greet(Hello { protocol: 2, challenge: Some(challenge.clone()), ..Hello::default() }).unwrap();
    assert_eq!(welcome.proof, Some(challenge.iter().rev().copied().collect()), "the proof answers the challenge");

    let read: ServerClock = client.call(method::SERVER_CLOCK, &ServerClock::default()).unwrap();
    assert_eq!(read.at, Some(1_790_000_000_000));
    let moved: ServerClock = client.call(method::SERVER_CLOCK, &ServerClock { advance: Some(1500), at: None }).unwrap();
    assert_eq!(moved.at, Some(1_790_000_001_500));
    let back = ServerClock { at: Some(1_000), advance: None };
    assert_eq!(client.call::<ServerClock>(method::SERVER_CLOCK, &back).unwrap_err().code, "invalid", "never back");

    client.call::<Empty>(method::SERVER_STOP, &Empty {}).unwrap();
    assert_eq!(stops.load(Ordering::SeqCst), 1, "the stop runs once its answer has left");
    drop(client);
    store.close().unwrap();
}

#[test]
fn an_embedded_store_refuses_to_stop_and_has_no_clock_to_move() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    assert_eq!(client.call::<Empty>(method::SERVER_STOP, &Empty {}).unwrap_err().code, "permission");
    assert_eq!(client.call::<ServerClock>(method::SERVER_CLOCK, &ServerClock::default()).unwrap_err().code, "invalid");
}

#[test]
fn reads_sent_together_are_each_answered_though_workers_take_the_crowd() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = client.bucket("sessions");
    for n in 0..40 {
        let set = KvCall { handle, key: format!("k{n}"), value: Some(Row::Int(n)), ..KvCall::default() };
        client.call::<KvWritten>(method::KV_SET, &set).unwrap();
    }
    // forty gets in one write are a crowd: the workers answer them, in any order
    let mut bytes = Vec::new();
    for n in 0..40 {
        let get = KvCall { handle, key: format!("k{n}"), ..KvCall::default() };
        let frame = Frame { method: method::KV_GET, ..Frame::new(Kind::Request, 1000 + n, get.encode()) };
        frame.ending().encode_into(&mut bytes);
    }
    let ready = client.pipe.send(&bytes);
    client.reader.push(&ready);
    for n in 0..40 {
        let got: KvEntry = answered(&client.next_on(1000 + n)).unwrap();
        assert_eq!((got.found, got.value), (true, Some(Row::Int(i64::from(n)))), "k{n}");
    }
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
}

#[test]
fn a_challenge_of_another_length_is_a_hello_the_server_cannot_take() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::connect(dir.path());
    let refused = client.greet(Hello { protocol: 2, challenge: Some(vec![1; 8]), ..Hello::default() }).unwrap_err();
    assert_eq!(refused.code, "protocol");
}

#[test]
fn a_closing_server_says_goaway_and_answers_a_later_request_unavailable_unrun() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let mut client = Client::over(served(&store, &Arc::new(AtomicUsize::new(0))));
    client.hello(2).unwrap();
    let handle = client.bucket("sessions");
    assert_eq!(client.pipe.streams(), 0);

    client.pipe.go_away();
    assert_eq!(client.read().kind, Kind::GoAway);
    let set = KvCall { handle, key: "k".to_owned(), value: Some(Row::Int(1)), ..KvCall::default() };
    assert_eq!(client.call::<KvWritten>(method::KV_SET, &set).unwrap_err().code, "unavailable");
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
    drop(client);

    let mut again = Client::over(served(&store, &Arc::new(AtomicUsize::new(0))));
    again.hello(2).unwrap();
    let handle = again.bucket("sessions");
    let get = KvCall { handle, key: "k".to_owned(), ..KvCall::default() };
    assert!(!again.call::<KvEntry>(method::KV_GET, &get).unwrap().found, "the refused set never ran");
    drop(again);
    store.close().unwrap();
}

#[test]
fn pipes_on_one_directory_share_its_store_until_the_last_closes() {
    let dir = tempfile::tempdir().unwrap();
    let first = Client::open(dir.path());
    let second = Client::open(dir.path());
    assert!(Store::open(dir.path(), Options::default()).is_err(), "the pipes' store holds the LOCK");
    drop(first);
    assert!(Store::open(dir.path(), Options::default()).is_err(), "one pipe still holds it");
    drop(second);
    Store::open(dir.path(), Options::default()).unwrap().close().unwrap();
}

/// Two thousand sets in flight at once through a pipe, with no host language:
/// where the time goes when the Bun numbers disappoint. Run by hand:
/// cargo test --release -p tinystore pipe::tests::sets_in_flight -- --ignored --nocapture
#[test]
#[ignore = "a timing, not a check"]
fn sets_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = client.bucket("smoke");
    for pass in 0..5 {
        let started = std::time::Instant::now();
        let mut bytes = Vec::new();
        for n in 0..2000u32 {
            let set = KvCall {
                handle,
                key: format!("p{pass}-{n}"),
                value: Some(Row::Bin(vec![b'x'; 100])),
                ..KvCall::default()
            };
            Frame { method: method::KV_SET, ..Frame::new(Kind::Request, n + 1, set.encode()) }
                .ending()
                .encode_into(&mut bytes);
        }
        client.reader.push(&client.pipe.send(&bytes));
        let mut answered = 0;
        while answered < 2000 {
            match client.reader.next(1 << 22).unwrap() {
                Some(frame) if frame.kind == Kind::Response => answered += 1,
                Some(_) => {}
                None => {
                    let ready = client.pipe.recv(Duration::from_secs(10));
                    client.reader.push(&ready);
                }
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        println!("pass {pass}: {:.0} sets/s", 2000.0 / seconds);
    }
}

#[test]
fn writes_of_kv_in_flight_hold_no_thread_of_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let mut client = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
    client.hello(2).unwrap();
    let sessions = client.bucket("sessions");
    let hits = KvCountersOpen { name: "hits".to_owned(), ..KvCountersOpen::default() };
    let hits = client.call::<Handle>(method::KV_COUNTERS_OPEN, &hits).unwrap().handle;
    let window = KvWindow { name: "minute".to_owned(), limit: 100, per: 60_000 };
    let uses = KvQuotaOpen { name: "uses".to_owned(), windows: vec![window] };
    let uses = client.call::<Handle>(method::KV_QUOTA_OPEN, &uses).unwrap().handle;
    let held = store.bucket::<i64>("held").open().unwrap();
    let (began, begun) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();

    std::thread::scope(|scope| {
        // a transaction of kv.db holds its writer, so that no write can commit
        let (store, held) = (&store, &held);
        let holding = scope.spawn(move || {
            store.tx(|tx| -> crate::Result<()> {
                tx.with(held).set("k", &1)?;
                began.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
        });
        begun.recv().unwrap();
        let key = |handle, n: usize| KvCall { handle, key: format!("k{n}"), ..KvCall::default() };
        let adds: Vec<u32> = (0..20).map(|n| client.start(method::KV_COUNTERS_ADD, &key(hits, n))).collect();
        let allows: Vec<u32> = (0..10).map(|n| client.start(method::KV_ALLOW, &key(uses, n))).collect();
        let txs: Vec<u32> = (0..10)
            .map(|n| {
                let set = KvCall { value: Some(Row::Int(1)), ..key(sessions, n) };
                let writes = vec![KvOp { method: u64::from(method::KV_SET), call: set }];
                client.start(method::KV_TX, &KvTx { checks: Vec::new(), writes })
            })
            .collect();
        let clear = client.start(method::KV_CLEAR, &KvBranch { handle: sessions, under: vec!["gone".to_owned()] });
        // more writes wait than the session has threads, and a call that needs one is still answered
        let page: KvPage = client.call(method::KV_LIST, &KvList { handle: sessions, ..KvList::default() }).unwrap();
        assert!(page.entries.is_empty(), "nothing is committed yet");
        release.send(()).unwrap();
        holding.join().unwrap().unwrap();
        for stream in adds {
            assert_eq!(answered::<KvCount>(&client.next_on(stream)).unwrap().value, 1);
        }
        for stream in allows {
            assert!(answered::<KvAllowance>(&client.next_on(stream)).unwrap().ok);
        }
        for stream in txs {
            assert_eq!(answered::<KvTxResults>(&client.next_on(stream)).unwrap().outcomes.len(), 1);
        }
        answered::<Empty>(&client.next_on(clear)).unwrap();
    });
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
    drop(client);
    store.close().unwrap();
}

#[test]
fn a_host_reading_into_its_own_bytes_gets_the_stream_in_order_and_a_wake_for_what_became_ready() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let wakes = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&wakes);
    let wake: Wake = std::sync::Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let pipe = Pipe::connect(&store, Connect { wake: Some(wake), ..Connect::default() }).unwrap();
    let mut hello = Vec::new();
    let said = Hello { protocol: 2, client: "test/0".to_owned(), ..Hello::default() };
    Frame::new(Kind::Hello, 0, said.encode()).encode_into(&mut hello);

    // given no room, a send leaves the WELCOME to the read, and the host is woken for it
    assert_eq!(pipe.send_into(&hello, &mut []), 0);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);

    // five bytes at a time: a read that fills its room says more may wait, and no wake does
    let (mut welcome, mut piece) = (Vec::new(), [0u8; 5]);
    loop {
        let read = pipe.recv_into(std::time::Duration::ZERO, &mut piece);
        welcome.extend_from_slice(&piece[..read]);
        if read < piece.len() {
            break;
        }
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 1, "what a read left wakes nobody");
    let mut reader = frame::Reader::default();
    reader.push(&welcome);
    assert_eq!(reader.next(1 << 16).unwrap().expect("the frame came whole").kind, Kind::Welcome);

    // read to its end, the stream wakes the host for the next frame
    let mut ping = Vec::new();
    Frame::new(Kind::Ping, 0, vec![7; 8]).encode_into(&mut ping);
    assert_eq!(pipe.send_into(&ping, &mut []), 0);
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    let mut pong = [0u8; 64];
    assert_eq!(pipe.recv_into(std::time::Duration::ZERO, &mut pong), 20, "a PONG, whole");
    drop(pipe);
    store.close().unwrap();
}
