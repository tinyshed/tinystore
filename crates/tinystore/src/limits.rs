//! The bounds one call meets, each by the name a `limit` error gives it. The
//! error carries three facts a program reads without parsing its text:
//! `limit`, the bound's name; `wanted`, what the call would have taken of it;
//! and `bound`, the most it takes.
//!
//! ```no_run
//! # use tinystore::limits::SET_MANY_DOCUMENTS;
//! # fn main() -> tinystore::Result<()> {
//! # let store = tinystore::Store::open("data", Default::default())?;
//! # let catalogue: Vec<(String, String)> = Vec::new();
//! let courses = store.index::<String>("courses").open()?;
//! for batch in catalogue.chunks(SET_MANY_DOCUMENTS.bound as usize) {
//!     courses.set_many(batch.iter().map(|(id, name)| (id, name)))?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Each SDK exports the same table, which `testdata/wire/limits.json` holds.
//! A bound the store's options set, as its memory or `max_series`, is named
//! the same way and is not here.

#[cfg(feature = "store")]
use crate::Error;

/// A bound by its name, which a `limit` error's `limit` fact gives, and the
/// most it takes, its `bound` fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Limit {
    pub name: &'static str,
    pub bound: u64,
}

impl Limit {
    /// The `limit` error of a call that wanted `wanted` of this bound, said
    /// as `what` says it, with the three facts every SDK reads.
    #[cfg(feature = "store")]
    pub(crate) fn refuse(self, wanted: u64, what: impl Into<String>) -> Error {
        self.run_out(what).naming("wanted", wanted.to_string())
    }

    /// The `limit` error of a bound that has run out, as a counter that cannot
    /// move once more: its name and its bound, with no number wanted.
    #[cfg(feature = "store")]
    pub(crate) fn run_out(self, what: impl Into<String>) -> Error {
        Error::limit(what).naming("limit", self.name).naming("bound", self.bound.to_string())
    }
}

/// A bucket's value, as it is stored.
#[cfg(feature = "kv")]
pub const VALUE_BYTES: Limit = Limit { name: "value bytes", bound: 1 << 20 };

/// A live setting's change, as JSON.
#[cfg(feature = "config")]
pub const SETTING_BYTES: Limit = Limit { name: "setting bytes", bound: 16 << 10 };
/// Every change a config keeps, together.
#[cfg(feature = "config")]
pub const CONFIG_BYTES: Limit = Limit { name: "config bytes", bound: 256 << 10 };

/// The callers that wait for one lock or semaphore at once.
#[cfg(feature = "locks")]
pub const LOCK_WAITERS: Limit = Limit { name: "lock waiters", bound: 10_000 };

/// The subscriptions to the store's channels at once.
#[cfg(feature = "channels")]
pub const SUBSCRIPTIONS: Limit = Limit { name: "subscriptions", bound: 10_000 };

/// A job's payload, its result or a step's answer, as JSON.
#[cfg(feature = "jobs")]
pub const JOB_BYTES: Limit = Limit { name: "job bytes", bound: 1 << 20 };
/// A run's progress, as JSON.
#[cfg(feature = "jobs")]
pub const PROGRESS_BYTES: Limit = Limit { name: "progress bytes", bound: 4 << 10 };

/// The values one call reads from a database: past them, read a page at a
/// time or a row at a time.
#[cfg(feature = "sql")]
pub const ROWS_BYTES: Limit = Limit { name: "bytes of a call's rows", bound: 64 << 20 };

/// One event, its name, fields and text together.
#[cfg(feature = "records")]
pub const EVENT_BYTES: Limit = Limit { name: "event bytes", bound: 256 << 10 };
/// The events of one append together.
#[cfg(feature = "records")]
pub const APPEND_BYTES: Limit = Limit { name: "append bytes", bound: 4 << 20 };
/// The rows one `aggregate` answers.
#[cfg(feature = "records")]
pub const AGGREGATE_ROWS: Limit = Limit { name: "aggregate rows", bound: 10_000 };
/// The different values one `top` or `distinct` holds, as JSON.
#[cfg(feature = "records")]
pub const COUNT_BYTES: Limit = Limit { name: "count bytes", bound: 64 << 20 };

/// The samples of one `ingest`.
#[cfg(feature = "metrics")]
pub const INGEST_SAMPLES: Limit = Limit { name: "ingest samples", bound: 10_000 };
/// The bytes of one `ingest`'s samples, each with its labels.
#[cfg(feature = "metrics")]
pub const INGEST_BYTES: Limit = Limit { name: "ingest bytes", bound: 4 << 20 };
/// The samples of one exporter's text.
#[cfg(feature = "metrics")]
pub const TEXT_SAMPLES: Limit = Limit { name: "text samples", bound: 100_000 };
/// One exporter's text, and the bytes of its samples.
#[cfg(feature = "metrics")]
pub const TEXT_BYTES: Limit = Limit { name: "text bytes", bound: 32 << 20 };
/// The series one query finds.
#[cfg(feature = "metrics")]
pub const QUERY_SERIES: Limit = Limit { name: "series", bound: 1000 };
/// What one query reads from the file: labels, directories, payloads, heads.
#[cfg(feature = "metrics")]
pub const QUERY_BYTES: Limit = Limit { name: "bytes read", bound: 16 << 20 };
/// The samples one query decodes.
#[cfg(feature = "metrics")]
pub const QUERY_SAMPLES: Limit = Limit { name: "samples decoded", bound: 1 << 20 };
/// The samples or buckets one query answers.
#[cfg(feature = "metrics")]
pub const QUERY_ANSWER: Limit = Limit { name: "answer", bound: 100_000 };

/// The documents of one `set_many`.
#[cfg(feature = "search")]
pub const SET_MANY_DOCUMENTS: Limit = Limit { name: "set_many documents", bound: 1000 };
/// The documents of one `set_many` as JSON. Over the wire one request body
/// bounds it first, [`REQUEST_BYTES`].
#[cfg(feature = "search")]
pub const SET_MANY_BYTES: Limit = Limit { name: "set_many bytes", bound: 16 << 20 };
/// One document as JSON, or a table's row as its index reads it.
#[cfg(feature = "search")]
pub const DOCUMENT_BYTES: Limit = Limit { name: "document bytes", bound: 1 << 20 };
/// The hits a search reaches, its offset and its limit together.
#[cfg(feature = "search")]
pub const SEARCH_HITS: Limit = Limit { name: "search hits", bound: 1000 };
/// A search's text, in bytes.
#[cfg(feature = "search")]
pub const SEARCH_TEXT: Limit = Limit { name: "search text", bound: 1 << 10 };
/// The conditions of one search's filter.
#[cfg(feature = "search")]
pub const FILTER_CONDITIONS: Limit = Limit { name: "filter conditions", bound: 8 };
/// The values the lists of one search's conditions hold together.
#[cfg(feature = "search")]
pub const FILTER_VALUES: Limit = Limit { name: "filter values", bound: 100 };
/// A text a filter keeps or asks for, in bytes.
#[cfg(feature = "search")]
pub const FILTER_TEXT: Limit = Limit { name: "filter text", bound: 256 };
/// A vector's dimensions.
#[cfg(feature = "search")]
pub const VECTOR_DIMENSIONS: Limit = Limit { name: "vector dimensions", bound: 4096 };

/// One request over the wire, as every SDK and the command line agree it at
/// their HELLO: a call whose message is larger fails before it is sent.
#[cfg(feature = "wire")]
pub const REQUEST_BYTES: Limit = Limit { name: "request bytes", bound: 2 << 20 };
/// One answer over the wire, agreed as a request is.
#[cfg(feature = "wire")]
pub const ANSWER_BYTES: Limit = Limit { name: "answer bytes", bound: 2 << 20 };

/// Every bound above, which testdata/wire/limits.json holds for the SDKs.
#[cfg(all(
    test,
    feature = "config",
    feature = "locks",
    feature = "channels",
    feature = "jobs",
    feature = "sql",
    feature = "records",
    feature = "metrics",
    feature = "search",
    feature = "wire"
))]
const ALL: &[Limit] = &[
    VALUE_BYTES,
    SETTING_BYTES,
    CONFIG_BYTES,
    LOCK_WAITERS,
    SUBSCRIPTIONS,
    JOB_BYTES,
    PROGRESS_BYTES,
    ROWS_BYTES,
    EVENT_BYTES,
    APPEND_BYTES,
    AGGREGATE_ROWS,
    COUNT_BYTES,
    INGEST_SAMPLES,
    INGEST_BYTES,
    TEXT_SAMPLES,
    TEXT_BYTES,
    QUERY_SERIES,
    QUERY_BYTES,
    QUERY_SAMPLES,
    QUERY_ANSWER,
    SET_MANY_DOCUMENTS,
    SET_MANY_BYTES,
    DOCUMENT_BYTES,
    SEARCH_HITS,
    SEARCH_TEXT,
    FILTER_CONDITIONS,
    FILTER_VALUES,
    FILTER_TEXT,
    VECTOR_DIMENSIONS,
    REQUEST_BYTES,
    ANSWER_BYTES,
];

#[cfg(all(test, feature = "store"))]
mod tests {
    use super::*;

    // contract: ERR-05
    #[test]
    fn a_refusal_names_its_bound_what_was_wanted_and_the_bound() {
        let refused = Limit { name: "widgets", bound: 3 }.refuse(4, "4 widgets, past 3");
        assert_eq!(refused.to_string(), "4 widgets, past 3: limit");
        let facts = (refused.fact("limit"), refused.fact("wanted"), refused.fact("bound"));
        assert_eq!(facts, (Some("widgets"), Some("4"), Some("3")));
        let gone = Limit { name: "ids", bound: 9 }.run_out("ids run out");
        assert_eq!((gone.fact("limit"), gone.fact("wanted"), gone.fact("bound")), (Some("ids"), None, Some("9")));
    }

    /// Every SDK reads the same table from testdata/wire/limits.json.
    #[test]
    #[cfg(all(
        feature = "config",
        feature = "locks",
        feature = "channels",
        feature = "jobs",
        feature = "sql",
        feature = "records",
        feature = "metrics",
        feature = "search",
        feature = "wire"
    ))]
    fn the_table_every_sdk_reads_is_every_bound() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/wire/limits.json");
        let mut written = String::from("{\n");
        for (at, limit) in ALL.iter().enumerate() {
            let comma = if at + 1 < ALL.len() { "," } else { "" };
            written.push_str(&format!("  {:?}: {}{comma}\n", limit.name, limit.bound));
        }
        written.push_str("}\n");
        let kept = std::fs::read_to_string(path).unwrap_or_default().replace("\r\n", "\n");
        assert_eq!(kept, written, "testdata/wire/limits.json is every bound: write the right side to it");
        let names: std::collections::HashSet<_> = ALL.iter().map(|limit| limit.name).collect();
        assert_eq!(names.len(), ALL.len(), "each bound has a name of its own");
    }
}
