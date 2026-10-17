//! A file's home moved while both nodes run, by a person, through the
//! catalog: the old home stops taking writes and its last commit reaches the
//! new home's copy, the catalog's route moves at the next epoch, the new home
//! opens its writer from there, and the old home becomes its copy.
//!
//! ```text
//! the new home                         the old home
//!   ──hand sql/users.db over──────────▶ refuses its writes, kept in `fenced`
//!   ◀──its last commit, 412──────────── and waits for the catalog's route
//!   waits until its copy holds 412
//!   claims the route at epoch 3 ────────▶ the catalog's home
//!   opens its writer at 413, epoch 3
//!                                       sees the route move, closes its writer,
//!                                       and copies the file from 412 on
//! ```
//!
//! No commit is lost: the old home took none after its last, and the new home
//! goes on from it. A write in between is refused, `Conflict`. A move that
//! does not finish within a minute leaves the old home writing again; a move
//! needs a catalog, through which the old home learns that the new one took
//! over.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::catalog::Claim;
use super::copy::{self, Copy, Position};
use super::entry::Point;
use super::forward;
use super::log::Log;
use super::message::{In, Out};
use super::node::{State, lock};
use crate::{Error, ErrorKind, Result, Store};

/// How long the old home waits for the catalog to name the new one before it
/// takes writes again.
const TAKEN_WITHIN: Duration = Duration::from_secs(60);

/// A file a live move made this node's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    /// The file, as `kv.db` or `sql/app.db`.
    pub file: String,
    /// The old home's last commit, which this node's log goes on after.
    pub sequence: u64,
    /// The epoch its commits take from now on.
    pub epoch: u64,
}

/// Makes this running node the home of `file`, which another running node
/// writes and this node holds a whole copy of. A draft, as its name is.
pub fn move_home(store: &Store, file: &str) -> Result<Moved> {
    let what = || format!("move {file} to this node");
    let node = store.node().ok_or_else(|| Error::invalid("a store that is no node").within(what()))?;
    let copy = node
        .copy(file)
        .ok_or_else(|| Error::new(ErrorKind::Unavailable, "this node holds no whole copy of it").within(what()))?;
    let address = node.address();
    if address.is_empty() || !node.uses_catalog() {
        let why = "a live move needs a catalog, and a node that serves its address";
        return Err(Error::invalid(why).within(what()));
    }
    let last = hand_over(store, node, file, &address, &copy).map_err(|error| error.within(what()))?;
    let epoch = last.epoch + 1;
    let claim = Claim { file, node: &node.id, address: &address, epoch };
    let home = node.catalog(store)?.claim(store, &claim).map_err(|error| error.within(what()))?;
    if home.node != node.id {
        return Err(Error::new(ErrorKind::Conflict, "the catalog keeps another home of it").within(what()));
    }
    node.take_over(store, file, copy).map_err(|error| error.within(what()))?;
    tracing::info!(target: "tinystore", file, sequence = last.sequence, epoch, "a file's home moved to this node");
    Ok(Moved { file: file.to_owned(), sequence: last.sequence, epoch })
}

/// Asks the file's home to stop taking writes, and gives its last commit
/// once this node's copy holds it.
fn hand_over(store: &Store, node: &State, file: &str, address: &str, copy: &Copy) -> Result<Point> {
    let mut op = Out(vec![forward::HANDOVER]);
    op.text(file);
    op.text(&node.id);
    op.text(address);
    let (answer, held) = forward::pass_on_held(store, file, op.0)?;
    let last = In { bytes: &answer, at: 0 }.point()?;
    if !held || copy.point() != last {
        return Err(Error::new(
            ErrorKind::Unavailable,
            format!("this node's copy does not hold the home's last commit, {}: the move stops", last.sequence),
        ));
    }
    Ok(last)
}

/// Gives the file a node asked for over, at its home, or passes the ask on
/// toward it: the answer's bytes, the home's last commit, and the file.
pub(crate) fn run(store: &Store, op: &[u8]) -> Result<(Vec<u8>, String)> {
    let mut at = In { bytes: &op[1..], at: 0 };
    let (file, to, address) = (at.text()?, at.text()?, at.text()?);
    let state = store.node().ok_or_else(|| Error::invalid("a move asked of a store that is no node"))?;
    if copy::is_copy(&state.log_dir(&file)) {
        return Ok((forward::pass_on(store, &file, op.to_vec())?, file));
    }
    let held =
        state.point_of(&file).ok_or_else(|| Error::not_found(format!("{file}: this node writes no such file")))?;
    state.fence(&file, held.epoch + 1)?;
    // the writer is held while the point is read: a commit under way at the
    // fence is in it, and none follows
    let last = state.point_of(&file).ok_or_else(|| Error::not_found(format!("{file}: its log")))?;
    let watching = Arc::clone(state);
    let watched = file.clone();
    state.spawn("tinystore node: handover", move || let_go_once_taken(&watching, &watched, &to, &address, last));
    let mut answer = Out(Vec::new());
    answer.point(&last);
    Ok((answer.0, file))
}

/// Waits for the catalog to name the new home at the next epoch, then makes
/// the file a copy of it; takes writes again when it does not within a minute.
fn let_go_once_taken(state: &Arc<State>, file: &str, to: &str, address: &str, last: Point) {
    let deadline = Instant::now() + TAKEN_WITHIN;
    while !state.stopping() {
        let Some(store) = state.store() else {
            return;
        };
        let home = state.catalog(&store).and_then(|catalog| catalog.home_of(file)).ok().flatten();
        if home.is_some_and(|home| home.node == to && home.epoch > last.epoch) {
            if let Err(error) = let_go(state, &store, file, address, last) {
                tracing::error!(target: "tinystore", file, %error, "a node could not make a file it handed over a copy");
            }
            return;
        }
        if Instant::now() >= deadline {
            tracing::warn!(target: "tinystore", file, "a move did not finish within a minute: this node writes the file again");
            if let Err(error) = state.unfence(file) {
                tracing::error!(target: "tinystore", file, %error, "a node could not take writes of a file again");
            }
            return;
        }
        drop(store);
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Makes the file this node handed over a copy of its new home at `address`,
/// going on from `last`, the commit it ended at.
pub(crate) fn let_go(state: &Arc<State>, store: &Store, file: &str, address: &str, last: Point) -> Result<()> {
    let dir = state.log_dir(file);
    let paused = state.pause_readers(file)?;
    state.switch_each(file, |switch| switch.become_copy())?;
    Log::unfence_in(&dir)?;
    copy::mark(&dir)?;
    Position::read(dir.join(Position::FILE))?.keep_durably(last)?;
    state.keep_copy(file, Arc::new(Copy::open(store, file)?));
    drop(paused);
    store.applied(file);
    let from = lock(&state.upstream_at).clone();
    if from.as_deref() != Some(address) {
        state.take_directly(store, file, address)?;
    }
    state.ask_for_what_is_served();
    tracing::info!(target: "tinystore", file, to = address, "a node handed a file's home over, and copies it");
    Ok(())
}
