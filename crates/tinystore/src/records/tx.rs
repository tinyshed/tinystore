//! A stream's appends inside a transaction or a batch of the database it was
//! opened from, `db.events("audit")`: each event is kept in the database's
//! file with the rows, and written to the stream once their commit is, before
//! `db.tx` or `db.batch` returns. One that rolled back was never written.

use std::sync::Arc;

use serde::Serialize;

use super::engine::FILE;
use super::event::{Kept, NewEvent};
use super::events::{EventCall, Events, check, stamped, window};
use crate::engine::{LentWrite, Wrote};
use crate::transaction::sealed::Sealed;
use crate::{Error, Result, Transaction, TxHandle};

impl Sealed for Events {}

impl TxHandle for Events {
    type InTx<'t>
        = TxEvents<'t>
    where
        Self: 't;

    fn in_tx<'t>(&'t self, tx: &'t Transaction<'t>) -> TxEvents<'t> {
        TxEvents { events: self, tx }
    }
}

/// A stream's appends inside a transaction of the database it was opened
/// from, `tx.with(&audit)`: each is kept with the transaction's rows and
/// written to the stream once it commits, before `db.tx` returns. A read goes
/// through the stream's own handle, and finds what committed.
pub struct TxEvents<'t> {
    events: &'t Events,
    tx: &'t Transaction<'t>,
}

impl std::fmt::Debug for TxEvents<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TxEvents({})", self.events.stream.name)
    }
}

impl<'t> TxEvents<'t> {
    /// Appends an event of `name` with `fields`, as [`Events::append`] does,
    /// once the transaction commits: always `true`, since only an event with
    /// an id can be one its stream holds.
    pub fn append(&self, name: &str, fields: &impl Serialize) -> Result<bool> {
        self.event(name).append(fields)
    }

    /// An append of one event, what it carries beside its fields before its
    /// verb, as [`Events::event`] has: its id, its time, its level.
    pub fn event(&self, name: &str) -> EventCall<'t> {
        EventCall { events: self.events, event: NewEvent::new(name), tx: Some(self.tx) }
    }
}

impl Events {
    /// Keeps `events` in `tx`, a transaction of the database the stream was
    /// opened from, all of them or none, and has the transaction move them
    /// into the stream once it commits; says how many their move will write.
    /// A refused one names its place in the list and its name.
    pub(crate) fn keep_in(&self, tx: &Transaction<'_>, events: Vec<NewEvent>) -> Result<usize> {
        let kept = || {
            let Some(outbox) = &self.outbox else {
                tx.takes(&self.records.file, FILE)?;
                return Err(Error::internal("a transaction that took records.db in"));
            };
            tx.takes(outbox.file(), outbox.place())?;
            let events = self.checked(events)?;
            let will = tx.call(|raw| {
                events.into_iter().try_fold(0, |will, event| {
                    outbox.keep(raw, &self.stream, event).map(|kept| will + usize::from(kept))
                })
            })?;
            let moving = Arc::clone(outbox);
            tx.after(move |committed| {
                if committed {
                    moving.move_left();
                }
            });
            Ok(will)
        };
        kept().map_err(|error| error.within(format!("events {}", self.stream.name)))
    }

    /// The appends of `events` for a batch of the database the stream was
    /// opened from, kept in the batch's savepoint and moved once its commit
    /// is durable, before the batch answers; checked now, before the batch
    /// waits for the writer.
    pub(crate) fn lent(&self, events: Vec<NewEvent>) -> Result<LentWrite> {
        let describe = format!("events {}", self.stream.name);
        let Some(outbox) = &self.outbox else {
            let why = "kept in records.db, outside a database's batch: open the stream from the database, db.events";
            return Err(Error::invalid(why).within(describe));
        };
        let events = self.checked(events).map_err(|error| error.within(describe.clone()))?;
        let weight = events.iter().map(Kept::weight).sum();
        let (stream, keeping, moving) = (self.stream.clone(), Arc::clone(outbox), Arc::clone(outbox));
        Ok(LentWrite {
            file: Arc::clone(outbox.file()),
            place: outbox.place().to_owned(),
            describe,
            weight,
            write: Box::new(move |raw| {
                let will = events.into_iter().try_fold(0, |will, event| {
                    keeping.keep(raw, &stream, event).map(|kept| will + usize::from(kept))
                })?;
                let moved = Box::new(move || moving.move_left()) as Box<dyn FnOnce() + Send>;
                Ok(Wrote { changes: will as u64, committed: None, before_answer: Some(moved) })
            }),
        })
    }

    /// Each of `events` as the stream keeps it, at its place's time from the
    /// store's unless it has one, as an append outside a database takes them:
    /// a refused one names its place in the list and its name. The outbox
    /// keeps each time, which its move appends as it is.
    fn checked(&self, events: Vec<NewEvent>) -> Result<Vec<Kept>> {
        let now = self.records.now();
        let window = window(&self.stream, now);
        let checked = events.into_iter().enumerate().map(|(place, event)| {
            let named = format!("event {place} {:?}", event.name);
            check(event, stamped(now, place), window).map_err(|error| error.within(named))
        });
        checked.collect()
    }
}

impl EventCall<'_> {
    /// The event's append for a batch of the database its stream was opened
    /// from, with what the call carries beside its fields. One made through
    /// a transaction runs in it, and fails the batch.
    pub(crate) fn lent(self, fields: &impl Serialize) -> Result<LentWrite> {
        if self.tx.is_some() {
            return Err(Error::invalid("an event's append made through a transaction runs in it, not in a batch"));
        }
        self.events.lent(vec![self.event.fields(fields)])
    }
}
