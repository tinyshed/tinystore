//! A judged node's side of its leases: the link to its witness, a renewal of
//! each file it writes every second and a stand for each file it copies, a
//! copy granted a lease taking the file over, and a home that lost one to
//! another node becoming its copy. A draft, as its names are.
//!
//! A home's log refuses its writes past what it keeps of its lease, by its
//! own clock and short of the witness's by a margin, whatever its link to the
//! witness does meanwhile: a link stuck on a dead peer delays no refusal.

use std::sync::Arc;
use std::time::{Duration, Instant};

use std::collections::{HashMap, VecDeque};

use super::entry::Point;
use super::log::Log;
use super::message::{Frames, LeaseAsk, LeaseHolder, Message, timed_out};
use super::node::State;
use super::serve::Out;
use super::witness::{Holder, TTL};
use crate::store::WeakStore;
use crate::{Error, ErrorKind, Result, Store};

/// How often a judged node renews and stands.
const RENEW: Duration = Duration::from_secs(1);
/// What a home keeps of its lease short of the witness's, for the round trip
/// and the clocks' drift.
pub(crate) const MARGIN: Duration = Duration::from_millis(1500);

/// Starts the thread that keeps the node's link to its witness.
pub(crate) fn start(state: &Arc<State>, store: &Store, witness: String) {
    let (state, store) = (Arc::clone(state), store.downgrade());
    state.clone().spawn("tinystore node: judge", move || {
        let mut said = None;
        while !state.stopping() {
            if let Err(error) = judged(&state, &store, &witness)
                && said.as_deref() != Some(error.to_string().as_str())
                && !state.stopping()
            {
                tracing::warn!(target: "tinystore", witness = %witness, %error, "a node's link to its witness");
                said = Some(error.to_string());
            }
            state.pause(RENEW);
        }
    });
}

/// Renews and stands over a link to the witness until it breaks.
fn judged(state: &Arc<State>, store: &WeakStore, witness: &str) -> Result<()> {
    let (mut reading, writing) = super::follow::dial(state, witness)?;
    reading.set_timeout(Duration::from_millis(100))?;
    writing.set_timeout(Duration::from_secs(5))?;
    let out = Out::new(writing);
    out.send(&state.hello(state.address()))?;
    let mut frames = Frames::default();
    let (mut welcomed, mut turn, mut asks) = (false, None::<Instant>, Asks::default());
    loop {
        if state.stopping() {
            return Ok(());
        }
        while let Some(message) = frames.next()? {
            match message {
                Message::Welcome { .. } if !welcomed => {
                    welcomed = true;
                    frames.unpack_from_here()?;
                    out.pack()?;
                }
                Message::Refused { why } => return Err(Error::new(ErrorKind::Invalid, format!("refused: {why}"))),
                Message::Leased { holders } => {
                    let Some(store) = store.upgrade() else {
                        return Ok(());
                    };
                    let Some(sent) = asks.unanswered.pop_front() else {
                        return Err(Error::invalid("a witness answered an ask this node did not make"));
                    };
                    asks.answered(state, &store, holders, sent);
                }
                _ => {}
            }
        }
        if welcomed && turn.is_none_or(|turn| turn.elapsed() >= RENEW) {
            let now = Instant::now();
            turn = Some(now);
            let asked = state.judged_files();
            tracing::debug!(target: "tinystore", asks = asked.len(), "a node asks its witness for its leases");
            if !asked.is_empty() {
                out.send(&Message::Lease { node: state.id.clone(), address: state.address(), asks: asked })?;
                asks.unanswered.push_back(now);
            }
        }
        match frames.fill(&mut reading) {
            Ok(true) => {}
            Ok(false) => return Err(Error::new(ErrorKind::Unavailable, "the witness closed the link")),
            Err(error) if timed_out(&error) => {}
            Err(error) => return Err(Error::io("a link to the witness", error)),
        }
    }
}

/// What a judged node knows of its asks of its witness.
#[derive(Default)]
struct Asks {
    /// When each ask the witness has not answered yet left, oldest first: one
    /// link carries them in order, and the witness answers each, in order.
    unanswered: VecDeque<Instant>,
    /// What failed of each file's lease, said once until it is acted on again.
    failing: HashMap<String, String>,
}

impl Asks {
    /// Acts on each holder the witness answered to the ask that left at `sent`.
    fn answered(&mut self, state: &Arc<State>, store: &Store, holders: Vec<LeaseHolder>, sent: Instant) {
        for held in holders {
            let file = held.file;
            match leased(state, store, &file, &held.holder, sent) {
                Err(error) if self.failing.get(&file) != Some(&error.to_string()) => {
                    tracing::warn!(target: "tinystore", %file, %error, "a node could not act on its lease");
                    self.failing.insert(file, error.to_string());
                }
                Err(_) => {}
                Ok(()) => {
                    if self.failing.remove(&file).is_some() {
                        tracing::info!(target: "tinystore", %file, "a node acts on its lease again");
                    }
                }
            }
        }
    }
}

/// Acts on the holder the witness answered for `file`, the ask having left at
/// `sent`: a home's lease kept on, a copy granted one taking the file over,
/// a home another node took it from letting go.
fn leased(state: &Arc<State>, store: &Store, file: &str, holder: &Holder, sent: Instant) -> Result<()> {
    let until = sent + TTL - MARGIN;
    if holder.node == state.id {
        // a lease granted now is of a later epoch than the copy's: one at its own
        // or older is what this node held before it became a copy
        if let Some(copy) = state.copy(file).filter(|copy| holder.epoch > copy.point().epoch) {
            state.granted(file, until);
            state.take_over_at(store, file, copy, holder.epoch)?;
            tracing::info!(
                target: "tinystore",
                file,
                epoch = holder.epoch,
                "a copy took a file over, its lease granted",
            );
            state.claim_taken(file, holder.epoch);
        }
        state.lease_until(file, until);
        return Ok(());
    }
    if holder.node.is_empty() || state.copy(file).is_some() || super::copy::is_copy(&state.log_dir(file)) {
        return Ok(());
    }
    // the epoch alone, with no writer, which a write waiting for this lease holds
    if state.epoch_held(file).is_none_or(|ours| holder.epoch <= ours) {
        return Ok(());
    }
    let dir = state.log_dir(file);
    if Log::fenced_in(&dir)?.is_none_or(|fenced| fenced < holder.epoch) {
        state.fence(file, holder.epoch)?;
    }
    // a file no engine opened since the node started has no log open: its
    // directory says where it ends
    let ours = match state.point_of(file) {
        Some(point) => point,
        None => Log::last_in(&dir)?
            .or(super::promote::promoted_at(&dir)?)
            .ok_or_else(|| Error::not_found(format!("{file}: its log")))?,
    };
    super::handover::let_go(state, store, file, &holder.address, ours)
}

impl State {
    /// What a judged node asks its witness each turn: a renewal of each file
    /// it writes, a stand for each copy it holds whole.
    pub(crate) fn judged_files(&self) -> Vec<LeaseAsk> {
        let Ok(logged) = self.files_logged_now() else {
            return Vec::new();
        };
        let mut asks = Vec::new();
        for file in logged {
            if super::NODE_LOCAL.contains(&file.as_str()) || file == super::catalog::FILE {
                continue;
            }
            let (holding, at) = match self.copy(&file) {
                Some(copy) => (false, copy.point()),
                None if super::copy::is_copy(&self.log_dir(&file)) => continue,
                // a renewal names its epoch alone, as a file born here does for its first
                None => match self.epoch_held(&file) {
                    Some(epoch) => (true, Point { epoch, ..Point::default() }),
                    None => continue,
                },
            };
            // only the copies it names hold every commit a synchronous home answered
            let synchronous = if holding { self.options.synchronous_with.clone() } else { Vec::new() };
            asks.push(LeaseAsk { file, epoch: at.epoch, position: at.sequence, holding, synchronous });
        }
        asks
    }
}
