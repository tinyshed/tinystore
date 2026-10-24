use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{CROWD, OUTPUT_MOST, Route, Session, route};
use crate::pipe::fixture::{Client, answered};
use crate::pipe::{Connect, Pipe};
use crate::pool::{MOST, Pool};
use crate::sync::lock;
use crate::wire::codec::Message;
use crate::wire::codec::Row;
use crate::wire::frame::{self, Frame, Kind, Reader};
use crate::wire::protocol::{
    Failure, Handle, Hello, KvBucketOpen, KvCall, KvEntry, KvList, KvWritten, METHODS, method,
};
use crate::{Options, Store};

/// Writes gets of one key on `streams` in one write, and returns what the
/// write itself answered.
fn get_at_once(client: &mut Client, handle: u64, streams: std::ops::Range<u32>) -> Vec<u8> {
    let get = KvCall { handle, key: "k".to_owned(), ..KvCall::default() };
    let mut bytes = Vec::new();
    for stream in streams {
        Frame { method: method::KV_GET, ..Frame::new(Kind::Request, stream, get.encode()) }
            .ending()
            .encode_into(&mut bytes);
    }
    client.pipe.send(&bytes)
}

/// Holds every worker of the store on a gate of its own, and returns the
/// gates' locks: dropping one lets one worker go on.
fn hold_every_worker<'gates>(store: &Store, gates: &'gates [Arc<Mutex<()>>]) -> Vec<MutexGuard<'gates, ()>> {
    let workers = Pool::of(store).unwrap();
    let closed = gates.iter().map(|gate| gate.lock().unwrap()).collect();
    for gate in gates {
        let gate = Arc::clone(gate);
        workers.run(Box::new(move || drop(gate.lock())));
    }
    closed
}

fn gates() -> Vec<Arc<Mutex<()>>> {
    (0..MOST).map(|_| Arc::new(Mutex::new(()))).collect()
}

#[test]
fn a_read_goes_to_the_workers_while_they_have_reads_of_its_connection() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let mut client = Client::over(Pipe::connect(&store, Connect::default()).unwrap());
    client.hello(1).unwrap();
    let handle = client.bucket("sessions");
    let crowd = CROWD as u32;

    let alone = get_at_once(&mut client, handle, 1000..1001);
    assert!(!alone.is_empty(), "a read alone is answered before its write returns");
    client.reader.push(&alone);
    answered::<KvEntry>(&client.next_on(1000)).unwrap();

    // every worker held, so that a read sent to them stays unanswered
    let gates = gates();
    let closed = hold_every_worker(&store, &gates);
    assert!(get_at_once(&mut client, handle, 2000..2000 + crowd).is_empty(), "a crowd goes to the workers");
    assert!(get_at_once(&mut client, handle, 3000..3001).is_empty(), "and a read that comes meanwhile goes with it");

    drop(closed);
    for stream in (2000..2000 + crowd).chain(3000..3001) {
        answered::<KvEntry>(&client.next_on(stream)).unwrap();
    }
    assert_eq!(client.pipe.streams(), 0, "every stream ended once");
    assert!(!get_at_once(&mut client, handle, 4000..4001).is_empty(), "with none away, a read is answered here again");
    drop(client);
    store.close().unwrap();
}

/// A call's stream ends, and gives its credit back, once its answer is
/// queued rather than taken: a client that asks for a large value again
/// and again and reads nothing is held at the connection's bound, and
/// answered the rest once it reads.
// contract: WIRE-03
#[test]
fn a_connection_whose_client_reads_nothing_reads_no_frame_more_past_its_bound_and_goes_on_once_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = client.bucket("files");
    let value = KvCall { handle, key: "k".to_owned(), value: Some(Row::Bin(vec![7; 1 << 20])), ..KvCall::default() };
    client.call::<KvWritten>(method::KV_SET, &value).unwrap();
    let gets = 64;
    for stream in 0..gets {
        // one at a time, each answered where it is read
        let mut bytes = Vec::new();
        let get = KvCall { handle, key: "k".to_owned(), ..KvCall::default() };
        Frame { method: method::KV_GET, ..Frame::new(Kind::Request, 1000 + stream, get.encode()) }
            .ending()
            .encode_into(&mut bytes);
        client.pipe.push(&bytes);
    }
    assert!(client.pipe.full(), "the connection reads no frame more");
    let first = client.pipe.recv(std::time::Duration::ZERO);
    assert!(first.len() <= super::OUTPUT_MOST + (1 << 20) + 64, "it owed {} bytes", first.len());

    let mut reader = crate::wire::frame::Reader::default();
    reader.push(&first);
    // the gets that waited are a crowd, which the workers answer in any order
    let mut answered = std::collections::BTreeSet::new();
    while answered.len() < gets as usize {
        while let Some(frame) = reader.next(1 << 22).unwrap() {
            assert_eq!(frame.kind, Kind::Response);
            answered.insert(frame.stream);
        }
        if answered.len() < gets as usize {
            let more = client.pipe.recv(std::time::Duration::from_secs(10));
            assert!(!more.is_empty(), "{} of {gets} answers, and then nothing", answered.len());
            reader.push(&more);
        }
    }
    assert_eq!(answered, (1000..1000 + gets).collect());
    assert!(!client.pipe.full());
}

/// Calls read in one write all reach the workers before a worker answers
/// any, and each looks at what the connection owes where it starts: a client
/// that asks for a large value 128 times at once and reads nothing is owed
/// the bound and the answers of the calls the workers had started, and the
/// rest once it reads. A crowd of gets goes to the workers, a list runs there
/// anyway, and a query is a download.
// contract: WIRE-03
#[test]
fn a_burst_of_large_reads_its_client_does_not_read_is_owed_no_further_than_the_bound() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let session = Session::new(store.clone(), Connect::default()).unwrap();
    let mut reader = Reader::default();
    let hello = Hello { protocol: 1, client: "test/0".to_owned(), ..Hello::default() };
    assert_eq!(ask(&session, &mut reader, Frame::new(Kind::Hello, 0, hello.encode())).kind, Kind::Welcome);
    let open = KvBucketOpen { name: "files".to_owned(), ..KvBucketOpen::default() };
    let handle = answered::<Handle>(&call(&session, &mut reader, 1, method::KV_BUCKET_OPEN, open.encode())).unwrap();
    let handle = handle.handle;
    let value = KvCall { handle, key: "k".to_owned(), value: Some(Row::Bin(vec![7; 1 << 20])), ..KvCall::default() };
    answered::<KvWritten>(&call(&session, &mut reader, 2, method::KV_SET, value.encode())).unwrap();

    let get = KvCall { handle, key: "k".to_owned(), ..KvCall::default() };
    let list = KvList { handle, limit: Some(1), ..KvList::default() };
    #[cfg_attr(not(feature = "sql"), expect(unused_mut, reason = "a query needs sql"))]
    let mut bursts = vec![(method::KV_GET, get.encode()), (method::KV_LIST, list.encode())];
    #[cfg(feature = "sql")]
    {
        use crate::wire::protocol::{SqlOpen, SqlQuery};
        let open = SqlOpen { name: "app".to_owned(), ..SqlOpen::default() };
        let handle = answered::<Handle>(&call(&session, &mut reader, 3, method::SQL_OPEN, open.encode())).unwrap();
        let text = "SELECT zeroblob(1000000)".to_owned();
        let query = SqlQuery { handle: handle.handle, text, want: "all".to_owned(), ..SqlQuery::default() };
        bursts.push((method::SQL_QUERY, query.encode()));
    }
    let calls = 128;
    for (called, request) in bursts {
        let mut burst = Vec::new();
        for stream in 1000..1000 + calls {
            let frame = Frame { method: called, ..Frame::new(Kind::Request, stream, request.clone()) };
            frame.ending().encode_into(&mut burst);
        }
        session.receive(&burst);
        let owed = owed_once_each_call_answered_or_waits(&session);
        let most = OUTPUT_MOST + MOST * ((1 << 20) + (1 << 10));
        assert!(owed <= most, "{called:#06x}: {calls} calls in one write owed {owed} bytes, past {most}");

        let mut got = BTreeSet::new();
        while got.len() < calls as usize {
            while let Some(frame) = reader.next(1 << 22).unwrap() {
                let failed = (frame.flags & frame::ERROR != 0).then(|| Failure::decode(&frame.body).unwrap());
                assert_eq!((frame.kind, frame.flags, failed), (Kind::Response, frame::END, None), "{called:#06x}");
                got.insert(frame.stream);
            }
            if got.len() < calls as usize {
                let more = session.take(Duration::from_secs(10));
                assert!(!more.is_empty(), "{called:#06x}: {} of {calls} answers, and then nothing", got.len());
                reader.push(&more);
            }
        }
        assert_eq!(got, (1000..1000 + calls).collect(), "{called:#06x}");
    }
    session.end();
    drop(session);
    store.close().unwrap();
}

/// Sends `request` on `stream` as a REQUEST of `called`, and returns the frame
/// that answers it.
fn call(session: &Session, reader: &mut Reader, stream: u32, called: u16, request: Vec<u8>) -> Frame {
    ask(session, reader, Frame { method: called, ..Frame::new(Kind::Request, stream, request) }.ending())
}

/// Sends `frame` and returns the next frame on its stream.
fn ask(session: &Session, reader: &mut Reader, frame: Frame) -> Frame {
    let (stream, mut bytes) = (frame.stream, Vec::new());
    frame.encode_into(&mut bytes);
    session.receive(&bytes);
    loop {
        match reader.next(1 << 22).unwrap() {
            Some(frame) if frame.stream == stream => return frame,
            Some(_) => {}
            None => {
                let more = session.take(Duration::from_secs(10));
                assert!(!more.is_empty(), "nothing answered stream {stream}");
                reader.push(&more);
            }
        }
    }
}

/// What a session owes once each of its streams is answered or waits for
/// the output to drain: a stream still open whose call does not wait runs.
fn owed_once_each_call_answered_or_waits(session: &Session) -> usize {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        // open streams only fall, and the calls waiting never pass them: the
        // two are equal only if every open stream's call waited at the second
        let open = session.shared.lock_open().len();
        let waiting = lock(&session.shared.waiting_output).len();
        if open == waiting {
            return session.shared.owed();
        }
        assert!(Instant::now() < deadline, "{open} streams open and {waiting} calls waiting, still");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A method's line in the schema and its engine's `route` are written apart;
/// a download that ran as a plain call would end its stream on the first item.
#[test]
fn every_method_runs_as_the_shape_its_schema_line_says() {
    for &(name, called, shape) in METHODS {
        let runs = match route(called) {
            Route::Inline | Route::Submit | Route::Worker => "call",
            Route::Handover => "handover",
            #[cfg(feature = "jobs")]
            Route::Exchange => "exchange",
            #[cfg(feature = "jobs")]
            Route::Watch => "download",
            #[cfg(feature = "sql")]
            Route::Download => "download",
            #[cfg(feature = "sql")]
            Route::Transaction => "exchange",
            #[cfg(feature = "blobs")]
            Route::Read => "download",
            #[cfg(feature = "blobs")]
            Route::Upload => "exchange",
            #[cfg(feature = "records")]
            Route::Tail => "download",
            #[cfg(feature = "records")]
            Route::Lines => "exchange",
            #[cfg(feature = "metrics")]
            Route::Parts => "download",
            #[cfg(feature = "metrics")]
            Route::Instruments => "exchange",
            #[cfg(feature = "config")]
            Route::Follow => "download",
            #[cfg(feature = "backup")]
            Route::Backup => "download",
            #[cfg(feature = "locks")]
            Route::Hold => "call",
            #[cfg(feature = "channels")]
            Route::Subscribe => "download",
        };
        assert_eq!(runs, shape, "{name}");
    }
}
