use crate::pipe::Capability;
use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::Message;
use crate::wire::frame::{Frame, Kind};
use crate::wire::protocol::{
    Empty, Handle, RecordsAggregateAsked, RecordsAppend, RecordsAppended, RecordsBatch, RecordsCondition, RecordsCount,
    RecordsDamage, RecordsDamages, RecordsDropped, RecordsEvents, RecordsFieldNamesAsked, RecordsFieldsAsked,
    RecordsLargestAsked, RecordsNames, RecordsNewEvent, RecordsOpen, RecordsPage, RecordsPiece, RecordsPollAsked,
    RecordsQuery, RecordsRows, RecordsStreamFields, RecordsStreams, RecordsTailBatch, RecordsTopAsked, RecordsTops,
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

// contract: REC-11
#[test]
fn an_append_answers_how_many_events_it_held_and_a_read_gives_each_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let deploys = open(&mut client, "deploys");
    let push =
        |id: Option<&str>| RecordsNewEvent { id: id.map(str::to_owned), ..event("push", r#"{"sha":"26a0865"}"#) };
    let mut held = |events: Vec<RecordsNewEvent>| {
        let append = RecordsAppend { handle: deploys, events };
        client.call::<RecordsAppended>(method::RECORDS_APPEND, &append).unwrap().held
    };
    assert_eq!(held(vec![push(Some("d-1")), push(None)]), 0);
    assert_eq!(held(vec![push(Some("d-1")), push(Some("d-2")), push(Some("d-2"))]), 2);
    let page: RecordsPage = client.call(method::RECORDS_LIST, &RecordsQuery::default()).unwrap();
    let ids: Vec<Option<&str>> = page.events.iter().map(|event| event.id.as_deref()).collect();
    assert_eq!(ids, [Some("d-1"), None, Some("d-2")]);
    // an append that held none answers what v0.1.1 answered, an empty map
    assert_eq!(RecordsAppended { held: 0 }.encode(), Empty {}.encode());
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

// contract: REC-18
#[test]
fn where_finds_a_value_itself_and_conditions_a_list_none_of_one_or_a_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let api = open(&mut client, "api");
    let calls = [r#"{"op":"review.list"}"#, r#"{"op":"drafts.get"}"#, r#"{"op":["review.list","drafts.get"]}"#, "{}"];
    append(&mut client, api, calls.iter().map(|fields| event("call", fields)).collect());
    let mut count = |r#where: &[(&str, &str)], conditions: &[(&str, RecordsCondition)]| {
        let query = RecordsQuery {
            handle: Some(api),
            r#where: r#where.iter().map(|(key, json)| (key.to_string(), json.to_string())).collect(),
            conditions: conditions.iter().map(|(key, condition)| (key.to_string(), condition.clone())).collect(),
            ..RecordsQuery::default()
        };
        client.call::<RecordsCount>(method::RECORDS_COUNT, &query).map(|count| count.count)
    };
    let listed = |values: &[&str], not: bool| RecordsCondition {
        values: values.iter().map(|value| value.to_string()).collect(),
        not,
        ..RecordsCondition::default()
    };
    // where keeps the meaning an older client gave it: the list itself
    assert_eq!(count(&[("op", r#"["review.list","drafts.get"]"#)], &[]).unwrap(), 1);
    assert_eq!(count(&[], &[("op", listed(&[r#""review.list""#, r#""drafts.get""#], false))]).unwrap(), 2);
    assert_eq!(count(&[], &[("op", listed(&[r#""drafts.get""#], true))]).unwrap(), 3);
    let prefix = RecordsCondition { prefix: Some("drafts.".to_owned()), ..RecordsCondition::default() };
    assert_eq!(count(&[], &[("op", prefix.clone())]).unwrap(), 1);
    for refused in [
        count(&[("op", r#""drafts.get""#)], &[("op", listed(&[r#""x""#], false))]),
        count(&[], &[("op", listed(&[], false))]),
        count(&[], &[("op", RecordsCondition { not: true, ..prefix })]),
    ] {
        assert_eq!(refused.unwrap_err().code, "invalid");
    }
}

// contract: REC-19
#[test]
fn largest_answers_the_events_of_a_keys_largest_numbers_and_refuses_a_limit_past_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let pages = open(&mut client, "pages");
    let opens = [120, 9000, 300].map(|ms| event("page-open", &format!(r#"{{"ms":{ms},"path":"/p{ms}"}}"#)));
    append(&mut client, pages, opens.into());
    let mut largest = |limit: u64| {
        let query = RecordsQuery { handle: Some(pages), limit: Some(limit), ..RecordsQuery::default() };
        client.call::<RecordsEvents>(method::RECORDS_LARGEST, &RecordsLargestAsked { query, key: "ms".to_owned() })
    };
    let events = largest(2).unwrap().events;
    let fields: Vec<&str> = events.iter().filter_map(|event| event.fields.as_deref()).collect();
    assert_eq!(fields, [r#"{"ms":9000,"path":"/p9000"}"#, r#"{"ms":300,"path":"/p300"}"#]);
    assert_eq!(largest(1001).unwrap_err().code, "invalid");
}

// contract: REC-20
#[test]
fn a_delete_answers_how_many_it_deleted_once_and_none_sent_again_and_takes_a_where() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let activity = open(&mut client, "activity");
    let users = [r#"{"userId":97516}"#, r#"{"userId":97517}"#, r#"{"userId":97516}"#];
    append(&mut client, activity, users.iter().map(|fields| event("click", fields)).collect());
    let of = |handle: Option<u64>, user: Option<&str>| RecordsQuery {
        handle,
        streams: if handle.is_none() { vec!["activity".to_owned()] } else { Vec::new() },
        r#where: user.map(|user| ("userId".to_owned(), user.to_owned())).into_iter().collect(),
        ..RecordsQuery::default()
    };
    let mut delete = |query: &RecordsQuery| client.call::<RecordsCount>(method::RECORDS_DELETE, query);
    assert_eq!(delete(&of(None, Some("97516"))).unwrap().count, 2);
    assert_eq!(delete(&of(None, Some("97516"))).unwrap().count, 0, "a delete sent again deletes nothing more");
    assert_eq!(delete(&of(Some(activity), None)).unwrap_err().code, "invalid");
    assert_eq!(
        delete(&RecordsQuery { limit: Some(1), ..of(Some(activity), Some("97517")) }).unwrap_err().code,
        "invalid"
    );
    let left = client.call::<RecordsCount>(method::RECORDS_COUNT, &of(Some(activity), None)).unwrap();
    assert_eq!(left.count, 1);
}

// contract: REC-16, REC-17
#[test]
fn an_aggregate_given_fill_answers_every_span_and_a_calendars_rows_their_date() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let api = open(&mut client, "api");
    append(&mut client, api, vec![event("call", r#"{"op":"a"}"#)]);
    let hours = RecordsAggregateAsked {
        query: RecordsQuery { handle: Some(api), since: Some(6 * 3_600_000), ..RecordsQuery::default() },
        every: Some(3_600_000),
        fill: true,
        ..RecordsAggregateAsked::default()
    };
    let rows: RecordsRows = client.call(method::RECORDS_AGGREGATE, &hours).unwrap();
    assert!((6..=7).contains(&rows.rows.len()), "{} rows", rows.rows.len());
    assert_eq!(rows.rows.iter().map(|row| row.count).sum::<u64>(), 1);
    assert!(rows.rows.iter().all(|row| row.date.is_none()));
    let days = RecordsAggregateAsked {
        calendar: Some("day".to_owned()),
        time_zone: Some("Europe/Moscow".to_owned()),
        every: None,
        ..hours.clone()
    };
    let rows: RecordsRows = client.call(method::RECORDS_AGGREGATE, &days).unwrap();
    assert!(rows.rows.iter().all(|row| row.date.as_ref().is_some_and(|date| date.len() == 10)), "{:?}", rows.rows);
    let unbounded = RecordsAggregateAsked { query: RecordsQuery { since: None, ..hours.query.clone() }, ..hours };
    assert_eq!(client.call::<RecordsRows>(method::RECORDS_AGGREGATE, &unbounded).unwrap_err().code, "invalid");
}

#[test]
fn every_stream_says_what_it_keeps_and_the_keys_its_events_hold() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let open_keep =
        RecordsOpen { stream: "audit".to_owned(), keep: Some(30 * 24 * 3_600_000), ..RecordsOpen::default() };
    let audit = client.call::<Handle>(method::RECORDS_OPEN, &open_keep).unwrap().handle;
    append(&mut client, audit, vec![event("signup", r#"{"plan":"pro","user":42}"#)]);
    let streams: RecordsStreams = client.call(method::RECORDS_STREAMS, &Empty {}).unwrap();
    assert_eq!((streams.streams[0].name.as_str(), streams.streams[0].keep), ("audit", 30 * 24 * 3_600_000));
    let asked = RecordsFieldNamesAsked { prefix: Some("p".to_owned()), ..RecordsFieldNamesAsked::default() };
    let names: RecordsNames = client.call(method::RECORDS_FIELD_NAMES, &asked).unwrap();
    assert_eq!(names.names, ["plan"]);
}

// contract: REC-14
#[test]
fn a_stream_says_which_names_and_fields_its_events_carry_and_a_name_no_stream_could_have_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let orders = open(&mut client, "orders");
    let paid = |order: u32| event("order.paid", &format!(r#"{{"order_id":{order},"amount":12.5,"note":null}}"#));
    append(&mut client, orders, (0..3).map(paid).chain([event("order.paid", r#"{"amount":"12"}"#)]).collect());
    let asked = RecordsFieldsAsked { stream: "orders".to_owned() };
    let carried: RecordsStreamFields = client.call(method::RECORDS_FIELDS, &asked).unwrap();
    let names: Vec<(&str, u64)> = carried.event_names.iter().map(|name| (name.name.as_str(), name.count)).collect();
    assert_eq!(names, [("order.paid", 4)]);
    let fields: Vec<(&str, Vec<&str>, u64)> = carried
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.kinds.iter().map(String::as_str).collect(), field.count))
        .collect();
    let expected = [("amount", vec!["float", "text"], 4), ("note", vec!["null"], 3), ("order_id", vec!["integer"], 3)];
    assert_eq!(fields, expected);
    assert!(carried.context.is_empty());
    let none: RecordsStreamFields =
        client.call(method::RECORDS_FIELDS, &RecordsFieldsAsked { stream: "none".to_owned() }).unwrap();
    assert_eq!(none, RecordsStreamFields::default());
    let refused = client.call::<RecordsStreamFields>(method::RECORDS_FIELDS, &RecordsFieldsAsked::default());
    assert_eq!(refused.unwrap_err().code, "invalid");
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
    // a read connection opens only a stream the store holds
    store.events("audit").open().unwrap();
    let mut client = Client::admitted(&store, Capability::Read);
    let audit = open(&mut client, "audit");
    let page: RecordsPage =
        client.call(method::RECORDS_LIST, &RecordsQuery { handle: Some(audit), ..RecordsQuery::default() }).unwrap();
    assert!(page.events.is_empty());
    let keep = RecordsOpen { stream: "audit".to_owned(), keep: Some(3_600_000), ..RecordsOpen::default() };
    assert_eq!(client.call::<Handle>(method::RECORDS_OPEN, &keep).unwrap_err().code, "permission");
    let append = RecordsAppend { handle: audit, events: vec![event("signup", "{}")] };
    assert_eq!(client.call::<Empty>(method::RECORDS_APPEND, &append).unwrap_err().code, "permission");
    let lines = client.call::<RecordsDropped>(method::RECORDS_LINES, &Handle { handle: audit });
    assert_eq!(lines.unwrap_err().code, "permission");
    let damage = RecordsDamage { part: "head row".to_owned(), id: 1, ..RecordsDamage::default() };
    assert_eq!(client.call::<Empty>(method::RECORDS_DROP, &damage).unwrap_err().code, "permission");
    let mut delete = RecordsQuery { handle: Some(audit), ..RecordsQuery::default() };
    delete.r#where.insert("user".to_owned(), "1".to_owned());
    assert_eq!(client.call::<RecordsCount>(method::RECORDS_DELETE, &delete).unwrap_err().code, "permission");
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

// contract: REC-22
#[cfg(feature = "sql")]
#[test]
fn a_condition_keeps_to_the_values_a_statement_of_a_database_of_this_connection_gives() {
    use crate::wire::codec::Cell;
    use crate::wire::protocol::{RecordsStatement, SqlDone, SqlMigration, SqlOpen, SqlStatement};
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let teachers = "create table teachers (id integer primary key) strict";
    let migrations = Some(vec![SqlMigration { name: "0001_teachers.sql".to_owned(), sql: teachers.to_owned() }]);
    let open_app = SqlOpen { name: "app".to_owned(), migrations, durability: None, backup: None };
    let database = client.call::<Handle>(method::SQL_OPEN, &open_app).unwrap().handle;
    let insert = SqlStatement {
        handle: database,
        text: "insert into teachers values (2)".to_owned(),
        values: vec![],
        now: None,
    };
    client.call::<SqlDone>(method::SQL_EXEC, &insert).unwrap();
    let activity = open(&mut client, "activity");
    append(&mut client, activity, vec![event("used", r#"{"userId":1}"#), event("used", r#"{"userId":2}"#)]);
    let statement = |database| RecordsStatement {
        database,
        text: "select id from teachers where id > ?".to_owned(),
        values: vec![Cell::Int(0)],
    };
    let kept = |not| RecordsCondition { statement: Some(0), not, ..RecordsCondition::default() };
    let query = |database, not| RecordsQuery {
        conditions: [("userId".to_owned(), kept(not))].into(),
        statements: vec![statement(database)],
        ..RecordsQuery::default()
    };
    assert_eq!(client.call::<RecordsCount>(method::RECORDS_COUNT, &query(database, false)).unwrap().count, 1);
    assert_eq!(client.call::<RecordsCount>(method::RECORDS_COUNT, &query(database, true)).unwrap().count, 1);
    let page: RecordsPage = client.call(method::RECORDS_LIST, &query(database, false)).unwrap();
    assert_eq!(page.events[0].fields.as_deref(), Some(r#"{"userId":2}"#));
    // a handle no database of this connection has, a statement the query does not hold, one beside values
    let refused = client.call::<RecordsCount>(method::RECORDS_COUNT, &query(database + 100, false)).unwrap_err();
    assert_eq!(refused.code, "invalid", "{}", refused.message);
    let unheld = RecordsQuery { statements: Vec::new(), ..query(database, false) };
    let refused = client.call::<RecordsCount>(method::RECORDS_COUNT, &unheld).unwrap_err();
    assert!(refused.message.contains("statement 0, which the query does not hold"), "{}", refused.message);
    let beside = RecordsCondition { values: vec!["1".to_owned()], ..kept(false) };
    let both = RecordsQuery { conditions: [("userId".to_owned(), beside)].into(), ..query(database, false) };
    let refused = client.call::<RecordsCount>(method::RECORDS_COUNT, &both).unwrap_err();
    assert!(refused.message.contains("a statement in place of values"), "{}", refused.message);
    let tailed = client.call::<Empty>(method::RECORDS_TAIL, &query(database, false)).unwrap_err();
    assert!(tailed.message.contains("a tail's where takes no statement"), "{}", tailed.message);
}

// contract: REC-23
#[test]
fn sessions_answer_each_visit_with_its_count_and_first_events_and_refuse_no_gap() {
    use crate::wire::protocol::{RecordsSessions, RecordsSessionsAsked};
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let activity = open(&mut client, "activity");
    let at = |minutes: i64| crate::unix_nanos(std::time::SystemTime::now()) - (180 - minutes) * 60_000_000_000;
    let visit = |user: u32, minutes: i64| RecordsNewEvent {
        at: Some(at(minutes)),
        ..event("open", &format!(r#"{{"userId":{user}}}"#))
    };
    append(&mut client, activity, vec![visit(1, 0), visit(1, 10), visit(1, 100), visit(2, 5)]);
    let asked = RecordsSessionsAsked {
        query: RecordsQuery { handle: Some(activity), since: Some(4 * 3_600_000), ..RecordsQuery::default() },
        by: vec!["userId".to_owned()],
        gap: 30 * 60_000,
        max_events: 1,
        ..RecordsSessionsAsked::default()
    };
    let answer: RecordsSessions = client.call(method::RECORDS_SESSIONS, &asked).unwrap();
    let shown: Vec<(&str, u64, usize, bool)> = answer
        .sessions
        .iter()
        .map(|session| (session.by["userId"].as_str(), session.count, session.events.len(), session.partial))
        .collect();
    assert_eq!(shown, [("1", 2, 1, false), ("2", 1, 1, false), ("1", 1, 1, false)]);
    assert_eq!(answer.sessions[0].from, answer.sessions[0].events[0].at);
    let first = client.call::<RecordsSessions>(
        method::RECORDS_SESSIONS,
        &RecordsSessionsAsked { first_sessions: Some(1), ..asked.clone() },
    );
    assert_eq!(first.unwrap().sessions.len(), 2);
    let refused = client.call::<RecordsSessions>(method::RECORDS_SESSIONS, &RecordsSessionsAsked { gap: 0, ..asked });
    assert_eq!(refused.unwrap_err().code, "invalid");
}
