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

use super::entry::Point;
use super::message::{Frames, LeaseAsk, Message, VERSION, timed_out};
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
            let until = Instant::now() + RENEW;
            while Instant::now() < until && !state.stopping() {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    });
}

/// Renews and stands over a link to the witness until it breaks.
fn judged(state: &Arc<State>, store: &WeakStore, witness: &str) -> Result<()> {
    let stream = super::follow::connect(witness)?;
    state.keep_link(&stream);
    let (mut reading, writing) = super::transport::connect(state, witness, stream)?;
    reading.set_timeout(Duration::from_millis(100))?;
    writing.set_timeout(Duration::from_secs(5))?;
    let out = Out::new(writing);
    out.send(&Message::Hello {
        version: VERSION,
        token: state.options.token.clone().unwrap_or_default(),
        node: state.id.clone(),
        address: state.address(),
    })?;
    let mut frames = Frames::default();
    let mut welcomed = false;
    let mut turn = Instant::now();
    let mut sent = None;
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
                    for held in holders {
                        let file = held.file;
                        if let Err(error) =
                            leased(state, &store, &file, &held.holder, sent.unwrap_or_else(Instant::now))
                        {
                            tracing::warn!(target: "tinystore", %file, %error, "a node could not act on its lease");
                        }
                    }
                }
                _ => {}
            }
        }
        if welcomed && turn.elapsed() >= RENEW || welcomed && sent.is_none() {
            turn = Instant::now();
            sent = Some(turn);
            let asks = state.judged_files();
            if !asks.is_empty() {
                out.send(&Message::Lease { node: state.id.clone(), address: state.address(), asks })?;
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

/// Acts on the holder the witness answered for `file`, the ask having left at
/// `sent`: a home's lease kept on, a copy granted one taking the file over,
/// a home another node took it from letting go.
fn leased(state: &Arc<State>, store: &Store, file: &str, holder: &Holder, sent: Instant) -> Result<()> {
    let until = sent + TTL - MARGIN;
    if holder.node == state.id {
        if let Some(copy) = state.copy(file) {
            state.granted(file, until);
            state.take_over_at(store, file, copy, holder.epoch)?;
            tracing::info!(target: "tinystore", file, epoch = holder.epoch, "a copy took a file over, its lease granted");
        }
        state.lease_until(file, until);
        return Ok(());
    }
    if holder.node.is_empty() || state.copy(file).is_some() || super::copy::is_copy(&state.log_dir(file)) {
        return Ok(());
    }
    let ours = state.point_of(file).ok_or_else(|| Error::not_found(format!("{file}: its log")))?;
    if holder.epoch > ours.epoch {
        state.fence(file, holder.epoch)?;
        super::handover::let_go(state, store, file, &holder.address, ours)?;
    }
    Ok(())
}

/// What a judged node asks its witness each turn: a renewal of each file it
/// writes, a stand for each copy it holds whole.
impl State {
    pub(crate) fn judged_files(&self) -> Vec<LeaseAsk> {
        let Ok(logged) = self.files_logged_now() else {
            return Vec::new();
        };
        let mut asks = Vec::new();
        for file in logged {
            if super::NODE_LOCAL.contains(&file.as_str()) {
                continue;
            }
            let (holding, at) = match self.copy(&file) {
                Some(copy) => (false, copy.point()),
                None if super::copy::is_copy(&self.log_dir(&file)) => continue,
                None => match (self.point_of(&file), self.epoch_written(&file)) {
                    (Some(at), _) => (true, at),
                    // a file born here, its log empty, asks for its first lease
                    (None, Some(epoch)) => (true, Point { epoch, ..Point::default() }),
                    (None, None) => continue,
                },
            };
            // only the copies it names hold every commit a synchronous home answered
            let synchronous = if holding { self.options.synchronous_with.clone() } else { Vec::new() };
            asks.push(LeaseAsk { file, epoch: at.epoch, position: at.sequence, holding, synchronous });
        }
        asks
    }
}
