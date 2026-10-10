use std::sync::{Arc, Mutex, MutexGuard};

use super::super::workers::{MOST, Workers};
use super::CROWD;
use crate::pipe::fixture::{Client, answered};
use crate::pipe::{Connect, Pipe};
use crate::wire::codec::Message;
use crate::wire::frame::{Frame, Kind};
use crate::wire::protocol::{KvCall, KvEntry, method};
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
    let workers = Workers::of(store).unwrap();
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
    client.hello(2).unwrap();
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
