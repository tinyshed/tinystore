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
//! goes on from it. A write in between is refused, `Conflict`. A move needs a
//! catalog, through which the old home learns that the new one took over.
//!
//! The catalog keeps the first claim made at an epoch, and that claim is the
//! move's point of no return. Until the new home's claim is there, the move
//! can be called off by a claim of the old home's own at the same epoch: the
//! new home's comes too late, and the old home writes again at that epoch.
//! The new home calls it off when its copy cannot reach the last commit, and
//! the old home when a minute passes without the new home's claim. An old
//! home that cannot reach the catalog to call it off stays fenced: it cannot
//! tell a new home that took the file from one that never will.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::catalog::{Claim, Home};
use super::copy::{self, Copy, Position};
use super::entry::Point;
use super::forward;
use super::log::Log;
use super::message::{In, Out};
use super::node::{State, lock};
use crate::{Error, ErrorKind, Result, Store};

/// How long the old home waits for the catalog to name the new one before it
/// calls the move off.
const TAKEN_WITHIN: Duration = if cfg!(test) { Duration::from_secs(2) } else { Duration::from_secs(60) };
/// How often an old home that could not reach the catalog asks it again.
const ASK_AGAIN: Duration = Duration::from_secs(5);

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
/// writes and this node holds a whole copy of. A move the catalog already
/// names this node the home of, past its point of no return, is finished.
/// A draft, as its name is.
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
    let catalog = node.catalog(store).map_err(|error| error.within(what()))?;
    let old =
        catalog.home_of(file)?.ok_or_else(|| Error::not_found("the catalog names no home of it").within(what()))?;
    if old.node == node.id {
        return finish(store, node, file, copy, old.epoch).map_err(|error| error.within(what()));
    }
    let last = match hand_over(store, node, file, &copy) {
        Ok(last) => last,
        Err((error, fenced)) => {
            if let Some(last) = fenced {
                call_off_for(store, node, &old, file, last);
            }
            return Err(error.within(what()));
        }
    };
    let epoch = last.epoch.max(old.epoch) + 1;
    let claim = Claim { file, node: &node.id, address: &address, epoch };
    let unknown = "its claim's outcome is unknown: call move_home again, which finishes a move the catalog names";
    let home = catalog.claim(store, &claim).map_err(|error| error.within(unknown).within(what()))?;
    if home.node != node.id {
        let why = "the old home called the move off before this node's claim reached the catalog";
        return Err(Error::new(ErrorKind::Conflict, why).within(what()));
    }
    finish(store, node, file, copy, epoch).map_err(|error| error.within(what()))
}

/// Takes the file over at `epoch`, the catalog naming this node its home
/// there: past the point of no return, so that a failure says to call again.
fn finish(store: &Store, node: &Arc<State>, file: &str, copy: Arc<Copy>, epoch: u64) -> Result<Moved> {
    let sequence = copy.point().sequence;
    let again = "the catalog names this node its home: call move_home again to finish";
    node.take_over_at(store, file, copy, epoch).map_err(|error| error.within(again))?;
    tracing::info!(target: "tinystore", file, sequence, epoch, "a file's home moved to this node");
    Ok(Moved { file: file.to_owned(), sequence, epoch })
}

/// Calls off the move of `file` from `old`, which fenced it at `last`, by a
/// claim of `old`'s own at the next epoch; a claim that does not reach the
/// catalog leaves it to the old home, once its wait ends.
fn call_off_for(store: &Store, node: &State, old: &Home, file: &str, last: Point) {
    let epoch = last.epoch.max(old.epoch) + 1;
    let claim = Claim { file, node: &old.node, address: &old.address, epoch };
    if let Err(error) = node.catalog(store).and_then(|catalog| catalog.claim(store, &claim)) {
        tracing::warn!(target: "tinystore", file, %error, "a node could not call a move off: its old home will");
    }
}

/// Asks the file's home to stop taking writes, and gives its last commit
/// once this node's copy holds it; a failure says the last commit when the
/// home fenced the file.
pub(super) fn hand_over(
    store: &Store,
    node: &State,
    file: &str,
    copy: &Copy,
) -> std::result::Result<Point, (Error, Option<Point>)> {
    let mut op = Out(vec![forward::HANDOVER]);
    op.text(file);
    op.text(&node.id);
    let (answer, held) = forward::pass_on_held(store, file, op.0).map_err(|error| (error, None))?;
    let last = In { bytes: &answer, at: 0 }.point().map_err(|error| (error, None))?;
    if !held || copy.point() != last {
        let why = format!("this node's copy does not hold the home's last commit, {}: the move stops", last.sequence);
        return Err((Error::new(ErrorKind::Unavailable, why), Some(last)));
    }
    Ok(last)
}

/// Gives the file a node asked for over, at its home, or passes the ask on
/// toward it: the answer's bytes, the home's last commit, and the file.
pub(crate) fn run(store: &Store, op: &[u8]) -> Result<(Vec<u8>, String)> {
    let mut at = In { bytes: &op[1..], at: 0 };
    let (file, to) = (at.text()?, at.text()?);
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
    state.spawn("tinystore node: handover", move || let_go_once_taken(&watching, &watched, &to, last));
    let mut answer = Out(Vec::new());
    answer.point(&last);
    Ok((answer.0, file))
}

/// Waits for the catalog to name a home of the file past `last`: the new
/// home, of which the file becomes a copy, or this node, the move called
/// off, which writes it again. With neither within a minute it calls the move
/// off itself, and stays fenced while the catalog cannot be reached.
fn let_go_once_taken(state: &Arc<State>, file: &str, to: &str, last: Point) {
    let mut deadline = Instant::now() + TAKEN_WITHIN;
    let mut said = false;
    while !state.stopping() {
        let Some(store) = state.store() else {
            return;
        };
        let mut home = state.catalog(&store).and_then(|catalog| catalog.home_of(file)).ok().flatten();
        if Instant::now() >= deadline && home.as_ref().is_none_or(|home| home.epoch <= last.epoch) {
            deadline = Instant::now() + ASK_AGAIN;
            let claim = Claim { file, node: &state.id, address: &state.address(), epoch: last.epoch + 1 };
            match state.catalog(&store).and_then(|catalog| catalog.claim(&store, &claim)) {
                Ok(claimed) => home = Some(claimed),
                Err(error) if !said => {
                    said = true;
                    tracing::warn!(target: "tinystore", file, %error, "a move went silent and the catalog cannot be reached: this node stays fenced");
                }
                Err(_) => {}
            }
        }
        if let Some(home) = home.filter(|home| home.epoch > last.epoch && (home.node == to || home.node == state.id)) {
            act_on_home(state, &store, file, &home, last);
            return;
        }
        drop(store);
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Acts on the home the catalog names of a file this node handed over at
/// `last`: a copy of the new home, or this node writing again at its epoch.
fn act_on_home(state: &Arc<State>, store: &Store, file: &str, home: &Home, last: Point) {
    if home.node != state.id {
        if let Err(error) = let_go(state, store, file, &home.address, last) {
            tracing::error!(target: "tinystore", file, %error, "a node could not make a file it handed over a copy");
        }
        return;
    }
    match state.go_on_at(file, home.epoch) {
        Ok(()) => {
            tracing::warn!(target: "tinystore", file, epoch = home.epoch, "a move was called off: this node writes the file again")
        }
        Err(error) => tracing::error!(target: "tinystore", file, %error, "a node could not write a file again"),
    }
}

/// Makes the file this node handed over a copy of its new home at `address`,
/// going on from `last`, the commit it ended at.
pub(crate) fn let_go(state: &Arc<State>, store: &Store, file: &str, address: &str, last: Point) -> Result<()> {
    let dir = state.log_dir(file);
    let paused = state.pause_readers(file)?;
    state.switch_each(file, |switch| switch.become_copy())?;
    Log::unfence_in(&dir)?;
    super::promote::forget_promotion(&dir)?;
    copy::mark(&dir)?;
    // the file's WAL holds the commits it made as the home, all named by `last`
    let wal = crate::sqlite::wal_end(&store.dir().join(file))?;
    Position::read(dir.join(Position::FILE))?.keep_durably_at(last, wal)?;
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
