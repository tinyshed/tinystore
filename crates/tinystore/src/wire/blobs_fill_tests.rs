use std::time::Duration;

use crate::pipe::Capability;
use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::Message;
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{
    BlobsAt, BlobsClaim, BlobsDelete, BlobsFilled, BlobsGot, BlobsOpen, BlobsPiece, BlobsWrite, BlobsWritten, Empty,
    Failure, Handle, method,
};

/// A store with no background work, and `clients` connections to it.
fn clients(dir: &std::path::Path, clients: usize) -> (crate::Store, Vec<Client>) {
    let store = crate::Store::open(dir, crate::Options { background: false, ..Default::default() }).unwrap();
    let connected = (0..clients).map(|_| Client::admitted(&store, Capability::Data)).collect();
    (store, connected)
}

/// Waits until `waiters` calls wait for the run under way in the store.
fn until_waiting(store: &crate::Store, waiters: usize) {
    let blobs = crate::blobs::Blobs::of(store).unwrap();
    let started = std::time::Instant::now();
    while blobs.fills.under_way() != (1, waiters) {
        assert!(started.elapsed() < Duration::from_secs(10), "under way: {:?}", blobs.fills.under_way());
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn open(client: &mut Client) -> u64 {
    let open = BlobsOpen { name: "pages".to_owned(), ..BlobsOpen::default() };
    client.call::<Handle>(method::BLOBS_OPEN, &open).unwrap().handle
}

fn claim(handle: u64, path: &str) -> BlobsClaim {
    BlobsClaim { handle, path: path.to_owned(), ..BlobsClaim::default() }
}

/// Asks for a getOrCreate; the run is handed over when its RESPONSE leaves
/// the stream open.
fn ask(client: &mut Client, claim: &BlobsClaim) -> u32 {
    client.open_stream(method::BLOBS_GET_OR_CREATE, claim)
}

fn handed(client: &mut Client, stream: u32) {
    let frame = client.next_on(stream);
    assert_eq!((frame.kind, frame.flags), (Kind::Response, 0), "handed: {:?}", answered::<BlobsFilled>(&frame));
    assert_eq!(BlobsFilled::decode(&frame.body).unwrap(), BlobsFilled::default());
}

/// The file a getOrCreate's stream ended with.
fn got(client: &mut Client, stream: u32) -> Result<BlobsFilled, Failure> {
    let frame = client.next_on(stream);
    assert_ne!(frame.flags & frame::END, 0, "the file ends the stream");
    answered(&frame)
}

/// A put that lands the run handed on `stream`.
fn land(client: &mut Client, handle: u64, (path, stream): (&str, u32), bytes: &[u8]) -> BlobsWritten {
    let write = BlobsWrite {
        handle,
        path: path.to_owned(),
        bytes: bytes.to_vec(),
        content_type: Some("application/pdf".to_owned()),
        fill: Some(u64::from(stream)),
        ..BlobsWrite::default()
    };
    client.call(method::BLOBS_PUT, &write).unwrap()
}

/// Ends the run handed on `stream` with its last DATA, a failure or none.
fn end(client: &mut Client, stream: u32, failure: Option<Failure>) {
    let filled = BlobsFilled { failure, ..BlobsFilled::default() };
    client.write(Frame::new(Kind::Data, stream, filled.encode()).ending());
    let last = client.next_on(stream);
    assert_eq!((last.kind, last.flags & frame::END != 0), (Kind::Data, true));
    assert_eq!(answered::<BlobsFilled>(&last).unwrap(), BlobsFilled::default());
}

#[test]
fn a_miss_is_handed_to_the_client_which_lands_its_file_and_the_next_call_gets_it_whole() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut connected) = clients(dir.path(), 1);
    let client = &mut connected[0];
    let handle = open(client);
    let asked = ask(client, &claim(handle, "a.pdf"));
    handed(client, asked);
    let written = land(client, handle, ("a.pdf", asked), b"%PDF");
    let info = written.info.expect("the run held the claim");
    assert_eq!(info.content_type, "application/pdf");
    end(client, asked, None);

    let again = ask(client, &claim(handle, "a.pdf"));
    let file = got(client, again).unwrap();
    assert_eq!((file.info, file.bytes, file.failure), (Some(info.clone()), b"%PDF".to_vec(), None));
    // a getOrCreate's file is a get's answer, the same bytes
    let at = BlobsAt { handle, path: "a.pdf".to_owned(), ..BlobsAt::default() };
    let get: BlobsGot = client.call(method::BLOBS_GET, &at).unwrap();
    assert_eq!(BlobsFilled { info: get.info, bytes: get.bytes, failure: None }.encode(), {
        let again = ask(client, &claim(handle, "a.pdf"));
        client.next_on(again).body
    });
    drop(connected);
    store.close().unwrap();
}

// contract: BLOB-21
#[test]
fn every_client_s_getorcreate_of_a_path_waits_for_the_run_handed_to_one_and_gets_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut connected) = clients(dir.path(), 3);
    let handles: Vec<u64> = connected.iter_mut().map(open).collect();
    let asked = ask(&mut connected[0], &claim(handles[0], "a.pdf"));
    handed(&mut connected[0], asked);
    let waiting: Vec<u32> = (1..3).map(|n| ask(&mut connected[n], &claim(handles[n], "a.pdf"))).collect();
    until_waiting(&store, 2);
    for (n, stream) in waiting.iter().enumerate() {
        assert!(connected[n + 1].silent_on(*stream, Duration::from_millis(10)), "the other clients wait");
    }
    land(&mut connected[0], handles[0], ("a.pdf", asked), b"made once");
    end(&mut connected[0], asked, None);
    for (n, stream) in waiting.into_iter().enumerate() {
        assert_eq!(got(&mut connected[n + 1], stream).unwrap().bytes, b"made once");
    }
    drop(connected);
    store.close().unwrap();
}

// contract: BLOB-22
#[test]
fn a_failure_the_client_sends_is_the_answer_of_every_call_that_waited_and_the_next_is_handed_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut connected) = clients(dir.path(), 2);
    let (a, b) = (open(&mut connected[0]), open(&mut connected[1]));
    let run = ask(&mut connected[0], &claim(a, "bad.pdf"));
    handed(&mut connected[0], run);
    let waited = ask(&mut connected[1], &claim(b, "bad.pdf"));
    until_waiting(&store, 1);
    let failure = Failure { code: "nope".to_owned(), message: "the document does not parse".to_owned(), what: None };
    end(&mut connected[0], run, Some(failure));
    let failed = got(&mut connected[1], waited).unwrap_err();
    assert_eq!(failed.code, "conflict", "a code the core does not know: {failed:?}");
    assert!(failed.message.contains("files pages: \"bad.pdf\": the document does not parse"), "{failed:?}");

    let next = ask(&mut connected[1], &claim(b, "bad.pdf"));
    handed(&mut connected[1], next);
    let waited = ask(&mut connected[0], &claim(a, "bad.pdf"));
    until_waiting(&store, 1);
    let limit = Failure { code: "limit".to_owned(), message: "a file past its bound".to_owned(), what: None };
    end(&mut connected[1], next, Some(limit));
    assert_eq!(got(&mut connected[0], waited).unwrap_err().code, "limit", "a kind of the core's is kept");
    drop(connected);
    store.close().unwrap();
}

#[test]
fn a_client_that_leaves_or_cancels_mid_run_lets_the_claim_go_and_the_next_call_is_handed_it() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut connected) = clients(dir.path(), 2);
    let b = open(&mut connected[1]);
    let mut leaving = connected.remove(0);
    let a = open(&mut leaving);
    let run = ask(&mut leaving, &claim(a, "a.pdf"));
    handed(&mut leaving, run);
    let waited = ask(&mut connected[0], &claim(b, "a.pdf"));
    until_waiting(&store, 1);
    drop(leaving);
    handed(&mut connected[0], waited);

    let mut other = Client::admitted(&store, Capability::Data);
    let c = open(&mut other);
    let again = ask(&mut other, &claim(c, "a.pdf"));
    until_waiting(&store, 1);
    connected[0].write(Frame::new(Kind::Cancel, waited, Vec::new()));
    let cancelled = connected[0].next_on(waited);
    assert_eq!(answered::<Empty>(&cancelled).unwrap_err().code, "cancelled");
    handed(&mut other, again);
    land(&mut other, c, ("a.pdf", again), b"made");
    end(&mut other, again, None);
    drop((connected, other));
    store.close().unwrap();
}

// contract: BLOB-22
#[test]
fn a_write_that_names_no_run_is_refused_and_one_after_a_delete_lands_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut connected) = clients(dir.path(), 1);
    let client = &mut connected[0];
    let handle = open(client);
    let stranger = BlobsWrite { handle, path: "a.pdf".to_owned(), fill: Some(99), ..BlobsWrite::default() };
    let refused = client.call::<BlobsWritten>(method::BLOBS_PUT, &stranger).unwrap_err();
    assert_eq!(refused.code, "invalid", "{refused:?}");

    let run = ask(client, &claim(handle, "a.pdf"));
    handed(client, run);
    let other_path = BlobsWrite { path: "b.pdf".to_owned(), fill: Some(u64::from(run)), ..stranger };
    assert_eq!(client.call::<BlobsWritten>(method::BLOBS_PUT, &other_path).unwrap_err().code, "invalid");
    let delete = BlobsDelete { handle, path: "a.pdf".to_owned(), ..BlobsDelete::default() };
    client.call::<Empty>(method::BLOBS_DELETE, &delete).unwrap();
    let dropped = land(client, handle, ("a.pdf", run), b"stale");
    assert_eq!(dropped.info, None, "the delete ended the claim: the write landed nothing");
    end(client, run, None);
    let at = BlobsAt { handle, path: "a.pdf".to_owned(), ..BlobsAt::default() };
    assert_eq!(client.call::<BlobsGot>(method::BLOBS_GET, &at).unwrap().info, None);
    drop(connected);
    store.close().unwrap();
}

#[test]
fn an_upload_lands_a_run_of_a_file_past_one_message() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut connected) = clients(dir.path(), 1);
    let client = &mut connected[0];
    let handle = open(client);
    let run = ask(client, &claim(handle, "big.pdf"));
    handed(client, run);
    let body: Vec<u8> = (0..300_000).map(|n| (n % 251) as u8).collect();
    let write = BlobsWrite { handle, path: "big.pdf".to_owned(), fill: Some(u64::from(run)), ..BlobsWrite::default() };
    let upload = client.open_stream(method::BLOBS_UPLOAD, &write);
    assert_eq!(client.next_on(upload).flags, 0, "the upload begins");
    // 300 KB, within the stream's credit of 2 MiB
    for (n, piece) in body.chunks(60_000).enumerate() {
        let data =
            Frame::new(Kind::Data, upload, BlobsPiece { bytes: piece.to_vec(), ..BlobsPiece::default() }.encode());
        client.write(if n == 4 { data.ending() } else { data });
    }
    let written = answered::<BlobsWritten>(&client.next_on(upload)).unwrap();
    assert_eq!(written.info.map(|info| info.size), Some(body.len() as u64));
    end(client, run, None);
    let again = ask(client, &claim(handle, "big.pdf"));
    let file = got(client, again).unwrap();
    assert_eq!(file.info.map(|info| info.size), Some(body.len() as u64));
    drop(connected);
    store.close().unwrap();
}
