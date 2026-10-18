//! A copy checked against the node it takes it from, page for page, when a
//! program asks: the chain of entries proves each commit came whole and in
//! order, and this proves what applying them made.
//!
//! ```text
//! the copy                               the node it takes from
//!   ──verify kv.db───────────────────────▶ holds kv.db still while a read begins,
//!                                          reads every page, hashes them
//!   ◀──entry 412, its SHA-256─────────────
//!   at entry 412, the copy hashes its own and compares
//! ```
//!
//! The node it takes from answers the point it holds now, which the copy
//! reaches, never passes, before it hashes: its entries come from that node.

use sha2::{Digest, Sha256};

use super::entry::Point;
use super::node::State;
use crate::sqlite::{Pages, Snapshot};
use crate::{Error, ErrorKind, Result, Store};

/// How long `verify` waits for the answer and the copy's own hash.
const VERIFIED_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// Checks this node's copy of `file` against the node it takes it from, page
/// for page, at the point that node holds now: the entry checked, or
/// `Corrupt` when the pages differ.
pub fn verify(store: &Store, file: &str) -> Result<u64> {
    let what = || format!("verify {file}");
    let node = store.node().ok_or_else(|| Error::invalid("a store that is no node").within(what()))?;
    node.ask_to_verify(file);
    let deadline = std::time::Instant::now() + VERIFIED_WITHIN;
    loop {
        if let Some(done) = node.verified(file) {
            return done.map_err(|error| error.within(what()));
        }
        if std::time::Instant::now() >= deadline || node.stopping() {
            node.forget_verifying(file);
            return Err(Error::new(ErrorKind::Unavailable, "no answer came from the node it takes the file from")
                .within(what()));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Where this node holds `file` and the SHA-256 of its pages there: the file
/// held still, by its home's writer or its copy's position, while the read
/// begins, and read after it let go.
pub(crate) fn here(state: &State, file: &str) -> Result<(Point, [u8; 32])> {
    let pages = Pages::open(&state.dir.join(file))?;
    let (point, snapshot) = match state.copy(file) {
        Some(copy) => copy.holding(|point| Ok((point, pages.begin()?)))?,
        None => state.holding_home(file, |point| Ok((point, pages.begin()?)))?,
    };
    Ok((point, checksum(&snapshot)?))
}

/// The SHA-256 of the copy of `file` as it stands, for the link that applies
/// it, which holds it still meanwhile.
pub(crate) fn of_copy(state: &State, file: &str) -> Result<[u8; 32]> {
    let pages = Pages::open(&state.dir.join(file))?;
    checksum(&pages.begin()?)
}

/// The SHA-256 of a snapshot's pages, page 1's change counters taken as
/// zeros: a copy's own commits count them.
fn checksum(snapshot: &Snapshot<'_>) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    snapshot.each(|number, page| {
        if number == 1 && page.len() >= 96 {
            let mut first = page.to_vec();
            first[24..28].fill(0);
            first[92..96].fill(0);
            hash.update(&first);
        } else {
            hash.update(page);
        }
    })?;
    Ok(hash.finalize().into())
}

/// Compares the copy's hash with the one its source answered at entry
/// `sequence`.
pub(crate) fn compare(file: &str, sequence: u64, ours: [u8; 32], theirs: &[u8]) -> Result<u64> {
    if ours.as_slice() == theirs {
        return Ok(sequence);
    }
    tracing::error!(
        target: "tinystore",
        file,
        entry = sequence,
        "a copy's pages differ from the node it takes them from",
    );
    Err(Error::new(
        ErrorKind::Corrupt,
        format!("its pages differ from those of the node it takes it from, at entry {sequence}"),
    ))
}
