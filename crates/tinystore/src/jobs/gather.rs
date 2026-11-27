//! A gather: what arrives under one id within a while becomes one job. The
//! store cannot call a program's merge, so the job's value is a JSON list and
//! a gather appends its items to it, each item's text kept as it came.
//!
//! ```text
//! 12:00:00 gather [a] settle 5m max 1h   → [a],     due 12:05:00, at the latest 13:00
//! 12:00:01 gather [b]                    → [a,b],   due 12:05:01
//! 12:58:00 gather [z]                    → [a,…,z], due 13:00:00, the first gather's max
//! ```

use std::time::{Duration, SystemTime};

use serde_json::value::RawValue;

use super::call::LONGEST_SPAN;
use super::values;
use crate::clock::{from_unix_millis, millis};
use crate::{Error, Result};

/// How a gather waits: `settle` after the last item, and never past `max`
/// after the first; `Gather::settle(5 * MINUTE).max(HOUR)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gather {
    settle: Duration,
    max: Option<Duration>,
}

impl Gather {
    /// The job runs `span` after the last gather under its id.
    pub fn settle(span: Duration) -> Gather {
        Gather { settle: span, max: None }
    }

    /// The job runs at the latest `span` after the gather that started it,
    /// however many follow.
    pub fn max(mut self, span: Duration) -> Gather {
        self.max = Some(span);
        self
    }

    /// The times a gather made at `now` gives: when it settles, and the
    /// latest the job it starts may run.
    pub(crate) fn times(&self, now: i64) -> Result<(i64, Option<i64>)> {
        let spans = std::iter::once(self.settle).chain(self.max);
        if spans.map(millis).any(|span| span > LONGEST_SPAN) {
            return Err(Error::invalid("a gather's settle or max longer than the years 1 to 9999"));
        }
        let latest = self.max.map(|max| now.saturating_add(millis(max)));
        Ok((now.saturating_add(millis(self.settle)), latest))
    }
}

/// What a gather did: whether it started the job, how many items the job
/// holds now, and when it is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gathered {
    pub added: bool,
    pub count: u64,
    pub at: SystemTime,
}

/// A gather's facts, checked before it waits for the writer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Gathering {
    /// When the job settles: the call's now and its settle.
    pub(crate) settled: i64,
    /// The latest the job runs when this gather starts it: now and its max.
    pub(crate) latest: Option<i64>,
    /// The items the call gives.
    pub(crate) count: u64,
}

/// What a gather's write did, the time in unix milliseconds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Wrote {
    pub(crate) added: bool,
    pub(crate) count: u64,
    pub(crate) due: i64,
}

impl Wrote {
    pub(crate) fn gathered(self) -> Gathered {
        Gathered { added: self.added, count: self.count, at: from_unix_millis(self.due) }
    }
}

/// The items of a JSON list, each as its text came: a gather never writes an
/// item again, so that a number's spelling stays as it was.
fn items<'a>(json: &'a [u8], what: &str) -> Result<Vec<&'a RawValue>> {
    serde_json::from_slice(json).map_err(|error| {
        Error::invalid(format!("{what} that is not a JSON list, which a gather appends to")).with_source(error)
    })
}

/// How many items a gather's value holds; one that is not a list is `invalid`.
pub(crate) fn count(given: &[u8]) -> Result<u64> {
    Ok(items(given, "a gather's value")?.len() as u64)
}

/// The kept list followed by the given one, how many items the kept one
/// held, and how many the whole holds; past a job's bound it is a `limit`.
pub(crate) fn appended(kept: &[u8], given: &[u8]) -> Result<(Vec<u8>, u64, u64)> {
    let kept = items(kept, "the job's value")?;
    let given = items(given, "a gather's value")?;
    let joined = joined(kept.iter().chain(&given).copied());
    values::check_size(joined.len(), "a gathered value")?;
    Ok((joined, kept.len() as u64, (kept.len() + given.len()) as u64))
}

/// A list cut after its first `taken` items: those, and the rest, each item's
/// text as it came. A run read the first part; what was gathered while it
/// ran is the second.
pub(crate) fn split(list: &[u8], taken: u64) -> Result<(Vec<u8>, Vec<u8>)> {
    let all = items(list, "the job's value")?;
    let at = usize::try_from(taken).unwrap_or(usize::MAX).min(all.len());
    Ok((joined(all[..at].iter().copied()), joined(all[at..].iter().copied())))
}

fn joined<'a>(items: impl Iterator<Item = &'a RawValue> + Clone) -> Vec<u8> {
    let length = items.clone().map(|item| item.get().len() + 1).sum::<usize>() + 1;
    let mut joined = Vec::with_capacity(length);
    joined.push(b'[');
    for (index, item) in items.enumerate() {
        if index > 0 {
            joined.push(b',');
        }
        joined.extend_from_slice(item.get().as_bytes());
    }
    joined.push(b']');
    joined
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    #[test]
    fn a_list_appended_keeps_each_items_text_as_it_came() {
        let (joined, kept, count) = appended(br#"[1.0, {"a" : 1e3}]"#, b"[ 7 ,\"x\"]").unwrap();
        assert_eq!(std::str::from_utf8(&joined).unwrap(), r#"[1.0,{"a" : 1e3},7,"x"]"#);
        assert_eq!((kept, count), (2, 4));
        assert_eq!(appended(b"[]", b"[]").unwrap(), (b"[]".to_vec(), 0, 0));
    }

    #[test]
    fn a_list_split_keeps_each_items_text_on_both_sides() {
        let (taken, rest) = split(br#"[1.0, {"a" : 1e3}, 7]"#, 2).unwrap();
        assert_eq!(std::str::from_utf8(&taken).unwrap(), r#"[1.0,{"a" : 1e3}]"#);
        assert_eq!(rest, b"[7]");
        assert_eq!(split(b"[1]", 5).unwrap(), (b"[1]".to_vec(), b"[]".to_vec()));
    }

    #[test]
    fn a_value_that_is_no_list_is_refused() {
        assert_eq!(appended(b"{}", b"[1]").unwrap_err().kind(), ErrorKind::Invalid);
        assert_eq!(count(b"3").unwrap_err().kind(), ErrorKind::Invalid);
    }
}
