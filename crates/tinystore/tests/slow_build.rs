//! A process of its own, since SQLite starts once a process: here it starts
//! before the store, which then warns that it keeps SQLite's own mutexes, and
//! no subscriber listens yet, as in the guides, which set it up after the
//! store opens with the store's logger in it.
#![cfg(feature = "records")]

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use tinystore::{Options, Store};

/// What a logger prints, read back by the test.
#[derive(Clone, Default)]
struct Printed(Arc<Mutex<Vec<u8>>>);

impl Write for Printed {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Printed {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

#[test]
fn a_stores_logger_says_first_what_the_store_warned_of_before_anyone_listened() {
    let started_first = rusqlite::Connection::open_in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();

    let printed = Printed::default();
    let logger = store.logger("app").to(printed.clone()).layer().unwrap();
    logger.flush();
    let text = printed.text();
    assert!(text.contains("SQLite had started before the store opened"), "{text}");
    assert!(text.contains("\"level\":\"WARN\""), "{text}");

    let again = Printed::default();
    let second = store.logger("app").to(again.clone()).layer().unwrap();
    second.flush();
    assert!(again.text().is_empty(), "said once: {}", again.text());

    drop((logger, second, started_first));
    store.close().unwrap();
}
