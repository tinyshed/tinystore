use crate::pipe::Capability;
use crate::pipe::fixture::Client;
use crate::wire::protocol::{
    Empty, Handle, RecordsAggregateAsked, RecordsAppend, RecordsBatch, RecordsCount, RecordsFieldNamesAsked,
    RecordsNames, RecordsNewEvent, RecordsOpen, RecordsPage, RecordsPollAsked, RecordsQuery, RecordsRows,
    RecordsStreams, RecordsTopAsked, RecordsTops, method,
};
use crate::{Options, Store};

const TRACE: [u8; 16] =
    [0x4b, 0xf9, 0x2f, 0x35, 0x77, 0xb3, 0x4d, 0xa6, 0xa3, 0xce, 0x92, 0x9d, 0x0e, 0x0e, 0x47, 0x36];
const SPAN: [u8; 8] = [0x00, 0xf0, 0x67, 0xaa, 0x0b, 0xa9, 0x02, 0xb7];

fn open(client: &mut Client, stream: &str) -> u64 {
    let open = RecordsOpen { stream: stream.to_owned(), ..RecordsOpen::default() };
    client.call::<Handle>(method::RECORDS_OPEN, &open).unwrap().handle
}

fn event(name: &str, fields: &str) -> RecordsNewEvent {
    RecordsNewEvent { name: name.to_owned(), fields: Some(fields.to_owned()), ..RecordsNewEvent::default() }
}

fn append(client: &mut Client, handle: u64, events: Vec<RecordsNewEvent>) {
    client.call::<Empty>(method::RECORDS_APPEND, &RecordsAppend { handle, events }).unwrap();
}

#[test]
fn an_appended_event_comes_back_whole_its_fields_spelled_as_they_were_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let audit = open(&mut client, "audit");
    let written = RecordsNewEvent {
        level: Some(4),
        message: Some("role changed".to_owned()),
        context: Some(r#"{"actor":7}"#.to_owned()),
        trace_id: Some(TRACE.to_vec()),
        span_id: Some(SPAN.to_vec()),
        ..event("role.changed", r#"{"user":42,"ratio":1.2300,"user":43}"#)
    };
    append(&mut client, audit, vec![written]);
    let page: RecordsPage = client.call(method::RECORDS_LIST, &RecordsQuery::default()).unwrap();
    let read = &page.events[0];
    assert_eq!((read.stream.as_str(), read.name.as_str(), read.level), ("audit", "role.changed", Some(4)));
    assert_eq!(read.fields.as_deref(), Some(r#"{"user":42,"ratio":1.2300,"user":43}"#), "a key written twice stays");
    assert_eq!(read.context.as_deref(), Some(r#"{"actor":7}"#));
    assert_eq!((read.trace_id.as_deref(), read.span_id.as_deref()), (Some(&TRACE[..]), Some(&SPAN[..])));
    assert_eq!(page.next, None);
}

#[test]
fn a_list_is_appended_whole_or_not_at_all_and_a_refused_event_names_its_place() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let clicks = open(&mut client, "clicks");
    let events = vec![event("view", r#"{"page":1}"#), event("", "{}")];
    let failed = client.call::<Empty>(method::RECORDS_APPEND, &RecordsAppend { handle: clicks, events }).unwrap_err();
    assert_eq!(failed.code, "invalid");
    assert!(failed.message.contains("event 1"), "{}", failed.message);
    let count: RecordsCount = client.call(method::RECORDS_COUNT, &RecordsQuery::default()).unwrap();
    assert_eq!(count.count, 0);
}

#[test]
fn a_query_reads_by_what_events_hold_and_counts_and_aggregates_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let api = open(&mut client, "api");
    let requests =
        (0..6).map(|n| event("request", &format!(r#"{{"route":"/{}","ms":{}}}"#, ["a", "b"][n % 2], n * 10)));
    append(&mut client, api, requests.collect());
    let at_a = RecordsQuery {
        handle: Some(api),
        r#where: [("route".to_owned(), r#""/a""#.to_owned())].into(),
        ..RecordsQuery::default()
    };
    let page: RecordsPage = client.call(method::RECORDS_LIST, &at_a).unwrap();
    assert_eq!(page.events.len(), 3);
    let count: RecordsCount = client.call(method::RECORDS_COUNT, &at_a).unwrap();
    assert_eq!(count.count, 3);
    let top: RecordsTops = client
        .call(method::RECORDS_TOP, &RecordsTopAsked { query: RecordsQuery::default(), key: "route".to_owned() })
        .unwrap();
    let values: Vec<(&str, u64)> = top.values.iter().map(|top| (top.value.as_str(), top.count)).collect();
    assert_eq!(values, [(r#""/a""#, 3), (r#""/b""#, 3)]);
    let asked = RecordsAggregateAsked {
        by: vec!["route".to_owned()],
        stats: vec!["ms".to_owned()],
        ..RecordsAggregateAsked::default()
    };
    let rows: RecordsRows = client.call(method::RECORDS_AGGREGATE, &asked).unwrap();
    let a = &rows.rows[0];
    assert_eq!((a.count, a.by["route"].as_str()), (3, r#""/a""#));
    let ms = &a.stats["ms"];
    assert_eq!((ms.samples, ms.sum.as_str(), ms.min.as_str(), ms.max.as_str(), ms.avg), (3, "60", "0", "40", 20.0));
}

#[test]
fn every_stream_says_what_it_keeps_and_the_keys_its_events_hold() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let open_keep = RecordsOpen { stream: "audit".to_owned(), keep: Some(30 * 24 * 3_600_000) };
    let audit = client.call::<Handle>(method::RECORDS_OPEN, &open_keep).unwrap().handle;
    append(&mut client, audit, vec![event("signup", r#"{"plan":"pro","user":42}"#)]);
    let streams: RecordsStreams = client.call(method::RECORDS_STREAMS, &Empty {}).unwrap();
    assert_eq!((streams.streams[0].name.as_str(), streams.streams[0].keep), ("audit", 30 * 24 * 3_600_000));
    let asked = RecordsFieldNamesAsked { prefix: Some("p".to_owned()), ..RecordsFieldNamesAsked::default() };
    let names: RecordsNames = client.call(method::RECORDS_FIELD_NAMES, &asked).unwrap();
    assert_eq!(names.names, ["plan"]);
}

#[test]
fn a_poll_from_no_cursor_finds_nothing_packed_yet_and_gives_a_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let audit = open(&mut client, "audit");
    append(&mut client, audit, vec![event("signup", "{}")]);
    let batch: RecordsBatch = client.call(method::RECORDS_POLL, &RecordsPollAsked::default()).unwrap();
    assert!(batch.events.is_empty(), "poll follows what is packed");
    assert!(!batch.cursor.is_empty());
    let refused = client.call::<RecordsBatch>(
        method::RECORDS_POLL,
        &RecordsPollAsked { limit: Some(0), ..RecordsPollAsked::default() },
    );
    assert_eq!(refused.unwrap_err().code, "invalid");
}

#[test]
fn a_read_connection_reads_and_gives_no_stream_a_keep_nor_appends() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let mut client = Client::admitted(&store, Capability::Read);
    let audit = open(&mut client, "audit");
    let page: RecordsPage =
        client.call(method::RECORDS_LIST, &RecordsQuery { handle: Some(audit), ..RecordsQuery::default() }).unwrap();
    assert!(page.events.is_empty());
    let keep = RecordsOpen { stream: "audit".to_owned(), keep: Some(3_600_000) };
    assert_eq!(client.call::<Handle>(method::RECORDS_OPEN, &keep).unwrap_err().code, "permission");
    let append = RecordsAppend { handle: audit, events: vec![event("signup", "{}")] };
    assert_eq!(client.call::<Empty>(method::RECORDS_APPEND, &append).unwrap_err().code, "permission");
}
