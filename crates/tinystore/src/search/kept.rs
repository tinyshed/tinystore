//! What searches of an index keep between them: the rows of lengths they
//! read and the documents gone. Each is read in a search's own snapshot and
//! kept with what makes it right for a later one, so that no search ever
//! reads a length or a gone document of another moment than its blocks'.
//!
//! - A document's lengths are written once, when a merge first reaches it,
//!   and never change: a row of lengths read where `normed` was `n` holds
//!   every document below `n` that it ever will, and serves any later
//!   search whose documents in that row are all below `n`.
//! - The documents gone change with sets and purges, and the index counts
//!   every change: a set read at one count serves a search at that count.
//! - A long piece changes only when a merge, a purge or upkeep writes it,
//!   and the index counts each such commit: a piece read at one count
//!   serves a search at that count. A set writes short pieces, which
//!   searches read anew. search-fast measured keeping directories so at 10 %
//!   of the median.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension};

use super::correct::Corrector;
use super::legacy::{self, Legacy};
use super::packed::Directory;
use super::postings::Posting;
use super::score::Part;
use super::segments::{self, Piece, Segment};
use crate::sqlite::sql_error;
use crate::sync::lock;
use crate::{Error, Memory, Reservation, Result};

/// Documents a row of lengths holds.
pub(crate) const NORMS: u32 = 65_536;

pub(super) const READ_NORMS: &str =
    "select lengths from _tinystore_search_norms where index_id = ?1 and part = ?2 and chunk = ?3";
const READ_GONE: &str = "select doc from _tinystore_search_gone where index_id = ?1";

#[derive(Default)]
pub(crate) struct Kept {
    norms: Mutex<HashMap<(Part, u32), Arc<Chunk>>>,
    gone: Mutex<Option<Arc<Gone>>>,
    lists: Mutex<Lists>,
    pub(super) corrector: Mutex<Option<Arc<Corrector>>>,
    pub(super) clusters: Mutex<Option<Arc<super::clusters::Clusters>>>,
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
    pub(crate) lists_version: i64,
}

/// A key's postings in a segment, or in a list a release before 0002
/// wrote, as searches read them: a directory decoded, or postings in hand.
pub(crate) enum Listed {
    /// Its directory, and its best postings.
    Packed(Arc<Directory>, Vec<Posting>),
    Short(Vec<Posting>),
    Legacy(Legacy),
    Nothing,
}

impl Listed {
    fn of(piece: Piece) -> Listed {
        match piece {
            Piece::Short(postings) => Listed::Short(postings),
            Piece::Packed(directory, best) => Listed::Packed(Arc::new(directory), best),
        }
    }

    /// The bytes it holds, about.
    fn bytes(&self) -> u64 {
        let held = match self {
            Listed::Packed(directory, best) => {
                directory.blocks.len() * 24 + directory.runs.len() * 16 + best.len() * 12
            }
            Listed::Legacy(legacy) => legacy.len() * 16,
            Listed::Short(postings) => postings.len() * 12,
            Listed::Nothing => 0,
        };
        held as u64 + 64
    }

    /// Whether keeping it saves a search more than a lookup: a short piece
    /// reads as fast as one would.
    fn decoded(&self) -> bool {
        matches!(self, Listed::Packed(..) | Listed::Legacy(_))
    }
}

/// The pieces kept, all of one `lists_version`, by their segment and key;
/// a list a release before 0002 wrote by segment 0.
#[derive(Default)]
struct Lists {
    version: i64,
    pieces: HashMap<(i64, i64), KeptPiece>,
}

/// A piece or a list as searches keep it between them, with its count.
pub(crate) type KeptPiece = Arc<Held<(i64, Listed)>>;

/// What a cache keeps, with the store's memory it holds.
pub(crate) struct Held<T> {
    pub(crate) value: T,
    _reserved: Option<Reservation>,
}

impl Kept {
    /// What a search on the writer of a transaction starts from, as its own:
    /// the lengths and the clusters, which only maintenance's commits change,
    /// and the dictionary, built only from snapshots of what committed. The
    /// documents gone stay out, since the transaction's writes change them,
    /// and what the search reads it keeps to itself: the transaction may
    /// roll back, and a version it counted come again with other documents.
    #[cfg(feature = "sql")]
    pub(crate) fn committed(&self) -> Kept {
        Kept {
            norms: Mutex::new(lock(&self.norms).clone()),
            gone: Mutex::default(),
            lists: Mutex::default(),
            corrector: Mutex::new(lock(&self.corrector).clone()),
            clusters: Mutex::new(lock(&self.clusters).clone()),
        }
    }

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

    /// A key's piece in `segment` as `moment` has it, with its count, read
    /// again unless one of its `lists_version` is kept; none when the
    /// segment holds none of the key's postings.
    pub(crate) fn piece(
        &self,
        connection: &Connection,
        (memory, moment): (&Arc<Memory>, Moment),
        segment: &Segment,
        key: i64,
    ) -> Result<Option<KeptPiece>> {
        if let Some(kept) = self.kept((segment.id, key), moment) {
            return Ok(Some(kept));
        }
        let Some((count, piece)) = segments::piece(connection, segment, key)? else { return Ok(None) };
        Ok(Some(self.keep((segment.id, key), (memory, moment), (count, Listed::of(piece)))))
    }

    /// A key's list as a release before 0002 wrote it, as `moment` has it.
    pub(crate) fn legacy(
        &self,
        connection: &Connection,
        (memory, moment): (&Arc<Memory>, Moment),
        key: i64,
    ) -> Result<KeptPiece> {
        if let Some(kept) = self.kept((0, key), moment) {
            return Ok(kept);
        }
        let listed = legacy::list(connection, key)?;
        Ok(self.keep((0, key), (memory, moment), (0, listed)))
    }

    fn kept(&self, at: (i64, i64), moment: Moment) -> Option<KeptPiece> {
        let lists = lock(&self.lists);
        if lists.version != moment.lists_version {
            return None;
        }
        lists.pieces.get(&at).map(Arc::clone)
    }

    /// `read`, kept for later searches when it is worth keeping and the
    /// store's memory allows.
    fn keep(&self, at: (i64, i64), (memory, moment): (&Arc<Memory>, Moment), read: (i64, Listed)) -> KeptPiece {
        let reserved = if read.1.decoded() { keep(memory, read.1.bytes()) } else { None };
        let keep = reserved.is_some();
        let read = Arc::new(Held { value: read, _reserved: reserved });
        let mut lists = lock(&self.lists);
        if lists.version < moment.lists_version {
            *lists = Lists { version: moment.lists_version, pieces: HashMap::new() };
        }
        if keep && lists.version == moment.lists_version {
            lists.pieces.insert(at, Arc::clone(&read));
        }
        read
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
        // Lengths are written before a posting reaches a block: none is damage.
        let bytes = bytes.ok_or_else(|| Error::corrupt(format!("lengths of part {} chunk {chunk}: none", part.0)))?;
        let lengths = lengths_of(&bytes);
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
