//! `tinystore metrics`: a store's series from a shell. `names`, `read` and
//! `aggregate` read beside whoever holds the directory; `ingest` and
//! `delete` write through the server that holds it, or a sidecar they start.
//!
//! ```text
//! tinystore metrics ./data names
//! tinystore metrics ./data read ping_rtt_ms --where target=poland --since 1h
//! tinystore metrics ./data aggregate ping_rtt_ms --since 1h --every 10m --op max --by target
//! curl -s localhost:9100/metrics | tinystore metrics ./data ingest --label instance=web-1
//! tinystore metrics ./data delete ping_rtt_ms target=poland
//! ```

use std::collections::BTreeSet;
use std::io::{IsTerminal as _, Read as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use serde_json::json;
use tinystore::metrics::{Aggregate, Condition, Labels, Metrics, Not, Op, Prefix, Query, Series, Value};
use tinystore::{ErrorKind, Store};

use crate::asked::{self, Asked, Done, Refused, failed, wrong};
use crate::help::{self, Command};
use crate::reach;
use crate::window::{self, Window};

/// A verb of `metrics`: its word, its help and what runs it.
struct Verb {
    word: &'static str,
    command: &'static Command,
    run: fn(Asked) -> Done,
}

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

pub(crate) fn run(args: &[String]) -> ExitCode {
    let Some(verb) = args.iter().find_map(|arg| VERBS.iter().find(|verb| verb.word == arg)) else {
        return given_no_verb(args);
    };
    asked::run(verb.command, args, verb.run)
}

/// `metrics` alone or with `--help`: every metrics command's help; with a
/// verb it does not know, that verb's nearest.
fn given_no_verb(args: &[String]) -> ExitCode {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return asked::exit("metrics", help::print_group("metrics"));
    }
    let verbs: Vec<&str> = VERBS.iter().map(|verb| verb.word).collect();
    let why = match args.iter().filter(|arg| !arg.starts_with('-')).nth(1) {
        Some(word) => format!("no command metrics {word}{}", crate::meant(word, &verbs)),
        None => format!("a verb after the directory: {}", verbs.join(", ")),
    };
    asked::end("metrics", wrong(why))
}

/// The directory before the verb and the words after it. A verb that only
/// reads takes the current directory when none comes before it.
fn placed(asked: &Asked, verb: &str, reads: bool) -> Result<(PathBuf, Vec<String>), Refused> {
    let at = asked.words.iter().position(|word| word == verb).unwrap_or_default();
    let after = asked.words.get(at + 1..).unwrap_or_default().to_vec();
    match (&asked.dir, at) {
        (Some(_), 1) => Err(wrong("a directory given twice: --dir, and a word before the verb")),
        (Some(dir), 0) => Ok((dir.clone(), after)),
        (None, 1) => Ok((PathBuf::from(&asked.words[0]), after)),
        (None, 0) if reads => Ok((PathBuf::from("."), after)),
        (None, 0) => Err(wrong(format!("a store's directory before the verb: tinystore metrics ./data {verb}"))),
        _ => Err(wrong(format!("one directory before {verb}, not {}", asked.words[..at].join(" ")))),
    }
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
    let (dir, rest) = placed(&asked, "names", true)?;
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
        Some(next) => asked::say("metrics names", format!("the first {PAGE} names; --after {next} reads on")),
        None if page.names.is_empty() => outln!("{} holds no metric whose name starts with that", dir.display())?,
        None => {}
    }
    Ok(())
}

fn read(asked: Asked) -> Done {
    let (dir, rest) = placed(&asked, "read", true)?;
    let [name] = &rest[..] else {
        return Err(wrong("read takes a metric's name: read ping_rtt_ms"));
    };
    let window = Window::asked(&asked, LAST_HOUR)?;
    let most = count(&asked, "--limit", SHOWN)?;
    let gaps = asked.value("--gaps").map(asked::span).transpose().map_err(wrong)?;
    let (_store, Some(metrics)) = beside(&dir)? else {
        return outln!("{} holds no metrics yet", dir.display());
    };
    let series = query_of(&metrics, name, &asked, window)?.read()?;
    if asked.has("--json") {
        return json_samples(&series, most);
    }
    shown_samples(&series, window, most, gaps)
}

/// Each series under a line that names it and the range, a sample a line;
/// `gaps` marks a silence longer than it.
fn shown_samples(series: &[Series], window: Window, most: usize, gaps: Option<Duration>) -> Done {
    if series.is_empty() {
        return outln!("no series in {}", window.shown());
    }
    let dated = !window.one_day();
    let total: usize = series.iter().map(|one| one.samples().len()).sum();
    let mut left = most;
    for one in series {
        if left == 0 {
            break;
        }
        outln!("{one}   {} · {}", window.shown(), counted(one.samples().len() as u64, "sample"))?;
        let mut last: Option<SystemTime> = None;
        for sample in one.samples().iter().take(left) {
            let silence = last.and_then(|last| sample.at.duration_since(last).ok());
            if let Some(silence) = silence.filter(|silence| gaps.is_some_and(|gap| *silence > gap)) {
                outln!("             · {} without samples", window::span_text(silence))?;
            }
            outln!("  {}   {}", window::clock(sample.at, dated), number(sample.value))?;
            last = Some(sample.at);
        }
        left = left.saturating_sub(one.samples().len());
    }
    if total > most {
        let narrow = "narrow with --where, --from and --to, aggregate with --every, or raise --limit";
        asked::say("metrics read", format!("showing {most} of {total} samples: {narrow}"));
    }
    Ok(())
}

/// A sample a line, and a last line with how many more there were.
fn json_samples(series: &[Series], most: usize) -> Done {
    let total: usize = series.iter().map(|one| one.samples().len()).sum();
    let mut left = most;
    for one in series {
        if left == 0 {
            break;
        }
        for sample in one.samples().iter().take(left) {
            let row = json!({
                "time": window::utc(sample.at), "name": one.name(), "labels": one.labels(),
                "value": json_number(sample.value),
            });
            outln!("{row}")?;
        }
        left = left.saturating_sub(one.samples().len());
    }
    if total > most { outln!("{}", json!({ "more": total - most })) } else { Ok(()) }
}

fn aggregate(asked: Asked) -> Done {
    let (dir, rest) = placed(&asked, "aggregate", true)?;
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
    let groups = query.aggregate(op)?;
    if asked.has("--json") {
        return json_buckets(&groups);
    }
    let every = every.map_or_else(String::new, |every| format!(" every {}", window::span_text(every)));
    outln!("{name} {word}{every}   {}", window.shown())?;
    shown_buckets(&groups, window)
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

/// Writes the text on stdin, read to its end: every series of it or none.
fn ingest(asked: Asked) -> Done {
    let (dir, rest) = placed(&asked, "ingest", false)?;
    if let Some(word) = rest.first() {
        return Err(wrong(format!("{word}: ingest reads Prometheus text on stdin and takes no words")));
    }
    let labels = labels(&asked.values("--label"))?;
    let keep = asked.value("--keep").map(asked::span).transpose().map_err(wrong)?;
    if std::io::stdin().is_terminal() {
        return Err(wrong(
            "it reads Prometheus text on stdin, and stdin is a terminal\n  curl -s localhost:9100/metrics | \
             tinystore metrics ./data ingest",
        ));
    }
    let mut text = Vec::new();
    std::io::stdin().read_to_end(&mut text).map_err(|error| failed(format!("stdin: {error}")))?;
    let samples = samples_in(&text);
    // the time of a sample the text gives none, the same if it is sent again
    let at = SystemTime::now();
    reach::write(&dir, "metrics ingest", move |client| client.ingest_prometheus(&text, &labels, at, keep))?;
    outln!("wrote {} to {}", counted(samples as u64, "sample"), dir.display())
}

/// Deletes one series, named by its exact labels.
fn delete(asked: Asked) -> Done {
    let (dir, rest) = placed(&asked, "delete", false)?;
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
    let mut query = metrics.query().name(name).from(window.from);
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

fn count(asked: &Asked, flag: &str, otherwise: usize) -> Result<usize, Refused> {
    asked
        .value(flag)
        .map_or(Ok(otherwise), |given| given.parse().map_err(|_| wrong(format!("{flag} {given}: a count"))))
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
