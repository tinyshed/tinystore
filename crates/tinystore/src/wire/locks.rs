//! locks' methods of protocol 2, protocol/locks.wire: a hold answered once
//! its key is given, and its lease's renewals and letting go, which name the
//! lease by its id so that a client that connected again goes on with it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use super::Route;
use super::codec::Message;
use super::protocol::{Empty, Failure, LocksHeld, LocksHold, LocksLease, LocksRenew, method};
use crate::kv::locks::{Answer, Granted, Locks, Remote, remote_ask};
use crate::{Clock, Error, ErrorKind, Store, unix_millis};

pub(crate) type Answered = Result<Vec<u8>, Failure>;

/// What a hold's answer goes to: the stream's RESPONSE.
pub(crate) type Answering = Box<dyn FnOnce(Answered) + Send>;

/// The shortest and the longest a lease lasts between renewals.
const TTL: std::ops::RangeInclusive<Duration> = Duration::from_secs(1)..=Duration::from_secs(60 * 60);

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::LOCKS_HOLD => Route::Hold,
        _ => Route::Worker,
    }
}

/// Answers a renewal or a letting go of a lease, by its id.
pub(crate) fn call(store: &Store, called: u16, body: &[u8]) -> Answered {
    match called {
        method::LOCKS_RENEW => {
            let renew = LocksRenew::decode(body)?;
            let ttl = Duration::from_millis(renew.ttl);
            if !TTL.contains(&ttl) {
                return Err(Failure::invalid(format!("a ttl of {ttl:?}, not 1 s to 1 hour")));
            }
            Locks::of(store)?.renew(&lease_id(&renew.lease)?, ttl)?;
            Ok(Empty {}.encode())
        }
        method::LOCKS_RELEASE => {
            let release = LocksLease::decode(body)?;
            Locks::of(store)?.release(&lease_id(&release.lease)?);
            Ok(Empty {}.encode())
        }
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

fn lease_id(bytes: &[u8]) -> Result<[u8; 16], Failure> {
    bytes.try_into().map_err(|_| Failure::invalid(format!("a lease's id is 16 bytes, not {}", bytes.len())))
}

/// The holds of a connection not yet answered, by stream, each with its
/// place in line once it waits.
#[derive(Default)]
pub(crate) struct Holds {
    asking: Mutex<HashMap<u32, Option<InLine>>>,
}

/// A hold's waiter in the line of the store's locks it waits in.
type InLine = (u64, Weak<Locks>);

/// Asks for the key a `locks.hold` names; `answer` is called once it is
/// given, or held past the wait, unless the client cancelled first.
pub(crate) fn hold(
    store: &Store,
    holds: &Arc<Holds>,
    stream: u32,
    body: &[u8],
    answer: Answering,
) -> Result<(), Failure> {
    let hold = LocksHold::decode(body)?;
    let remote = Remote {
        kind: &hold.kind,
        name: &hold.name,
        key: &hold.key,
        limit: u32::try_from(hold.limit).unwrap_or(u32::MAX),
        ttl: Duration::from_millis(hold.ttl),
        max_hold: hold.max_hold.map(Duration::from_millis),
        wait: Duration::from_millis(hold.wait),
        request: lease_id(&hold.request).map_err(|_| Failure::invalid("a hold's request is 16 bytes"))?,
    };
    let (locks, ask) = remote_ask(store, &remote)?;
    let replying = Replying {
        holds: Arc::downgrade(holds),
        locks: Arc::downgrade(&locks),
        stream,
        place: ask.place.to_string(),
        waits: hold.wait > 0,
        fenced: hold.kind == "lock",
        clock: store.clock(),
    };
    lock(&holds.asking).insert(stream, None);
    if let Some(waiter) = locks.ask(ask, Box::new(move |answered| replying.reply(answered, answer)))
        && let Some(asking) = lock(&holds.asking).get_mut(&stream)
    {
        *asking = Some((waiter, Arc::downgrade(&locks)));
    }
    Ok(())
}

/// What a hold's answer needs once the key is given.
struct Replying {
    holds: Weak<Holds>,
    locks: Weak<Locks>,
    stream: u32,
    place: String,
    waits: bool,
    fenced: bool,
    clock: std::sync::Arc<dyn Clock>,
}

impl Replying {
    /// Answers the stream, unless the client cancelled it or left: a key
    /// given then is let go at once rather than held by nobody until its ttl.
    fn reply(self, answered: crate::Result<Answer>, answer: Answering) {
        let wanted = self.holds.upgrade().is_some_and(|holds| lock(&holds.asking).remove(&self.stream).is_some());
        match answered {
            Ok(Answer::Granted(granted)) if !wanted => {
                if let Some(locks) = self.locks.upgrade() {
                    locks.release(&granted.lease);
                }
            }
            _ if !wanted => {}
            Ok(Answer::Granted(granted)) => answer(Ok(self.held(&granted))),
            Ok(Answer::Busy) if !self.waits => answer(Ok(LocksHeld::default().encode())),
            Ok(Answer::Busy) => {
                let busy = Error::new(ErrorKind::InUse, "held past the hold's wait").within(&self.place);
                answer(Err(Failure::from(busy)));
            }
            Err(error) => answer(Err(Failure::from(error))),
        }
    }

    fn held(&self, granted: &Granted) -> Vec<u8> {
        let now = unix_millis(self.clock.now());
        LocksHeld {
            acquired: true,
            lease: granted.lease.to_vec(),
            fence: if self.fenced { granted.fence.to_string() } else { String::new() },
            revoke_in: granted.revokes.map(|at| u64::try_from(at.saturating_sub(now)).unwrap_or(0)),
        }
        .encode()
    }
}

impl Holds {
    /// Takes a hold the client cancelled out of its line; says whether there
    /// was one not yet answered.
    pub(crate) fn cancel(&self, stream: u32) -> bool {
        let Some(asking) = lock(&self.asking).remove(&stream) else { return false };
        if let Some((waiter, locks)) = asking
            && let Some(locks) = locks.upgrade()
        {
            locks.cancel(waiter);
        }
        true
    }

    /// Takes every hold of the connection out of its line: it is gone. The
    /// leases it was given live on to their deadline, for the client to
    /// renew from its next connection.
    pub(crate) fn end(&self) {
        let streams: Vec<u32> = lock(&self.asking).keys().copied().collect();
        for stream in streams {
            self.cancel(stream);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "locks_tests.rs"]
mod tests;
