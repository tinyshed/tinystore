use std::time::Duration;

use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::Message;
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{
    BlobsAt, BlobsGot, BlobsHead, BlobsOpen, BlobsPiece, BlobsWrite, BlobsWritten, Empty, Handle, Hello, method,
};

fn open(client: &mut Client, name: &str) -> u64 {
    let open = BlobsOpen { name: name.to_owned(), ..BlobsOpen::default() };
    client.call::<Handle>(method::BLOBS_OPEN, &open).unwrap().handle
}

fn bytes(size: usize) -> Vec<u8> {
    (0..size).map(|n| (n * 31 % 251) as u8).collect()
}

/// A client greeted with `hello`, and the DATA bytes the server takes on a
/// stream before it gives credit back.
fn connect(dir: &std::path::Path, hello: Hello) -> (Client, u64) {
    let mut client = Client::connect(dir);
    let welcome = client.greet(hello).unwrap();
    (client, welcome.stream_credit)
}

/// Uploads `body` in pieces of `piece` bytes within the server's `credit`,
/// and the answer of its last DATA.
fn upload(client: &mut Client, write: &BlobsWrite, body: &[u8], piece: usize, mut credit: u64) -> Frame {
    let stream = client.open_stream(method::BLOBS_UPLOAD, write);
    let begun = client.next_on(stream);
    assert_eq!((begun.kind, begun.flags), (Kind::Response, 0), "an upload begins: {:?}", answered::<Empty>(&begun));
    let pieces: Vec<&[u8]> = body.chunks(piece).collect();
    for (n, bytes) in pieces.iter().enumerate() {
        let data = Frame::new(Kind::Data, stream, BlobsPiece { bytes: bytes.to_vec() }.encode());
        while credit < data.body.len() as u64 {
            credit += client.credit_on(stream);
        }
        credit -= data.body.len() as u64;
        client.write(if n + 1 == pieces.len() { data.ending() } else { data });
    }
    if pieces.is_empty() {
        client.write(Frame::new(Kind::Data, stream, Vec::new()).ending());
    }
    client.next_on(stream)
}

/// A get's whole file: its RESPONSE, then pieces never past the `credit` the
/// client gave, which it grants back once half of it was taken, as the SDK does.
fn download(client: &mut Client, at: &BlobsAt, credit: u64) -> (BlobsGot, Vec<u8>, Frame) {
    let stream = client.start(method::BLOBS_GET, at);
    let first = client.next_on(stream);
    let got = answered::<BlobsGot>(&first).unwrap();
    let mut body = got.bytes.clone();
    let mut last = first;
    let mut taken = 0_u64;
    while last.flags & frame::END == 0 {
        last = client.next_on(stream);
        if last.flags & frame::END != 0 {
            break;
        }
        taken += last.body.len() as u64;
        assert!(taken <= credit, "the server sends within the client's credit");
        body.extend_from_slice(&BlobsPiece::decode(&last.body).unwrap().bytes);
        if taken >= credit / 2 {
            client.write(Frame::new(Kind::Credit, stream, u32::try_from(taken).unwrap().to_le_bytes().to_vec()));
            taken = 0;
        }
    }
    (got, body, last)
}

#[test]
fn a_file_put_over_the_wire_reads_back_whole_in_one_answer() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client, "avatars");
    let write = BlobsWrite {
        handle,
        folder: vec!["users".to_owned(), "42".to_owned()],
        path: "a.png".to_owned(),
        content_type: Some("image/png".to_owned()),
        bytes: b"not really a png".to_vec(),
        ..BlobsWrite::default()
    };
    let written: BlobsWritten = client.call(method::BLOBS_PUT, &write).unwrap();
    let info = written.info.expect("a put writes");
    assert_eq!((info.path.as_str(), info.size, info.content_type.as_str()), ("a.png", 16, "image/png"));
    let at = BlobsAt { handle, folder: write.folder.clone(), path: "a.png".to_owned() };
    let head: BlobsHead = client.call(method::BLOBS_HEAD, &at).unwrap();
    assert_eq!(head.info.as_ref(), Some(&info));
    let got: BlobsGot = client.call(method::BLOBS_GET, &at).unwrap();
    assert_eq!((got.info, got.bytes), (Some(info), write.bytes));
    let none: BlobsGot = client.call(method::BLOBS_GET, &BlobsAt { path: "b.png".to_owned(), ..at }).unwrap();
    assert_eq!(none.info, None);
}

#[test]
fn create_over_the_wire_writes_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client, "docs");
    let create =
        BlobsWrite { handle, path: "a".to_owned(), create: true, bytes: b"1".to_vec(), ..BlobsWrite::default() };
    let first: BlobsWritten = client.call(method::BLOBS_PUT, &create).unwrap();
    assert!(first.info.is_some());
    let again: BlobsWritten = client.call(method::BLOBS_PUT, &BlobsWrite { bytes: b"2".to_vec(), ..create }).unwrap();
    assert_eq!(again.info, None, "a create that found a file writes nothing");
}

#[test]
fn a_large_file_goes_up_and_comes_down_in_pieces_within_the_credit() {
    let dir = tempfile::tempdir().unwrap();
    let credit = 256 << 10;
    let (mut client, server_credit) =
        connect(dir.path(), Hello { protocol: 2, stream_credit: Some(credit), ..Hello::default() });
    let handle = open(&mut client, "exports");
    let body = bytes(3 << 20);
    let write =
        BlobsWrite { handle, path: "big.bin".to_owned(), size: Some(body.len() as u64), ..BlobsWrite::default() };
    let last = upload(&mut client, &write, &body, 100_000, server_credit);
    let written = answered::<BlobsWritten>(&last).unwrap();
    assert_eq!(written.info.unwrap().size, body.len() as u64);

    let (got, downloaded, last) =
        download(&mut client, &BlobsAt { handle, folder: Vec::new(), path: "big.bin".to_owned() }, credit);
    assert!(got.bytes.len() as u64 <= credit / 2, "the first bytes within half the credit");
    assert_eq!(answered::<Empty>(&last), Ok(Empty {}));
    assert!(downloaded == body, "the bytes that went up come down");
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn a_get_of_a_changed_byte_ends_corrupt_before_its_last_piece() {
    let dir = tempfile::tempdir().unwrap();
    let credit = 1 << 20;
    let (mut client, server_credit) =
        connect(dir.path(), Hello { protocol: 2, stream_credit: Some(credit), ..Hello::default() });
    let handle = open(&mut client, "docs");
    let body = bytes(5 << 20);
    let write = BlobsWrite { handle, path: "a".to_owned(), ..BlobsWrite::default() };
    answered::<BlobsWritten>(&upload(&mut client, &write, &body, 256 << 10, server_credit)).unwrap();
    let fan = std::fs::read_dir(dir.path().join("blobs/objects")).unwrap().next().unwrap().unwrap().path();
    let file = std::fs::read_dir(fan).unwrap().next().unwrap().unwrap().path();
    let mut changed = std::fs::read(&file).unwrap();
    changed[4096] ^= 0x5a;
    std::fs::write(&file, changed).unwrap();

    let at = BlobsAt { handle, folder: Vec::new(), path: "a".to_owned() };
    let (_, downloaded, last) = download(&mut client, &at, credit);
    assert!(downloaded.len() < body.len(), "the last bytes are held back");
    let failed = answered::<Empty>(&last).unwrap_err();
    assert_eq!(failed.code, "corrupt", "{}", failed.message);
}

#[test]
fn an_upload_cancelled_or_refused_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client, "docs");
    let write = BlobsWrite { handle, path: "a".to_owned(), ..BlobsWrite::default() };
    let stream = client.open_stream(method::BLOBS_UPLOAD, &write);
    assert_eq!(client.next_on(stream).flags, 0);
    client.write(Frame::new(Kind::Data, stream, BlobsPiece { bytes: bytes(1 << 20) }.encode()));
    client.write(Frame::new(Kind::Cancel, stream, Vec::new()));
    assert_eq!(answered::<Empty>(&client.next_on(stream)).unwrap_err().code, "cancelled");

    let sized = BlobsWrite { size: Some(10), ..write.clone() };
    let refused = answered::<BlobsWritten>(&upload(&mut client, &sized, &bytes(11), 4, 1 << 20)).unwrap_err();
    assert_eq!(refused.code, "invalid", "{}", refused.message);
    let head: BlobsHead =
        client.call(method::BLOBS_HEAD, &BlobsAt { handle, folder: Vec::new(), path: "a".to_owned() }).unwrap();
    assert_eq!(head.info, None);
    assert_eq!(std::fs::read_dir(dir.path().join("blobs/uploads")).unwrap().count(), 0);
}

#[test]
fn a_connection_that_ends_mid_upload_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut client = Client::open(dir.path());
        let handle = open(&mut client, "docs");
        let write = BlobsWrite { handle, path: "a".to_owned(), ..BlobsWrite::default() };
        let stream = client.open_stream(method::BLOBS_UPLOAD, &write);
        assert_eq!(client.next_on(stream).flags, 0);
        client.write(Frame::new(Kind::Data, stream, BlobsPiece { bytes: bytes(1 << 20) }.encode()));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(std::fs::read_dir(dir.path().join("blobs/uploads")).unwrap().count(), 0);
}
