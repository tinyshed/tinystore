use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "the condition never held");
        thread::sleep(Duration::from_millis(1));
    }
}

/// The readers of a file one reader at a time, a read waiting for one a
/// minute at most.
fn one_reader() -> (tempfile::TempDir, Arc<Readers>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let config = Arc::new(Config { readers: 1, reader_patience: Duration::from_secs(60), ..Config::default() });
    drop(connection::open(&path, &config, Role::Writer).unwrap());
    (dir, Arc::new(Readers::new(path, config)))
}

#[cfg(feature = "replication")]
#[test]
fn a_read_whose_reader_a_pause_took_before_it_woke_reads_once_the_pause_ends() {
    let (_dir, readers) = one_reader();
    let mut lease = readers.lease().unwrap();
    let reading = {
        let readers = Arc::clone(&readers);
        thread::spawn(move || {
            let started = Instant::now();
            readers.read(|_| Ok(())).unwrap();
            started.elapsed()
        })
    };
    wait_until(|| readers.waiting() == 1);

    // The reader comes back and is given to the read that waits, and a pause
    // takes it before that read wakes to find it.
    let connection = lease.connection.take().unwrap();
    let woken = readers.lock().give(connection).expect("the reader goes to the read that waits");
    readers.pause(Duration::from_secs(1)).unwrap();
    woken.unpark();
    // time for the read to wake, find no reader, and sleep again
    thread::sleep(Duration::from_millis(300));
    readers.resume();

    let waited = reading.join().unwrap();
    assert!(waited < Duration::from_secs(30), "the read slept out its patience: {waited:?}");
}
