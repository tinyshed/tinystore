//! metrics over the protocol: queries and their answers, what a store holds of
//! its series, and a client's instruments.
//!
//! A read, the newest samples and an aggregate answer in parts within the
//! client's credit, as many series or groups a part as a body holds, a series
//! longer than one cut across parts; a series' samples go as two columns, an
//! int64 time and a float64 value a sample, 8 bytes each, little end first,
//! so that every value's bits come back. A client's instruments send their
//! batches on a stream of their own, applied in turn, each answered with the
//! last batch of the session the store applied.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::codec::Message;
use super::protocol::{
    Empty, Failure, MetricsAck, MetricsAggregateAsked, MetricsAggregates, MetricsBatch, MetricsBucket,
    MetricsCondition, MetricsDamage, MetricsDamages, MetricsDeleted, MetricsDescription, MetricsExceeds,
    MetricsExposition, MetricsGroup, MetricsIngest, MetricsLatest, MetricsName, MetricsNameAsked, MetricsNamePage,
    MetricsNewSeries, MetricsNewest, MetricsPageAsked, MetricsPlan, MetricsQuery, MetricsRead, MetricsRefused,
    MetricsSeries, MetricsSeriesNamed, MetricsSession, MetricsTextPage, method,
};
use super::{Link, Route};
use crate::Calendar;
use crate::clock::from_unix_millis;
use crate::metrics::{
    Ack, Aggregate, Batch, Buckets, Change, Condition, Declared, Description, Latest, List, Metrics, Op, Query, Remote,
    Series, Set, Value,
};
use crate::sync::lock;
use crate::{Store, unix_millis};

use super::Answered;

/// What a part's message takes past its series or groups, and each of them
/// past its samples or buckets.
const ENVELOPE: usize = 32;
/// What a name of a page takes past its text: its numbers and its fields'
/// keys.
const NAME_NUMBERS: usize = 64;
/// What a sample of a read takes of a part: its time and its value.
const SAMPLE: usize = 16;
/// What a bucket of an aggregate takes of a part at most.
const BUCKET: usize = 64;

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::METRICS_READ | method::METRICS_LATEST | method::METRICS_AGGREGATE => Route::Parts,
        method::METRICS_INSTRUMENTS => Route::Instruments,
        _ => Route::Worker,
    }
}

/// A connection's streams that send DATA: its downloads, and its clients'
/// instruments.
#[derive(Default)]
pub(crate) struct Handles {
    downloads: Mutex<HashMap<u32, Arc<Downloading>>>,
    instruments: Arc<Mutex<HashMap<u32, Arc<Instruments>>>>,
}

impl Handles {
    /// Credit the client granted a download.
    pub(crate) fn grant(&self, stream: u32, credit: u32) {
        let download = lock(&self.downloads).get(&stream).cloned();
        if let Some(download) = download {
            download.grant(credit, &self.downloads);
        }
    }

    /// Ends a download or an instruments' stream the client cancelled; says
    /// whether the stream was one.
    pub(crate) fn cancel(&self, stream: u32) -> bool {
        if let Some(download) = lock(&self.downloads).remove(&stream) {
            download.end(Err(Failure::cancelled("the client cancelled its read")));
            return true;
        }
        if let Some(instruments) = lock(&self.instruments).remove(&stream) {
            instruments.end(Err(Failure::cancelled("the client cancelled its instruments")));
            return true;
        }
        false
    }

    /// Lets go of every stream: the connection ended, and a client's gauges
    /// leave the totals with it.
    pub(crate) fn end(&self) {
        lock(&self.downloads).clear();
        lock(&self.instruments).clear();
    }

    /// Takes a client's batch on its instruments' stream, applied in turn on
    /// a worker; says whether the stream is an instruments' stream.
    pub(crate) fn batch(&self, stream: u32, body: &[u8], last: bool) -> bool {
        let Some(instruments) = lock(&self.instruments).get(&stream).cloned() else { return false };
        let mut waiting = lock(&instruments.waiting);
        waiting.batches.push_back((body.to_vec(), last));
        if !std::mem::replace(&mut waiting.running, true) {
            let (spawn, open) = (Arc::clone(&instruments.link.spawn), Arc::clone(&self.instruments));
            let applying = Arc::clone(&instruments);
            spawn(Box::new(move || applying.drain(stream, &open)));
        }
        true
    }
}

/// Answers a call of one message.
pub(crate) fn call(store: &Store, called: u16, body: &[u8], max_body: usize) -> Answered {
    let metrics = store.metrics()?;
    match called {
        method::METRICS_EXPLAIN => explain(&metrics, MetricsAggregateAsked::decode(body)?),
        method::METRICS_NAMES => names(&metrics, MetricsPageAsked::decode(body)?, max_body),
        method::METRICS_LABEL_NAMES | method::METRICS_LABEL_VALUES => {
            texts(&metrics, called, MetricsPageAsked::decode(body)?, max_body)
        }
        method::METRICS_DESCRIPTION => description(&metrics, &MetricsNameAsked::decode(body)?.name),
        method::METRICS_DAMAGED => damaged(&metrics, max_body),
        method::METRICS_INGEST => ingest(&metrics, MetricsIngest::decode(body)?),
        method::METRICS_INGEST_PROMETHEUS => ingest_prometheus(&metrics, MetricsExposition::decode(body)?),
        method::METRICS_DESCRIBE => describe(&metrics, MetricsDescription::decode(body)?),
        method::METRICS_DELETE => delete(&metrics, MetricsSeriesNamed::decode(body)?),
        method::METRICS_FLUSH => metrics.flush().map(|()| Empty {}.encode()).map_err(Failure::from),
        _ => Err(Failure::unimplemented(format!("method {called:#06x}"))),
    }
}

/// The parts of a read's, the newest samples' or an aggregate's answer, each
/// of at most `bound` bytes.
pub(crate) fn parts(store: &Store, called: u16, body: &[u8], bound: usize) -> Result<VecDeque<Vec<u8>>, Failure> {
    let metrics = store.metrics()?;
    match called {
        method::METRICS_READ => Ok(read_parts(query(&metrics, MetricsQuery::decode(body)?)?.read()?, bound)),
        method::METRICS_LATEST => Ok(latest_parts(query(&metrics, MetricsQuery::decode(body)?)?.latest()?, bound)),
        _ => {
            let asked = MetricsAggregateAsked::decode(body)?;
            let op = op_of(asked.op.as_deref())?;
            Ok(aggregate_parts(aggregating(&metrics, asked)?.aggregate(op)?, bound))
        }
    }
}

/// A query as the engine takes it: its name, its conditions and its range.
fn query(metrics: &Metrics, asked: MetricsQuery) -> Result<Query, Failure> {
    let mut query = metrics.query();
    if let Some(name) = asked.name {
        query = query.name(name);
    }
    for (label, condition) in asked.r#where {
        query = query.filter(label, condition_of(condition)?);
    }
    if let Some(since) = asked.since {
        query = query.since(Duration::from_millis(since));
    }
    if let Some(from) = asked.from {
        query = query.from(from_unix_millis(from));
    }
    if let Some(to) = asked.to {
        query = query.to(from_unix_millis(to));
    }
    Ok(query)
}

fn condition_of(condition: MetricsCondition) -> Result<Condition, Failure> {
    match condition {
        MetricsCondition { values, not: false, prefix: Some(prefix) } if values.is_empty() => {
            Ok(Condition::Prefix(prefix))
        }
        MetricsCondition { prefix: Some(_), .. } => {
            Err(Failure::invalid("a condition is a list of values, `not` one, or a prefix alone"))
        }
        MetricsCondition { values, not: true, .. } => Ok(Condition::NoneOf(values)),
        MetricsCondition { values, .. } => Ok(Condition::OneOf(values)),
    }
}

/// An aggregate's query: its buckets, groups and estimate beside read's.
fn aggregating(metrics: &Metrics, asked: MetricsAggregateAsked) -> Result<Query, Failure> {
    let mut query = query(metrics, asked.query)?;
    if let Some(every) = asked.every {
        query = query.every(Duration::from_millis(every));
    }
    if let Some((unit, zone)) = Calendar::asked(asked.calendar.as_deref(), asked.time_zone.as_deref())? {
        query = query.calendar(unit, zone);
    }
    if let Some(by) = &asked.by {
        query = query.by(&by.iter().map(String::as_str).collect::<Vec<_>>());
    }
    if let Some(without) = &asked.without {
        query = query.without(&without.iter().map(String::as_str).collect::<Vec<_>>());
    }
    if asked.estimate {
        query = query.estimate();
    }
    Ok(query)
}

/// An `op` as a query writes it: `increase`, `p95`, `p99.9`.
fn op_of(op: Option<&str>) -> Result<Op, Failure> {
    let ops = [
        ("count", Op::Count),
        ("sum", Op::Sum),
        ("avg", Op::Avg),
        ("min", Op::Min),
        ("max", Op::Max),
        ("increase", Op::Increase),
        ("rate", Op::Rate),
        ("delta", Op::Delta),
        ("first", Op::First),
        ("last", Op::Last),
    ];
    let op = op.ok_or_else(|| Failure::invalid("an aggregate needs an op"))?;
    if let Some((_, found)) = ops.iter().find(|(word, _)| *word == op) {
        return Ok(*found);
    }
    let rank =
        op.strip_prefix('p').filter(|rank| !rank.is_empty() && rank.chars().all(|c| c.is_ascii_digit() || c == '.'));
    match rank.and_then(|rank| rank.parse::<f64>().ok()) {
        Some(rank) => Ok(Op::Percentile(rank)),
        None => Err(Failure::invalid(format!(
            "op {op:?}: count, sum, avg, min, max, increase, rate, delta, first, last, or p and a rank, p95"
        ))),
    }
}

fn explain(metrics: &Metrics, asked: MetricsAggregateAsked) -> Answered {
    let op = asked.op.as_deref().map(|op| op_of(Some(op))).transpose()?;
    let plan = aggregating(metrics, asked)?.explain(op)?;
    let exceeds = plan.exceeds.map(|exceeds| MetricsExceeds {
        limit: exceeds.limit.to_owned(),
        wanted: exceeds.wanted,
        bound: exceeds.bound,
    });
    Ok(MetricsPlan { series: plan.series, bytes: plan.bytes, decoded: plan.decoded, exceeds }.encode())
}

/// A page's options as the client gave them.
fn paged<P>(mut list: List<P>, asked: &MetricsPageAsked) -> List<P> {
    if let Some(prefix) = &asked.prefix {
        list = list.prefix(prefix.clone());
    }
    if let Some(after) = &asked.after {
        list = list.after(after.clone());
    }
    if let Some(limit) = asked.limit {
        list = list.limit(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    list
}

/// A page of names, cut where a body ends, `next` past the last it holds.
fn names(metrics: &Metrics, asked: MetricsPageAsked, max_body: usize) -> Answered {
    let page = paged(metrics.names(), &asked).page()?;
    let names = page.names.into_iter().map(|info| MetricsName {
        kind: info.kind.as_str().to_owned(),
        series_count: info.series_count,
        keep: crate::clock::millis(info.keep) as u64,
        retained_from: unix_millis(info.retained_from),
        newest: unix_millis(info.newest),
        name: info.name,
        unit: info.unit,
        help: info.help,
    });
    let (names, next) = fitted(names.collect(), page.next, max_body, |name| {
        let text = name.name.len() + name.kind.len() + name.unit.len() + name.help.len();
        (name.name.clone(), text + NAME_NUMBERS)
    });
    Ok(MetricsNamePage { names, next }.encode())
}

/// A page of a name's labels or of a label's values.
fn texts(metrics: &Metrics, called: u16, asked: MetricsPageAsked, max_body: usize) -> Answered {
    let name = asked.name.clone().unwrap_or_default();
    let (texts, next) = if called == method::METRICS_LABEL_NAMES {
        let page = paged(metrics.label_names(name), &asked).page()?;
        (page.labels, page.next)
    } else {
        let label = asked.label.clone().unwrap_or_default();
        let page = paged(metrics.label_values(name, label), &asked).page()?;
        (page.values, page.next)
    };
    let (texts, next) = fitted(texts, next, max_body, |text| (text.clone(), text.len() + 8));
    Ok(MetricsTextPage { texts, next }.encode())
}

/// What a page holds that a body holds: past it, the page ends at the last
/// item that fits and `next` names it, room kept for the name.
fn fitted<T>(
    items: Vec<T>,
    next: Option<String>,
    max_body: usize,
    weigh: impl Fn(&T) -> (String, usize),
) -> (Vec<T>, Option<String>) {
    let (mut size, mut kept) = (ENVELOPE, Vec::with_capacity(items.len()));
    for item in items {
        let (key, weight) = weigh(&item);
        if size + weight + key.len() > max_body && !kept.is_empty() {
            let last = kept.last().map(|kept| weigh(kept).0);
            return (kept, last);
        }
        size += weight;
        kept.push(item);
    }
    (kept, next)
}

fn description(metrics: &Metrics, name: &str) -> Answered {
    let Description { unit, help } = metrics.description(name)?;
    Ok(MetricsDescription { name: name.to_owned(), unit, help }.encode())
}

/// The damaged series a body holds, 1000 at most: deleting them shows the
/// next.
fn damaged(metrics: &Metrics, max_body: usize) -> Answered {
    let mut size = ENVELOPE;
    let mut damages = Vec::new();
    for damage in metrics.damaged()? {
        let weight = damage.name.len() + damage.error.len() + labels_weight(&damage.labels) + ENVELOPE;
        if size + weight > max_body && !damages.is_empty() {
            break;
        }
        size += weight;
        damages.push(MetricsDamage { name: damage.name, labels: damage.labels, error: damage.error });
    }
    Ok(MetricsDamages { damages }.encode())
}

fn ingest(metrics: &Metrics, ingest: MetricsIngest) -> Answered {
    let series: Vec<Series> = ingest.series.into_iter().map(series_of).collect::<Result<_, _>>()?;
    metrics.ingest(&series)?;
    Ok(Empty {}.encode())
}

/// A text an exporter serves, read by the core's one parser for every SDK.
fn ingest_prometheus(metrics: &Metrics, given: MetricsExposition) -> Answered {
    let mut text = metrics.prometheus();
    for (label, value) in given.labels {
        text = text.label(label, value);
    }
    if let Some(at) = given.at {
        text = text.at(from_unix_millis(at));
    }
    if let Some(keep) = given.keep {
        text = text.keep(Duration::from_millis(keep));
    }
    text.ingest(&given.text)?;
    Ok(Empty {}.encode())
}

/// A series to write, its columns read back into samples.
fn series_of(given: MetricsNewSeries) -> Result<Series, Failure> {
    let mut series = match given.kind.as_str() {
        "gauge" => Series::gauge(given.name),
        "counter" => Series::counter(given.name),
        "histogram" => Series::histogram(given.name),
        kind => return Err(Failure::invalid(format!("a kind {kind:?}: gauge, counter or histogram"))),
    };
    for (label, value) in given.labels {
        series = series.label(label, value);
    }
    if given.times.len() != given.values.len() || !given.times.len().is_multiple_of(8) {
        return Err(Failure::invalid("a series' times and values: 8 bytes a sample each, as many of each"));
    }
    let times = given.times.as_chunks::<8>().0.iter().map(|time| i64::from_le_bytes(*time));
    let values = given.values.as_chunks::<8>().0.iter().map(|value| f64::from_le_bytes(*value));
    series.extend(times.zip(values).map(|(at, value)| (from_unix_millis(at), value)));
    if let Some(keep) = given.keep {
        series = series.keep(Duration::from_millis(keep));
    }
    Ok(series)
}

fn describe(metrics: &Metrics, described: MetricsDescription) -> Answered {
    metrics.describe(&described.name, Description { unit: described.unit, help: described.help })?;
    Ok(Empty {}.encode())
}

fn delete(metrics: &Metrics, named: MetricsSeriesNamed) -> Answered {
    let found = metrics.delete(&named.name, &named.labels)?;
    Ok(MetricsDeleted { found }.encode())
}

/// A read's series in parts of at most `bound` bytes: a series longer than
/// what a part leaves goes on in the next, `more` set on the piece before.
fn read_parts(series: Vec<Series>, bound: usize) -> VecDeque<Vec<u8>> {
    let mut parts: Parts<MetricsRead> = Parts::new(bound);
    for read in series {
        let head = read.name().len() + labels_weight(read.labels()) + ENVELOPE;
        let samples = read.samples();
        let mut from = 0;
        loop {
            let take = parts.room(head, SAMPLE).min(samples.len() - from);
            let cut = &samples[from..from + take];
            let piece = MetricsSeries {
                name: read.name().to_owned(),
                kind: read.kind().as_str().to_owned(),
                labels: read.labels().clone(),
                times: cut.iter().flat_map(|sample| unix_millis(sample.at).to_le_bytes()).collect(),
                values: cut.iter().flat_map(|sample| sample.value.to_le_bytes()).collect(),
                more: from + take < samples.len(),
            };
            parts.take(head + take * SAMPLE, |part| part.series.push(piece));
            from += take;
            if from == samples.len() {
                break;
            }
            parts.close();
        }
    }
    parts.finish()
}

fn latest_parts(latest: Vec<Latest>, bound: usize) -> VecDeque<Vec<u8>> {
    let mut parts: Parts<MetricsLatest> = Parts::new(bound);
    for newest in latest {
        let weight = newest.name.len() + labels_weight(&newest.labels) + ENVELOPE;
        parts.room(weight, 0);
        let newest =
            MetricsNewest { name: newest.name, labels: newest.labels, at: unix_millis(newest.at), value: newest.value };
        parts.take(weight, |part| part.series.push(newest));
    }
    parts.finish()
}

/// An aggregate's groups in parts of at most `bound` bytes: a group longer
/// than what a part leaves goes on in the next, `more` set on the piece
/// before.
fn aggregate_parts(groups: Vec<Aggregate>, bound: usize) -> VecDeque<Vec<u8>> {
    let mut parts: Parts<MetricsAggregates> = Parts::new(bound);
    for group in groups {
        let head = group.name.len() + labels_weight(&group.labels) + ENVELOPE;
        let mut buckets = group.buckets.into_iter().map(bucket_of).peekable();
        loop {
            let take = parts.room(head, BUCKET);
            let cut: Vec<MetricsBucket> = buckets.by_ref().take(take).collect();
            let more = buckets.peek().is_some();
            let piece = MetricsGroup { name: group.name.clone(), labels: group.labels.clone(), buckets: cut, more };
            parts.take(head + piece.buckets.len() * BUCKET, |part| part.groups.push(piece));
            if !more {
                break;
            }
            parts.close();
        }
    }
    parts.finish()
}

fn bucket_of(bucket: crate::metrics::Bucket) -> MetricsBucket {
    let mut out = MetricsBucket {
        at: unix_millis(bucket.at),
        samples: bucket.samples,
        resets: bucket.resets,
        partial: bucket.partial,
        gap: bucket.gap,
        ..MetricsBucket::default()
    };
    match bucket.value {
        Value::Exact(value) => out.value = Some(value),
        Value::Between { lower, upper, estimate } => {
            (out.lower, out.upper, out.estimate) = (Some(lower), Some(upper), estimate);
        }
    }
    out
}

fn labels_weight(labels: &BTreeMap<String, String>) -> usize {
    labels.iter().map(|(label, value)| label.len() + value.len() + 4).sum()
}

/// An answer's parts as they are filled: those closed, the one being
/// filled and what it weighs.
struct Parts<M> {
    bound: usize,
    done: VecDeque<Vec<u8>>,
    open: M,
    filled: bool,
    size: usize,
}

impl<M: Message> Parts<M> {
    fn new(bound: usize) -> Parts<M> {
        Parts { bound, done: VecDeque::new(), open: M::default(), filled: false, size: ENVELOPE }
    }

    /// How many items of `each` bytes the open part takes beside a head of
    /// `head`, closing it first when it holds something and has no room:
    /// one at least, so that an item larger than a part still goes.
    fn room(&mut self, head: usize, each: usize) -> usize {
        if self.filled && self.size + head + each > self.bound {
            self.close();
        }
        let left = self.bound.saturating_sub(self.size + head);
        left.checked_div(each).map_or(1, |fits| fits.max(1))
    }

    fn take(&mut self, weight: usize, put: impl FnOnce(&mut M)) {
        put(&mut self.open);
        self.filled = true;
        self.size += weight;
    }

    fn close(&mut self) {
        if std::mem::replace(&mut self.filled, false) {
            self.done.push_back(std::mem::take(&mut self.open).encode());
        }
        self.size = ENVELOPE;
    }

    /// Every part, the last among them: an empty answer is one empty part.
    fn finish(mut self) -> VecDeque<Vec<u8>> {
        self.done.push_back(self.open.encode());
        self.done
    }
}

/// Sends an answer's parts within the client's credit, its RESPONSE gone
/// out; the last ends the stream.
pub(crate) fn download(handles: &Handles, stream: u32, parts: VecDeque<Vec<u8>>, link: Link) {
    let download = Arc::new(Downloading { stream, state: Mutex::new((parts, link.credit, false)), link });
    lock(&handles.downloads).insert(stream, Arc::clone(&download));
    download.flush(&handles.downloads);
}

/// An answer's parts going out within the client's credit: those left, the
/// credit, and whether the stream ended.
struct Downloading {
    stream: u32,
    state: Mutex<(VecDeque<Vec<u8>>, u64, bool)>,
    link: Link,
}

impl Downloading {
    fn grant(&self, credit: u32, downloads: &Mutex<HashMap<u32, Arc<Downloading>>>) {
        lock(&self.state).1 += u64::from(credit);
        self.flush(downloads);
    }

    /// Sends the parts the credit takes; the last ends the stream.
    fn flush(&self, downloads: &Mutex<HashMap<u32, Arc<Downloading>>>) {
        let mut state = lock(&self.state);
        let (parts, credit, ended) = &mut *state;
        while !*ended {
            let Some(next) = parts.front() else { return };
            if next.len() as u64 > *credit {
                return;
            }
            let part = parts.pop_front().unwrap_or_default();
            *credit -= part.len() as u64;
            if parts.is_empty() {
                *ended = true;
                drop(state);
                lock(downloads).remove(&self.stream);
                return (self.link.finish)(Ok(part));
            }
            (self.link.send)(part);
        }
    }

    fn end(&self, last: Answered) {
        if std::mem::replace(&mut lock(&self.state).2, true) {
            return;
        }
        (self.link.finish)(last);
    }
}

/// Opens a client's instruments' stream: its session on the store, answered
/// in the RESPONSE with what the store knows of it.
pub(crate) fn instruments(store: &Store, handles: &Handles, stream: u32, body: &[u8], link: Link) -> Answered {
    let session = MetricsSession::decode(body)?;
    let id: [u8; 16] = session.id.as_slice().try_into().map_err(|_| Failure::invalid("a session of 16 bytes"))?;
    let (remote, ack) = store.metrics()?.remote(id, session.after);
    let instruments = Instruments { remote: Mutex::new(remote), waiting: Mutex::default(), link };
    lock(&handles.instruments).insert(stream, Arc::new(instruments));
    Ok(ack_of(ack).encode())
}

/// A client's instruments' stream: its session's stream on the store, the
/// batches waiting their turn, and what the stream needs of the session.
struct Instruments {
    remote: Mutex<Remote>,
    waiting: Mutex<Waiting>,
    link: Link,
}

#[derive(Default)]
struct Waiting {
    /// Each batch's bytes, and whether it was the client's last DATA.
    batches: VecDeque<(Vec<u8>, bool)>,
    /// A worker applies them.
    running: bool,
    /// The stream's final frame went: a batch applied after answers nothing.
    ended: bool,
}

impl Instruments {
    /// Applies the waiting batches in turn, each answered as a DATA; the
    /// client's last ends the stream with the last answer, and a batch
    /// refused ends it with why. A cancel may end the stream while a batch
    /// is applied: what goes out is decided under `waiting`, where the
    /// cancel says the stream ended, so that nothing follows its final frame.
    fn drain(self: &Arc<Self>, stream: u32, open: &Mutex<HashMap<u32, Arc<Instruments>>>) {
        loop {
            let (body, last) = {
                let mut waiting = lock(&self.waiting);
                let Some(next) = waiting.batches.pop_front().filter(|_| !waiting.ended) else {
                    waiting.running = false;
                    return;
                };
                next
            };
            let applied = self.apply(&body);
            let mut waiting = lock(&self.waiting);
            if waiting.ended {
                waiting.running = false;
                return;
            }
            match (applied, last) {
                (Ok(ack), false) => {
                    // Credit goes back once a batch is applied, as records and
                    // blobs grant theirs: a client sends no faster than it is taken.
                    (self.link.grant)(u32::try_from(body.len()).unwrap_or(u32::MAX));
                    (self.link.send)(ack);
                }
                (answered, _) => {
                    (waiting.ended, waiting.running) = (true, false);
                    drop(waiting);
                    // the number may name another stream by now, whose instruments stay
                    let mut open = lock(open);
                    if open.get(&stream).is_some_and(|kept| Arc::ptr_eq(kept, self)) {
                        open.remove(&stream);
                    }
                    drop(open);
                    return (self.link.finish)(answered);
                }
            }
        }
    }

    /// Ends the stream with `last` unless it ended already, the batches
    /// waiting dropped unapplied.
    fn end(&self, last: Answered) {
        {
            let mut waiting = lock(&self.waiting);
            if std::mem::replace(&mut waiting.ended, true) {
                return;
            }
            waiting.batches.clear();
        }
        (self.link.finish)(last);
    }

    fn apply(&self, body: &[u8]) -> Answered {
        let mut remote = lock(&self.remote);
        if body.is_empty() {
            return Ok(ack_of(remote.ack()).encode());
        }
        let batch = batch_of(MetricsBatch::decode(body)?)?;
        Ok(ack_of(remote.apply(batch)?).encode())
    }
}

fn ack_of(ack: Ack) -> MetricsAck {
    let refused = ack.refused.into_iter().map(|(instrument, error)| MetricsRefused { instrument, error }).collect();
    MetricsAck { applied: ack.applied, known: ack.known, refused }
}

/// A batch as the store applies it.
fn batch_of(batch: MetricsBatch) -> Result<Batch, Failure> {
    let instruments = batch.instruments.into_iter().map(|declared| {
        let buckets = match (declared.bounds, declared.exponential) {
            (Some(_), true) => return Err(Failure::invalid("an instrument's bounds or its grid, not both")),
            (Some(bounds), false) => Some(Buckets::Bounds(bounds)),
            (None, true) => Some(Buckets::Exponential),
            (None, false) => None,
        };
        Ok(Declared {
            number: declared.number,
            name: declared.name,
            form: declared.form,
            unit: declared.unit,
            help: declared.help,
            keep: declared.keep.map(Duration::from_millis),
            buckets,
        })
    });
    let sets = batch.sets.into_iter().map(|set| Set {
        number: set.number,
        instrument: set.instrument,
        labels: set.labels,
        overflow: set.overflow,
    });
    let changes = batch.changes.into_iter().map(|change| Change {
        set: change.set,
        add: change.add,
        value: change.value,
        count: change.count,
        sum: change.sum,
        longest: change.longest,
        buckets: change.buckets,
        bins: change.bins.into_iter().collect(),
    });
    Ok(Batch {
        number: batch.number,
        instruments: instruments.collect::<Result<_, _>>()?,
        sets: sets.collect(),
        changes: changes.collect(),
        flush: batch.flush,
    })
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
