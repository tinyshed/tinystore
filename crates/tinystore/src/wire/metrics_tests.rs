use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::pipe::fixture::{Client, answered};
use crate::pipe::{Capability, Connect, Pipe};
use crate::wire::codec::Message;
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{
    Empty, Hello, MetricsAck, MetricsAggregateAsked, MetricsAggregates, MetricsBatch, MetricsChange, MetricsCondition,
    MetricsDamages, MetricsDeleted, MetricsDescription, MetricsExposition, MetricsIngest, MetricsInstrument,
    MetricsLatest, MetricsNameAsked, MetricsNamePage, MetricsNewSeries, MetricsPageAsked, MetricsQuery, MetricsRead,
    MetricsSeriesNamed, MetricsSession, MetricsSet, MetricsTextPage, method,
};
use crate::{Options, Store, unix_millis};

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(label, value)| ((*label).to_owned(), (*value).to_owned())).collect()
}

fn series(name: &str, kind: &str, labelled: &[(&str, &str)], samples: &[(i64, f64)]) -> MetricsNewSeries {
    MetricsNewSeries {
        name: name.to_owned(),
        kind: kind.to_owned(),
        labels: labels(labelled),
        times: samples.iter().flat_map(|(at, _)| at.to_le_bytes()).collect(),
        values: samples.iter().flat_map(|(_, value)| value.to_le_bytes()).collect(),
        keep: None,
    }
}

fn ingest(client: &mut Client, written: Vec<MetricsNewSeries>) {
    client.call::<Empty>(method::METRICS_INGEST, &MetricsIngest { series: written }).unwrap();
}

fn named(name: &str) -> MetricsQuery {
    MetricsQuery { name: Some(name.to_owned()), since: Some(3_600_000), ..MetricsQuery::default() }
}

fn now() -> i64 {
    unix_millis(SystemTime::now())
}

/// A download's parts: the RESPONSE alone when it ends the stream, else every
/// DATA up to the last.
fn downloaded<M: Message>(client: &mut Client, called: u16, request: &impl Message) -> Vec<M> {
    let stream = client.start(called, request);
    let head = client.next_on(stream);
    assert_eq!(head.kind, Kind::Response);
    if head.flags & frame::END != 0 {
        return vec![answered(&head).unwrap()];
    }
    let mut parts = Vec::new();
    loop {
        let part = client.next_on(stream);
        assert_eq!(part.kind, Kind::Data);
        parts.push(answered(&part).unwrap());
        if part.flags & frame::END != 0 {
            return parts;
        }
    }
}

/// A client of `store` whose bodies are `max_body` bytes at most.
fn client(store: &Store, max_body: u64) -> Client {
    let mut client = Client::over(Pipe::connect(store, Connect::default()).unwrap());
    let hello = Hello { protocol: 1, client: "test/0".to_owned(), max_body: Some(max_body), ..Hello::default() };
    client.greet(hello).unwrap();
    client
}

fn store(dir: &tempfile::TempDir) -> Store {
    Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap()
}

#[test]
fn a_read_comes_back_in_parts_each_value_bit_for_bit() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut client = client(&store, 4096);
    let start = now() - 600_000;
    let odd = [-0.0, f64::from_bits(0x7ff8_0000_dead_beef), f64::INFINITY, 1e-310];
    let samples: Vec<(i64, f64)> =
        (0..600).map(|n| (start + n * 1000, odd.get(n as usize % 50).copied().unwrap_or(n as f64 * 0.1))).collect();
    for chunk in samples.chunks(200) {
        ingest(&mut client, vec![series("cpu", "gauge", &[("host", "a")], chunk)]);
    }
    let parts: Vec<MetricsRead> = downloaded(&mut client, method::METRICS_READ, &named("cpu"));
    assert!(parts.len() > 2, "{} parts", parts.len());
    let pieces: Vec<_> = parts.iter().flat_map(|part| &part.series).collect();
    assert!(pieces[..pieces.len() - 1].iter().all(|piece| piece.more) && !pieces[pieces.len() - 1].more);
    let times: Vec<i64> = pieces
        .iter()
        .flat_map(|piece| piece.times.as_chunks::<8>().0.iter().map(|time| i64::from_le_bytes(*time)))
        .collect();
    let bits: Vec<u64> = pieces
        .iter()
        .flat_map(|piece| piece.values.as_chunks::<8>().0.iter().map(|value| u64::from_le_bytes(*value)))
        .collect();
    assert_eq!(times, samples.iter().map(|(at, _)| *at).collect::<Vec<_>>());
    assert_eq!(bits, samples.iter().map(|(_, value)| value.to_bits()).collect::<Vec<_>>());
    assert_eq!((pieces[0].kind.as_str(), &pieces[0].labels), ("gauge", &labels(&[("host", "a")])));
}

// contract: MET-04
#[test]
fn an_aggregate_and_the_newest_samples_answer_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut client = client(&store, 1 << 20);
    let at = now() - 60_000;
    let route = |route: &str, values: [f64; 3]| {
        let samples: Vec<(i64, f64)> =
            values.iter().enumerate().map(|(n, value)| (at + n as i64 * 1000, *value)).collect();
        series("requests_total", "counter", &[("route", route)], &samples)
    };
    ingest(&mut client, vec![route("/a", [0.0, 10.0, 25.0]), route("/b", [0.0, 3.0, 1.0])]);
    let asked = MetricsAggregateAsked {
        query: named("requests_total"),
        op: Some("increase".to_owned()),
        by: Some(vec!["route".to_owned()]),
        ..MetricsAggregateAsked::default()
    };
    let parts: Vec<MetricsAggregates> = downloaded(&mut client, method::METRICS_AGGREGATE, &asked);
    let groups: Vec<_> = parts.iter().flat_map(|part| &part.groups).collect();
    let answers: Vec<(Option<&String>, Option<f64>, u64)> = groups
        .iter()
        .map(|group| (group.labels.get("route"), group.buckets[0].value, group.buckets[0].resets))
        .collect();
    let (a, b) = ("/a".to_owned(), "/b".to_owned());
    assert_eq!(answers, [(Some(&a), Some(25.0), 0), (Some(&b), Some(4.0), 1)], "a reset counted as its new value");
    let wrong = MetricsAggregateAsked { op: Some("p".to_owned()), ..asked };
    let stream = client.start(method::METRICS_AGGREGATE, &wrong);
    let refused = answered::<MetricsAggregates>(&client.next_on(stream)).unwrap_err();
    assert_eq!(refused.code, "invalid");
    let newest: Vec<MetricsLatest> = downloaded(&mut client, method::METRICS_LATEST, &named("requests_total"));
    let mut values: Vec<(String, f64)> = newest
        .iter()
        .flat_map(|part| &part.series)
        .map(|series| (series.labels["route"].clone(), series.value))
        .collect();
    values.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(values, [(a, 25.0), (b, 1.0)]);
}

// contract: MET-04
#[test]
fn an_aggregate_takes_a_calendar_with_its_time_zone_and_refuses_one_without_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut client = client(&store, 1 << 20);
    let at = now() - 60_000;
    ingest(&mut client, vec![series("orders", "gauge", &[], &[(at, 1.0), (at + 1000, 2.0)])]);
    let asked = MetricsAggregateAsked {
        query: named("orders"),
        op: Some("sum".to_owned()),
        calendar: Some("day".to_owned()),
        time_zone: Some("Europe/Moscow".to_owned()),
        ..MetricsAggregateAsked::default()
    };
    let parts: Vec<MetricsAggregates> = downloaded(&mut client, method::METRICS_AGGREGATE, &asked);
    let buckets: Vec<_> = parts.iter().flat_map(|part| &part.groups).flat_map(|group| &group.buckets).collect();
    assert_eq!(buckets.iter().map(|bucket| bucket.value).collect::<Vec<_>>(), [Some(3.0)]);
    for (calendar, time_zone) in [(Some("day"), None), (None, Some("UTC")), (Some("fortnight"), Some("UTC"))] {
        let wrong = MetricsAggregateAsked {
            calendar: calendar.map(str::to_owned),
            time_zone: time_zone.map(str::to_owned),
            ..asked.clone()
        };
        let stream = client.start(method::METRICS_AGGREGATE, &wrong);
        let refused = answered::<MetricsAggregates>(&client.next_on(stream)).unwrap_err();
        assert_eq!(refused.code, "invalid", "{calendar:?} {time_zone:?}");
    }
}

#[test]
fn what_a_store_holds_and_its_removal_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut client = client(&store, 1 << 20);
    let at = now() - 1000;
    let gauges =
        ["cpu", "disk", "memory"].map(|name| series(name, "gauge", &[("host", "a"), ("zone", "eu")], &[(at, 1.0)]));
    ingest(&mut client, gauges.into());
    let first: MetricsNamePage = client
        .call(method::METRICS_NAMES, &MetricsPageAsked { limit: Some(2), ..MetricsPageAsked::default() })
        .unwrap();
    assert_eq!(first.names.iter().map(|name| name.name.as_str()).collect::<Vec<_>>(), ["cpu", "disk"]);
    assert_eq!(
        (first.next.as_deref(), first.names[0].kind.as_str(), first.names[0].series_count),
        (Some("disk"), "gauge", 1)
    );
    let rest: MetricsNamePage = client
        .call(method::METRICS_NAMES, &MetricsPageAsked { after: first.next, ..MetricsPageAsked::default() })
        .unwrap();
    assert_eq!((rest.names.len(), rest.next), (1, None));
    let asked = |label: Option<&str>| MetricsPageAsked {
        name: Some("cpu".to_owned()),
        label: label.map(str::to_owned),
        ..MetricsPageAsked::default()
    };
    let labelled: MetricsTextPage = client.call(method::METRICS_LABEL_NAMES, &asked(None)).unwrap();
    assert_eq!(labelled.texts, ["host", "zone"]);
    let hosts: MetricsTextPage = client.call(method::METRICS_LABEL_VALUES, &asked(Some("host"))).unwrap();
    assert_eq!(hosts.texts, ["a"]);
    let busy = MetricsDescription { name: "cpu".to_owned(), unit: "%".to_owned(), help: "Busy.".to_owned() };
    client.call::<Empty>(method::METRICS_DESCRIBE, &busy).unwrap();
    let read: MetricsDescription =
        client.call(method::METRICS_DESCRIPTION, &MetricsNameAsked { name: "cpu".to_owned() }).unwrap();
    assert_eq!(read, busy);
    let one = MetricsSeriesNamed { name: "cpu".to_owned(), labels: labels(&[("host", "a"), ("zone", "eu")]) };
    assert!(client.call::<MetricsDeleted>(method::METRICS_DELETE, &one).unwrap().found);
    assert!(!client.call::<MetricsDeleted>(method::METRICS_DELETE, &one).unwrap().found);
    let damaged: MetricsDamages = client.call(method::METRICS_DAMAGED, &Empty {}).unwrap();
    assert!(damaged.damages.is_empty());
}

#[test]
fn a_page_holds_what_a_body_holds_and_says_where_the_rest_starts() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut client = client(&store, 512);
    let at = now() - 1000;
    for chunk in (0..40).collect::<Vec<_>>().chunks(4) {
        let named =
            chunk.iter().map(|n| series(&format!("metric_{n:02}_{}", "x".repeat(20)), "gauge", &[], &[(at, 1.0)]));
        ingest(&mut client, named.collect());
    }
    let (mut seen, mut after) = (Vec::new(), None);
    loop {
        let page: MetricsNamePage =
            client.call(method::METRICS_NAMES, &MetricsPageAsked { after, ..MetricsPageAsked::default() }).unwrap();
        assert!(page.names.len() < 40, "a body of 512 bytes holds a few");
        seen.extend(page.names.into_iter().map(|name| name.name));
        if page.next.is_none() {
            break;
        }
        after = page.next;
    }
    assert_eq!(seen.len(), 40, "every name once");
}

fn instruments(client: &mut Client, after: u64) -> (u32, MetricsAck) {
    let stream = client.open_stream(method::METRICS_INSTRUMENTS, &MetricsSession { id: vec![9; 16], after });
    let head = client.next_on(stream);
    assert_eq!((head.kind, head.flags & frame::END), (Kind::Response, 0), "the stream stays open");
    (stream, answered(&head).unwrap())
}

/// Batch `number`, adding `n` to a counter its stream declares in it.
fn counted(number: u64, n: f64, flush: bool) -> MetricsBatch {
    MetricsBatch {
        number,
        instruments: vec![MetricsInstrument {
            number: 1,
            name: "jobs_total".to_owned(),
            form: "counter".to_owned(),
            ..MetricsInstrument::default()
        }],
        sets: vec![MetricsSet { number: 1, instrument: 1, labels: labels(&[("queue", "mail")]), overflow: false }],
        changes: vec![MetricsChange { set: 1, add: Some(n), ..MetricsChange::default() }],
        flush,
    }
}

/// Batch `number`, adding `n` to the counter its stream declared before.
fn more(number: u64, n: f64, flush: bool) -> MetricsBatch {
    let change = MetricsChange { set: 1, add: Some(n), ..MetricsChange::default() };
    MetricsBatch { number, changes: vec![change], flush, ..MetricsBatch::default() }
}

fn sent(client: &mut Client, stream: u32, batch: &MetricsBatch) -> Frame {
    client.write(Frame::new(Kind::Data, stream, batch.encode()));
    client.next_on(stream)
}

// contract: MET-09
#[test]
fn an_instruments_batch_sent_again_on_another_connection_counts_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut one = client(&store, 1 << 20);
    let (stream, ack) = instruments(&mut one, 0);
    assert_eq!((ack.known, ack.applied), (false, 0));
    assert_eq!(answered::<MetricsAck>(&sent(&mut one, stream, &counted(1, 1.0, false))).unwrap().applied, 1);
    assert_eq!(answered::<MetricsAck>(&sent(&mut one, stream, &more(2, 2.0, false))).unwrap().applied, 2);
    // the connection ends, and with it the client's last answer
    drop(one);
    let mut two = client(&store, 1 << 20);
    let (stream, ack) = instruments(&mut two, 1);
    assert_eq!((ack.known, ack.applied), (true, 2), "the store says what it applied");
    assert_eq!(answered::<MetricsAck>(&sent(&mut two, stream, &counted(2, 2.0, false))).unwrap().applied, 2);
    let flushed = sent(&mut two, stream, &more(3, 4.0, true));
    assert_eq!(answered::<MetricsAck>(&flushed).unwrap().applied, 3);
    let read: Vec<MetricsRead> = downloaded(
        &mut two,
        method::METRICS_READ,
        &MetricsQuery {
            r#where: [(
                "queue".to_owned(),
                MetricsCondition { values: vec!["mail".to_owned()], ..MetricsCondition::default() },
            )]
            .into(),
            ..named("jobs_total")
        },
    );
    let last = read[0].series[0].values.as_chunks::<8>().0.last().map(|value| f64::from_le_bytes(*value));
    assert_eq!(last, Some(7.0), "1, 2 and 4, each once");
    let past = sent(&mut two, stream, &more(5, 1.0, false));
    assert_ne!(past.flags & frame::END, 0, "a batch past the next ends the stream");
    assert_eq!(answered::<MetricsAck>(&past).unwrap_err().code, "invalid");
}

#[test]
fn a_clients_last_data_ends_its_instruments_with_the_last_answer() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut client = client(&store, 1 << 20);
    let (stream, _) = instruments(&mut client, 0);
    client.write(Frame::new(Kind::Data, stream, counted(1, 1.0, true).encode()));
    client.write(Frame::new(Kind::Data, stream, Vec::new()).ending());
    let first = client.next_on(stream);
    assert_eq!((first.flags & frame::END, answered::<MetricsAck>(&first).unwrap().applied), (0, 1));
    let last = client.next_on(stream);
    assert_ne!(last.flags & frame::END, 0);
    assert_eq!(answered::<MetricsAck>(&last).unwrap().applied, 1);
}

/// The batches a cancel finds waiting are applied by a worker after it: the
/// stream's final frame is the cancel's, and the number named again is
/// another stream's, which the batches that waited leave alone.
// contract: WIRE-04
#[test]
fn an_instruments_stream_cancelled_with_batches_waiting_ends_once_and_its_number_is_free() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut client = client(&store, 1 << 20);
    let (stream, _) = instruments(&mut client, 0);
    let held = crate::pipe::fixture::hold_every_worker(&store);
    client.write(Frame::new(Kind::Data, stream, counted(1, 1.0, false).encode()));
    client.write(Frame::new(Kind::Data, stream, Vec::new()).ending());
    client.write(Frame::new(Kind::Cancel, stream, Vec::new()));
    let last = client.next_on(stream);
    assert_eq!(answered::<MetricsAck>(&last).unwrap_err().code, "cancelled");

    client.next_stream = stream - 1;
    let again = client.open_stream(method::METRICS_INSTRUMENTS, &MetricsSession { id: vec![7; 16], after: 0 });
    let head = client.next_on(again);
    assert_eq!((head.kind, head.flags & frame::END), (Kind::Response, 0), "the number names a stream again");
    drop(held);
    assert!(client.silent_on(again, std::time::Duration::from_millis(300)), "the cancelled stream sent nothing more");
    let acked = sent(&mut client, again, &counted(1, 1.0, false));
    assert_eq!((acked.flags & frame::END, answered::<MetricsAck>(&acked).unwrap().applied), (0, 1));
}

// contract: MET-15
#[test]
fn a_text_an_exporter_serves_is_read_by_the_core_and_one_it_cannot_read_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut client = client(&store, 1 << 20);
    let at = now() - 60_000;
    let text = b"# TYPE rpc_seconds histogram\nrpc_seconds_bucket{le=\"+Inf\"} 3\nrpc_seconds_count 3\nup -0\n";
    let given = MetricsExposition {
        text: text.to_vec(),
        labels: labels(&[("instance", "web-1")]),
        at: Some(at),
        keep: Some(7 * 86_400_000),
    };
    client.call::<Empty>(method::METRICS_INGEST_PROMETHEUS, &given).unwrap();
    let read = |client: &mut Client, name: &str| {
        let parts: Vec<MetricsRead> = downloaded(client, method::METRICS_READ, &named(name));
        parts.into_iter().flat_map(|part| part.series).collect::<Vec<_>>()
    };
    let bucket = &read(&mut client, "rpc_seconds_bucket")[0];
    assert_eq!(bucket.kind, "histogram");
    assert_eq!(bucket.labels, labels(&[("instance", "web-1"), ("le", "+Inf")]));
    assert_eq!(
        (bucket.times.clone(), read(&mut client, "rpc_seconds_count")[0].kind.as_str()),
        (at.to_le_bytes().to_vec(), "counter")
    );
    assert_eq!(read(&mut client, "up")[0].values, (-0.0_f64).to_le_bytes(), "a value bit for bit");
    let names: MetricsNamePage = client.call(method::METRICS_NAMES, &MetricsPageAsked::default()).unwrap();
    assert!(names.names.iter().all(|name| name.keep == 7 * 86_400_000), "{:?}", names.names);
    let broken = MetricsExposition { text: b"down 1\ndown{host=\"a} 2\n".to_vec(), ..MetricsExposition::default() };
    let refused = client.call::<Empty>(method::METRICS_INGEST_PROMETHEUS, &broken).unwrap_err();
    assert_eq!(refused.code, "invalid");
    assert!(refused.message.starts_with("metrics: line 2, "), "{}", refused.message);
    assert!(read(&mut client, "down").is_empty(), "a refused text writes nothing");
}

#[test]
fn a_read_connection_reads_metrics_and_neither_ingests_nor_counts() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let mut reader = Client::admitted(&store, Capability::Read);
    reader.call::<MetricsNamePage>(method::METRICS_NAMES, &MetricsPageAsked::default()).unwrap();
    let refused = reader.call::<Empty>(method::METRICS_INGEST, &MetricsIngest::default()).unwrap_err();
    assert_eq!(refused.code, "permission");
    let text = MetricsExposition { text: b"up 1\n".to_vec(), ..MetricsExposition::default() };
    let refused = reader.call::<Empty>(method::METRICS_INGEST_PROMETHEUS, &text).unwrap_err();
    assert_eq!(refused.code, "permission");
    let stream = reader.start(method::METRICS_INSTRUMENTS, &MetricsSession { id: vec![1; 16], after: 0 });
    assert_eq!(answered::<MetricsAck>(&reader.next_on(stream)).unwrap_err().code, "permission");
}

#[test]
fn an_answer_past_the_agreed_body_is_limit_and_the_connection_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let long = MetricsDescription { name: "cpu".to_owned(), unit: "%".to_owned(), help: "h".repeat(1000) };
    client(&store, 1 << 20).call::<Empty>(method::METRICS_DESCRIBE, &long).unwrap();
    let mut small = client(&store, 512);
    let asked = MetricsNameAsked { name: "cpu".to_owned() };
    let refused = small.call::<MetricsDescription>(method::METRICS_DESCRIPTION, &asked).unwrap_err();
    assert_eq!(refused.code, "limit", "{}", refused.message);
    let short = MetricsDescription { name: "mem".to_owned(), unit: "B".to_owned(), help: "Used.".to_owned() };
    small.call::<Empty>(method::METRICS_DESCRIBE, &short).unwrap();
}
