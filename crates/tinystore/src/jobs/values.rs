//! A job's value: JSON, so that a worker in another language reads what Rust
//! wrote, kept in its row or, past 512 bytes, in a row of its own.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

/// The most a value may be; larger bytes are the blobs engine's, with their
/// key in the value.
pub(crate) const MAX_VALUE: usize = 1 << 20;

/// A value past this lives in a row of its own, where a claim of the jobs
/// around it need not read it.
const INLINE_VALUE: usize = 512;

/// A value as a job's row holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    Inline(Vec<u8>),
    Spilled(Vec<u8>),
}

impl Kept {
    pub(crate) fn of(encoded: Vec<u8>) -> Kept {
        if encoded.len() > INLINE_VALUE { Kept::Spilled(encoded) } else { Kept::Inline(encoded) }
    }

    pub(crate) fn inline(&self) -> Option<&[u8]> {
        match self {
            Kept::Inline(bytes) => Some(bytes),
            Kept::Spilled(_) => None,
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Kept::Inline(bytes) | Kept::Spilled(bytes) => bytes.len(),
        }
    }
}

/// Writes a value as JSON; one JSON cannot write is `invalid`, one past
/// `MAX_VALUE` a `limit`.
pub(crate) fn encode<V: Serialize>(value: &V) -> Result<Vec<u8>> {
    let encoded =
        serde_json::to_vec(value).map_err(|error| Error::invalid("a value JSON cannot write").with_source(error))?;
    check_size(encoded.len(), "a value")?;
    Ok(encoded)
}

/// Reads a value back. One that no longer reads as the queue's type, because
/// the type changed, fails its job for good rather than stopping the queue.
pub(crate) fn decode<V: DeserializeOwned>(encoded: &[u8]) -> Result<V> {
    serde_json::from_slice(encoded).map_err(|error| {
        Error::invalid(format!("a value that no longer reads as {}", std::any::type_name::<V>())).with_source(error)
    })
}

pub(crate) fn check_size(bytes: usize, what: &str) -> Result<()> {
    if bytes > MAX_VALUE {
        return Err(Error::limit(format!("{what} of {bytes} bytes, past {MAX_VALUE}")));
    }
    Ok(())
}
