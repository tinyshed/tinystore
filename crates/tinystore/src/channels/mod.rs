//! Channels: messages a program publishes on a named channel, which every
//! subscriber listening at that moment receives, in memory and at most once.
//! A message published while nobody listens is gone; work that must be done
//! is a job's, and events to read again are records'. docs/guides/channels/README.md is
//! the book.
//!
//! ```no_run
//! # fn main() -> tinystore::Result<()> {
//! # let store = tinystore::Store::open("data", Default::default())?;
//! let chat = store.channel::<String>("chat").open()?;
//! let sub = chat.under("room-7").subscribe(|message| println!("{}", message.value))?;
//! chat.under("room-7").publish(&"hi".to_owned())?;
//! sub.stop();
//! # Ok(())
//! # }
//! ```

mod deliver;
mod hub;
mod log;

use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;

use self::deliver::{Delivery, Run, Subscriber};
pub(crate) use self::hub::{Hub, LogKey};
pub(crate) use self::log::{Log, Released};
use crate::{Error, Key, Result, Store};

/// The longest message, as JSON, and the longest branch, its owners together.
const MOST_MESSAGE: usize = 1 << 20;
const MOST_BRANCH: usize = 1024;

/// Messages a loop takes from its log at a time.
const BATCH: usize = 64;

/// How often a subscription without `on_missed` or `on_error` says what it
/// missed or what failed in the store's log.
const QUIET: Duration = Duration::from_secs(60);

/// A channel being opened.
#[must_use = "a channel opens with open()"]
pub struct ChannelBuilder<T> {
    store: Store,
    name: String,
    _value: PhantomData<fn() -> T>,
}

impl Store {
    /// A channel of messages of one type: every subscriber listening when a
    /// message is published receives it, and nobody after.
    pub fn channel<T>(&self, name: &str) -> ChannelBuilder<T> {
        ChannelBuilder { store: self.clone(), name: name.to_owned(), _value: PhantomData }
    }
}

impl<T: Serialize + DeserializeOwned + Send + 'static> ChannelBuilder<T> {
    pub fn open(self) -> Result<Channel<T>> {
        check_name(&self.name)?;
        let hub = Hub::of(&self.store)?;
        Ok(Channel { hub, name: Arc::from(self.name.as_str()), branch: Vec::new(), _value: PhantomData })
    }
}

/// A channel, or one of its branches.
pub struct Channel<T> {
    hub: Arc<Hub>,
    name: Arc<str>,
    branch: Vec<String>,
    _value: PhantomData<fn() -> T>,
}

/// A message as a subscriber receives it: what was published, and the branch
/// it was published under.
#[derive(Clone, Debug, PartialEq)]
pub struct Message<T> {
    pub value: T,
    pub branch: Vec<String>,
}

/// Messages a subscriber lost: it fell so far behind that its branch's log
/// let them go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Missed {
    pub count: u64,
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Channel<T> {
    /// The branch of `owner` under this one: whose messages these are.
    pub fn under(&self, owner: impl Key) -> Channel<T> {
        let mut branch = self.branch.clone();
        branch.push(owner.text().into_owned());
        Channel { hub: Arc::clone(&self.hub), name: Arc::clone(&self.name), branch, _value: PhantomData }
    }

    /// Hands `value` to every subscriber of this branch, and of the branches
    /// above it listened to `recursive`, that listens now. It answers once
    /// the store has the message, not once anyone handled it.
    pub fn publish(&self, value: &T) -> Result<()> {
        let encoded = self.encoded(value)?;
        self.hub.publish(&self.name, self.branch.clone(), encoded).map_err(|error| error.within(self.shown()))
    }

    /// A subscription that takes every branch below this one too.
    pub fn recursive(&self) -> Subscribing<T> {
        self.subscribing().recursive()
    }

    /// A subscription told what it missed: by default the store's log says
    /// it, once a minute at most.
    pub fn on_missed(&self, missed: impl FnMut(Missed) + Send + 'static) -> Subscribing<T> {
        self.subscribing().on_missed(missed)
    }

    /// A subscription told what failed: a handler that panicked, a message
    /// that does not read as the channel's type.
    pub fn on_error(&self, failed: impl FnMut(Error) + Send + 'static) -> Subscribing<T> {
        self.subscribing().on_error(failed)
    }

    /// Calls `handler` with each message published from now on, one at a
    /// time and in order, on a thread of the store's.
    pub fn subscribe(&self, handler: impl FnMut(Message<T>) + Send + 'static) -> Result<Subscription> {
        self.subscribing().subscribe(handler)
    }

    /// The messages published from now on, on the caller's thread.
    pub fn each(&self) -> Result<Each<T>> {
        self.subscribing().each()
    }

    fn subscribing(&self) -> Subscribing<T> {
        Subscribing { channel: self.clone(), recursive: false, on_missed: None, on_error: None }
    }

    fn encoded(&self, value: &T) -> Result<Vec<u8>> {
        check_branch(&self.branch).map_err(|error| error.within(self.shown()))?;
        let encoded = serde_json::to_vec(value)
            .map_err(|error| Error::invalid(format!("a message that does not write as JSON: {error}")))
            .map_err(|error| error.within(self.shown()))?;
        if encoded.len() > MOST_MESSAGE {
            let long = format!("a message of {} bytes, past {MOST_MESSAGE}", encoded.len());
            return Err(Error::invalid(long).within(self.shown()));
        }
        Ok(encoded)
    }
}

impl<T> Channel<T> {
    fn shown(&self) -> String {
        shown(&self.name, &self.branch)
    }
}

/// A subscription being made: whether it takes the branches below, and what
/// it is told when it misses messages or a call fails.
#[must_use = "a subscription starts with subscribe() or each()"]
pub struct Subscribing<T> {
    channel: Channel<T>,
    recursive: bool,
    on_missed: Option<Box<dyn FnMut(Missed) + Send>>,
    on_error: Option<Box<dyn FnMut(Error) + Send>>,
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Subscribing<T> {
    pub fn recursive(mut self) -> Self {
        self.recursive = true;
        self
    }

    pub fn on_missed(mut self, missed: impl FnMut(Missed) + Send + 'static) -> Self {
        self.on_missed = Some(Box::new(missed));
        self
    }

    pub fn on_error(mut self, failed: impl FnMut(Error) + Send + 'static) -> Self {
        self.on_error = Some(Box::new(failed));
        self
    }

    pub fn subscribe(self, mut handler: impl FnMut(Message<T>) + Send + 'static) -> Result<Subscription> {
        let key = self.key()?;
        let shown = shown(&key.channel, &key.branch);
        let mut told = Told::new(shown.clone(), self.on_missed, self.on_error);
        let run: Run = Box::new(move |delivery| match delivery {
            Delivery::Missed(count) => told.missed(Missed { count }),
            Delivery::Message(released) => match decoded::<T>(&shown, &released) {
                Ok(message) => {
                    if catch_unwind(AssertUnwindSafe(|| handler(message))).is_err() {
                        told.failed(Error::internal("the handler panicked").within(&shown));
                    }
                }
                Err(error) => told.failed(error),
            },
        });
        let hub = &self.channel.hub;
        let subscriber = hub.deliverer.subscribe(hub, key, run).map_err(|error| error.within(self.channel.shown()))?;
        Ok(Subscription { subscriber })
    }

    /// The messages as an iterator on the caller's thread. A message that
    /// does not read is an `Err` item there, so `on_error` has nothing to say.
    pub fn each(self) -> Result<Each<T>> {
        let key = self.key()?;
        let signal = Arc::new(Signal::default());
        let woken = Arc::clone(&signal);
        let (log, reader) = self.channel.hub.join(&key, Arc::new(move || woken.wake()))?;
        let told = Told::new(shown(&key.channel, &key.branch), self.on_missed, None);
        let hub = Arc::clone(&self.channel.hub);
        Ok(Each { hub, key, log, reader, signal, taken: VecDeque::new(), told, _value: PhantomData })
    }

    fn key(&self) -> Result<LogKey> {
        check_branch(&self.channel.branch).map_err(|error| error.within(self.channel.shown()))?;
        let (channel, branch) = (Arc::clone(&self.channel.name), self.channel.branch.clone());
        Ok(LogKey { channel, branch, recursive: self.recursive })
    }
}

/// A subscription whose handler runs on the store's threads. Dropped, it goes
/// on listening, as a queue's worker goes on working: `stop` ends it, and so
/// does the store's close.
pub struct Subscription {
    subscriber: Arc<Subscriber>,
}

impl Subscription {
    /// No call starts after it, and it returns once the running call ended;
    /// from inside the subscription's own handler, at once.
    pub fn stop(&self) {
        self.subscriber.stop();
    }
}

/// The messages published from now on, as an iterator on the caller's
/// thread: `next` waits for the next, and answers `None` once stopped or
/// once the store closed. A message that does not read as the channel's type
/// is an `Err` item, and the next follows.
pub struct Each<T> {
    hub: Arc<Hub>,
    key: LogKey,
    log: Arc<Log>,
    reader: u64,
    signal: Arc<Signal>,
    taken: VecDeque<Delivery>,
    told: Told,
    _value: PhantomData<fn() -> T>,
}

impl<T> Each<T> {
    /// What stops the loop from another thread: its `next` answers `None`.
    pub fn stopper(&self) -> Stopper {
        Stopper(Arc::clone(&self.signal))
    }
}

impl<T: DeserializeOwned> Iterator for Each<T> {
    type Item = Result<Message<T>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.signal.stopped() {
                self.taken.clear();
                return None;
            }
            match self.taken.pop_front() {
                Some(Delivery::Missed(count)) => self.told.missed(Missed { count }),
                Some(Delivery::Message(released)) => return Some(decoded(&self.told.shown, &released)),
                None if self.hub.closed() => return None,
                None => self.take(),
            }
        }
    }
}

impl<T> Each<T> {
    /// Takes what the log holds, or waits until it holds something.
    fn take(&mut self) {
        let taken = self.log.take(self.reader, BATCH);
        if taken.missed == 0 && taken.messages.is_empty() {
            self.signal.wait();
            return;
        }
        if taken.missed > 0 {
            self.taken.push_back(Delivery::Missed(taken.missed));
        }
        self.taken.extend(taken.messages.into_iter().map(Delivery::Message));
    }
}

impl<T> Drop for Each<T> {
    fn drop(&mut self) {
        self.hub.leave(&self.key, self.reader);
    }
}

/// Stops an [`Each`] from another thread.
#[derive(Clone)]
pub struct Stopper(Arc<Signal>);

impl Stopper {
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::Release);
        self.0.wake();
    }
}

/// What an `each` sleeps on until its log holds something for it.
#[derive(Default)]
struct Signal {
    woken: Mutex<bool>,
    changed: Condvar,
    stopped: AtomicBool,
}

impl Signal {
    fn wake(&self) {
        *self.woken.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let woken = self.woken.lock().unwrap_or_else(PoisonError::into_inner);
        let mut woken =
            self.changed.wait_while(woken, |woken| !*woken && !self.stopped()).unwrap_or_else(PoisonError::into_inner);
        *woken = false;
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

/// What a subscription tells of what it missed and what failed: its own
/// handlers, or the store's log, once a minute at most.
struct Told {
    shown: String,
    on_missed: Option<Box<dyn FnMut(Missed) + Send>>,
    on_error: Option<Box<dyn FnMut(Error) + Send>>,
    said: Option<Instant>,
}

impl Told {
    fn new(
        shown: String,
        on_missed: Option<Box<dyn FnMut(Missed) + Send>>,
        on_error: Option<Box<dyn FnMut(Error) + Send>>,
    ) -> Told {
        Told { shown, on_missed, on_error, said: None }
    }

    fn missed(&mut self, missed: Missed) {
        if let Some(told) = &mut self.on_missed {
            return told(missed);
        }
        if !self.said_recently() {
            tracing::warn!(
                target: "tinystore",
                channel = %self.shown,
                count = missed.count,
                "a subscriber missed messages",
            );
        }
    }

    fn failed(&mut self, error: Error) {
        if let Some(told) = &mut self.on_error {
            return told(error);
        }
        if !self.said_recently() {
            tracing::warn!(target: "tinystore", channel = %self.shown, %error, "a subscriber's call failed");
        }
    }

    /// Whether the log said something less than a minute ago; if not, it says
    /// something now.
    fn said_recently(&mut self) -> bool {
        let now = Instant::now();
        if self.said.is_some_and(|said| now.duration_since(said) < QUIET) {
            return true;
        }
        self.said = Some(now);
        false
    }
}

fn decoded<T: DeserializeOwned>(shown: &str, released: &Released) -> Result<Message<T>> {
    let value = serde_json::from_slice(&released.value)
        .map_err(|error| Error::corrupt(format!("a message that does not read as the channel's type: {error}")))
        .map_err(|error| error.within(shown))?;
    Ok(Message { value, branch: released.branch.clone() })
}

/// A channel's branch named by a client, checked as a handle's is.
#[cfg(feature = "wire")]
pub(crate) fn remote_branch(name: &str, under: Vec<String>) -> Result<(Arc<str>, Vec<String>)> {
    check_name(name)?;
    check_branch(&under).map_err(|error| error.within(shown(name, &under)))?;
    Ok((Arc::from(name), under))
}

fn check_name(name: &str) -> Result<()> {
    crate::text::check_name("a channel's", name)
}

fn check_branch(branch: &[String]) -> Result<()> {
    let bytes: usize = branch.iter().map(String::len).sum();
    if bytes > MOST_BRANCH {
        return Err(Error::invalid(format!("a branch of {bytes} bytes, past {MOST_BRANCH}")));
    }
    Ok(())
}

/// A channel's branch as an error names it: `channel chat: under "room-7"`.
fn shown(name: &str, branch: &[String]) -> String {
    if branch.is_empty() {
        return format!("channel {name}");
    }
    let under: Vec<String> = branch.iter().map(|owner| format!("{owner:?}")).collect();
    format!("channel {name}: under {}", under.join(" "))
}

impl<T> Clone for Channel<T> {
    fn clone(&self) -> Self {
        Channel {
            hub: Arc::clone(&self.hub),
            name: Arc::clone(&self.name),
            branch: self.branch.clone(),
            _value: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Channel<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Channel({})", shown(&self.name, &self.branch))
    }
}

impl<T> fmt::Debug for ChannelBuilder<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChannelBuilder({})", self.name)
    }
}

impl<T> fmt::Debug for Subscribing<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Subscribing({}, recursive: {})", self.channel.shown(), self.recursive)
    }
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Subscription")
    }
}

impl<T> fmt::Debug for Each<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Each({})", self.told.shown)
    }
}

impl fmt::Debug for Stopper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Stopper")
    }
}

/// A channel inside a transaction: its messages wait for the commit, and a
/// rollback hands none over.
#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
pub struct TxChannel<'t, T> {
    channel: Channel<T>,
    tx: &'t crate::Transaction<'t>,
}

#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
impl<'t, T: Serialize + DeserializeOwned + Send + 'static> TxChannel<'t, T> {
    /// The branch of `owner` under this one, in the same transaction.
    pub fn under(&self, owner: impl Key) -> TxChannel<'t, T> {
        TxChannel { channel: self.channel.under(owner), tx: self.tx }
    }

    /// Hands `value` over once the transaction commits, in the order its file
    /// commits; nothing when it rolls back. A store that dies between the
    /// commit and the handing over loses it: an event that must go out with
    /// its rows is a queue in the same database.
    pub fn publish(&self, value: &T) -> Result<()> {
        let encoded = self.channel.encoded(value)?;
        let (hub, name, branch) =
            (Arc::clone(&self.channel.hub), Arc::clone(&self.channel.name), self.channel.branch.clone());
        self.tx.after_commit(Box::new(move || {
            if let Err(error) = hub.publish(&name, branch, encoded) {
                tracing::warn!(target: "tinystore", channel = %name, %error, "a committed message was not handed over");
            }
        }));
        Ok(())
    }
}

#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
impl<T> crate::transaction::sealed::Sealed for Channel<T> {}

#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
impl<T: Serialize + DeserializeOwned + Send + 'static> crate::TxHandle for Channel<T> {
    type InTx<'t>
        = TxChannel<'t, T>
    where
        Self: 't;

    fn in_tx<'t>(&'t self, tx: &'t crate::Transaction<'t>) -> TxChannel<'t, T> {
        TxChannel { channel: self.clone(), tx }
    }
}

#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
impl<T> fmt::Debug for TxChannel<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TxChannel({})", self.channel.shown())
    }
}

#[cfg(test)]
#[path = "channels_tests.rs"]
mod tests;
