//! What `status --names` reads of each name: the count a program would give,
//! and bytes that follow what the name keeps.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::json;

use super::{Held, buckets_held, file_sets_held, indexes_held, queues_held, streams_held};
use crate::records::NewEvent;
use crate::{Options, Store, TestClock};

const HOUR: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

struct Fixture {
    store: Store,
    clock: Arc<TestClock>,
    // Last: fields drop in order, and Windows removes no file the store still holds.
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    Fixture { store, clock, _dir: dir }
}

fn named<'a>(held: &'a [Held], name: &str) -> &'a Held {
    held.iter().find(|held| held.name == name).unwrap_or_else(|| panic!("no {name} in {held:?}"))
}

#[test]
fn a_bucket_counts_its_live_keys_and_weighs_their_values() {
    let f = fixture();
    let sessions = f.store.bucket::<String>("sessions").open().unwrap();
    for i in 0..500 {
        sessions.set(format!("s{i}").as_str(), &"x".repeat(4000)).unwrap();
    }
    let pins = f.store.bucket::<Vec<u32>>("pins").open().unwrap();
    for i in 0..50 {
        pins.set(format!("u{i}").as_str(), &vec![2518, 4293]).unwrap();
    }
    pins.under("tenant-7").set("u1", &vec![1]).unwrap();
    pins.expire("u0", Duration::from_secs(60)).unwrap();
    f.clock.advance(Duration::from_secs(61));

    let held = buckets_held(&f.store).unwrap();
    assert_eq!(held.iter().map(|held| held.name.as_str()).collect::<Vec<_>>(), ["pins", "sessions"]);
    let (sessions, pins) = (named(&held, "sessions"), named(&held, "pins"));
    assert_eq!(sessions.count, 500);
    assert_eq!(pins.count, 50, "u0 expired, and the branch's u1 is a key of its own");
    assert!((500 * 4000..500 * 4100).contains(&sessions.bytes), "{sessions:?}: a spilled value's bytes");
    assert!(pins.bytes < 50 * 30, "{pins:?}");
}

#[test]
fn a_queue_counts_its_jobs_and_a_set_its_files() {
    let f = fixture();
    let mail = f.store.queue::<String>("mail").open().unwrap();
    for to in ["ann@example.com", "bob@example.com", "eve@example.com"] {
        mail.add(&to.to_owned()).unwrap();
    }
    let avatars = f.store.files("avatars").open().unwrap();
    avatars.put("users/42.png", [7u8; 300]).unwrap();
    avatars.put("users/9.png", [7u8; 100]).unwrap();
    avatars.put("users/old.png", [7u8; 50]).unwrap();
    avatars.delete("users/old.png").unwrap();

    let queues = queues_held(&f.store).unwrap();
    assert_eq!(named(&queues, "mail").count, 3);
    assert!(named(&queues, "mail").bytes >= 3 * 15, "{queues:?}");
    assert_eq!(file_sets_held(&f.store).unwrap(), [Held { name: "avatars".to_owned(), count: 2, bytes: 400 }]);
}

#[test]
fn a_stream_counts_its_events_in_its_head_and_once_packed() {
    let f = fixture();
    let audit = f.store.events("audit").keep(400 * DAY).open().unwrap();
    audit.append_all((0..2000).map(|user| NewEvent::new("signed_in").fields(&json!({ "userId": user })))).unwrap();
    let errors = f.store.events("client-errors").keep(90 * DAY).open().unwrap();
    errors.append("client.query", &json!({ "message": "x".repeat(2000) })).unwrap();
    f.store.events("quiet").open().unwrap();

    let in_heads = streams_held(&f.store).unwrap();
    assert_eq!(
        in_heads.iter().map(|held| (held.name.as_str(), held.count)).collect::<Vec<_>>(),
        [("audit", 2000), ("client-errors", 1)]
    );
    f.clock.advance(2 * HOUR);
    let packed = crate::records::maintain(&f.store).unwrap();
    assert_eq!(packed.packed, 2001);
    let in_segments = streams_held(&f.store).unwrap();
    assert_eq!(in_segments.iter().map(|held| held.count).collect::<Vec<_>>(), [2000, 1]);
    assert!(in_segments.iter().all(|held| held.bytes > 0), "{in_segments:?}");
    assert!(named(&in_segments, "audit").bytes > named(&in_segments, "client-errors").bytes, "{in_segments:?}");
}

#[test]
fn an_index_counts_its_documents_and_weighs_its_postings_in_the_head_and_packed() {
    let f = fixture();
    let courses = f.store.index::<serde_json::Value>("courses").field("name", 1.0).open().unwrap();
    let values: Vec<serde_json::Value> =
        (0..300).map(|i| json!({ "name": format!("Курс {i}: линейная алгебра и инженерная графика") })).collect();
    courses.set_many(values.iter().enumerate().map(|(i, value)| (i as u64, value))).unwrap();
    let names = f.store.index::<serde_json::Value>("names").field("name", 1.0).open().unwrap();
    names.set("ann", &json!({ "name": "Ann" })).unwrap();

    let in_head = indexes_held(&f.store).unwrap();
    assert_eq!(
        in_head.iter().map(|held| (held.name.as_str(), held.count)).collect::<Vec<_>>(),
        [("courses", 300), ("names", 1)]
    );
    let values_bytes: u64 = values.iter().map(|value| value.to_string().len() as u64).sum();
    crate::search::maintain(&f.store).unwrap();
    let packed = indexes_held(&f.store).unwrap();
    let (in_head, packed) = (named(&in_head, "courses").bytes, named(&packed, "courses").bytes);
    assert!(packed > values_bytes, "{packed} against values of {values_bytes}");
    // 47 KB in the head, 43 KB packed; 35 KB in the head when its rows counted nothing
    assert!(in_head > packed, "{in_head} in the head against {packed} packed: a head row weighs more than its posting");
}
