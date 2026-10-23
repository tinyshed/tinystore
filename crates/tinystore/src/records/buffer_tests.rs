use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use tracing_subscriber::layer::SubscriberExt;

use super::*;
use crate::console::Format;
use crate::records::fixture::fixture;

// contract: LOG-02
#[test]
fn a_logger_dropped_while_a_flush_writes_loses_none_of_its_lines() {
    let f = fixture();
    let log = f.store.logger("api").console(Format::Off).no_env().layer().unwrap();
    logged(&log, "first");
    let records = f.records();
    let buffer = Arc::downgrade(&records.lock_held().buffers[0]);
    let taken = || buffer.upgrade().is_some_and(|buffer| lock(&buffer.waiting).lines.is_empty());
    let (holding, held) = (channel(), channel::<()>());
    std::thread::scope(|scope| {
        // the writer held, so that a flush that took the first line waits to write it
        let (writer_held, go) = (holding.0, held.1);
        scope.spawn(|| {
            let hold = move |_: &crate::sqlite::Tx<'_>| {
                writer_held.send(()).unwrap();
                go.recv().unwrap();
                Ok(())
            };
            records.file.write(0, hold).unwrap();
        });
        holding.1.recv().unwrap();
        let flush = scope.spawn(|| records.flush_held(false));
        let start = Instant::now();
        while !taken() {
            assert!(start.elapsed() < Duration::from_secs(10), "the flush never took the first line");
            std::thread::sleep(Duration::from_millis(1));
        }
        // the last line, and its logger gone, while the flush waits
        logged(&log, "last");
        drop(log);
        held.0.send(()).unwrap();
        flush.join().unwrap().unwrap();
    });
    records.flush_held(false).unwrap();
    let events = f.events("api").list().page().unwrap().events;
    let messages: Vec<_> = events.iter().map(|event| event.message.as_deref()).collect();
    assert_eq!(messages, [Some("first"), Some("last")]);
}

fn logged(layer: &crate::console::Layer, line: &str) {
    let subscriber = tracing_subscriber::registry().with(layer.clone());
    tracing::subscriber::with_default(subscriber, || tracing::info!("{line}"));
}
