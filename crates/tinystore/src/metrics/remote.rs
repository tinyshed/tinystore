//! A client's instruments: what an SDK's counters, gauges, timers and
//! histograms changed, sent over the wire in batches its session numbers and
//! added to the totals the store keeps of its own instruments, which it
//! writes every 15 seconds.
//!
//! ```text
//! a stream opens   session S, after N   the last batch of S applied, or that it knows no S
//! batch N + 1                           applied once, the one after the last, and answered
//! batch N + 1 again                     answered, changing nothing
//! batch N + 3                           invalid: the client sends again from the answer
//! ```
//!
//! A session lives in memory: a store that started again knows none, and the
//! client drops what it had sent and had no answer to, which the store may
//! have written before it stopped, so that nothing counts twice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::Metrics;
use super::buckets::{Bounds, Buckets};
use super::engine::Engine;
use super::grid::{Bin, FINEST};
use super::series::Labels;
use super::totals::{Connection, Form, Instrument, Measures, Slot};
use crate::sync::lock;
use crate::{Error, Result};

/// How long the store remembers a session after its last stream ended.
const FORGET: i64 = 3_600_000;

/// The sessions of clients' instruments, by their id.
#[derive(Default)]
pub(super) struct Sessions {
    known: Mutex<HashMap<[u8; 16], Arc<Mutex<Session>>>>,
}

/// What the store keeps of a session: the last batch it applied, and how
/// many of its streams are open.
#[derive(Debug, Default)]
struct Session {
    applied: u64,
    streams: usize,
    /// When its last stream ended.
    left_at: i64,
}

/// An instrument a client declares, by its stream's number for it.
#[derive(Debug)]
pub(crate) struct Declared {
    pub(crate) number: u64,
    pub(crate) name: String,
    /// `counter`, `gauge`, `timer` or `histogram`.
    pub(crate) form: String,
    pub(crate) unit: Option<String>,
    pub(crate) help: Option<String>,
    pub(crate) keep: Option<Duration>,
    pub(crate) buckets: Option<Buckets>,
}

/// A set of labels of an instrument, by its stream's numbers, or its series
/// of `__overflow`, where a client counts past the sets it holds.
#[derive(Debug)]
pub(crate) struct Set {
    pub(crate) number: u64,
    pub(crate) instrument: u64,
    pub(crate) labels: Labels,
    pub(crate) overflow: bool,
}

/// What a set of labels changed since the batch before.
#[derive(Debug, Default)]
pub(crate) struct Change {
    pub(crate) set: u64,
    /// A counter's sum to add.
    pub(crate) add: Option<f64>,
    /// A gauge's value.
    pub(crate) value: Option<f64>,
    pub(crate) count: u64,
    pub(crate) sum: f64,
    pub(crate) longest: f64,
    /// Each given bound's bucket's own count, `+Inf` last.
    pub(crate) buckets: Vec<u64>,
    /// Each interval of the grid's count, by its label.
    pub(crate) bins: Vec<(String, u64)>,
}

/// A batch of a client's changes, numbered in its session.
#[derive(Debug, Default)]
pub(crate) struct Batch {
    pub(crate) number: u64,
    pub(crate) instruments: Vec<Declared>,
    pub(crate) sets: Vec<Set>,
    pub(crate) changes: Vec<Change>,
    /// The store writes its totals before it answers.
    pub(crate) flush: bool,
}

/// The store's answer: the last batch of the session it applied, whether it
/// knew the session, and the instruments it refused, each with why.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ack {
    pub(crate) applied: u64,
    pub(crate) known: bool,
    pub(crate) refused: Vec<(u64, String)>,
}

/// One stream of a client's instruments: its session, what it declared, and
/// the connection whose gauges leave the totals with it.
pub(crate) struct Remote {
    engine: Arc<Engine>,
    session: Arc<Mutex<Session>>,
    /// The instruments it declared, none for one the store refused.
    instruments: HashMap<u64, Option<Arc<Instrument>>>,
    /// Its sets of labels, none for those of a refused instrument.
    sets: HashMap<u64, Option<Labelled>>,
    connection: Connection,
}

/// A set of labels a stream declared: its instrument and the slot it counts into.
type Labelled = (Arc<Instrument>, Arc<Slot>);

impl Metrics {
    /// A stream of a client's instruments in session `id`, which says the
    /// last batch it was answered: the stream, and what the store knows.
    pub(crate) fn remote(&self, id: [u8; 16], after: u64) -> (Remote, Ack) {
        let engine = Arc::clone(&self.engine);
        let now = engine.now();
        let (session, known) = {
            let mut sessions = lock(&engine.sessions.known);
            sessions.retain(|_, session| {
                let session = lock(session);
                session.streams > 0 || now.saturating_sub(session.left_at) < FORGET
            });
            match sessions.get(&id) {
                Some(session) => (Arc::clone(session), true),
                None => {
                    let session = Arc::new(Mutex::new(Session::default()));
                    sessions.insert(id, Arc::clone(&session));
                    (session, false)
                }
            }
        };
        let applied = {
            let mut held = lock(&session);
            held.streams += 1;
            // a session the store forgot takes up where its client was answered
            held.applied = held.applied.max(after);
            held.applied
        };
        let connection = engine.totals.connect();
        let remote = Remote { engine, session, instruments: HashMap::new(), sets: HashMap::new(), connection };
        (remote, Ack { applied, known, refused: Vec::new() })
    }
}

impl Remote {
    /// What the store has applied of its session.
    pub(crate) fn ack(&self) -> Ack {
        Ack { applied: lock(&self.session).applied, known: true, refused: Vec::new() }
    }

    /// Applies a batch once, the one after the last its session applied: one
    /// applied already, sent again on a new stream, declares what it names
    /// for the batches after it, and changes nothing.
    pub(crate) fn apply(&mut self, batch: Batch) -> Result<Ack> {
        let session = Arc::clone(&self.session);
        let mut held = lock(&session);
        if batch.number > held.applied + 1 {
            let next = held.applied + 1;
            return Err(Error::invalid(format!("batch {}, where the session's next is {next}", batch.number)));
        }
        // A batch sent again declares again only what this stream lacks: on
        // the stream that sent it first, its instruments and sets are known.
        let again = batch.number <= held.applied;
        let refused = self.declare(&batch.instruments, again)?;
        self.label(&batch.sets, again)?;
        if again {
            return Ok(Ack { applied: held.applied, known: true, refused });
        }
        let changed: Vec<_> = batch.changes.iter().map(|change| self.slot_of(change)).collect::<Result<_>>()?;
        for (target, change) in changed.into_iter().zip(&batch.changes) {
            if let Some((instrument, slot)) = target {
                self.change(&instrument, &slot, change);
            }
        }
        held.applied = batch.number;
        drop(held);
        if batch.flush {
            self.engine.write_totals()?;
        }
        Ok(Ack { applied: batch.number, known: true, refused })
    }

    /// Opens the instruments a batch declares; one the store refuses is
    /// answered with why, and the changes of its sets go nowhere.
    fn declare(&mut self, declared: &[Declared], again: bool) -> Result<Vec<(u64, String)>> {
        let mut refused = Vec::new();
        for instrument in declared {
            if again && self.instruments.contains_key(&instrument.number) {
                continue;
            }
            if self.instruments.contains_key(&instrument.number) {
                return Err(Error::invalid(format!("instrument {} declared twice", instrument.number)));
            }
            let opened = Form::parse(&instrument.form)
                .ok_or_else(|| {
                    Error::invalid(format!("a form {:?}: a counter, a gauge, a timer or a histogram", instrument.form))
                })
                .and_then(|form| self.engine.open_declared(instrument, form));
            match opened {
                Ok(opened) => drop(self.instruments.insert(instrument.number, Some(opened))),
                Err(error) => {
                    refused.push((instrument.number, error.to_string()));
                    self.instruments.insert(instrument.number, None);
                }
            }
        }
        Ok(refused)
    }

    /// The slots of the sets of labels a batch declares; labels an
    /// instrument refuses give a slot nothing writes, said once.
    fn label(&mut self, sets: &[Set], again: bool) -> Result<()> {
        for set in sets {
            if again && self.sets.contains_key(&set.number) {
                continue;
            }
            if self.sets.contains_key(&set.number) {
                return Err(Error::invalid(format!("set {} declared twice", set.number)));
            }
            let instrument = self.instruments.get(&set.instrument).ok_or_else(|| {
                Error::invalid(format!("set {} of instrument {}, declared nowhere", set.number, set.instrument))
            })?;
            let slot = match instrument {
                Some(instrument) if set.overflow => Some((Arc::clone(instrument), instrument.overflowed()?)),
                Some(instrument) => {
                    let labels: Vec<(&str, &str)> =
                        set.labels.iter().map(|(label, value)| (label.as_str(), value.as_str())).collect();
                    let slot = instrument.with(&instrument.base()?, &labels);
                    Some((Arc::clone(instrument), slot))
                }
                None => None,
            };
            self.sets.insert(set.number, slot);
        }
        Ok(())
    }

    fn slot_of(&self, change: &Change) -> Result<Option<Labelled>> {
        self.sets
            .get(&change.set)
            .cloned()
            .ok_or_else(|| Error::invalid(format!("a change of set {}, declared nowhere", change.set)))
    }

    /// Applies a change as the instrument's own calls would; one the
    /// instrument refuses changes nothing and is said once.
    fn change(&self, instrument: &Instrument, slot: &Slot, change: &Change) {
        match instrument.form {
            Form::Counter => match change.add {
                Some(n) if n.is_finite() && n >= 0.0 => {
                    if !slot.add(n) {
                        slot.refuse("a counter's total past what a float holds");
                    }
                }
                n => slot.refuse(&format!("a counter's add of {n:?}: a finite number of 0 or more")),
            },
            Form::Gauge => match change.value {
                Some(value) if value.is_finite() => slot.set(value, self.connection.id),
                value => slot.refuse(&format!("a gauge set to {value:?}: a gauge is a finite number")),
            },
            Form::Timer | Form::Histogram => match measures(instrument.bounds.as_ref(), change) {
                Ok(measures) => slot.merge(&measures),
                Err(why) => slot.refuse(&why),
            },
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let mut held = lock(&self.session);
        held.streams = held.streams.saturating_sub(1);
        held.left_at = self.engine.now();
    }
}

/// What a timer's or a histogram's change measured, checked against its
/// bounds: its buckets' counts or its intervals' add up to its count.
fn measures(bounds: Option<&Bounds>, change: &Change) -> std::result::Result<Measures, String> {
    let (sum, longest) = (change.sum, change.longest);
    if !(sum.is_finite() && longest.is_finite() && sum >= 0.0 && longest >= 0.0) {
        return Err(format!("a measure of sum {sum} and longest {longest}: finite numbers of 0 or more"));
    }
    let mut measures =
        Measures { count: change.count, sum, longest, buckets: change.buckets.clone(), bins: Vec::new() };
    let counted: u64 = match bounds {
        Some(given @ Bounds::Given(_)) if change.buckets.len() == given.buckets() && change.bins.is_empty() => {
            change.buckets.iter().sum()
        }
        Some(Bounds::Exponential) if change.buckets.is_empty() => {
            for (label, count) in &change.bins {
                let bin = Bin::parse(label).filter(|bin| bin.schema().is_none_or(|schema| schema == FINEST));
                let bin = bin.ok_or_else(|| format!("an interval {label:?} of no grid it counts on"))?;
                measures.bins.push((bin, *count));
            }
            measures.bins.iter().map(|(_, count)| count).sum()
        }
        None if change.buckets.is_empty() && change.bins.is_empty() => change.count,
        _ => return Err("buckets other than it keeps".to_owned()),
    };
    if counted != change.count {
        return Err(format!("{counted} counted in its buckets, where it measured {}", change.count));
    }
    Ok(measures)
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
