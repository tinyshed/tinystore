use std::time::Duration;

use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::Message;
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{
    BlobsAt, BlobsDelete, BlobsGot, BlobsHead, BlobsOpen, BlobsPiece, BlobsRead, BlobsWrite, BlobsWritten, Empty,
    Failure, Handle, Hello, method,
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

/// A read of a file's bytes and how its stream ended: the RESPONSE's piece,
/// then pieces never past the `credit` the client gave, which it grants back
/// once half of it was taken, as the SDK does.
fn read(client: &mut Client, read: &BlobsRead, credit: u64) -> (Vec<u8>, Result<(), Failure>) {
    let stream = client.start(method::BLOBS_READ, read);
    let mut frame = client.next_on(stream);
    let mut bytes = match answered::<BlobsPiece>(&frame) {
        Ok(first) => first.bytes,
        Err(failure) => return (Vec::new(), Err(failure)),
    };
    assert!(bytes.len() as u64 <= credit / 2, "a piece is at most half the credit");
    let mut taken = 0_u64;
    while frame.flags & frame::END == 0 {
        frame = client.next_on(stream);
        if frame.flags & frame::END != 0 {
            return (bytes, answered::<Empty>(&frame).map(drop));
        }
        taken += frame.body.len() as u64;
        assert!(taken <= credit, "the server sends within the client's credit");
        bytes.extend_from_slice(&BlobsPiece::decode(&frame.body).unwrap().bytes);
        if taken >= credit / 2 {
            client.write(Frame::new(Kind::Credit, stream, u32::try_from(taken).unwrap().to_le_bytes().to_vec()));
            taken = 0;
        }
    }
    (bytes, Ok(()))
}

/// A read of the whole file at `path`, on the ETag its get gave.
fn whole(client: &mut Client, handle: u64, path: &str, credit: u64) -> (Vec<u8>, Result<(), Failure>) {
    let at = BlobsAt { handle, folder: Vec::new(), path: path.to_owned() };
    let got: BlobsGot = client.call(method::BLOBS_GET, &at).unwrap();
    assert!(got.bytes.is_empty(), "a file past half a body waits for its read");
    let if_match = got.info.map(|info| info.etag);
    read(client, &BlobsRead { handle, path: path.to_owned(), if_match, ..BlobsRead::default() }, credit)
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

    let (downloaded, ended) = whole(&mut client, handle, "big.bin", credit);
    assert_eq!(ended, Ok(()));
    assert!(downloaded == body, "the bytes that went up come down");
    assert_eq!(client.pipe.streams(), 0);
}

#[test]
fn a_range_reads_only_its_bytes_and_is_not_checked() {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, credit) = connect(dir.path(), Hello { protocol: 2, ..Hello::default() });
    let handle = open(&mut client, "videos");
    let body = bytes(5 << 20);
    let write = BlobsWrite { handle, path: "film.mp4".to_owned(), ..BlobsWrite::default() };
    answered::<BlobsWritten>(&upload(&mut client, &write, &body, 1 << 20, credit)).unwrap();
    let range = |offset: u64, length: Option<u64>| BlobsRead {
        handle,
        path: "film.mp4".to_owned(),
        offset: Some(offset),
        length,
        ..BlobsRead::default()
    };

    let (middle, ended) = read(&mut client, &range(1_000_000, Some(3_000_000)), 2 << 20);
    assert_eq!(ended, Ok(()));
    assert!(middle == body[1_000_000..4_000_000], "the range is its bytes");
    let (tail, _) = read(&mut client, &range(body.len() as u64 - 500, None), 2 << 20);
    assert!(tail == body[body.len() - 500..], "a range to the end");
    let (past, ended) = read(&mut client, &range(body.len() as u64 + 1, Some(10)), 2 << 20);
    assert_eq!((past.len(), ended), (0, Ok(())), "a range past the end is empty");

    let fan = std::fs::read_dir(dir.path().join("blobs/objects")).unwrap().next().unwrap().unwrap().path();
    let file = std::fs::read_dir(fan).unwrap().next().unwrap().unwrap().path();
    let mut changed = std::fs::read(&file).unwrap();
    changed[4096] ^= 0x5a;
    std::fs::write(&file, changed).unwrap();
    let (head, ended) = read(&mut client, &range(0, Some(8192)), 2 << 20);
    assert_eq!((head.len(), ended), (8192, Ok(())), "a range is not checked");
}

#[test]
fn a_read_on_an_etag_the_path_no_longer_holds_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, credit) = connect(dir.path(), Hello { protocol: 2, ..Hello::default() });
    let handle = open(&mut client, "docs");
    let write = BlobsWrite { handle, path: "a".to_owned(), ..BlobsWrite::default() };
    answered::<BlobsWritten>(&upload(&mut client, &write, &bytes(3 << 20), 1 << 20, credit)).unwrap();
    let at = BlobsAt { handle, folder: Vec::new(), path: "a".to_owned() };
    let got: BlobsGot = client.call(method::BLOBS_GET, &at).unwrap();
    let on =
        BlobsRead { handle, path: "a".to_owned(), if_match: got.info.map(|info| info.etag), ..BlobsRead::default() };

    client.call::<BlobsWritten>(method::BLOBS_PUT, &BlobsWrite { bytes: b"other".to_vec(), ..write }).unwrap();
    let (_, ended) = read(&mut client, &on, 2 << 20);
    assert_eq!(ended.unwrap_err().code, "conflict", "a file replaced since its get");
    let (now, ended) = read(&mut client, &BlobsRead { if_match: None, ..on.clone() }, 2 << 20);
    assert_eq!((now, ended), (b"other".to_vec(), Ok(())), "without an ETag, what is there now");

    let delete = BlobsDelete { handle, path: "a".to_owned(), ..BlobsDelete::default() };
    client.call::<Empty>(method::BLOBS_DELETE, &delete).unwrap();
    let (_, ended) = read(&mut client, &on, 2 << 20);
    assert_eq!(ended.unwrap_err().code, "conflict", "a file deleted since its get");
    let (_, ended) = read(&mut client, &BlobsRead { if_match: None, ..on }, 2 << 20);
    assert_eq!(ended.unwrap_err().code, "not_found");
}

#[test]
fn a_whole_read_of_a_changed_byte_ends_corrupt_before_its_last_piece() {
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

    let (downloaded, ended) = whole(&mut client, handle, "a", credit);
    assert!(downloaded.len() < body.len(), "the last bytes are held back");
    let failed = ended.unwrap_err();
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
