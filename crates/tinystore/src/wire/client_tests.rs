use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use super::Client;
use crate::metrics::Series;
use crate::pipe::{Connect, Pipe};
use crate::{ErrorKind, Options, Store};

/// A pipe to a store as the client reads and writes a socket. Once `deaf`,
/// what the client sends still reaches the store, and the client reads the
/// end of the stream: an answer lost on the way.
struct Piped {
    pipe: Pipe,
    ready: VecDeque<u8>,
    deaf: Arc<AtomicBool>,
}

impl Write for Piped {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let answered = self.pipe.send(bytes);
        self.ready.extend(answered);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for Piped {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if self.deaf.load(Ordering::Acquire) {
            return Ok(0);
        }
        if self.ready.is_empty() {
            self.ready.extend(self.pipe.recv(Duration::from_secs(10)));
        }
        let count = into.len().min(self.ready.len());
        for (slot, byte) in into.iter_mut().zip(self.ready.drain(..count)) {
            *slot = byte;
        }
        Ok(count)
    }
}

fn welcomed(store: &Store) -> (Client<Piped>, Arc<AtomicBool>) {
    let deaf = Arc::new(AtomicBool::new(false));
    let pipe = Pipe::connect(store, Connect::default()).unwrap();
    let piped = Piped { pipe, ready: VecDeque::new(), deaf: Arc::clone(&deaf) };
    let (client, welcome) = Client::hello(piped, "test/0", None).unwrap();
    assert_eq!(welcome.server, env!("CARGO_PKG_VERSION"));
    (client, deaf)
}

fn samples_of(store: &Store, name: &str) -> usize {
    let read = store.metrics().unwrap().query().name(name).since(Duration::from_secs(3600)).read().unwrap();
    read.iter().map(|series| series.samples().len()).sum()
}

#[test]
fn series_written_through_a_server_read_back_from_its_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let (mut client, _) = welcomed(&store);
    let at = SystemTime::now() - Duration::from_secs(5);
    let rtt = Series::gauge("ping_rtt_ms").label("target", "poland").sample(at, 23.1).keep(Duration::from_secs(86_400));
    client.ingest(std::slice::from_ref(&rtt)).unwrap();
    client.ingest(&[rtt]).unwrap();
    assert_eq!(samples_of(&store, "ping_rtt_ms"), 1, "an ingest sent again changes nothing");
}

#[test]
fn a_refusal_keeps_its_kind_and_reads_as_the_server_said_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let (mut client, _) = welcomed(&store);
    let too_old = Series::gauge("cpu").sample(SystemTime::now() - Duration::from_secs(400 * 86_400), 0.4);
    let refused = client.ingest(&[too_old]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::TooOld, "{refused}");
    assert!(!refused.to_string().ends_with("too_old: too_old"), "{refused}");
    assert!(refused.to_string().contains("cpu"), "{refused}");
}

#[test]
fn a_write_whose_answer_was_lost_is_outcome_unknown_and_sent_again_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let (mut client, deaf) = welcomed(&store);
    let rtt = Series::gauge("ping_rtt_ms").sample(SystemTime::now() - Duration::from_secs(5), 23.1);
    deaf.store(true, Ordering::Release);
    let lost = client.ingest(std::slice::from_ref(&rtt)).unwrap_err();
    assert_eq!(lost.kind(), ErrorKind::OutcomeUnknown, "{lost}");
    let waited = Instant::now();
    while samples_of(&store, "ping_rtt_ms") == 0 && waited.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(samples_of(&store, "ping_rtt_ms"), 1, "the write committed though its answer was lost");
    let (mut again, _) = welcomed(&store);
    again.ingest(&[rtt]).unwrap();
    assert_eq!(samples_of(&store, "ping_rtt_ms"), 1);
}

#[cfg(feature = "records")]
fn events_of(store: &Store, stream: &str) -> Vec<crate::records::Event> {
    let events = store.events(stream).open().unwrap();
    events.list().since(Duration::from_secs(3600)).page().unwrap().events
}

#[cfg(feature = "records")]
#[test]
fn an_event_appended_through_a_server_is_written_once_by_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let (mut client, _) = welcomed(&store);
    let changed = || super::Event {
        name: "role.changed".to_owned(),
        fields: Some(r#"{"user":42,"role":"admin"}"#.to_owned()),
        id: Some("delivery-1".to_owned()),
        ..super::Event::default()
    };
    assert_eq!(client.append("audit", &[changed()]).unwrap(), 0);
    assert_eq!(client.append("audit", &[changed()]).unwrap(), 1, "the stream holds its id");
    let written = events_of(&store, "audit");
    assert_eq!(written.len(), 1);
    assert_eq!((written[0].name.as_str(), written[0].id.as_deref()), ("role.changed", Some("delivery-1")));
}

#[cfg(feature = "records")]
#[test]
fn another_programs_output_becomes_events_through_a_server_however_long() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let (mut client, _) = welcomed(&store);
    let output = "level=warn msg=\"slow request\" ms=1200\n{\"level\":30,\"msg\":\"ok\"}\nthe last line";
    let failed = |error: io::Error| crate::Error::io("the output", error);
    assert_eq!(client.append_lines("worker", &mut output.as_bytes(), failed).unwrap(), 0);
    let names: Vec<String> = events_of(&store, "worker").into_iter().map(|event| event.name).collect();
    assert_eq!(names, ["logfmt", "json", "log"]);
    // past a stream's credit, so that the server's grants must come back
    let long: String = (0..60_000).map(|line| format!("line {line} of a worker's long output, padded out\n")).collect();
    assert!(long.len() > 2 << 20);
    let dropped = client.append_lines("long", &mut long.as_bytes(), failed).unwrap();
    let kept = store.events("long").open().unwrap().list().since(Duration::from_secs(3600)).count().unwrap();
    assert_eq!(kept + dropped, 60_000);
}

#[test]
fn a_text_past_one_message_goes_up_in_pieces_and_is_written_whole() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let (mut client, _) = welcomed(&store);
    let pad = "p".repeat(80);
    let text: String = (0..30_000).map(|n| format!("wide{{n=\"{n}\",pad=\"{pad}\"}} {n}\n")).collect();
    assert!(text.len() > 2 << 20, "past the body a server agrees");
    let labels = crate::metrics::Labels::from([("instance".to_owned(), "web-1".to_owned())]);
    client.ingest_prometheus(text.as_bytes(), &labels, SystemTime::now(), None).unwrap();
    let names = store.metrics().unwrap().names().prefix("wide").page().unwrap().names;
    assert_eq!(names[0].series_count, 30_000);
    let broken = format!("{text}wide{{n=\"x\"}} not-a-number\n");
    let refused = client.ingest_prometheus(broken.as_bytes(), &labels, SystemTime::now(), None).unwrap_err();
    assert!(refused.to_string().contains("line 30001"), "{refused}");
}
