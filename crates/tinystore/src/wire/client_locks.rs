//! A shell's hold of a lock, `lock hold`: the key asked for, its lease renewed
//! while the command it guards runs, and let go once it ends.

use std::io::{Read, Write};
use std::time::Duration;

use super::{Client, millis_of};
use crate::Result;
use crate::wire::protocol::{self as wire, method};

/// A hold of one key of a lock, as `Lock::hold` asks it.
#[derive(Clone, Debug)]
pub struct Hold {
    pub lock: String,
    pub key: String,
    /// How long the lease lasts from its grant and from each renewal.
    pub ttl: Duration,
    /// How long to stand in line: none takes the key only if it is free.
    pub wait: Duration,
    pub max_hold: Option<Duration>,
    /// 16 random bytes: a hold asked again is answered with the lease given.
    pub request: [u8; 16],
}

/// A key held: the lease only its holder knows, and its fence.
#[derive(Clone, Debug)]
pub struct Lease {
    pub lease: Vec<u8>,
    /// A decimal integer larger than every earlier lease's.
    pub fence: String,
}

impl<S: Read + Write> Client<S> {
    /// Holds the key: its lease, none when others hold it past the wait.
    pub fn hold(&mut self, hold: &Hold) -> Result<Option<Lease>> {
        let asked = wire::LocksHold {
            kind: "lock".to_owned(),
            name: hold.lock.clone(),
            key: hold.key.clone(),
            limit: 1,
            ttl: millis_of(hold.ttl),
            max_hold: hold.max_hold.map(millis_of),
            wait: millis_of(hold.wait),
            request: hold.request.to_vec(),
        };
        match self.answer::<wire::LocksHeld>(method::LOCKS_HOLD, &asked) {
            Ok(held) if held.acquired => Ok(Some(Lease { lease: held.lease, fence: held.fence })),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == crate::ErrorKind::InUse => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Renews the lease for `ttl` from now; one that ran out or was revoked
    /// is `lost`.
    pub fn renew(&mut self, lease: &[u8], ttl: Duration) -> Result<()> {
        self.call(method::LOCKS_RENEW, &wire::LocksRenew { lease: lease.to_vec(), ttl: millis_of(ttl) }).map(drop)
    }

    /// Lets the key go.
    pub fn release(&mut self, lease: &[u8]) -> Result<()> {
        self.call(method::LOCKS_RELEASE, &wire::LocksLease { lease: lease.to_vec() }).map(drop)
    }
}
