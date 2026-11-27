use crate::pipe::fixture::{Client, answered};
use crate::pipe::{Connect, Pipe};
use crate::wire::codec::{Message, Row};
use crate::wire::frame::{self, END, ERROR, Frame, Kind, MORE};
use crate::wire::protocol::{Failure, Hello, KvCall, KvEntry, KvWritten, method};
use crate::{Options, Store};

/// A body of 64 KiB, so that a value of a few hundred KiB goes in pieces.
const BODY: u64 = 64 << 10;

/// A client whose HELLO takes bodies of 64 KiB, and messages of `most` in
/// pieces when given, of a store opened in `dir`.
fn connect(store: &Store, most: Option<u64>) -> Client {
    let mut client = Client::over(Pipe::connect(store, Connect::default()).unwrap());
    let hello = Hello { protocol: 1, max_body: Some(BODY), max_message: most, ..Hello::default() };
    let welcome = client.greet(hello).unwrap();
    assert_eq!(welcome.max_body, BODY);
    assert_eq!(welcome.max_message, most.map(|most| most.min(64 << 20)));
    client
}

fn store(dir: &std::path::Path) -> Store {
    Store::open(dir, Options { background: false, ..Options::default() }).unwrap()
}

fn set(handle: u64, key: &str, value: Vec<u8>) -> KvCall {
    KvCall { handle, key: key.to_owned(), value: Some(Row::Bin(value)), ..KvCall::default() }
}

fn get(handle: u64, key: &str) -> KvCall {
    KvCall { handle, key: key.to_owned(), ..KvCall::default() }
}

/// Sends `request` on `stream` as `called`, in pieces of the agreed body.
fn send_in_pieces(client: &mut Client, stream: u32, called: u16, request: &impl Message) {
    let message = Frame { method: called, ..Frame::new(Kind::Request, stream, request.encode()) }.ending();
    for piece in message.pieces(BODY as usize) {
        client.write(piece);
    }
}

/// The frames of `stream` up to its last, the others' kept for later.
fn frames_of(client: &mut Client, stream: u32) -> Vec<Frame> {
    let mut frames = Vec::new();
    loop {
        let frame = client.next_on(stream);
        let last = frame.flags & END != 0;
        frames.push(frame);
        if last {
            return frames;
        }
    }
}

/// The message the frames of one answer join into.
fn joined(frames: &[Frame]) -> Frame {
    let (last, pieces) = frames.split_last().unwrap();
    assert!(pieces.iter().all(|piece| piece.flags == MORE && piece.kind == last.kind));
    Frame { body: frames.iter().flat_map(|frame| frame.body.clone()).collect(), ..last.clone() }
}

// contract: WIRE-13
#[test]
fn a_request_and_an_answer_past_the_body_travel_in_pieces_joined_into_one_message() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut client = connect(&store, Some(1 << 20));
    let handle = client.bucket("files");
    let value: Vec<u8> = (0..300_000u32).map(|at| at as u8).collect();

    send_in_pieces(&mut client, 1, method::KV_SET, &set(handle, "k", value.clone()));
    let written = frames_of(&mut client, 1);
    assert_eq!(written.len(), 1, "a small answer is one frame");
    answered::<KvWritten>(&written[0]).unwrap();

    let stream = client.start(method::KV_GET, &get(handle, "k"));
    let answer = frames_of(&mut client, stream);
    assert!(answer.len() >= 5, "300 KB in bodies of 64 KiB is five pieces at least, not {}", answer.len());
    assert!(answer.iter().all(|piece| piece.kind == Kind::Response && piece.body.len() as u64 <= BODY));
    let entry = answered::<KvEntry>(&joined(&answer)).unwrap();
    assert_eq!(entry.value, Some(Row::Bin(value)));
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
    drop(client);
    store.close().unwrap();
}

// contract: WIRE-16
#[test]
fn an_answer_in_pieces_shares_the_connection_with_other_streams() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut client = connect(&store, Some(4 << 20));
    let handle = client.bucket("files");
    for key in ["z", "a", "b"] {
        send_in_pieces(&mut client, 1, method::KV_SET, &set(handle, key, vec![7; 1000 << 10]));
        answered::<KvWritten>(&frames_of(&mut client, 1)[0]).unwrap();
    }
    // z's pieces fill what the connection owes past its low mark, so that
    // a's and b's wait together for the host's take
    let mut bytes = Vec::new();
    for (stream, key) in [(10, "z"), (11, "a"), (12, "b"), (13, "none")] {
        let request = Frame { method: method::KV_GET, ..Frame::new(Kind::Request, stream, get(handle, key).encode()) };
        request.ending().encode_into(&mut bytes);
    }
    let mut reader = frame::Reader::default();
    reader.push(&client.pipe.send(&bytes));
    let mut order = Vec::new();
    while order.iter().filter(|(_, flags)| flags & END != 0).count() < 4 {
        match reader.next(1 << 22).unwrap() {
            Some(frame) if frame.kind == Kind::Response => order.push((frame.stream, frame.flags)),
            Some(_) => {}
            None => reader.push(&client.pipe.recv(std::time::Duration::from_secs(10))),
        }
    }
    let at = |wanted: &dyn Fn(&(u32, u8)) -> bool| order.iter().position(wanted).unwrap();
    let a_ends = at(&|&(stream, flags)| stream == 11 && flags & END != 0);
    let b_starts = at(&|&(stream, _)| stream == 12);
    let small = at(&|&(stream, _)| stream == 13);
    assert!(small < a_ends, "a small answer goes out before a large one's last piece: {order:?}");
    assert!(b_starts < a_ends, "two large answers go out a piece each in turn: {order:?}");
    drop(client);
    store.close().unwrap();
}

// contract: WIRE-14
#[test]
fn a_request_past_the_max_message_is_refused_with_its_facts_and_the_connection_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let most = 256 << 10;
    let mut client = connect(&store, Some(most));
    let handle = client.bucket("files");

    send_in_pieces(&mut client, 1, method::KV_SET, &set(handle, "k", vec![1; 300 << 10]));
    let refused = frames_of(&mut client, 1);
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].flags, END | ERROR);
    let failure = answered::<KvWritten>(&refused[0]).unwrap_err();
    assert_eq!(failure.code, "limit", "{failure:?}");
    let facts = failure.what.unwrap();
    assert_eq!(facts["limit"], "request bytes");
    assert_eq!(facts["bound"], most.to_string());
    assert!(facts["wanted"].parse::<u64>().unwrap() > 300 << 10, "{facts:?}");

    // two messages joining at once hold the connection's bound together:
    // the second passes it while the first waits for its last piece
    let half = set(handle, "k", vec![2; 150 << 10]).encode();
    let message = |stream| Frame { method: method::KV_SET, ..Frame::new(Kind::Request, stream, half.clone()) };
    let mut first = message(2).ending().pieces(BODY as usize);
    let last = first.pop().unwrap();
    for piece in first.into_iter().chain(message(3).ending().pieces(BODY as usize)) {
        client.write(piece);
    }
    let beside = answered::<KvWritten>(&frames_of(&mut client, 3)[0]).unwrap_err();
    assert_eq!(beside.code, "limit");
    assert!(beside.message.contains("this connection joins already"), "{}", beside.message);
    client.write(last);
    answered::<KvWritten>(&frames_of(&mut client, 2)[0]).unwrap();

    // a request cancelled before its last piece holds nothing
    let mut cancelled = message(4).pieces(BODY as usize);
    cancelled.pop();
    for piece in cancelled {
        client.write(piece);
    }
    client.write(Frame::new(Kind::Cancel, 4, Vec::new()));
    assert_eq!(answered::<KvWritten>(&client.next_on(4)).unwrap_err().code, "cancelled");

    // what was refused and cancelled holds none of the bound: a message near it still fits
    let stream = client.start(method::KV_GET, &get(handle, "k"));
    let entry = answered::<KvEntry>(&joined(&frames_of(&mut client, stream))).unwrap();
    assert_eq!(entry.value, Some(Row::Bin(vec![2; 150 << 10])), "the one that fit was written");
    send_in_pieces(&mut client, 5, method::KV_SET, &set(handle, "k", vec![5; 250 << 10]));
    answered::<KvWritten>(&frames_of(&mut client, 5)[0]).unwrap();
    drop(client);
    store.close().unwrap();
}

// contract: WIRE-15
#[test]
fn a_client_that_asks_for_no_max_message_gets_one_frame_and_the_body_bound() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let mut old = connect(&store, None);
    let handle = old.bucket("files");
    let mut new = connect(&store, Some(1 << 20));
    let new_handle = new.bucket("files");
    send_in_pieces(&mut new, 1, method::KV_SET, &set(new_handle, "large", vec![4; 100 << 10]));
    answered::<KvWritten>(&frames_of(&mut new, 1)[0]).unwrap();

    // an answer past the body is refused, as before pieces were
    let failure = old.call::<KvEntry>(method::KV_GET, &get(handle, "large")).unwrap_err();
    assert_eq!(failure.code, "limit");
    let facts = failure.what.unwrap();
    assert_eq!((facts["limit"].as_str(), facts["bound"].as_str()), ("answer bytes", "65536"));

    // and a piece is a frame it never agreed to
    let request = set(handle, "k", vec![3; 100 << 10]).encode();
    let mut pieces =
        Frame { method: method::KV_SET, ..Frame::new(Kind::Request, 9, request) }.ending().pieces(BODY as usize);
    old.write(pieces.remove(0));
    let away = old.read();
    assert_eq!(away.kind, Kind::GoAway);
    let away = crate::wire::protocol::GoAway::decode(&away.body).unwrap();
    assert_eq!(away.code, "protocol", "{away:?}");
    drop((old, new));
    store.close().unwrap();
}

#[test]
fn a_failure_is_never_sent_in_pieces() {
    let failure = Failure::invalid("x".repeat(100)).encode();
    let pieces = Frame::new(Kind::Response, 1, failure).failing().pieces(BODY as usize);
    assert_eq!(pieces.len(), 1);
}

/// Sends `frame` to the session as its client would.
fn send(session: &super::super::Session, frame: Frame) {
    let mut bytes = Vec::new();
    frame.encode_into(&mut bytes);
    session.receive(&bytes);
}

/// The frames a session owes until `stream`'s last, joined where they came
/// in pieces; what other streams were owed is dropped.
fn answer_on(session: &super::super::Session, reader: &mut frame::Reader, stream: u32) -> Frame {
    let mut pieces = Vec::new();
    loop {
        match reader.next(1 << 22).unwrap() {
            Some(frame) if frame.stream == stream && frame.kind == Kind::Response => {
                let last = frame.flags & MORE == 0;
                pieces.push(frame);
                if last {
                    return joined(&pieces);
                }
            }
            Some(_) => {}
            None => reader.push(&session.take(std::time::Duration::from_secs(10))),
        }
    }
}

// contract: WIRE-16
#[test]
fn a_client_that_reads_nothing_is_held_no_more_of_large_answers_than_the_max_message() {
    use super::super::{OUTPUT_LOW, OUTPUT_MOST, Session};
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let session = Session::new(store.clone(), Connect::default()).unwrap();
    let most = 4 << 20;
    let hello = Hello { protocol: 1, max_body: Some(BODY), max_message: Some(most), ..Hello::default() };
    send(&session, Frame::new(Kind::Hello, 0, hello.encode()));
    let mut reader = frame::Reader::default();
    reader.push(&session.take(std::time::Duration::from_secs(10)));
    assert_eq!(reader.next(1 << 22).unwrap().unwrap().kind, Kind::Welcome);

    let open = crate::wire::protocol::KvBucketOpen { name: "files".to_owned(), ..Default::default() };
    send(&session, Frame { method: method::KV_BUCKET_OPEN, ..Frame::new(Kind::Request, 1, open.encode()) }.ending());
    let handle = answered::<crate::wire::protocol::Handle>(&answer_on(&session, &mut reader, 1)).unwrap().handle;
    let keys = 40u32;
    for key in 0..keys {
        let value = vec![key as u8; 1000 << 10];
        let message = Frame {
            method: method::KV_SET,
            ..Frame::new(Kind::Request, 2, set(handle, &key.to_string(), value).encode())
        };
        for piece in message.ending().pieces(BODY as usize) {
            send(&session, piece);
        }
        answered::<KvWritten>(&answer_on(&session, &mut reader, 2)).unwrap();
    }

    // forty reads of a megabyte each, and a client that takes nothing
    let mut bytes = Vec::new();
    for key in 0..keys {
        let request = Frame {
            method: method::KV_GET,
            ..Frame::new(Kind::Request, 100 + key, get(handle, &key.to_string()).encode())
        };
        request.ending().encode_into(&mut bytes);
    }
    session.receive(&bytes);
    let shared = &session.shared;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        // each read is answered and gone, waiting in the line, or waiting to run again
        let (open, waiting) = {
            let answering = crate::sync::lock(&shared.answering);
            (shared.lock_open().len(), answering.line.len() + answering.again.len())
        };
        if open == waiting {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{open} streams open and {waiting} answers waiting, still");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let held = shared.held_for_client();
    let bound = most as usize + OUTPUT_LOW + BODY as usize + (64 << 10);
    assert!(held <= bound, "the session holds {held} bytes for a client that reads nothing, past {bound}");
    assert!(held <= OUTPUT_MOST + most as usize, "and no more than it would hold of answers in one frame");
    assert!(!crate::sync::lock(&shared.answering).again.is_empty(), "reads waited for the line to empty");

    // once the client reads, every read is answered whole
    let mut pieces: std::collections::HashMap<u32, Vec<Frame>> = std::collections::HashMap::new();
    let mut whole = 0;
    while whole < keys {
        match reader.next(1 << 22).unwrap() {
            Some(frame) if frame.kind == Kind::Response => {
                let (stream, last) = (frame.stream, frame.flags & MORE == 0);
                pieces.entry(stream).or_default().push(frame);
                if last {
                    let key = stream - 100;
                    let entry = answered::<KvEntry>(&joined(&pieces[&stream])).unwrap();
                    assert_eq!(entry.value, Some(Row::Bin(vec![key as u8; 1000 << 10])), "key {key}");
                    whole += 1;
                }
            }
            Some(_) => {}
            None => reader.push(&session.take(std::time::Duration::from_secs(10))),
        }
    }
    assert_eq!(shared.held_for_client(), 0);
    session.end();
    drop(session);
    store.close().unwrap();
}
