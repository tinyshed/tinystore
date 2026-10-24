//! The async API's handles of the Channels engine, beside the blocking ones
//! of [`crate::channels`]: a loop over a channel's messages is a [`Stream`].
//!
//! ```no_run
//! # use serde::{Deserialize, Serialize};
//! # #[derive(Serialize, Deserialize)]
//! # struct ChatEvent { kind: String, id: u64 }
//! # mod futures {
//! #     pub trait StreamExt: futures_core::Stream + Unpin + Sized {
//! #         async fn next(&mut self) -> Option<Self::Item> {
//! #             std::future::poll_fn(|cx| std::pin::Pin::new(&mut *self).poll_next(cx)).await
//! #         }
//! #     }
//! #     impl<S: futures_core::Stream + Unpin> StreamExt for S {}
//! # }
//! # async fn example() -> tinystore::Result<()> {
//! use futures::StreamExt; // next() on a Stream
//! use tinystore::Options;
//! use tinystore::aio::Store;
//!
//! let store = Store::open("data", Options::default())?;
//! let events = store.channel::<ChatEvent>("chat-events").open()?;
//!
//! let mut each = events.under("chat-7").each()?; // listens from here
//! events.under("chat-7").publish(&ChatEvent { kind: "message".into(), id: 41 })?; // memory: not awaited
//! while let Some(m) = each.next().await {
//!     println!("{}", m?.value.kind); // the loop ends once stopped, or once the store closed
//! }
//! # Ok(())
//! # }
//! ```
//!
//! | A call              | Runs                                                         | Dropped                                           |
//! |---------------------|--------------------------------------------------------------|---------------------------------------------------|
//! | `publish`           | a plain call: the message put in memory, its listeners woken | —                                                 |
//! | `each`, a `Stream`  | listens from the call; woken by a publish, on no thread      | it stops listening, and what it had not read goes |
//! | `tx.with(&channel)` | inside an awaited transaction's closure, on the store's pool | as the transaction: once polled, it runs          |
//!
//! A poll takes up to 64 messages from the branch's log and decodes them on
//! the polling thread, one an item. A handler on the store's threads is the
//! blocking handle's, [`Channel::blocking`]: in the async API a loop is a
//! task the program spawns over [`Channel::each`].

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::channels::{Message, Missed, Stopper};
use crate::{Key, Result};

/// A channel being opened, as [`crate::channels::ChannelBuilder`].
#[must_use = "a channel opens with open()"]
pub struct ChannelBuilder<T> {
    builder: crate::channels::ChannelBuilder<T>,
}

impl super::Store {
    /// A channel of messages of one type, as [`crate::Store::channel`]: every
    /// listener when a message is published receives it, and nobody after.
    pub fn channel<T>(&self, name: &str) -> ChannelBuilder<T> {
        ChannelBuilder { builder: self.blocking().channel(name) }
    }
}

impl<T: Serialize + DeserializeOwned + Send + 'static> ChannelBuilder<T> {
    /// Opens the channel: a plain call, in memory.
    pub fn open(self) -> Result<Channel<T>> {
        Ok(Channel { channel: self.builder.open()? })
    }
}

/// A channel, or one of its branches, as [`crate::channels::Channel`].
pub struct Channel<T> {
    channel: crate::channels::Channel<T>,
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Channel<T> {
    /// The branch of `owner` under this one: whose messages these are.
    pub fn under(&self, owner: impl Key) -> Channel<T> {
        Channel { channel: self.channel.under(owner) }
    }

    /// Hands `value` to every listener of this branch, and of the branches
    /// above it listened to `recursive`, as
    /// [`crate::channels::Channel::publish`]. A plain call: the message is
    /// put in memory, the oldest shed rather than waited for, and nothing is
    /// awaited.
    pub fn publish(&self, value: &T) -> Result<()> {
        self.channel.publish(value)
    }

    /// A loop that takes every branch below this one too.
    pub fn recursive(&self) -> Subscribing<T> {
        Subscribing { subscribing: self.channel.recursive() }
    }

    /// A loop told what it missed, before its next message: by default the
    /// store's log says it, once a minute at most. `missed` runs on the
    /// thread that polls the stream.
    pub fn on_missed(&self, missed: impl FnMut(Missed) + Send + 'static) -> Subscribing<T> {
        Subscribing { subscribing: self.channel.on_missed(missed) }
    }

    /// The messages published from now on, as a [`Stream`]: it listens from
    /// this call, polled yet or not, so a program that must hear its own
    /// message makes it first. A message that does not read as the
    /// channel's type is an `Err` item, and the next follows; the stream ends
    /// once its stopper stops it or once the store closed.
    pub fn each(&self) -> Result<Each<T>> {
        Ok(Each { each: self.channel.each()? })
    }

    /// The same channel, for its blocking calls, `subscribe` among them.
    pub fn blocking(&self) -> &crate::channels::Channel<T> {
        &self.channel
    }
}

impl<T> From<crate::channels::Channel<T>> for Channel<T> {
    /// The same channel, its loops a `Stream`: a clone of its handle.
    fn from(channel: crate::channels::Channel<T>) -> Channel<T> {
        Channel { channel }
    }
}

/// A loop being made: whether it takes the branches below, and what it is
/// told when it misses messages.
#[must_use = "a loop starts with each()"]
pub struct Subscribing<T> {
    subscribing: crate::channels::Subscribing<T>,
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Subscribing<T> {
    pub fn recursive(self) -> Self {
        Subscribing { subscribing: self.subscribing.recursive() }
    }

    pub fn on_missed(self, missed: impl FnMut(Missed) + Send + 'static) -> Self {
        Subscribing { subscribing: self.subscribing.on_missed(missed) }
    }

    /// As [`Channel::each`].
    pub fn each(self) -> Result<Each<T>> {
        Ok(Each { each: self.subscribing.each()? })
    }
}

/// The messages published from now on, as a [`Stream`]: woken by each
/// publish, it takes up to 64 messages a poll from its log, which keeps
/// what it has not read, 1,024 messages or 4 MiB, and tells `on_missed`
/// what it lost past them. It parks no thread. Dropped, it stops listening.
pub struct Each<T> {
    each: crate::channels::Each<T>,
}

impl<T> Each<T> {
    /// What stops the stream from another task or thread: it ends, `None`.
    pub fn stopper(&self) -> Stopper {
        self.each.stopper()
    }
}

impl<T: DeserializeOwned> Stream for Each<T> {
    type Item = Result<Message<T>>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().each.poll_next(context.waker())
    }
}

/// A channel joins an awaited transaction as a blocking one does: inside the
/// transaction's closure, `tx.with(&channel)` publishes once it commits.
#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
impl<T> crate::transaction::sealed::Sealed for Channel<T> {}

#[cfg(any(feature = "kv", feature = "jobs", feature = "sql"))]
impl<T: Serialize + DeserializeOwned + Send + 'static> crate::TxHandle for Channel<T> {
    type InTx<'t>
        = crate::channels::TxChannel<'t, T>
    where
        Self: 't;

    fn in_tx<'t>(&'t self, tx: &'t crate::Transaction<'t>) -> Self::InTx<'t> {
        crate::TxHandle::in_tx(&self.channel, tx)
    }
}

impl<T> Clone for Channel<T> {
    fn clone(&self) -> Self {
        Channel { channel: self.channel.clone() }
    }
}

impl<T> fmt::Debug for ChannelBuilder<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

impl<T> fmt::Debug for Channel<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.channel, f)
    }
}

impl<T> fmt::Debug for Subscribing<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.subscribing, f)
    }
}

impl<T> fmt::Debug for Each<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.each, f)
    }
}

#[cfg(test)]
#[path = "channels_tests.rs"]
mod tests;
