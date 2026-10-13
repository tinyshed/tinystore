use std::sync::Arc;
use std::time::Duration;

use crate::pipe::fixture::{Client, answered};
use crate::pipe::{Admit, Capability, Connect, Pipe};
use crate::wire::codec::Message;
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{BackupPiece, BackupWrite, BackupWritten, Empty, Hello, method};
use crate::{Options, Store};

/// Bytes past several pieces', so that a backup goes in many.
const PHOTO: usize = 900 << 10;
/// The DATA a test's client takes before it grants more, which bounds a
/// piece too: a message is never larger than a stream's credit.
const WINDOW: u64 = 100 << 10;
/// How long a server that is owed no more credit stays silent for a test.
const QUIET: Duration = Duration::from_millis(150);

fn filled(dir: &std::path::Path) -> Store {
    let store = Store::open(dir, Options { background: false, ..Options::default() }).unwrap();
    store.bucket::<String>("codes").open().unwrap().set("K7Q2", &"kept".to_owned()).unwrap();
    let photo: Vec<u8> = (0..PHOTO).map(|at| (at % 241) as u8).collect();
    store.files("photos").open().unwrap().put("cat.jpg", photo).unwrap();
    store
}

/// A client of `store` that may administer and takes `WINDOW` bytes of DATA
/// a stream before it grants more.
fn admin(store: &Store) -> Client {
    let admit: Admit = Arc::new(|_: Option<&str>| Some(Capability::Admin));
    let mut client = Client::over(Pipe::connect(store, Connect { admit: Some(admit), ..Connect::default() }).unwrap());
    let token = Some("a token".to_owned());
    client.greet(Hello { protocol: 2, token, stream_credit: Some(WINDOW), ..Hello::default() }).unwrap();
    client
}

fn credit(client: &mut Client, stream: u32, bytes: usize) {
    client.write(Frame::new(Kind::Credit, stream, u32::try_from(bytes).unwrap().to_le_bytes().to_vec()));
}

#[test]
fn a_backup_over_the_wire_is_its_stores_zip_in_pieces_within_the_clients_credit() {
    let dirs = tempfile::tempdir().unwrap();
    let store = filled(&dirs.path().join("data"));
    let mut client = admin(&store);
    let stream = client.start(method::BACKUP_WRITE, &BackupWrite::default());
    assert_eq!(client.next_on(stream).kind, Kind::Response);
    let (mut zip, mut pieces) = (Vec::new(), 0);
    let backed = loop {
        let piece = client.next_on(stream);
        assert_eq!(piece.flags & frame::END, 0, "a backup's END comes after its pieces");
        assert!(piece.body.len() as u64 <= WINDOW, "{} bytes past the credit", piece.body.len());
        zip.extend(BackupPiece::decode(&piece.body).unwrap().bytes);
        pieces += 1;
        if !client.silent_on(stream, QUIET) {
            let next = client.next_on(stream);
            assert_ne!(next.flags & frame::END, 0, "no DATA goes past the client's credit");
            break answered::<BackupWritten>(&next).unwrap();
        }
        credit(&mut client, stream, piece.body.len());
    };
    assert!(pieces > 3, "{pieces} pieces");
    assert_eq!((backed.files, backed.key_left_out), (3, false), "kv.db, blobs.db and the photo's bytes");
    drop(client);
    drop(store);

    let (archive, restored) = (dirs.path().join("backup.zip"), dirs.path().join("restored"));
    std::fs::write(&archive, &zip).unwrap();
    let held = crate::backup::restore(&archive, &restored).unwrap();
    assert_eq!((held.files as u64, held.bytes), (backed.files, backed.bytes));
    let back = Store::open(&restored, Options::default()).unwrap();
    assert_eq!(back.bucket::<String>("codes").open().unwrap().get("K7Q2").unwrap().as_deref(), Some("kept"));
}

#[test]
fn a_connection_that_may_not_administer_takes_no_backup() {
    let dir = tempfile::tempdir().unwrap();
    let store = filled(dir.path());
    for capability in [Capability::Data, Capability::Read] {
        let mut client = Client::admitted(&store, capability);
        let refused = client.call::<BackupWritten>(method::BACKUP_WRITE, &BackupWrite::default()).unwrap_err();
        assert_eq!(refused.code, "permission", "{capability:?}");
    }
}

#[test]
fn a_backup_its_client_cancels_ends_and_lets_its_worker_go() {
    let dir = tempfile::tempdir().unwrap();
    let store = filled(dir.path());
    let mut client = admin(&store);
    let stream = client.start(method::BACKUP_WRITE, &BackupWrite::default());
    assert_eq!(client.next_on(stream).kind, Kind::Response);
    // the first piece takes the credit, and the worker waits once four more are held
    let first = client.next_on(stream);
    assert_eq!(first.flags & frame::END, 0);
    assert!(client.silent_on(stream, QUIET * 2), "no piece past the credit");
    client.write(Frame::new(Kind::Cancel, stream, Vec::new()));
    let ended = loop {
        let frame = client.next_on(stream);
        if frame.flags & frame::END != 0 {
            break frame;
        }
    };
    assert_eq!(answered::<Empty>(&ended).unwrap_err().code, "cancelled");
    drop(client);
    store.close().unwrap();
    let snapshots = std::fs::read_dir(dir.path()).unwrap().flatten();
    let left: Vec<_> =
        snapshots.filter(|entry| entry.file_name().to_string_lossy().starts_with(".snapshot-")).collect();
    assert!(left.is_empty(), "the snapshot of a backup cancelled is gone: {left:?}");
}
