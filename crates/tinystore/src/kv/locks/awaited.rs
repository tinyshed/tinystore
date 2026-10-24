//! A hold an awaited call makes: its ask answered through a waker rather than
//! a parked thread, and its key let go when its future is dropped, waiting or
//! holding.
//!
//! | its future dropped    | what happens                                                      |
//! |-----------------------|-------------------------------------------------------------------|
//! | before its first poll | nothing was asked                                                 |
//! | waiting in line       | it leaves the line; a grant that raced the drop is let go at once |
//! | holding               | the key goes to the next in line                                  |
//!
//! The lease an awaited hold gives its function is owned, so it may outlive
//! the hold, returned or moved into a task; it says lost from the moment the
//! hold ends, before the key can go to the next in line.
//!
//! The answer's reply stores the answer and wakes the task: it runs on the
//! thread that decided it, the asking, releasing, timer or closing one, and
//! must not block, since the store's locks wait for it.

use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use super::lease::{Answer, Ask, Ending, Place, Reply, Watch};
use super::{Fence, Holding, Locks, Mark, Opened, refuse_where_a_hold_would_wait_for_itself};
use crate::sync::lock;
use crate::{Error, ErrorKind, Result};

/// A key to hold, owned, so that the future of its hold borrows no handle.
pub(crate) struct Awaited {
    opened: Opened,
    key: String,
    wait: Duration,
}

impl super::LockKey<'_> {
    /// The hold an awaited call makes of this key.
    pub(crate) fn awaited(self) -> Awaited {
        Awaited { opened: self.opened.clone(), key: self.key, wait: self.wait }
    }
}

impl super::SemaphoreKey<'_> {
    /// The hold an awaited call makes of this key.
    pub(crate) fn awaited(self) -> Awaited {
        Awaited { opened: self.opened.clone(), key: self.key, wait: self.wait }
    }
}

impl Awaited {
    /// Holds the key once it is free, its `wait` at most: `in use` past it.
    /// The ask is made in the first poll, and a key held puts a waiter in its
    /// line that parks no thread.
    pub(crate) async fn hold(self) -> Result<Held> {
        let wait = self.wait;
        let asking = self.ask(wait)?;
        let place = asking.place.clone();
        let held = asking.await?;
        held.ok_or_else(|| Error::new(ErrorKind::InUse, format!("held past a wait of {wait:?}")).within(place))
    }

    /// Holds the key if nobody holds it, and answers none without waiting if
    /// somebody does.
    pub(crate) async fn try_hold(self) -> Result<Option<Held>> {
        self.ask(Duration::ZERO)?.await
    }

    fn ask(self, wait: Duration) -> Result<Asking> {
        let place = self.opened.place(&self.key)?;
        refuse_where_a_hold_would_wait_for_itself(&self.opened.locks, &place)?;
        let ask = Ask {
            place: place.clone(),
            limit: self.opened.limit,
            ttl: None,
            max_hold: self.opened.max_hold,
            wait,
            request: None,
        };
        Ok(Asking::new(&self.opened.locks, place, ask))
    }
}

/// An ask in the store's locks, and the answer its reply leaves.
struct Asking {
    slot: Arc<Mutex<Slot>>,
    locks: Weak<Locks>,
    address: usize,
    place: Place,
    /// The waiter's id while it waits in line.
    waiter: Option<u64>,
}

enum Slot {
    Waiting(Option<Waker>),
    Answered(Result<Answer>),
    /// The future took the answer.
    Taken,
    /// The future was dropped first: a grant is let go as it comes.
    Gone,
}

impl Asking {
    fn new(locks: &Arc<Locks>, place: Place, ask: Ask) -> Asking {
        let slot = Arc::new(Mutex::new(Slot::Waiting(None)));
        let (replied, letting_go) = (Arc::clone(&slot), Arc::downgrade(locks));
        let reply: Reply = Box::new(move |answer| {
            let mut slot = lock(&replied);
            if matches!(*slot, Slot::Gone) {
                drop(slot);
                if let (Ok(Answer::Granted(granted)), Some(locks)) = (answer, letting_go.upgrade()) {
                    locks.release(&granted.lease);
                }
                return;
            }
            let waker = match mem::replace(&mut *slot, Slot::Answered(answer)) {
                Slot::Waiting(waker) => waker,
                _ => None,
            };
            drop(slot);
            if let Some(waker) = waker {
                waker.wake();
            }
        });
        let address = Arc::as_ptr(locks) as usize;
        let waiter = locks.ask(ask, reply);
        Asking { slot, locks: Arc::downgrade(locks), address, place, waiter }
    }
}

impl Future for Asking {
    type Output = Result<Option<Held>>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut slot = lock(&self.slot);
        match mem::replace(&mut *slot, Slot::Taken) {
            Slot::Answered(answer) => {
                drop(slot);
                Poll::Ready(answer.map(|answer| match answer {
                    Answer::Granted(granted) => Some(Held {
                        holding: Holding { locks: self.locks.clone(), place: self.place.clone(), granted, _mark: None },
                        address: self.address,
                    }),
                    Answer::Busy => None,
                }))
            }
            Slot::Waiting(waker) => {
                let waker = match waker {
                    Some(waker) if waker.will_wake(context.waker()) => waker,
                    _ => context.waker().clone(),
                };
                *slot = Slot::Waiting(Some(waker));
                Poll::Pending
            }
            Slot::Taken | Slot::Gone => {
                Poll::Ready(Err(Error::internal("a hold's answer was awaited after it was taken")))
            }
        }
    }
}

impl Drop for Asking {
    /// Leaves the line, or lets go of a key given and never taken; the cancel
    /// and the release take the locks' state, so the slot is let go first.
    fn drop(&mut self) {
        let left = mem::replace(&mut *lock(&self.slot), Slot::Gone);
        let Some(locks) = self.locks.upgrade() else { return };
        match left {
            Slot::Waiting(_) => {
                if let Some(waiter) = self.waiter {
                    locks.cancel(waiter);
                }
            }
            Slot::Answered(Ok(Answer::Granted(granted))) => {
                locks.release(&granted.lease);
            }
            _ => {}
        }
    }
}

/// A key an awaited hold was given: let go when it is dropped.
pub(crate) struct Held {
    holding: Holding,
    /// The locks' address, which marks a thread as holding the key.
    address: usize,
}

impl Held {
    pub(crate) fn fence(&self) -> Fence {
        self.holding.granted.fence
    }

    /// What says whether the lease ended, for the lease a function is given.
    pub(crate) fn ends(&self) -> Ends {
        Ends(Arc::clone(&self.holding.granted.watch))
    }

    /// Runs `step` with the polling thread marked as holding the key, so that
    /// a hold of the same key inside it is refused rather than wait for
    /// itself. The mark lasts the step alone: the future that holds the key
    /// may be polled next on another thread.
    pub(crate) fn marked<R>(&self, step: impl FnOnce() -> R) -> R {
        let _mark = Mark::at(self.address, &self.holding.place);
        step()
    }

    /// What the hold answers once its function answered `value`: the value,
    /// unless the lease ended meanwhile.
    pub(crate) fn kept<T, E: From<Error>>(&self, value: T) -> Result<T, E> {
        self.holding.kept(value)
    }
}

impl Drop for Held {
    /// Ends the lease before `holding`'s drop lets the key go, so that a lease
    /// kept past its hold never reads live while another holds the key.
    fn drop(&mut self) {
        self.holding.granted.watch.end(Ending::Lost);
    }
}

/// Whether a lease ended, while its function runs or once its hold is over.
pub(crate) struct Ends(Arc<Watch>);

impl Ends {
    pub(crate) fn lost(&self) -> bool {
        self.0.ending() != Ending::Live
    }
}
