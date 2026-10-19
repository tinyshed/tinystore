# Channels API for Rust

Every public type, trait and function of the Channels engine in `tinystore::channels`, generated from its source. The [Channels guide](../../start/why.md#the-engines) explains how to use them, and the [Bun and Node](../bun/channels.md), [Python](../python/channels.md) and [Go](../go/channels.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.channel

```rust
pub fn channel<T>(&self, name: &str) -> ChannelBuilder<T>
```

A channel of messages of one type: every subscriber listening when a
message is published receives it, and nobody after.

## ChannelBuilder

```rust
pub struct ChannelBuilder<T> { /* private fields */ }
```

A channel being opened.

### ChannelBuilder.open

```rust
pub fn open(self) -> Result<Channel<T>>
```

## Channel

```rust
pub struct Channel<T> { /* private fields */ }
```

A channel, or one of its branches.

### Channel.under

```rust
pub fn under(&self, owner: impl Key) -> Channel<T>
```

The branch of `owner` under this one: whose messages these are.

### Channel.publish

```rust
pub fn publish(&self, value: &T) -> Result<()>
```

Hands `value` to every subscriber of this branch, and of the branches
above it listened to `recursive`, that listens now. It answers once
the store has the message, not once anyone handled it.

### Channel.recursive

```rust
pub fn recursive(&self) -> Subscribing<T>
```

A subscription that takes every branch below this one too.

### Channel.on_missed

```rust
pub fn on_missed(&self, missed: impl FnMut(Missed) + Send + 'static) -> Subscribing<T>
```

A subscription told what it missed: by default the store's log says
it, once a minute at most.

### Channel.on_error

```rust
pub fn on_error(&self, failed: impl FnMut(Error) + Send + 'static) -> Subscribing<T>
```

A subscription told what failed: a handler that panicked, a message
that does not read as the channel's type.

### Channel.subscribe

```rust
pub fn subscribe(&self, handler: impl FnMut(Message<T>) + Send + 'static) -> Result<Subscription>
```

Calls `handler` with each message published from now on, one at a
time and in order, on a thread of the store's.

### Channel.each

```rust
pub fn each(&self) -> Result<Each<T>>
```

The messages published from now on, on the caller's thread.

## Message

```rust
pub struct Message<T> {
    pub value: T,
    pub branch: Vec<String>,
}
```

A message as a subscriber receives it: what was published, and the branch
it was published under.

## Missed

```rust
pub struct Missed {
    pub count: u64,
}
```

Messages a subscriber lost: it fell so far behind that its branch's log
let them go.

## Subscribing

```rust
pub struct Subscribing<T> { /* private fields */ }
```

A subscription being made: whether it takes the branches below, and what
it is told when it misses messages or a call fails.

### Subscribing.recursive

```rust
pub fn recursive(self) -> Self
```

### Subscribing.on_missed

```rust
pub fn on_missed(self, missed: impl FnMut(Missed) + Send + 'static) -> Self
```

### Subscribing.on_error

```rust
pub fn on_error(self, failed: impl FnMut(Error) + Send + 'static) -> Self
```

### Subscribing.subscribe

```rust
pub fn subscribe(self, handler: impl FnMut(Message<T>) + Send + 'static) -> Result<Subscription>
```

### Subscribing.each

```rust
pub fn each(self) -> Result<Each<T>>
```

The messages as an iterator on the caller's thread. A message that
does not read is an `Err` item there, so `on_error` has nothing to say.

## Subscription

```rust
pub struct Subscription { /* private fields */ }
```

A subscription whose handler runs on the store's threads. Dropped, it goes
on listening, as a queue's worker goes on working: `stop` ends it, and so
does the store's close.

### Subscription.stop

```rust
pub fn stop(&self)
```

No call starts after it, and it returns once the running call ended;
from inside the subscription's own handler, at once.

## Each

```rust
pub struct Each<T> { /* private fields */ }
```

The messages published from now on, as an iterator on the caller's
thread: `next` waits for the next, and answers `None` once stopped or
once the store closed. A message that does not read as the channel's type
is an `Err` item, and the next follows.

### Each.stopper

```rust
pub fn stopper(&self) -> Stopper
```

What stops the loop from another thread: its `next` answers `None`.

## Stopper

```rust
pub struct Stopper(_)
```

Stops an `Each` from another thread.

### Stopper.stop

```rust
pub fn stop(&self)
```

## TxChannel

```rust
pub struct TxChannel<'t, T> { /* private fields */ }
```

A channel inside a transaction: its messages wait for the commit, and a
rollback hands none over.

### TxChannel.under

```rust
pub fn under(&self, owner: impl Key) -> TxChannel<'t, T>
```

The branch of `owner` under this one, in the same transaction.

### TxChannel.publish

```rust
pub fn publish(&self, value: &T) -> Result<()>
```

Hands `value` over once the transaction commits, in the order its file
commits; nothing when it rolls back. A store that dies between the
commit and the handing over loses it: an event that must go out with
its rows is a queue in the same database.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
