use crate::pipe::Capability;
use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::Message;
use crate::wire::frame::{Frame, Kind};
use crate::wire::protocol::{
    Empty, Handle, RecordsAggregateAsked, RecordsAppend, RecordsBatch, RecordsCount, RecordsDamage, RecordsDamages,
    RecordsDropped, RecordsFieldNamesAsked, RecordsNames, RecordsNewEvent, RecordsOpen, RecordsPage, RecordsPiece,
    RecordsPollAsked, RecordsQuery, RecordsRows, RecordsStreams, RecordsTailBatch, RecordsTopAsked, RecordsTops,
    method,
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
    let lines = client.call::<RecordsDropped>(method::RECORDS_LINES, &Handle { handle: audit });
    assert_eq!(lines.unwrap_err().code, "permission");
    let damage = RecordsDamage { part: "head row".to_owned(), id: 1, ..RecordsDamage::default() };
    assert_eq!(client.call::<Empty>(method::RECORDS_DROP, &damage).unwrap_err().code, "permission");
}

/// Opens a tail of what `asked` finds, the stream open once its RESPONSE came.
fn tail(client: &mut Client, asked: &RecordsQuery) -> u32 {
    let stream = client.start(method::RECORDS_TAIL, asked);
    let begun = client.next_on(stream);
    assert_eq!((begun.kind, begun.flags), (Kind::Response, 0), "the tail stays open: {:?}", answered::<Empty>(&begun));
    stream
}

fn batch(client: &mut Client, stream: u32) -> RecordsTailBatch {
    let frame = client.next_on(stream);
    assert_eq!((frame.kind, frame.flags), (Kind::Data, 0), "a batch, the tail open");
    RecordsTailBatch::decode(&frame.body).unwrap()
}

#[test]
fn a_tail_sends_the_events_appended_after_it_that_its_query_finds_until_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let audit = open(&mut client, "audit");
    append(&mut client, audit, vec![event("signup", r#"{"user":1}"#)]);
    let stream = tail(&mut client, &RecordsQuery { names: vec!["signup".to_owned()], ..RecordsQuery::default() });
    append(&mut client, audit, vec![event("view", "{}"), event("signup", r#"{"user":42}"#)]);
    let first = batch(&mut client, stream);
    let fields: Vec<Option<&str>> = first.events.iter().map(|event| event.fields.as_deref()).collect();
    assert_eq!((fields, first.missed), (vec![Some(r#"{"user":42}"#)], 0), "what came after that the query finds");
    client.write(Frame::new(Kind::Cancel, stream, Vec::new()));
    assert_eq!(answered::<Empty>(&client.next_on(stream)).unwrap_err().code, "cancelled");
    let limited = RecordsQuery { limit: Some(1), ..RecordsQuery::default() };
    assert_eq!(client.call::<Empty>(method::RECORDS_TAIL, &limited).unwrap_err().code, "invalid", "no limit");
}

#[test]
fn a_tail_slower_than_the_writers_says_how_many_left_the_hold_before_it_read_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let audit = open(&mut client, "audit");
    let stream = tail(&mut client, &RecordsQuery::default());
    // one write past the megabyte held: its oldest leave before the tail reads
    let padding = "x".repeat(1000);
    let big = (0..1500).map(|n| event("big", &format!(r#"{{"n":{n},"pad":"{padding}"}}"#)));
    append(&mut client, audit, big.collect());
    let (mut missed, mut seen) = (0, Vec::new());
    while missed + seen.len() < 1500 {
        let read = batch(&mut client, stream);
        missed += usize::try_from(read.missed).unwrap();
        seen.extend(read.events);
    }
    assert!(missed > 0, "a megabyte is held");
    let first = seen[0].fields.as_deref().unwrap();
    assert!(first.starts_with(&format!(r#"{{"n":{missed},"#)), "the first read follows those missed: {first}");
}

// contract: REC-10
#[test]
fn a_writers_pieces_become_events_however_its_lines_were_cut() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let worker = open(&mut client, "worker");
    let stream = client.open_stream(method::RECORDS_LINES, &Handle { handle: worker });
    let begun = client.next_on(stream);
    assert_eq!((begun.kind, begun.flags), (Kind::Response, 0), "a writer begins: {:?}", answered::<Empty>(&begun));
    for piece in ["level=warn msg=\"slow request\" ms=12", "00\n{\"level\":30,\"msg\":\"ok\"}\nthe last line"] {
        let bytes = piece.as_bytes().to_vec();
        client.write(Frame::new(Kind::Data, stream, RecordsPiece { bytes }.encode()));
    }
    client.write(Frame::new(Kind::Data, stream, Vec::new()).ending());
    assert_eq!(answered::<RecordsDropped>(&client.next_on(stream)).unwrap().dropped, 0);
    let page: RecordsPage = client.call(method::RECORDS_LIST, &RecordsQuery::default()).unwrap();
    let names: Vec<(&str, Option<i64>)> = page.events.iter().map(|event| (event.name.as_str(), event.level)).collect();
    assert_eq!(names, [("logfmt", Some(4)), ("json", Some(0)), ("log", None)]);
    assert_eq!(page.events[2].message.as_deref(), Some("the last line"), "handed over without its newline");
}

#[test]
fn what_no_longer_reads_is_listed_and_dropped_and_what_still_reads_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let audit = open(&mut client, "audit");
    append(&mut client, audit, vec![event("signup", "{}")]);
    let reads =
        RecordsDamage { part: "head row".to_owned(), id: 1, stream: "audit".to_owned(), ..RecordsDamage::default() };
    assert_eq!(client.call::<Empty>(method::RECORDS_DROP, &reads).unwrap_err().code, "invalid", "it still reads");

    let file = rusqlite::Connection::open(dir.path().join("records.db")).unwrap();
    file.execute("update heads set body = zeroblob(length(body))", []).unwrap();
    let failed = client.call::<RecordsPage>(method::RECORDS_LIST, &RecordsQuery::default()).unwrap_err();
    assert_eq!(failed.code, "corrupt");
    let damaged: RecordsDamages = client.call(method::RECORDS_DAMAGED, &Empty {}).unwrap();
    let [damage] = damaged.damages.as_slice() else { panic!("{damaged:?}") };
    assert!(failed.message.contains(&format!("head row {} of audit", damage.id)), "{}", failed.message);
    client.call::<Empty>(method::RECORDS_DROP, damage).unwrap();
    let page: RecordsPage = client.call(method::RECORDS_LIST, &RecordsQuery::default()).unwrap();
    assert!(page.events.is_empty());
}
