use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::json;

use super::super::block;
use super::super::bloom::may_hold;
use super::super::fixture::{Fixture, carried_by, fixture, released};
use super::super::head::Codec;
use super::super::maintain::maintain;
use super::super::segment::Schema;
use crate::records::{AllEvents, Event, NewEvent, Not, SpanId, TraceId};
use crate::{Error, ErrorKind};

const HOUR: Duration = Duration::from_secs(60 * 60);

fn shown(event: &Event) -> String {
    format!("{}/{}", event.stream, event.name)
}

/// Every event of every stream as a read gives them.
fn read(all: &AllEvents) -> Vec<String> {
    all.list().all().map(|event| shown(&event.unwrap())).collect()
}

/// Every event polls from `cursor` give.
fn drain(all: &AllEvents, mut cursor: Option<String>) -> Vec<String> {
    let mut given = Vec::new();
    loop {
        let batch = all.poll(cursor.as_deref()).unwrap();
        if batch.events.is_empty() {
            return given;
        }
        given.extend(batch.events.iter().map(shown));
        cursor = Some(batch.cursor);
    }
}

/// Each cursor a poll of one event a batch hands out, the first none, with
/// every event polls from it give.
fn from_every_cursor(all: &AllEvents) -> Vec<(Option<String>, Vec<String>)> {
    let mut cursors = vec![None];
    loop {
        let batch = all.poll_limit(cursors.last().unwrap().as_deref(), 1).unwrap();
        if batch.events.is_empty() {
            break;
        }
        cursors.push(Some(batch.cursor));
    }
    cursors.into_iter().map(|cursor| (cursor.clone(), drain(all, cursor))).collect()
}

/// That polls from each cursor taken before give what they gave then, less
/// `deleted`, each once and in its order, and after it only events packed
/// since, `packed` less `deleted`.
fn polls_kept(before: &[(Option<String>, Vec<String>)], all: &AllEvents, deleted: &[&str], packed: &[String]) {
    let gone = |name: &String| deleted.contains(&name.as_str());
    let new: BTreeSet<&String> = packed.iter().filter(|name| !gone(name)).collect();
    for (cursor, rest) in before {
        let left: Vec<&String> = rest.iter().filter(|name| !gone(name)).collect();
        let given = drain(all, cursor.clone());
        assert_eq!(given.len(), left.len() + new.len(), "each once, from {cursor:?}");
        assert_eq!(given.iter().take(left.len()).collect::<Vec<_>>(), left, "from {cursor:?}");
        assert_eq!(given[left.len()..].iter().collect::<BTreeSet<_>>(), new, "packed since, from {cursor:?}");
    }
}

/// What `deleted` leaves of `events`, in their order.
fn less(events: &[String], deleted: &[&str]) -> Vec<String> {
    events.iter().filter(|name| !deleted.contains(&name.as_str())).cloned().collect()
}

/// Every block's segment, id, body and the row of its segment.
fn blocks(fixture: &Fixture) -> Vec<(i64, i64, Vec<u8>, Vec<u8>)> {
    let sql = "select b.segment, b.id, b.body, s.body from blocks b join segments s on s.id = b.segment order by b.id";
    let found = |connection: &rusqlite::Connection| {
        let mut select = connection.prepare(sql).unwrap();
        let rows = select.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)));
        Ok(rows.unwrap().map(Result::unwrap).collect())
    };
    fixture.records().file.read(found).unwrap()
}

/// Writes each block of `segment` again as `version`, which a release
/// before wrote.
fn write_as(fixture: &Fixture, segment: i64, version: u8) {
    let mut codec = Codec::default();
    for (_, id, body, row) in blocks(fixture).into_iter().filter(|(of, ..)| *of == segment) {
        let old = block::written_as(&Schema::parse(&row, &mut codec).unwrap(), &body, version);
        let write = move |tx: &crate::sqlite::Tx<'_>| {
            tx.execute("update blocks set body = ?1 where id = ?2", rusqlite::params![old, id]).unwrap();
            Ok(())
        };
        fixture.records().file.write(0, write).unwrap();
    }
}

/// The version of each segment's blocks.
fn versions(fixture: &Fixture) -> BTreeSet<(i64, u8)> {
    blocks(fixture).into_iter().map(|(segment, _, body, _)| (segment, body[0])).collect()
}

// contract: REC-21, NOT-12
#[test]
fn a_delete_moves_no_cursor_and_no_order_of_what_v0_1_0_packed_and_merged() {
    let fixture = released();
    let all = fixture.store.all_events().unwrap();
    assert!(versions(&fixture).iter().all(|(_, version)| *version == block::VERSION_2));
    let (before, read_before) = (from_every_cursor(&all), read(&all));
    let polled: BTreeSet<String> = drain(&all, None).into_iter().collect();
    let waited: Vec<String> = read_before.iter().filter(|name| !polled.contains(*name)).cloned().collect();
    // of a merged segment, of a segment of its own, and of what waits
    let deleted = ["api/api-1-a", "api/late-0", "web/web-2", "web/wait-web"];
    let named = deleted.map(|name| name.split_once('/').unwrap().1);
    let gone = all.list().names(named).filter("user", Not(0)).delete().unwrap();
    assert_eq!(gone, 4);
    assert_eq!(read(&all), less(&read_before, &deleted), "events of one time keep v0.1.0's order");
    polls_kept(&before, &all, &deleted, &waited);
    // its merges carry the holes on
    let mut appended = waited;
    for round in 0..3 {
        let name = format!("more-{round}");
        fixture.events("api").append(&name, &()).unwrap();
        appended.push(format!("api/{name}"));
        fixture.pass(HOUR);
        maintain(&fixture.store).unwrap();
    }
    polls_kept(&before, &all, &deleted, &appended);
    let read_now: Vec<String> = read(&all).into_iter().filter(|name| !name.starts_with("api/more-")).collect();
    assert_eq!(read_now, less(&read_before, &deleted));
}

// contract: REC-21, STORE-11
#[test]
fn a_delete_moves_no_cursor_of_blocks_of_versions_3_4_and_5_and_its_merges_carry_the_holes() {
    let fixture = fixture();
    let api = fixture.events("api");
    for round in 0..6 {
        let event = |n: usize| {
            let event = NewEvent::new(&format!("r{round}-{n}")).fields(&json!({ "user": n % 3 }));
            // v0.1.1 wrote no id
            if round >= 4 { event.id(format!("r{round}-{n}")) } else { event }
        };
        api.append_all((0..3).map(event)).unwrap();
        fixture.pass(HOUR);
        maintain(&fixture.store).unwrap();
    }
    // rounds 0 to 3 merged into segment 1, 4 and 5 are segments 5 and 6
    write_as(&fixture, 1, block::VERSION_3);
    write_as(&fixture, 5, block::VERSION_4);
    let written = BTreeSet::from([(1, block::VERSION_3), (5, block::VERSION_4), (6, block::VERSION)]);
    assert_eq!(versions(&fixture), written);
    let all = fixture.store.all_events().unwrap();
    let (before, read_before) = (from_every_cursor(&all), read(&all));
    assert_eq!(api.list().filter("user", &1).delete().unwrap(), 6);
    let deleted: Vec<String> = (0..6).map(|round| format!("api/r{round}-1")).collect();
    let deleted: Vec<&str> = deleted.iter().map(String::as_str).collect();
    assert_eq!(read(&all), less(&read_before, &deleted));
    polls_kept(&before, &all, &deleted, &[]);
    assert!(versions(&fixture).iter().all(|(_, version)| *version == block::VERSION));
    // segments 5 and 6, which hold holes, merge with two more into 5
    let mut appended = Vec::new();
    for round in 6..8 {
        api.append_all((0..3).map(|n| NewEvent::new(&format!("r{round}-{n}")))).unwrap();
        appended.extend((0..3).map(|n| format!("api/r{round}-{n}")));
        fixture.pass(HOUR);
        maintain(&fixture.store).unwrap();
    }
    let holds = |connection: &rusqlite::Connection| {
        let sql =
            "select held, (select sum(erased) from segments where id = 5 or holder = 5) from segments where id = 5";
        Ok(connection.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap())
    };
    let holds: (i64, i64) = fixture.records().file.read(holds).unwrap();
    assert_eq!(holds, (12, 2), "four segments of three slots, two of them holes");
    polls_kept(&before, &all, &deleted, &appended);
    assert_eq!(read(&all), [less(&read_before, &deleted), appended].concat());
    // and keep removes them as it would have, a poll behind counting the events and not the holes
    fixture.pass(15 * 24 * HOUR);
    api.append("after", &()).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    let batch = all.poll(Some("1:0")).unwrap();
    assert_eq!((batch.events.len(), batch.expired), (1, 18));
}

/// Events of two users, each with fields, a context and an id, packed and
/// waiting: user 1's with a key and a context of their own, and the
/// stream's newest event.
fn two_users(fixture: &Fixture) {
    let activity = fixture.events("activity");
    let event = |user: u64, n: u64| {
        let request = format!("req-{user}-{n:04}");
        let (fields, context) = match user {
            1 => (json!({ "user": 1, "request": request, "secret": n }), json!({ "device": "d-1" })),
            _ => (json!({ "user": user, "request": request, "ms": n }), json!({ "tenant": 7 })),
        };
        NewEvent::new("request").fields(&fields).context(&context).id(format!("{user}-{n}"))
    };
    activity.append_all((0..20).flat_map(|n| [event(2, n), event(1, n)])).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    activity.append_all((20..25).flat_map(|n| [event(2, n), event(1, n)])).unwrap();
    fixture.pass(Duration::from_secs(1));
    activity.append_all([event(1, 25)]).unwrap();
}

// contract: REC-20, REC-11
#[test]
fn after_a_delete_nothing_finds_a_deleted_event_or_a_value_only_it_held() {
    let fixture = fixture();
    two_users(&fixture);
    let all = fixture.store.all_events().unwrap();
    let activity = fixture.events("activity");
    let newest_before = all.streams().unwrap()[0].newest;
    assert_eq!(all.list().streams(["activity"]).filter("user", &1).delete().unwrap(), 26);
    assert!(all.list().filter("user", &1).page().unwrap().events.is_empty());
    let left: Vec<Event> = activity.list().all().map(Result::unwrap).collect();
    assert_eq!(left.len(), 25);
    assert!(left.iter().all(|event| event.fields.get::<u64>("user").unwrap() == Some(2)));
    assert_eq!(activity.list().count().unwrap(), 25);
    assert_eq!(activity.list().filter("device", "d-1").count().unwrap(), 0);
    let tops: Vec<serde_json::Value> = activity.list().top("user").unwrap().into_iter().map(|top| top.value).collect();
    assert_eq!(tops, [json!(2)]);
    // the keys and their counts, a context's among them
    let fields = all.fields("activity").unwrap();
    assert_eq!(fields, carried_by(&left));
    assert!(!fields.fields.iter().any(|key| key.name == "secret"));
    assert!(!fields.context.iter().any(|key| key.name == "device"));
    assert!(!activity.list().field_names().unwrap().contains(&"secret".to_owned()));
    // the newest event was user 1's
    let newest = all.streams().unwrap()[0].newest;
    assert!(newest < newest_before);
    assert_eq!(newest, left.last().unwrap().at);
    // the filters of the blocks of a request's id
    let filters = |connection: &rusqlite::Connection| {
        let mut select = connection.prepare("select bloom from block_filters where key = 'request'").unwrap();
        Ok(select.query_map([], |row| row.get::<_, Vec<u8>>(0)).unwrap().map(Result::unwrap).collect::<Vec<_>>())
    };
    let filters = fixture.records().file.read(filters).unwrap();
    assert!(!filters.is_empty());
    for n in [0, 7, 19] {
        let value = format!("\"req-1-{n:04}\"");
        assert!(filters.iter().all(|bloom| !may_hold(bloom, value.as_bytes())), "{value}");
    }
    // an id stays held till keep passes its time: sent again, it writes nothing
    let again = NewEvent::new("request").fields(&json!({ "user": 1 })).id("1-3");
    assert_eq!(activity.append_all([again]).unwrap(), 0);
    // and a delete run again finds nothing more
    assert_eq!(activity.list().filter("user", &1).delete().unwrap(), 0);
}

/// Sixteen bytes no compressor shortens, which a block keeps as they are: a
/// trace id.
fn marker(seed: u64) -> [u8; 16] {
    let mut state = seed.wrapping_add(1);
    std::array::from_fn(|_| {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (state >> 56) as u8
    })
}

/// Whether records.db or its WAL holds `bytes`.
fn on_disk(fixture: &Fixture, bytes: &[u8]) -> bool {
    let dir = fixture.dir.path();
    ["records.db", "records.db-wal"].iter().any(|name| {
        let file = std::fs::read(dir.join(name)).unwrap_or_default();
        file.windows(bytes.len()).any(|window| window == bytes)
    })
}

// contract: REC-20
#[test]
fn a_delete_leaves_no_byte_of_what_it_deleted_in_the_file_or_its_wal() {
    let fixture = fixture();
    let notes = fixture.events("notes");
    let note =
        |n: u64| NewEvent::new("note").fields(&json!({ "user": n % 2 })).trace(TraceId(marker(n)), SpanId([1; 8]));
    // two rounds packed, their head rows freed, and one waiting
    for round in 0..3 {
        notes.append_all((round * 4..round * 4 + 4).map(note)).unwrap();
        if round < 2 {
            fixture.pass(HOUR);
            maintain(&fixture.store).unwrap();
        }
    }
    assert!((0..8).all(|n| on_disk(&fixture, &marker(n))), "a block holds each as it is");
    assert_eq!(notes.list().filter("user", &1).delete().unwrap(), 6);
    for n in (0..12).filter(|n| n % 2 == 1) {
        assert!(!on_disk(&fixture, &marker(n)), "event {n}");
    }
    assert!((0..12).filter(|n| n % 2 == 0).all(|n| on_disk(&fixture, &marker(n))), "what is left stays");
}

#[test]
fn a_delete_takes_a_where_and_no_search_order_limit_or_after() {
    let fixture = fixture();
    let api = fixture.events("api");
    api.append("signed.in", &json!({ "user": 1 })).unwrap();
    let all = fixture.store.all_events().unwrap();
    let refused = |deleted: crate::Result<u64>| deleted.unwrap_err();
    let cases = [
        (
            refused(api.list().delete()),
            "events api: a delete without a where, which would delete every event of its range: invalid",
        ),
        (
            refused(api.list().filter("user", &1).search("in").delete()),
            "events api: a delete takes no search: a phrase may find more than a person's events: invalid",
        ),
        (
            refused(api.list().filter("user", &1).newest().delete()),
            "events api: a delete takes no newest, limit or after: it deletes every event it finds: invalid",
        ),
        (
            refused(api.list().filter("user", &1).limit(5).delete()),
            "events api: a delete takes no newest, limit or after: it deletes every event it finds: invalid",
        ),
        (
            refused(api.list().filter("user", &1).streams(["web"]).delete()),
            "events api: streams, which a read of every stream takes: a stream reads its own: invalid",
        ),
    ];
    for (error, said) in cases {
        assert_eq!((error.kind(), error.to_string()), (ErrorKind::Invalid, said.to_owned()));
    }
    assert_eq!(refused(all.list().filter("user", &[0_u8; 0]).delete()).kind(), ErrorKind::Invalid);
    assert_eq!(all.list().filter("user", &1).streams(["nowhere"]).delete().unwrap(), 0);
    assert_eq!(api.list().filter("user", &1).count().unwrap(), 1, "nothing was deleted");
    fixture.store.close().unwrap();
    let closed: Error = api.list().filter("user", &1).delete().unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed);
}
