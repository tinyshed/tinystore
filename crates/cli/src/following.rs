//! `metrics ingest -f`: Prometheus' text as a stream that does not end
//! brings it, `ping | awk` or a scrape in a loop, read a line at a time by
//! the library's own reader and written a batch at a time, every second or
//! at 10,000 lines. A line that does not read, or one the store refuses, is
//! said with its number and skipped, and the command ends 1 once the stream
//! does.
//!
//! ```text
//! ping … | awk '{ print "ping_rtt_ms{target=\"poland\"}", $7 }' | tinystore metrics ./data ingest -f
//! tinystore metrics ingest: ping_rtt_ms came with no # TYPE, so it is a gauge; --kind … says otherwise
//! tinystore metrics ingest: line 7, `ping_rtt_ms{target="poland"} time=23.1`: … invalid; skipped
//! ```

use std::collections::HashSet;
use std::io::{self, BufRead as _, Read as _};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use tinystore::client::PrometheusStream;
use tinystore::metrics::{Labels, Series};
use tinystore::{Error, ErrorKind};

use crate::asked::{self, Done, failed, wrong};
use crate::reach::Writer;

const COMMAND: &str = "metrics ingest";
/// How long a line read waits to be written.
const EVERY: Duration = Duration::from_secs(1);
/// The lines a batch holds at most: the samples one ingest takes.
const LINES: usize = 10_000;
/// What a batch's series weigh at most, well within what one message carries.
const WEIGHT: usize = 1 << 20;
/// What a series weighs in a request beside its name and its labels.
const SERIES_WEIGHT: usize = 48;
/// The longest line read as one: the rest of a longer one reads as lines of
/// its own, which do not read as samples.
const LONGEST: u64 = 64 << 10;

/// Follows stdin until it ends, each series with `labels` beside its own,
/// each name kept `keep` when given, and each of `kinds` a name and its type.
pub(crate) fn run(dir: &Path, labels: Labels, keep: Option<Duration>, kinds: &[(String, String)]) -> Done {
    let mut stream = PrometheusStream::new(labels);
    for (name, word) in kinds {
        stream.declare(name, word).map_err(|error| wrong(format!("--kind {name}={word}: {error}")))?;
    }
    let mut writer = Writer::new(dir, COMMAND);
    writer.reach()?;
    let mut following = Following::new(stream, writer, keep);
    let lines = lines_of_stdin();
    let mut number = 0;
    loop {
        match lines.recv_timeout(following.wait()) {
            Ok(Ok((line, at))) => {
                number += 1;
                following.line(number, &line, at)?;
            }
            Ok(Err(error)) => {
                following.send()?;
                return Err(failed(format!("stdin: {error}")));
            }
            Err(RecvTimeoutError::Timeout) => following.send()?,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    following.send()?;
    following.end(dir)
}

/// Each line of stdin with the time it was read, read on a thread of its
/// own so that a batch is written on time while no line comes. A writer
/// slower than the stream stops the reading once `LINES` wait.
fn lines_of_stdin() -> mpsc::Receiver<io::Result<(Vec<u8>, SystemTime)>> {
    let (send, lines) = mpsc::sync_channel(LINES);
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        loop {
            let mut line = Vec::new();
            let read = match stdin.by_ref().take(LONGEST).read_until(b'\n', &mut line) {
                Ok(0) => return,
                Ok(_) => Ok((line, SystemTime::now())),
                Err(error) => Err(error),
            };
            let ended = read.is_err();
            if send.send(read).is_err() || ended {
                return;
            }
        }
    });
    lines
}

/// The stream as it is read: the series of the lines not yet written, and
/// what was written and skipped.
struct Following {
    stream: PrometheusStream,
    writer: Writer,
    keep: Option<Duration>,
    series: Vec<Series>,
    /// The line each series of the batch came from.
    numbers: Vec<usize>,
    weight: usize,
    /// When the batch's first series was read.
    since: Option<Instant>,
    /// The names said to be gauges, and the names given their description.
    said: HashSet<String>,
    described: HashSet<String>,
    wrote: u64,
    skipped: u64,
}

impl Following {
    fn new(stream: PrometheusStream, writer: Writer, keep: Option<Duration>) -> Following {
        let (said, described) = (HashSet::new(), HashSet::new());
        let (series, numbers, weight, since, wrote, skipped) = (Vec::new(), Vec::new(), 0, None, 0, 0);
        Following { stream, writer, keep, series, numbers, weight, since, said, described, wrote, skipped }
    }

    fn line(&mut self, number: usize, line: &[u8], at: SystemTime) -> Done {
        match self.stream.line(line, at) {
            Ok(Some(series)) => self.add(number, series),
            Ok(None) => Ok(()),
            Err(error) => {
                self.skip(number, &error);
                Ok(())
            }
        }
    }

    fn add(&mut self, number: usize, series: Series) -> Done {
        let name = series.name();
        if self.stream.untyped(name) && !self.said.contains(name) {
            self.said.insert(name.to_owned());
            asked::say(
                COMMAND,
                format!("{name} came with no # TYPE, so it is a gauge; --kind {name}=counter says otherwise"),
            );
        }
        let labels: usize = series.labels().iter().map(|(label, value)| label.len() + value.len()).sum();
        self.weight += SERIES_WEIGHT + name.len() + labels;
        self.since.get_or_insert_with(Instant::now);
        self.series.push(match self.keep {
            Some(keep) => series.keep(keep),
            None => series,
        });
        self.numbers.push(number);
        if self.series.len() >= LINES || self.weight >= WEIGHT { self.send() } else { Ok(()) }
    }

    /// How long the next line may be waited for before the batch is due.
    fn wait(&self) -> Duration {
        self.since.map_or(Duration::from_secs(3600), |since| EVERY.saturating_sub(since.elapsed()))
    }

    /// Writes the batch. One the store refuses goes a series at a time, so
    /// that the lines it refuses are said and skipped and the rest written.
    fn send(&mut self) -> Done {
        if self.series.is_empty() {
            return Ok(());
        }
        let series = Arc::new(std::mem::take(&mut self.series));
        let numbers = std::mem::take(&mut self.numbers);
        (self.weight, self.since) = (0, None);
        let sent = Arc::clone(&series);
        match self.writer.try_write(move |client| client.ingest(&sent))? {
            Ok(()) => self.wrote += series.len() as u64,
            Err(error) if refused(&error) => self.send_alone(&series, &numbers)?,
            Err(error) => return Err(failed(error)),
        }
        self.describe(&series)
    }

    fn send_alone(&mut self, series: &Arc<Vec<Series>>, numbers: &[usize]) -> Done {
        for (index, number) in numbers.iter().enumerate() {
            let all = Arc::clone(series);
            match self.writer.try_write(move |client| client.ingest(std::slice::from_ref(&all[index])))? {
                Ok(()) => self.wrote += 1,
                Err(error) if refused(&error) => self.skip(*number, &error),
                Err(error) => return Err(failed(error)),
            }
        }
        Ok(())
    }

    /// Gives each name the unit and the help its family describes, the first
    /// time it is written.
    fn describe(&mut self, series: &[Series]) -> Done {
        for one in series {
            let name = one.name();
            if self.described.contains(name) {
                continue;
            }
            self.described.insert(name.to_owned());
            if let Some(description) = self.stream.description(name) {
                let name = name.to_owned();
                self.writer.write(move |client| client.describe(&name, &description))?;
            }
        }
        Ok(())
    }

    fn skip(&mut self, number: usize, error: &Error) {
        self.skipped += 1;
        asked::say(COMMAND, format!("line {number}, {error}; skipped"));
    }

    /// The line a write prints, and exit 1 when a line was skipped.
    fn end(self, dir: &Path) -> Done {
        let samples = if self.wrote == 1 { "1 sample".to_owned() } else { format!("{} samples", self.wrote) };
        outln!("wrote {samples} to {}", dir.display())?;
        match self.skipped {
            0 => Ok(()),
            1 => Err(failed("skipped 1 line")),
            skipped => Err(failed(format!("skipped {skipped} lines"))),
        }
    }
}

/// Whether the store refused what a write carried, rather than the write.
fn refused(error: &Error) -> bool {
    matches!(error.kind(), ErrorKind::Invalid | ErrorKind::TooOld | ErrorKind::TooNew | ErrorKind::Conflict)
}
