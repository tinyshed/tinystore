//! channels' methods of protocol 2, protocol/channels.wire: a publish, one
//! inside a database's transaction handed over at its commit, and a
//! subscription whose batches go within the client's credit.
//!
//! A subscription drains its branch's log on a worker, as much as the
//! client's credit takes, and sleeps until the log or the client wakes it.
//! A client that grants nothing falls behind and misses messages: the log is
//! bounded, and neither the publisher nor the session waits for it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use super::codec::Message;
use super::protocol::{ChannelsBatch, ChannelsMessage, ChannelsPublish, ChannelsSubscribe, Empty, Failure, method};
use super::{Link, Route};
use crate::Store;
use crate::channels::{Hub, Log, LogKey, remote_branch};

pub(crate) type Answered = Result<Vec<u8>, Failure>;

/// The messages a batch takes at most.
const BATCH: usize = 256;

/// What a batch costs beyond its messages: its own heads and the count of
/// what was missed.
const BATCH_HEAD: usize = 32;

/// What a connection may owe its client before its subscriptions stop for
/// it: past it they fall behind, rather than grow the server's memory.
const OUTPUT_BOUND: usize = 8 << 20;

/// The batches a subscription sends before it gives the worker to another.
const TURN: usize = 4;

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::CHANNELS_SUBSCRIBE => Route::Subscribe,
        _ => Route::Inline,
    }
}

/// Answers a publish: the store has the message.
pub(crate) fn call(store: &Store, called: u16, body: &[u8]) -> Answered {
    match called {
        method::CHANNELS_PUBLISH => {
            let Published { hub, name, branch, value } = published(store, body)?;
            hub.publish(&name, branch, value)?;
            Ok(Empty {}.encode())
        }
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

/// Answers a publish inside a database's transaction: the message is handed
/// over once the transaction commits, and never if it rolls back.
#[cfg(feature = "sql")]
pub(crate) fn call_in(store: &Store, tx: &crate::Transaction<'_>, called: u16, body: &[u8]) -> Answered {
    if called != method::CHANNELS_PUBLISH {
        return Err(Failure::invalid(format!("method {called:#06x} does not run inside a transaction")));
    }
    let Published { hub, name, branch, value } = published(store, body)?;
    tx.after_commit(Box::new(move || {
        if let Err(error) = hub.publish(&name, branch, value) {
            tracing::warn!(target: "tinystore", channel = %name, %error, "a committed message was not handed over");
        }
    }));
    Ok(Empty {}.encode())
}

/// A publish's message, checked, and the channels it goes to.
struct Published {
    hub: Arc<Hub>,
    name: Arc<str>,
    branch: Vec<String>,
    value: Vec<u8>,
}

/// Reads a publish's body: its channel, its branch and its JSON.
fn published(store: &Store, body: &[u8]) -> Result<Published, Failure> {
    let publish = ChannelsPublish::decode(body)?;
    let (name, branch) = remote_branch(&publish.name, publish.under)?;
    if let Err(error) = serde_json::from_str::<serde::de::IgnoredAny>(&publish.value) {
        return Err(Failure::invalid(format!("channel {name}: a message that is not JSON: {error}")));
    }
    if publish.value.len() > 1 << 20 {
        return Err(Failure::invalid(format!(
            "channel {name}: a message of {} bytes, past 1 MiB",
            publish.value.len()
        )));
    }
    Ok(Published { hub: Hub::of(store)?, name, branch, value: publish.value.into_bytes() })
}

/// The subscriptions of a connection, by stream.
#[derive(Default)]
pub(crate) struct Subscriptions {
    by_stream: Mutex<HashMap<u32, Arc<Subscribed>>>,
}

/// Registers the subscription a `channels.subscribe` asks for; it sends
/// nothing until [`Subscribed::begin`], once the stream's RESPONSE went out.
pub(crate) fn subscribe(
    store: &Store,
    subscriptions: &Subscriptions,
    stream: u32,
    body: &[u8],
    link: Link,
) -> Result<Arc<Subscribed>, Failure> {
    let subscribe = ChannelsSubscribe::decode(body)?;
    let (channel, branch) = remote_branch(&subscribe.name, subscribe.under)?;
    let hub = Hub::of(store)?;
    let key = LogKey { channel, branch, recursive: subscribe.recursive };
    let draining = Draining { credit: link.credit, scheduled: true, ended: false };
    let subscribed = Arc::new(Subscribed { hub, key, place: OnceLock::new(), link, draining: Mutex::new(draining) });
    let woken = Arc::downgrade(&subscribed);
    let wake = Arc::new(move || {
        if let Some(subscribed) = woken.upgrade() {
            subscribed.wake();
        }
    });
    let place = subscribed.hub.join(&subscribed.key, wake)?;
    let _ = subscribed.place.set(place);
    lock(&subscriptions.by_stream).insert(stream, Arc::clone(&subscribed));
    Ok(subscribed)
}

/// A subscription over the wire: its place in its branch's log, and what it
/// may send.
pub(crate) struct Subscribed {
    hub: Arc<Hub>,
    key: LogKey,
    place: OnceLock<(Arc<Log>, u64)>,
    link: Link,
    draining: Mutex<Draining>,
}

struct Draining {
    credit: u64,
    /// A drain is on a worker, or waits to begin: a wake does nothing more.
    scheduled: bool,
    ended: bool,
}

impl Subscribed {
    /// Lets the subscription send, once its RESPONSE went out: whatever came
    /// meanwhile goes now.
    pub(crate) fn begin(self: &Arc<Self>) {
        lock(&self.draining).scheduled = false;
        self.wake();
    }

    fn wake(self: &Arc<Self>) {
        {
            let mut draining = lock(&self.draining);
            if draining.scheduled || draining.ended {
                return;
            }
            draining.scheduled = true;
        }
        let drain = Arc::clone(self);
        (self.link.spawn)(Box::new(move || drain.drain()));
    }

    /// Sends batches while the client's credit takes them, then sleeps until
    /// the log or the client wakes it.
    fn drain(self: &Arc<Self>) {
        let Some((log, reader)) = self.place.get() else { return };
        for sent in 0.. {
            if sent == TURN {
                let drain = Arc::clone(self);
                return (self.link.spawn)(Box::new(move || drain.drain()));
            }
            if (self.link.owed)() > OUTPUT_BOUND {
                return self.wait_for_output();
            }
            let (credit, budget) = {
                let draining = lock(&self.draining);
                if draining.ended {
                    return;
                }
                (draining.credit, usize::try_from(draining.credit).unwrap_or(usize::MAX).min(self.link.max_body))
            };
            // Not even a batch of what was missed fits: wait for the client.
            if budget < BATCH_HEAD {
                if self.rest(log, *reader, Some(credit)) {
                    return;
                }
                continue;
            }
            let (taken, waits) = log.take_within(*reader, BATCH, budget - BATCH_HEAD);
            if taken.missed == 0 && taken.messages.is_empty() {
                // More credit would not help a message past the connection's body.
                if waits && budget == self.link.max_body {
                    log.skip(*reader);
                    continue;
                }
                if self.rest(log, *reader, waits.then_some(credit)) {
                    return;
                }
                continue;
            }
            let messages = taken.messages.iter().map(|message| ChannelsMessage {
                under: message.branch.clone(),
                value: String::from_utf8_lossy(&message.value).into_owned(),
            });
            let body = ChannelsBatch { missed: taken.missed, messages: messages.collect() }.encode();
            lock(&self.draining).credit -= body.len() as u64;
            (self.link.send)(body);
        }
    }

    /// Stops until the connection's output drains.
    fn wait_for_output(self: &Arc<Self>) {
        lock(&self.draining).scheduled = false;
        let woken = Arc::downgrade(self);
        (self.link.when_drained)(Box::new(move || {
            if let Some(subscribed) = woken.upgrade() {
                subscribed.wake();
            }
        }));
    }

    /// Stops draining; says whether to stay stopped. What it waits for may
    /// have come since it looked, and a wake then found the drain still
    /// scheduled: a message, or, when `credit` is the credit it saw, a grant.
    fn rest(&self, log: &Log, reader: u64, credit: Option<u64>) -> bool {
        let mut draining = lock(&self.draining);
        let stays = match credit {
            Some(seen) => draining.credit == seen,
            None => !log.holds_for(reader),
        };
        if stays {
            draining.scheduled = false;
        }
        stays
    }

    fn grant(self: &Arc<Self>, credit: u32) {
        lock(&self.draining).credit += u64::from(credit);
        self.wake();
    }

    /// Ends the subscription without a frame; says whether it was going.
    fn end(&self) -> bool {
        if std::mem::replace(&mut lock(&self.draining).ended, true) {
            return false;
        }
        if let Some((_, reader)) = self.place.get() {
            self.hub.leave(&self.key, *reader);
        }
        true
    }

    fn close(&self, last: Answered) {
        if self.end() {
            (self.link.finish)(last);
        }
    }
}

impl Subscriptions {
    pub(crate) fn grant(&self, stream: u32, credit: u32) {
        let subscribed = lock(&self.by_stream).get(&stream).cloned();
        if let Some(subscribed) = subscribed {
            subscribed.grant(credit);
        }
    }

    /// Ends a subscription the client cancelled; says whether there was one.
    pub(crate) fn cancel(&self, stream: u32) -> bool {
        let subscribed = lock(&self.by_stream).remove(&stream);
        let Some(subscribed) = subscribed else { return false };
        subscribed.close(Err(Failure::cancelled("the client cancelled its subscription")));
        true
    }

    /// Ends every subscription: the server is closing, and the client
    /// subscribes again on another connection, told it may have missed some.
    pub(crate) fn go_away(&self) {
        let all: Vec<_> = lock(&self.by_stream).drain().map(|(_, subscribed)| subscribed).collect();
        for subscribed in all {
            subscribed.close(Err(Failure::unavailable("the server is closing; subscribe again on another connection")));
        }
    }

    /// Lets go of every subscription at the session's end.
    pub(crate) fn end(&self) {
        let all: Vec<_> = lock(&self.by_stream).drain().map(|(_, subscribed)| subscribed).collect();
        for subscribed in all {
            subscribed.end();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "channels_tests.rs"]
mod tests;
