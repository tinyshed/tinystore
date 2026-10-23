//! The bodies of metrics' fuzz targets, which `fuzz/` runs by hand on
//! nightly through cargo-fuzz, and tests run on seeds on every platform CI
//! runs on: what each decoder does with any bytes past the version and the
//! checksum that guard them, and what the reader of an exporter's text does
//! with any text. A decoder gives what it read or fails `corrupt`, the
//! reader what it read or `invalid`, and none panics.

use super::clock::{self, MOST_BLOCKS, Times};
use super::codec::{self, Head, Point};
use super::directory::{self, Group, Row};
use super::head::{self, Extent};
use super::prometheus;
use super::series::{Labels, Series};
use crate::compress::checksum::Crc32;
use crate::metrics::codec::zstd;
use crate::{ErrorKind, Result, unix_millis};

/// The series a head's checksum and a directory's are bound to.
const SERIES: i64 = 7;
/// The bytes `head_of` reads a head from.
const HEAD: usize = 25;

/// Any bytes read as a run's body under the head their first bytes give: a
/// run gives as many samples as its head says, or fails `corrupt`.
pub fn runs(bytes: &[u8]) {
    let (given, body) = bytes.split_at(bytes.len().min(HEAD));
    let head = head_of(given);
    let mut points = Vec::new();
    if read(codec::decode(&mut zstd(), &head, body, &mut points)) {
        assert_eq!(points.len(), head.count, "a run gives as many samples as its head says");
    }
}

/// Any bytes read as a series' head, a group's clock and its directory, each
/// sealed as its row is, so that a decoder reads past the checksum.
pub fn rows(bytes: &[u8]) {
    let (given, body) = bytes.split_at(bytes.len().min(HEAD));
    let head = head_of(given);
    let extent = Extent { count: head.count, start: head.start, end: head.end };
    let sealed_head = sealed(Crc32::new().update(&SERIES.to_le_bytes()), head::VERSION, body);
    if let Ok(chunks) = kept(head::parse(SERIES, extent, &sealed_head)) {
        read(head::decode(&mut zstd(), &chunks, i64::MIN, i64::MAX, &mut Vec::new()));
    }
    read(clock::decode(&sealed(Crc32::new(), clock::VERSION, body)));
    let group = Group { series: SERIES, start: head.start, end: head.end, clock: 1, live: 0, blocks: Vec::new() };
    let mut directory = vec![directory::VERSION];
    directory.extend_from_slice(body);
    directory.extend_from_slice(&group.checksum(&directory).to_le_bytes());
    let row = Row { series: SERIES, start: head.start, end: head.end, clock: 1, directory };
    // a directory's count is its second byte, the body's first
    let count = usize::from(body.first().copied().unwrap_or(0)).min(MOST_BLOCKS);
    read(Group::decode(&mut zstd(), &row, vec![times(); count]));
}

/// Any bytes read as a text in Prometheus' format or OpenMetrics': its series
/// read again the same from the text they spell, or the text refused,
/// `invalid` or a `limit`, naming its line.
pub fn exposition(bytes: &[u8]) {
    let given = Labels::from([("instance".to_owned(), "web-1".to_owned())]);
    let series = match prometheus::parse(bytes, &given, 1_790_000_000_000) {
        Ok(series) => series,
        Err(error) => {
            assert!(matches!(error.kind(), ErrorKind::Invalid | ErrorKind::Limit), "{error}");
            assert!(error.what().starts_with("metrics: line "), "{error}");
            return;
        }
    };
    let spelled: String = series.iter().map(spelled).collect();
    let again = prometheus::parse(spelled.as_bytes(), &Labels::new(), 0).expect("the text a reading spells reads");
    assert_eq!(seen(&again), seen(&series), "{spelled}");
}

/// A series of one sample as Prometheus' text spells it, its time in
/// milliseconds and its value's sign kept, a NaN's among them.
fn spelled(series: &Series) -> String {
    let escaped = |value: &str| value.replace('\\', r"\\").replace('"', r#"\""#).replace('\n', r"\n");
    let labels: Vec<String> =
        series.labels.iter().map(|(label, value)| format!("{label}=\"{}\"", escaped(value))).collect();
    let sample = series.samples[0];
    let value = match sample.value {
        nan if nan.is_nan() && nan.is_sign_negative() => "-NaN".to_owned(),
        value => format!("{value:?}"),
    };
    format!("{}{{{}}} {value} {}\n", series.name, labels.join(","), unix_millis(sample.at))
}

/// What a reading gives of each series but its kind, which a text spelt
/// without its types does not say.
fn seen(series: &[Series]) -> Vec<(&str, &Labels, i64, u64)> {
    let sample = |series: &Series| series.samples[0];
    series
        .iter()
        .map(|series| (series.name(), series.labels(), unix_millis(sample(series).at), sample(series).value.to_bits()))
        .collect()
}

/// A head its first bytes give: its start, its end, its count and the bits
/// of its first value.
fn head_of(given: &[u8]) -> Head {
    let mut padded = [0_u8; HEAD];
    padded[..given.len()].copy_from_slice(given);
    let word = |at: usize| padded[at..at + 8].try_into().unwrap_or_default();
    Head {
        start: i64::from_le_bytes(word(0)),
        end: i64::from_le_bytes(word(8)),
        count: usize::from(padded[16]),
        first: f64::from_bits(u64::from_le_bytes(word(17))),
    }
}

/// Bytes framed as a row is, its version first and its checksum last.
fn sealed(sum: Crc32, version: u8, body: &[u8]) -> Vec<u8> {
    let mut row = Vec::with_capacity(body.len() + 5);
    row.push(version);
    row.extend_from_slice(body);
    let sum = sum.update(&row).finish();
    row.extend_from_slice(&sum.to_le_bytes());
    row
}

/// The times of a block in a group's clock: four samples a second apart.
fn times() -> Times {
    let points: Vec<Point> = (0..4).map(|n| Point { at: n * 1000, value: 0.0 }).collect();
    Times::of(&points)
}

/// Whether a decoder read what it was given; a failure is `corrupt`.
fn read<T>(result: Result<T>) -> bool {
    kept(result).is_ok()
}

fn kept<T>(result: Result<T>) -> Result<T> {
    if let Err(error) = &result {
        assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run of 240 samples 15 seconds apart, each value from `value`.
    fn run(value: impl Fn(i64) -> f64) -> Vec<Point> {
        (0..240).map(|n| Point { at: 1_700_000_000_000 + n * 15_000, value: value(n) }).collect()
    }

    /// The bytes a head gives `head_of`.
    fn spelled(head: &Head) -> Vec<u8> {
        let mut bytes = head.start.to_le_bytes().to_vec();
        bytes.extend_from_slice(&head.end.to_le_bytes());
        bytes.push(u8::try_from(head.count).unwrap());
        bytes.extend_from_slice(&head.first.to_bits().to_le_bytes());
        bytes
    }

    /// Every byte of a seed flipped in turn, and every cut of it.
    fn broken(seed: &[u8], target: fn(&[u8])) {
        target(seed);
        for place in 0..seed.len() {
            let mut flipped = seed.to_vec();
            flipped[place] ^= 0x10;
            target(&flipped);
            target(&seed[..place]);
        }
    }

    // contract: ERR-04
    #[test]
    fn a_run_read_from_any_bytes_gives_its_samples_or_fails_corrupt() {
        for values in
            [run(|n| n as f64 * 0.25), run(|n| (n as f64).sin()), run(|n| if n % 7 == 0 { f64::NAN } else { 1.5 })]
        {
            let (head, body) = codec::encode(&mut zstd(), &values).unwrap();
            let mut seed = spelled(&head);
            seed.extend_from_slice(&body);
            broken(&seed, runs);
        }
    }

    // contract: ERR-04
    #[test]
    fn a_head_a_clock_and_a_directory_read_from_any_bytes_give_what_they_hold_or_fail_corrupt() {
        let points = run(|n| n as f64);
        let head = Head::of(&points);
        let clock = clock::encode(&[times(), times()]);
        let mut seed = spelled(&head);
        // a clock's content, past its version and before its checksum
        seed.extend_from_slice(&clock[1..clock.len() - 4]);
        broken(&seed, rows);
        let encoded = head::encode(&mut zstd(), SERIES, &[], &points).unwrap();
        let mut seed = spelled(&head);
        seed.extend_from_slice(&encoded[1..encoded.len() - 4]);
        broken(&seed, rows);
        rows(&[]);
        rows(&[0xff; 64]);
    }

    // contract: MET-15
    #[test]
    fn a_text_read_from_any_bytes_gives_the_series_it_spells_or_is_refused_naming_its_line() {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../../../../testdata/metrics/prometheus.json")).unwrap();
        for case in vectors["cases"].as_array().unwrap() {
            broken(case["text"].as_str().unwrap().as_bytes(), exposition);
        }
        exposition(b"\xff\xfe");
        exposition(&[b'#'; 64]);
    }
}
