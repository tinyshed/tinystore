//! `tinystore metrics`: a store's series from a shell. `names`, `read` and
//! `aggregate` read beside whoever holds the directory; `ingest` and
//! `delete` write through the server that holds it, or a sidecar they start.
//!
//! ```text
//! tinystore metrics ./data names
//! tinystore metrics ./data read ping_rtt_ms --where target=poland --since 1h
//! tinystore metrics ./data aggregate ping_rtt_ms --since 1h --every 10m --op max --by target
//! curl -s localhost:9100/metrics | tinystore metrics ./data ingest --label instance=web-1
//! ping … | awk '…' | tinystore metrics ./data ingest -f --kind ping_lost_total=counter
//! tinystore metrics ./data delete ping_rtt_ms target=poland
//! ```

use std::collections::BTreeSet;
use std::io::{IsTerminal as _, Read as _};
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime};

use serde_json::json;
use tinystore::metrics::{Aggregate, Condition, Labels, Metrics, Not, Op, Prefix, Query, Series, Value};
use tinystore::{ErrorKind, Store};

use crate::asked::{self, Asked, Done, Refused, Verb, failed, wrong};
use crate::reach;
use crate::window::{self, Window};
use crate::{following, help};

const VERBS: [Verb; 5] = [
    Verb { word: "names", command: &help::METRICS_NAMES, run: names },
    Verb { word: "read", command: &help::METRICS_READ, run: read },
    Verb { word: "aggregate", command: &help::METRICS_AGGREGATE, run: aggregate },
    Verb { word: "ingest", command: &help::METRICS_INGEST, run: ingest },
    Verb { word: "delete", command: &help::METRICS_DELETE, run: delete },
];

/// The samples `read` prints unless `--limit` says.
const SHOWN: usize = 1000;
/// The names a page of `names` holds.
const PAGE: usize = 1000;
/// The range a read takes when none is given.
const LAST_HOUR: Duration = Duration::from_secs(3600);
/// The width past which `aggregate` gives a line a bucket rather than a table.
const TABLE_WIDTH: usize = 120;
const OPS: &str = "count, sum, avg, min, max, increase, rate, delta, first, last, or p and a rank, as p95";
/// How long `ingest` reads stdin before it says a stream needs `-f`.
const STILL_READING: Duration = Duration::from_secs(5);
/// The longest text one ingest takes.
const ONE_TEXT: usize = 32 << 20;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run_verb("metrics", &VERBS, args)
}

/// The store beside whoever holds it, and its metrics; none while it keeps
/// none.
fn beside(dir: &Path) -> Result<(Store, Option<Metrics>), Refused> {
    asked::existing(dir)?;
    let store = Store::open_beside(dir).map_err(failed)?;
    let metrics = match store.metrics() {
        Ok(metrics) => Some(metrics),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(failed(error)),
    };
    Ok((store, metrics))
}

fn names(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("metrics", "names", true)?;
    let prefix = match &rest[..] {
        [] => String::new(),
        [prefix] => prefix.clone(),
        _ => return Err(wrong("names takes a prefix at most: names http_")),
    };
    let (_store, Some(metrics)) = beside(&dir)? else {
        return outln!("{} holds no metrics yet", dir.display());
    };
    let mut list = metrics.names().prefix(prefix).limit(PAGE);
    if let Some(after) = asked.value("--after") {
        list = list.after(after);
    }
    let page = list.page()?;
    if asked.has("--json") {
        for info in &page.names {
            let row = json!({
                "name": info.name, "kind": info.kind.as_str(), "series": info.series_count, "unit": info.unit,
                "help": info.help, "keep": window::span_text(info.keep),
                "retained_from": window::utc(info.retained_from), "newest": window::utc(info.newest),
            });
            outln!("{row}")?;
        }
        return page.next.map_or(Ok(()), |next| outln!("{}", json!({ "next": next })));
    }
    let width = page.names.iter().map(|info| info.name.len()).max().unwrap_or(0);
    for info in &page.names {
        let (series, keep) = (counted(info.series_count, "series"), window::span_text(info.keep));
        outln!(
            "{:width$}  {:9}  {series:>12}  keep {keep:5}  newest {}",
            info.name,
            info.kind,
            window::ago(info.newest)
        )?;
    }
    match page.next {
        Some(next) => read_on("metrics names", &format!("{PAGE} names"), Some(&next))?,
        None if page.names.is_empty() => outln!("{} holds no metric whose name starts with that", dir.display())?,
        None => {}
    }
    Ok(())
}

fn read(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("metrics", "read", true)?;
    let [name] = &rest[..] else {
        return Err(wrong("read takes a metric's name: read ping_rtt_ms"));
    };
    let window = Window::asked(&asked, LAST_HOUR)?;
    let most = asked.count("--limit", SHOWN)?;
    let gaps = asked.value("--gaps").map(asked::span).transpose().map_err(wrong)?;
    let (_store, Some(metrics)) = beside(&dir)? else {
        return outln!("{} holds no metrics yet", dir.display());
    };
    let page = paged(query_of(&metrics, name, &asked, window)?, &asked, most).read()?;
    if asked.has("--json") {
        return json_samples(&page.series, page.next.as_deref());
    }
    shown_samples(&page.series, window, gaps)?;
    read_on("metrics read", &format!("{most} samples"), page.next.as_deref())
}

/// A query read a page of `most` at a time, from `--after` on when given.
fn paged(query: Query, asked: &Asked, most: usize) -> Query {
    let query = query.limit(most);
    match asked.value("--after") {
        Some(after) => query.after(after),
        None => query,
    }
}

/// Says how to read the page after this one, when there is one: `most` is
/// what a page holds at most, the same words on the first page and the tenth.
fn read_on(command: &str, most: &str, next: Option<&str>) -> Done {
    if let Some(next) = next {
        asked::say(command, format!("a page of {most} at most; --after {next} reads on"));
    }
    Ok(())
}

/// Each series under a line that names it and the range, a sample a line;
/// `gaps` marks a silence longer than it.
fn shown_samples(series: &[Series], window: Window, gaps: Option<Duration>) -> Done {
    if series.is_empty() {
        return outln!("no series in {}", window.shown());
    }
    let dated = !window.one_day();
    for one in series {
        outln!("{one}   {} · {}", window.shown(), counted(one.samples().len() as u64, "sample"))?;
        let mut last: Option<SystemTime> = None;
        for sample in one.samples() {
            let silence = last.and_then(|last| sample.at.duration_since(last).ok());
            if let Some(silence) = silence.filter(|silence| gaps.is_some_and(|gap| *silence > gap)) {
                outln!("             · {} without samples", window::span_text(silence))?;
            }
            outln!("  {}   {}", window::clock(sample.at, dated), number(sample.value))?;
            last = Some(sample.at);
        }
    }
    Ok(())
}

/// A sample a line, and a last line with the page after it when there is
/// one, which `--after` takes.
fn json_samples(series: &[Series], next: Option<&str>) -> Done {
    for one in series {
        for sample in one.samples() {
            let row = json!({
                "time": window::utc(sample.at), "name": one.name(), "labels": one.labels(),
                "value": json_number(sample.value),
            });
            outln!("{row}")?;
        }
    }
    json_next(next)
}

/// The JSON's last line when more is there: `{"next": …}`, which `--after` takes.
fn json_next(next: Option<&str>) -> Done {
    match next {
        Some(next) => outln!("{}", json!({ "next": next })),
        None => Ok(()),
    }
}

fn aggregate(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("metrics", "aggregate", true)?;
    let [name] = &rest[..] else {
        return Err(wrong("aggregate takes a metric's name: aggregate ping_rtt_ms --op max"));
    };
    let word = asked.value("--op").ok_or_else(|| wrong(format!("--op: {OPS}")))?;
    let op = op_of(word)?;
    let window = Window::asked(&asked, LAST_HOUR)?;
    let (_store, Some(metrics)) = beside(&dir)? else {
        return outln!("{} holds no metrics yet", dir.display());
    };
    let mut query = query_of(&metrics, name, &asked, window)?;
    let every = asked.value("--every").map(asked::span).transpose().map_err(wrong)?;
    if let Some(every) = every {
        query = query.every(every);
    }
    let by = asked.values("--by");
    if !by.is_empty() {
        query = query.by(&by.iter().map(String::as_str).collect::<Vec<_>>());
    }
    let most = asked.count("--limit", SHOWN)?;
    let page = paged(query, &asked, most).aggregate(op)?;
    let groups = page.groups;
    if asked.has("--json") {
        json_buckets(&groups)?;
        return json_next(page.next.as_deref());
    }
    let every = every.map_or_else(String::new, |every| format!(" every {}", window::span_text(every)));
    outln!("{name} {word}{every}   {}", window.shown())?;
    shown_buckets(&groups, window)?;
    read_on("metrics aggregate", &format!("{most} buckets"), page.next.as_deref())
}

/// The buckets as a table, a row a bucket and a column a group, or a line a
/// bucket when the columns do not fit.
fn shown_buckets(groups: &[Aggregate], window: Window) -> Done {
    let times: BTreeSet<SystemTime> =
        groups.iter().flat_map(|group| group.buckets.iter().map(|bucket| bucket.at)).collect();
    if times.is_empty() {
        return outln!("  no samples");
    }
    let dated = !window.one_day();
    let columns: Vec<String> = groups.iter().map(|group| group_name(&group.labels)).collect();
    let cell = |group: &Aggregate, at: SystemTime| {
        group.buckets.iter().find(|bucket| bucket.at == at).map_or_else(String::new, |bucket| value_text(bucket.value))
    };
    let rows: Vec<(String, Vec<String>)> = times
        .iter()
        .map(|at| (window::clock(*at, dated), groups.iter().map(|group| cell(group, *at)).collect()))
        .collect();
    let widths: Vec<usize> = (0..columns.len())
        .map(|at| rows.iter().map(|(_, cells)| cells[at].len()).chain([columns[at].len()]).max().unwrap_or(0))
        .collect();
    let first = rows.iter().map(|(time, _)| time.len()).max().unwrap_or(0);
    if first + widths.iter().map(|width| width + 3).sum::<usize>() > TABLE_WIDTH {
        return lines_of(&rows, &columns);
    }
    let header: String = columns.iter().zip(&widths).map(|(column, width)| format!("   {column:>width$}")).collect();
    outln!("  {:first$}{header}", "")?;
    for (time, cells) in &rows {
        let shown: String = cells.iter().zip(&widths).map(|(cell, width)| format!("   {cell:>width$}")).collect();
        outln!("  {time:first$}{shown}")?;
    }
    Ok(())
}

/// A bucket a line: its time, its group and its value.
fn lines_of(rows: &[(String, Vec<String>)], columns: &[String]) -> Done {
    for (time, cells) in rows {
        for (column, cell) in columns.iter().zip(cells).filter(|(_, cell)| !cell.is_empty()) {
            outln!("  {time}  {column}  {cell}")?;
        }
    }
    Ok(())
}

fn json_buckets(groups: &[Aggregate]) -> Done {
    for group in groups {
        for bucket in &group.buckets {
            let mut row = json!({
                "time": window::utc(bucket.at), "name": group.name, "labels": group.labels, "samples": bucket.samples,
            });
            match bucket.value {
                Value::Exact(value) => row["value"] = json_number(value),
                Value::Between { lower, upper, .. } => {
                    row["lower"] = json_number(lower);
                    row["upper"] = json_number(upper);
                }
                _ => {}
            }
            outln!("{row}")?;
        }
    }
    Ok(())
}

/// Writes the text on stdin, read to its end, every series of it or none;
/// with `-f`, a stream that does not end, a batch at a time.
fn ingest(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("metrics", "ingest", false)?;
    if let Some(word) = rest.first() {
        return Err(wrong(format!("{word}: ingest reads Prometheus text on stdin and takes no words")));
    }
    let labels = labels(&asked.values("--label"))?;
    let keep = asked.value("--keep").map(asked::span).transpose().map_err(wrong)?;
    let kinds = asked.values("--kind").iter().map(|given| kind_of(given)).collect::<Result<Vec<_>, _>>()?;
    reach::store_at(&dir)?;
    if std::io::stdin().is_terminal() {
        return Err(wrong(
            "it reads Prometheus text on stdin, and stdin is a terminal\n  curl -s localhost:9100/metrics | \
             tinystore metrics ./data ingest",
        ));
    }
    if asked.has("-f") {
        return following::run(&dir, labels, keep, &kinds);
    }
    if !kinds.is_empty() {
        return Err(wrong("--kind names a type for a stream, -f; a whole text gives its own with # TYPE"));
    }
    let text = whole_stdin()?;
    let samples = samples_in(&text);
    // the time of a sample the text gives none, the same if it is sent again
    let at = SystemTime::now();
    reach::write(&dir, "metrics ingest", move |client| client.ingest_prometheus(&text, &labels, at, keep))?;
    outln!("wrote {} to {}", counted(samples as u64, "sample"), dir.display())
}

/// Stdin read to its end, saying after a while that a stream that does not
/// end needs `-f`, and refusing a text past what one ingest takes before a
/// server is reached.
fn whole_stdin() -> Result<Vec<u8>, Refused> {
    let (reading, read) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if read.recv_timeout(STILL_READING) == Err(RecvTimeoutError::Timeout) {
            asked::say("metrics ingest", "still reading stdin: a stream that does not end needs -f");
        }
    });
    let mut text = Vec::new();
    let read = std::io::stdin().take(ONE_TEXT as u64 + 1).read_to_end(&mut text);
    drop(reading);
    read.map_err(|error| failed(format!("stdin: {error}")))?;
    if text.len() > ONE_TEXT {
        return Err(failed("a text past the 32 MiB one ingest takes; a stream goes in a batch at a time with -f"));
    }
    Ok(text)
}

/// `--kind`'s name and type: `ping_lost_total=counter`.
fn kind_of(given: &str) -> Result<(String, String), Refused> {
    match given.split_once('=') {
        Some((name, word)) if !name.is_empty() && !word.is_empty() => Ok((name.to_owned(), word.to_owned())),
        _ => Err(wrong(format!("--kind {given}: a name and its type, as ping_lost_total=counter"))),
    }
}

/// Deletes one series, named by its exact labels.
fn delete(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("metrics", "delete", false)?;
    let Some((name, pairs)) = rest.split_first() else {
        return Err(wrong("delete takes a name and its series' labels: delete ping_rtt_ms target=poland"));
    };
    let labels = labels(pairs)?;
    let shown = labels.iter().fold(Series::gauge(name), |series, (label, value)| series.label(label, value));
    let (name, asked_labels) = (name.clone(), labels.clone());
    let found = reach::write(&dir, "metrics delete", move |client| client.delete_series(&name, &asked_labels))?;
    if found { outln!("deleted {shown}") } else { outln!("{} holds no series {shown}", dir.display()) }
}

fn query_of(metrics: &Metrics, name: &str, asked: &Asked, window: Window) -> Result<Query, Refused> {
    let query = metrics.query().name(name);
    let mut query = match window.since {
        Some(span) => query.since(span),
        None => query.from(window.from),
    };
    if let Some(to) = window.to {
        query = query.to(to);
    }
    for given in asked.values("--where") {
        let (label, condition) = condition_of(&given)?;
        query = query.filter(label, condition);
    }
    Ok(query)
}

/// `--where`'s condition: `target=poland`, `status=500,502`, `env!=dev`,
/// `host=api-*`.
fn condition_of(given: &str) -> Result<(String, Condition), Refused> {
    let listed = |values: &str| values.split(',').map(str::to_owned).collect::<Vec<_>>();
    if let Some((label, values)) = given.split_once("!=") {
        return Ok((label.to_owned(), Not(listed(values)).into()));
    }
    let Some((label, values)) = given.split_once('=').filter(|(label, _)| !label.is_empty()) else {
        return Err(wrong(format!("--where {given}: as target=poland, status=500,502, env!=dev or host=api-*")));
    };
    match values.strip_suffix('*') {
        Some(prefix) => Ok((label.to_owned(), Prefix(prefix.to_owned()).into())),
        None => Ok((label.to_owned(), listed(values).into())),
    }
}

fn op_of(word: &str) -> Result<Op, Refused> {
    let op = match word {
        "count" => Op::Count,
        "sum" => Op::Sum,
        "avg" => Op::Avg,
        "min" => Op::Min,
        "max" => Op::Max,
        "increase" => Op::Increase,
        "rate" => Op::Rate,
        "delta" => Op::Delta,
        "first" => Op::First,
        "last" => Op::Last,
        _ => match word.strip_prefix('p').and_then(|rank| rank.parse::<f64>().ok()) {
            Some(rank) => Op::Percentile(rank),
            None => return Err(wrong(format!("--op {word}: {OPS}"))),
        },
    };
    Ok(op)
}

/// A group as its column names it: the one label's value, or every label.
fn group_name(labels: &Labels) -> String {
    match labels.len() {
        0 => "value".to_owned(),
        1 => labels.values().next().cloned().unwrap_or_default(),
        _ => labels.iter().map(|(label, value)| format!("{label}={value}")).collect::<Vec<_>>().join(" "),
    }
}

fn value_text(value: Value) -> String {
    match value {
        Value::Exact(value) => number(value),
        Value::Between { lower, upper, .. } => format!("{}–{}", number(lower), number(upper)),
        _ => String::new(),
    }
}

/// A number as Prometheus prints one, `-0` and a NaN as they are.
fn number(value: f64) -> String {
    match value {
        value if value.is_nan() => "NaN".to_owned(),
        f64::INFINITY => "+Inf".to_owned(),
        f64::NEG_INFINITY => "-Inf".to_owned(),
        value => value.to_string(),
    }
}

/// A number as JSON carries one; JSON has none for a NaN or an infinity, so
/// they go as their text.
fn json_number(value: f64) -> serde_json::Value {
    if value.is_finite() { json!(value) } else { json!(number(value)) }
}

fn counted(count: u64, what: &str) -> String {
    match (count, what) {
        (1, _) | (_, "series") => format!("{count} {what}"),
        _ => format!("{count} {what}s"),
    }
}

/// `--label`'s pairs, and a series' labels: `instance=web-1`.
fn labels(given: &[String]) -> Result<Labels, Refused> {
    given
        .iter()
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) if !name.is_empty() => Ok((name.to_owned(), value.to_owned())),
            _ => Err(wrong(format!("{pair}: a label's name and its value, as instance=web-1"))),
        })
        .collect()
}

/// A Prometheus text's samples: a line each that is neither empty nor a
/// comment.
fn samples_in(text: &[u8]) -> usize {
    text.split(|byte| *byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_texts_samples_are_its_lines_that_are_no_comment() {
        let text = b"# HELP rtt round trip\n# TYPE rtt gauge\nrtt{target=\"poland\"} 23.1\n\n  rtt 1 1793014320000\r\n";
        assert_eq!(samples_in(text), 2);
    }

    #[test]
    fn a_label_is_a_name_and_its_value() {
        let given = labels(&["instance=web-1".to_owned(), "empty=".to_owned()]).unwrap();
        assert_eq!(given["instance"], "web-1");
        assert_eq!(given["empty"], "");
        assert!(labels(&["=web-1".to_owned()]).is_err());
        assert!(labels(&["web-1".to_owned()]).is_err());
    }

    #[test]
    fn a_condition_reads_as_a_shell_writes_it() {
        assert!(matches!(condition_of("target=poland").unwrap().1, Condition::OneOf(values) if values == ["poland"]));
        assert!(matches!(condition_of("status=500,502").unwrap().1, Condition::OneOf(values) if values.len() == 2));
        assert!(matches!(condition_of("env!=dev").unwrap().1, Condition::NoneOf(values) if values == ["dev"]));
        assert!(matches!(condition_of("host=api-*").unwrap().1, Condition::Prefix(prefix) if prefix == "api-"));
        assert!(condition_of("poland").is_err() && condition_of("=poland").is_err());
    }

    #[test]
    fn a_number_reads_as_prometheus_prints_it() {
        assert_eq!(number(23.1), "23.1");
        assert_eq!(number(-0.0), "-0");
        assert_eq!(number(f64::INFINITY), "+Inf");
        assert_eq!(number(f64::NAN), "NaN");
        assert_eq!(json_number(f64::NAN), json!("NaN"));
    }

    #[test]
    fn an_op_is_a_word_or_a_rank() {
        assert!(matches!(op_of("max"), Ok(Op::Max)));
        assert!(matches!(op_of("p95"), Ok(Op::Percentile(rank)) if rank == 95.0));
        assert!(op_of("median").is_err());
    }
}
