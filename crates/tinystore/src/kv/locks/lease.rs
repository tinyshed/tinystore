//! What a lease is made of: the key it holds, its fence, its id, and what
//! tells its holder that it ended.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use super::breaker::Rule;
use crate::Result;

/// A lock and a semaphore of one name are two things.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Kind {
    Lock,
    Semaphore,
}

/// A key of a lock or a semaphore: what is held, waited for, and named in
/// every error, `lock invoices: key "42"`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Place {
    kind: Kind,
    name: Arc<str>,
    key: String,
}

impl Place {
    pub(crate) fn new(kind: Kind, name: Arc<str>, key: String) -> Place {
        Place { kind, name, key }
    }

    /// The lock's or the semaphore's name, never its key.
    pub(crate) fn name(&self) -> &Arc<str> {
        &self.name
    }
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            Kind::Lock => "lock",
            Kind::Semaphore => "semaphore",
        };
        write!(f, "{kind} {}: key {:?}", self.name, self.key)
    }
}

/// A number that grows with every lease a store gives, so that whoever keeps
/// the largest it has seen can refuse a late holder: the store's epoch in the
/// high 64 bits, which grows each time the locks of a store open, and a count
/// of its leases since in the low 64.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fence(u128);

impl Fence {
    pub(crate) fn of(epoch: u64, count: u64) -> Fence {
        Fence((u128::from(epoch) << 64) | u128::from(count))
    }

    /// The fence as one integer, compared as it is.
    pub fn get(self) -> u128 {
        self.0
    }
}

impl fmt::Display for Fence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for Fence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fence({}·{})", self.0 >> 64, self.0 & u128::from(u64::MAX))
    }
}

/// 128 random bits that only a lease's holder learns: renewing and letting go
/// name them, while the fence, which the holder hands to others, proves
/// nothing.
pub(crate) type LeaseId = [u8; 16];

/// 128 random bits a client draws for a request, so that a grant whose answer
/// was lost is asked for again rather than given twice.
pub(crate) type RequestId = [u8; 16];

/// Which line of its key a hold waits in. Urgent holds are given the key
/// before background ones, and a background hold waiting never holds an
/// urgent one back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Priority {
    /// Every hold unless said: a person waiting for its answer.
    #[default]
    Urgent,
    /// Work nobody waits for, which takes no more of the key's places than
    /// the semaphore's `background` and waits while urgent holds keep coming.
    Background,
}

/// What a holder asks for.
pub(crate) struct Ask {
    pub(crate) place: Place,
    /// How many may hold the key at once: one for a lock.
    pub(crate) limit: u32,
    pub(crate) priority: Priority,
    /// How many of the key's places background holds may take at once; none
    /// for every place.
    pub(crate) background: Option<u32>,
    /// How long a lease lasts from its last renewal; none for a holder in the
    /// store's own process, whose lease ends with its function.
    pub(crate) ttl: Option<Duration>,
    /// How long a lease lasts from its grant, however it is renewed: the
    /// lock's or semaphore's `max_hold`, or a hold's `timeout` when shorter.
    pub(crate) max_hold: Option<Duration>,
    /// How long the holder waits in line; zero to take the key only if free.
    pub(crate) wait: Duration,
    pub(crate) request: Option<RequestId>,
    /// How many failed holds in a row put the key down, and for how long.
    pub(crate) breaker: Option<Rule>,
    /// Set by the state alone: the one ask a down key lets through once its
    /// pause is over.
    pub(crate) probe: bool,
}

/// What an ask comes to.
pub(crate) enum Answer {
    Granted(Granted),
    /// The key was held, and stayed held past the wait.
    Busy,
}

/// What a holder is answered with, on whichever thread decided it; it must
/// not block, since the store's locks wait for it.
pub(crate) type Reply = Box<dyn FnOnce(Result<Answer>) + Send>;

/// A lease as it was given.
#[derive(Clone)]
pub(crate) struct Granted {
    pub(crate) lease: LeaseId,
    pub(crate) fence: Fence,
    /// Unix milliseconds by the store's clock; none while its function runs.
    pub(crate) expires: Option<i64>,
    /// When `max_hold` revokes it, unix milliseconds.
    pub(crate) revokes: Option<i64>,
    pub(crate) watch: Arc<Watch>,
}

/// How a lease ended, told to its holder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    Live,
    /// It ran out or was revoked, or an awaited hold ended with its lease
    /// kept: another holder may run beside this one.
    Lost,
    /// The store closed.
    Closed,
}

/// What a holder reads to learn that its lease ended: a Rust holder between
/// the steps of its function. A holder through a connection learns it from
/// its renewal, or from `max_hold`'s time, which it was told.
#[derive(Default)]
pub(crate) struct Watch {
    ending: AtomicU8,
    /// The holder said the guarded service is healthy, whatever its work
    /// answers after.
    healthy: AtomicBool,
}

impl Watch {
    pub(crate) fn healthy(&self) {
        self.healthy.store(true, Ordering::Release);
    }

    pub(crate) fn said_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub(crate) fn ending(&self) -> Ending {
        match self.ending.load(Ordering::Acquire) {
            0 => Ending::Live,
            1 => Ending::Lost,
            _ => Ending::Closed,
        }
    }

    /// Ends the lease, once: a second ending changes nothing.
    pub(crate) fn end(&self, ending: Ending) {
        let code = if ending == Ending::Closed { 2 } else { 1 };
        let _ = self.ending.compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
    }
}
