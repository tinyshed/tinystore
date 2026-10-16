//! What searches of an index keep between them: the rows of lengths they
//! read and the documents gone. Each is read in a search's own snapshot and
//! kept with what makes it right for a later one, so that no search ever
//! reads a length or a gone document of another moment than its blocks'.
//!
//! - A document's lengths are written once, when packing first reaches it,
//!   and never change: a row of lengths read where `normed` was `n` holds
//!   every document below `n` that it ever will, and serves any later
//!   search whose documents in that row are all below `n`.
//! - The documents gone change with sets and purges, and the index counts
//!   every change: a set read at one count serves a search at that count.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, OptionalExtension};

use super::correct::Corrector;
use super::score::Part;
use crate::sqlite::sql_error;
use crate::{Memory, Reservation, Result};

/// Documents a row of lengths holds.
pub(crate) const NORMS: u32 = 65_536;

const READ_NORMS: &str = "select lengths from _tinystore_search_norms where index_id = ?1 and part = ?2 and chunk = ?3";
const READ_GONE: &str = "select doc from _tinystore_search_gone where index_id = ?1";

#[derive(Default)]
pub(crate) struct Kept {
    norms: Mutex<HashMap<(Part, u32), Arc<Chunk>>>,
    gone: Mutex<Option<Arc<Gone>>>,
    pub(super) corrector: Mutex<Option<Arc<Corrector>>>,
}

impl std::fmt::Debug for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kept").field("norms", &lock(&self.norms).len()).finish_non_exhaustive()
    }
}

/// A row of a part's lengths, a document's at its place in the row.
pub(crate) struct Chunk {
    /// The `normed` of the snapshot it was read in.
    normed: u32,
    lengths: Vec<u16>,
    _reserved: Option<Reservation>,
}

impl Chunk {
    pub(crate) fn length(&self, doc: u32) -> u32 {
        u32::from(self.lengths.get((doc % NORMS) as usize).copied().unwrap_or(0))
    }
}

/// The documents a set replaced or a delete removed, not yet purged.
pub(crate) struct Gone {
    version: i64,
    pub(crate) docs: HashSet<u32>,
}

/// What one snapshot of an index says about its kept parts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Moment {
    pub(crate) index: i64,
    pub(crate) normed: u32,
    pub(crate) gone_version: i64,
}

impl Kept {
    /// The documents gone at `moment`, read again if it is not the one kept.
    pub(crate) fn gone(&self, connection: &Connection, moment: Moment) -> Result<Arc<Gone>> {
        if let Some(gone) = lock(&self.gone).as_ref().filter(|gone| gone.version == moment.gone_version) {
            return Ok(Arc::clone(gone));
        }
        let failed = |error| sql_error("read the documents gone", error);
        let mut select = connection.prepare_cached(READ_GONE).map_err(failed)?;
        let docs = select.query_map([moment.index], |row| row.get::<_, u32>(0)).and_then(Iterator::collect);
        let gone = Arc::new(Gone { version: moment.gone_version, docs: docs.map_err(failed)? });
        *lock(&self.gone) = Some(Arc::clone(&gone));
        Ok(gone)
    }

    /// The row of `part`'s lengths that holds `chunk`, as `moment` has it.
    pub(crate) fn chunk(
        &self,
        connection: &Connection,
        memory: &Arc<Memory>,
        moment: Moment,
        key: (Part, u32),
    ) -> Result<Arc<Chunk>> {
        let (part, chunk) = key;
        let needed = moment.normed.min(chunk.saturating_add(1).saturating_mul(NORMS));
        if let Some(kept) = lock(&self.norms).get(&key).filter(|kept| kept.normed >= needed) {
            return Ok(Arc::clone(kept));
        }
        let mut select = connection.prepare_cached(READ_NORMS).map_err(|error| sql_error("read lengths", error))?;
        let bytes: Option<Vec<u8>> = select
            .query_row((moment.index, part.0, chunk), |row| row.get(0))
            .optional()
            .map_err(|error| sql_error("read lengths", error))?;
        let lengths = lengths_of(&bytes.unwrap_or_default());
        let reserved = keep(memory, lengths.len() as u64 * 2);
        let keep = reserved.is_some();
        let read = Arc::new(Chunk { normed: moment.normed, lengths, _reserved: reserved });
        let mut norms = lock(&self.norms);
        if keep && norms.get(&key).is_none_or(|kept| kept.normed < read.normed) {
            norms.insert(key, Arc::clone(&read));
        }
        Ok(read)
    }
}

/// The store's memory for `bytes` of lengths to keep, while what is kept
/// leaves half a bounded store's memory to the work that gives it back.
fn keep(memory: &Arc<Memory>, bytes: u64) -> Option<Reservation> {
    if memory.bounded() && memory.used().saturating_add(bytes) > memory.limit() / 2 {
        return None;
    }
    memory.try_reserve(bytes, "search: lengths kept").ok()
}

/// A row of lengths as `u16`s, little-endian.
pub(crate) fn lengths_of(bytes: &[u8]) -> Vec<u16> {
    bytes.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect()
}

pub(crate) fn bytes_of(lengths: &[u16]) -> Vec<u8> {
    lengths.iter().flat_map(|length| length.to_le_bytes()).collect()
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
