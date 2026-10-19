//! A series is its name and its labels. The store keeps the name as one label
//! more, `__name__`, as Prometheus does, so that a series is one sorted list of
//! pairs: its identity is the SHA-256 of that list, and a match by name and by
//! label is the same walk over postings.
//!
//! ```text
//! cpu, { host: web-1, zone: a }  →  __name__=cpu host=web-1 zone=a  →  cpu{host="web-1",zone="a"}
//! ```

use std::fmt;

use sha2::{Digest, Sha256};

use super::series::Labels;
use crate::compress::varint::{Reader, put_count};
use crate::{Error, Result};

/// The label a series' name is kept as, and why a label of a program's own
/// may not begin with `__`.
pub(super) const NAME: &str = "__name__";
/// The one label of the series an instrument counts into past its sets of
/// labels, which a grouping keeps whatever it names.
pub(super) const OVERFLOW: &str = "__overflow";
/// The label of a timer's `_bucket` series that counts the values of one
/// interval of an exponential grid, where `le` counts every value at most a
/// bound.
pub(super) const BIN: &str = "__bin";

/// A hundred short labels and thirty enormous ones are different threats, so
/// a series is bounded by four budgets rather than one count, its name
/// counted as a label.
pub(super) const MOST_LABELS: usize = 128;
const MOST_LABEL_BYTES: usize = 16 << 10;
/// A label's name; a series' name is a value, `__name__`'s, and takes
/// `MOST_VALUE_BYTES`.
pub(super) const MOST_NAME_BYTES: usize = 256;
pub(super) const MOST_VALUE_BYTES: usize = 4 << 10;

/// A series' labels as the store keeps them: its name among them, in the
/// order of their names' bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct Kept(Vec<(String, String)>);

impl Kept {
    /// The labels of the series `name` with `labels`, checked against every
    /// bound.
    pub(super) fn of(name: &str, labels: &Labels) -> Result<Kept> {
        if name.is_empty() {
            return Err(Error::invalid("a series needs a name"));
        }
        let mut pairs = Vec::with_capacity(labels.len() + 1);
        let mut named = false;
        for (label, value) in labels {
            if is_own(label) {
                return Err(Error::invalid(format!("label {label:?}: a name beginning with __ is the store's own")));
            }
            if !named && label.as_str() > NAME {
                pairs.push((NAME.to_owned(), name.to_owned()));
                named = true;
            }
            pairs.push((label.clone(), value.clone()));
        }
        if !named {
            pairs.push((NAME.to_owned(), name.to_owned()));
        }
        let kept = Kept(pairs);
        kept.check()?;
        Ok(kept)
    }

    /// The labels of a series the store may hold, the store's own among
    /// them, as a call that names one series exactly gives them: `delete`.
    pub(super) fn held(name: &str, labels: &Labels) -> Result<Kept> {
        if name.is_empty() {
            return Err(Error::invalid("a series needs a name"));
        }
        let mut pairs: Vec<(String, String)> =
            labels.iter().map(|(label, value)| (label.clone(), value.clone())).collect();
        pairs.push((NAME.to_owned(), name.to_owned()));
        pairs.sort_unstable();
        let kept = Kept(pairs);
        kept.check()?;
        Ok(kept)
    }

    /// The series of `name` an instrument counts into past its sets of
    /// labels: `__overflow` its one label, the store's own.
    pub(super) fn overflow(name: &str) -> Kept {
        Kept(vec![(NAME.to_owned(), name.to_owned()), (OVERFLOW.to_owned(), "true".to_owned())])
    }

    /// The same labels under another name, and one label more: the series a
    /// timer writes beside its own name, `http_request_ms_bucket{le="5"}`.
    /// A label given twice is refused.
    pub(super) fn derived(&self, name: &str, more: Option<(&str, &str)>) -> Result<Kept> {
        let mut pairs: Vec<(String, String)> = self.0.iter().filter(|(label, _)| label != NAME).cloned().collect();
        pairs.push((NAME.to_owned(), name.to_owned()));
        pairs.extend(more.map(|(label, value)| (label.to_owned(), value.to_owned())));
        pairs.sort_unstable();
        let kept = Kept(pairs);
        kept.check()?;
        Ok(kept)
    }

    /// Labels read back from the dictionary, which must still be a series'.
    pub(super) fn read(mut pairs: Vec<(String, String)>) -> Result<Kept> {
        pairs.sort_unstable();
        let kept = Kept(pairs);
        kept.check()
            .map_err(|error| Error::corrupt(format!("a series' labels in the dictionary: {}", error.what())))?;
        if kept.name().is_empty() {
            return Err(Error::corrupt("a series in the dictionary without a name"));
        }
        Ok(kept)
    }

    fn check(&self) -> Result<()> {
        if self.0.len() > MOST_LABELS {
            return Err(Error::invalid(format!("{} labels, where a series has {MOST_LABELS} at most", self.0.len())));
        }
        let mut bytes = 0;
        for (index, (label, value)) in self.0.iter().enumerate() {
            if label.is_empty() || label.len() > MOST_NAME_BYTES {
                return Err(Error::invalid(format!("label {label:?}: a name of 1 to {MOST_NAME_BYTES} bytes")));
            }
            if value.len() > MOST_VALUE_BYTES {
                return Err(Error::invalid(format!("label {label:?}: a value of {MOST_VALUE_BYTES} bytes at most")));
            }
            if index > 0 && self.0[index - 1].0 >= *label {
                return Err(Error::invalid(format!("label {label:?} twice")));
            }
            bytes += label.len() + value.len();
        }
        if bytes > MOST_LABEL_BYTES {
            return Err(Error::invalid(format!(
                "{bytes} bytes of labels, where a series has {MOST_LABEL_BYTES} at most"
            )));
        }
        Ok(())
    }

    pub(super) fn name(&self) -> &str {
        self.0.iter().find(|(label, _)| label == NAME).map_or("", |(_, value)| value)
    }

    /// The labels beside the name, as a program gives them.
    pub(super) fn labels(&self) -> Labels {
        self.0.iter().filter(|(label, _)| label != NAME).cloned().collect()
    }

    pub(super) fn pairs(&self) -> &[(String, String)] {
        &self.0
    }

    /// What its names and values weigh, against an ingest's bound.
    pub(super) fn bytes(&self) -> usize {
        self.0.iter().map(|(label, value)| label.len() + value.len()).sum()
    }

    /// The SHA-256 of the pairs, each name and value behind its length: two
    /// series with one identity are one series.
    pub(super) fn identity(&self) -> [u8; 32] {
        let mut canonical = Vec::with_capacity(self.bytes() + 4 * self.0.len());
        for (label, value) in &self.0 {
            put_count(&mut canonical, label.len() as u64);
            canonical.extend_from_slice(label.as_bytes());
            put_count(&mut canonical, value.len() as u64);
            canonical.extend_from_slice(value.as_bytes());
        }
        Sha256::digest(&canonical).into()
    }
}

impl fmt::Display for Kept {
    /// As Prometheus prints a series: `cpu{host="web-1"}`, `up`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format(f, self.name(), self.0.iter().filter(|(label, _)| label != NAME).map(|(label, value)| (label, value)))
    }
}

/// Writes a series as Prometheus prints it, a value's quotes, backslashes and
/// line breaks escaped.
/// Whether `label` is one of the store's own, `__name__` and its kind, which
/// a program does not set.
pub(super) fn is_own(label: &str) -> bool {
    label.starts_with("__")
}

pub(super) fn format<'a>(
    f: &mut impl fmt::Write,
    name: &str,
    labels: impl Iterator<Item = (&'a String, &'a String)>,
) -> fmt::Result {
    f.write_str(name)?;
    let mut first = true;
    for (label, value) in labels {
        f.write_char(if first { '{' } else { ',' })?;
        first = false;
        write!(f, "{label}=\"")?;
        for c in value.chars() {
            match c {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                c => f.write_char(c)?,
            }
        }
        f.write_char('"')?;
    }
    if !first {
        f.write_char('}')?;
    }
    Ok(())
}

/// The dictionary ids of a series' pairs, sorted and gap-coded behind their
/// count.
pub(super) fn encode_ids(ids: &mut [i64]) -> Vec<u8> {
    ids.sort_unstable();
    let mut out = Vec::with_capacity(2 * ids.len() + 1);
    put_count(&mut out, ids.len() as u64);
    let mut previous = 0;
    for &id in ids.iter() {
        put_count(&mut out, (id - previous) as u64);
        previous = id;
    }
    out
}

pub(super) fn decode_ids(bytes: &[u8]) -> Result<Vec<i64>> {
    let mut reader = Reader::new(bytes);
    let count = reader.count(MOST_LABELS);
    let mut ids = Vec::with_capacity(count);
    let mut previous = 0_i64;
    for _ in 0..count {
        let gap = reader.uvarint();
        match i64::try_from(gap).ok().filter(|gap| *gap > 0).and_then(|gap| previous.checked_add(gap)) {
            Some(id) => previous = id,
            None => reader.fail("a series' label ids do not increase"),
        }
        ids.push(previous);
    }
    if count == 0 {
        reader.fail("a series without label ids");
    }
    reader.finish().map_err(|error| error.within("a series' label ids"))?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs.iter().map(|(label, value)| ((*label).to_owned(), (*value).to_owned())).collect()
    }

    // contract: MET-02
    #[test]
    fn a_series_is_its_name_and_its_labels_in_any_order() {
        let one = Kept::of("cpu", &labels(&[("zone", "a"), ("host", "web-1"), ("Region", "eu")])).unwrap();
        let names: Vec<&str> = one.pairs().iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(names, ["Region", "__name__", "host", "zone"]);
        assert_eq!(one.to_string(), r#"cpu{Region="eu",host="web-1",zone="a"}"#);
        assert_eq!(one.name(), "cpu");
        assert_eq!(one.labels(), labels(&[("Region", "eu"), ("host", "web-1"), ("zone", "a")]));
        let again = Kept::of("cpu", &labels(&[("host", "web-1"), ("Region", "eu"), ("zone", "a")])).unwrap();
        assert_eq!(one.identity(), again.identity());
        let other = Kept::of("cpu", &labels(&[("host", "web-1"), ("Region", "eu"), ("zone", "b")])).unwrap();
        assert_ne!(one.identity(), other.identity());
        // a name and a value cannot run into each other
        let split = Kept::of("cpu", &labels(&[("ab", "c")])).unwrap();
        let moved = Kept::of("cpu", &labels(&[("a", "bc")])).unwrap();
        assert_ne!(split.identity(), moved.identity());
        assert_eq!(Kept::of("up", &Labels::new()).unwrap().to_string(), "up");
    }

    #[test]
    fn a_value_is_printed_as_prometheus_escapes_it() {
        let kept = Kept::of("say", &labels(&[("text", "a \"b\" \\ c\nd")])).unwrap();
        assert_eq!(kept.to_string(), r#"say{text="a \"b\" \\ c\nd"}"#);
    }

    #[test]
    fn a_series_past_a_bound_or_with_a_label_of_the_stores_is_refused() {
        let invalid = |name: &str, given: Labels| Kept::of(name, &given).unwrap_err().to_string();
        assert!(invalid("", Labels::new()).contains("needs a name"));
        assert!(invalid("cpu", labels(&[("__host", "a")])).contains("the store's own"));
        assert!(invalid("cpu", labels(&[("", "a")])).contains("1 to 256 bytes"));
        assert!(invalid("cpu", labels(&[(&"n".repeat(257), "a")])).contains("1 to 256 bytes"));
        assert!(invalid("cpu", labels(&[("host", &"v".repeat(4097))])).contains("4096 bytes at most"));
        let many: Labels = (0..128).map(|n| (format!("l{n}"), String::new())).collect();
        assert!(invalid("cpu", many).contains("129 labels"));
        let heavy: Labels = (0..5).map(|n| (format!("l{n}"), "v".repeat(4000))).collect();
        assert!(invalid("cpu", heavy).contains("bytes of labels"));
        Kept::of(&"n".repeat(4096), &labels(&[("host", &"v".repeat(4096))])).unwrap();
    }

    #[test]
    fn label_ids_read_back_and_refuse_what_does_not_increase() {
        let mut ids = [40, 3, 1_000_000, 7];
        let encoded = encode_ids(&mut ids);
        assert_eq!(decode_ids(&encoded).unwrap(), [3, 7, 40, 1_000_000]);
        assert!(decode_ids(&[2, 3, 0]).is_err(), "a gap of zero");
        assert!(decode_ids(&[0]).is_err(), "no ids");
        assert!(decode_ids(&[2, 3]).is_err(), "cut short");
    }
}
