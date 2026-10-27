use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use super::named;
use crate::aio::{Store, run};
use crate::cancel::{self, Captured};
use crate::kv::{Entry, Key, PAGE_KEYS, Page, Put, Stamp, TxBucket, Value, Version, WriteOptions};
use crate::pool::Pool;
use crate::transaction::sealed::Sealed;
use crate::{Result, Transaction, TxHandle};

/// A bucket being opened, as [`crate::kv::BucketBuilder`] is: its name, its
/// type, how its keys expire, whether its values are encrypted.
#[must_use = "a bucket opens with open()"]
pub struct BucketBuilder<V> {
    builder: crate::kv::BucketBuilder<V>,
    pool: Arc<Pool>,
}

impl Store {
    /// A bucket of values of type `V` by key, in the store's kv.db, whose
    /// calls are awaited.
    pub fn bucket<V: Value>(&self, name: &str) -> BucketBuilder<V> {
        BucketBuilder::new(self.blocking().bucket(name), Arc::clone(self.pool()))
    }
}

impl<V: Value> BucketBuilder<V> {
    pub(crate) fn new(builder: crate::kv::BucketBuilder<V>, pool: Arc<Pool>) -> Self {
        BucketBuilder { builder, pool }
    }

    /// Keys expire this long after they are written, unless a write says
    /// otherwise.
    pub fn ttl(self, ttl: Duration) -> Self {
        BucketBuilder { builder: self.builder.ttl(ttl), ..self }
    }

    /// Keys expire this long after they were last read or written.
    pub fn idle(self, idle: Duration) -> Self {
        BucketBuilder { builder: self.builder.idle(idle), ..self }
    }

    /// Keeps every value sealed with the store's encryption key, as
    /// [`crate::kv::BucketBuilder::encrypted`] does.
    pub fn encrypted(self) -> Self {
        BucketBuilder { builder: self.builder.encrypted(), ..self }
    }

    /// Opens the bucket: a plain call, made once at start, since the engine's
    /// first handle may migrate kv.db.
    pub fn open(self) -> Result<Bucket<V>> {
        Ok(Bucket { bucket: self.builder.open()?, pool: self.pool })
    }
}

/// Values of one type by key, their calls awaited: the same bucket as
/// [`crate::kv::Bucket`], cheap to clone and to keep.
pub struct Bucket<V> {
    bucket: crate::kv::Bucket<V>,
    pool: Arc<Pool>,
}

impl<V: Value> Bucket<V> {
    /// Writes `value` under `key`, as [`crate::kv::Bucket::set`] does. The
    /// value is encoded when the call is made.
    pub fn set(&self, key: impl Key, value: &V) -> impl Future<Output = Result<()>> + Send + 'static {
        let written = self.put(key.text().into_owned(), value, Put::Always);
        async move { written.await.map(drop) }
    }

    /// Writes `value` only when `key` is not there, and says whether it did.
    pub fn create(&self, key: impl Key, value: &V) -> impl Future<Output = Result<bool>> + Send + 'static {
        let written = self.put(key.text().into_owned(), value, Put::OnlyNew);
        async move { written.await.map(|stamp| stamp.written) }
    }

    /// A call on `key` with what it asks beside its value, run by its last
    /// step: `sessions.key(&token).if_version(seen).set(&next).await`.
    pub fn key(&self, key: impl Key) -> KeyCall<'_, V> {
        KeyCall { call: self.bucket.key(key), pool: &self.pool }
    }

    /// The value under `key`. A key of an idle bucket read in its last
    /// minute is renewed before the read answers: that read runs on the
    /// store's pool, where it may wait for the writer.
    pub fn get(&self, key: impl Key) -> impl Future<Output = Result<Option<V>>> + Send + 'static {
        let (bucket, key) = (self.bucket.clone(), key.text().into_owned());
        let what = named(bucket.scope.shown_key(&key), "a get");
        run::point(&self.pool, what, move || bucket.get(&*key))
    }

    /// The value with its version and expiry, read as `get` reads it.
    pub fn entry(&self, key: impl Key) -> impl Future<Output = Result<Option<Entry<V>>>> + Send + 'static {
        let (bucket, key) = (self.bucket.clone(), key.text().into_owned());
        let what = named(bucket.scope.shown_key(&key), "an entry");
        run::point(&self.pool, what, move || bucket.entry(&*key))
    }

    /// Whether `key` is there, read as `get` reads it.
    pub fn has(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static {
        let (bucket, key) = (self.bucket.clone(), key.text().into_owned());
        let what = named(bucket.scope.shown_key(&key), "a has");
        run::point(&self.pool, what, move || bucket.has(&*key))
    }

    /// Reads `key` and removes it in one commit: of two callers taking the
    /// same key, one gets the value. A value that no longer reads as the
    /// bucket's type is `Corrupt`, and stays.
    pub fn take(&self, key: impl Key) -> impl Future<Output = Result<Option<V>>> + Send + 'static {
        take(&self.bucket, &self.pool, key.text().into_owned(), WriteOptions::default())
    }

    /// Removes `key` and says whether a live key was there.
    pub fn delete(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static {
        remove(&self.bucket, &self.pool, key.text().into_owned(), WriteOptions::default())
    }

    /// Gives a live key a new expiry, `ttl` from now, keeping its value and
    /// version; says whether the key was there.
    pub fn expire(&self, key: impl Key, ttl: Duration) -> impl Future<Output = Result<bool>> + Send + 'static {
        expire(&self.bucket, &self.pool, key.text().into_owned(), WriteOptions::default().ttl(ttl))
    }

    /// Removes every key of this branch and of the branches under it, at once
    /// for every reader, however many there are.
    pub fn clear(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        let bucket = self.bucket.clone();
        let what = named(bucket.scope.shown_branch(), "a clear");
        run::write(&self.pool, what, move |answer| bucket.clear_then(move |cleared| answer.send(cleared)))
    }

    /// Seals again with the store's sealing key what an older key sealed, as
    /// [`crate::kv::Bucket::reseal`] does, on the store's pool: its batches
    /// commit one after another, and once polled it runs to its end.
    pub fn reseal(&self) -> impl Future<Output = Result<u64>> + Send + 'static {
        let bucket = self.bucket.clone();
        let what = named(bucket.scope.shown_branch(), "a reseal");
        run::blocking(&self.pool, None, what, move || bucket.reseal())
    }

    /// One page of this branch's own keys, after `after` when it continues
    /// another, as [`crate::kv::Bucket::list`] reads it.
    pub fn list(&self, limit: usize, after: Option<&str>) -> impl Future<Output = Result<Page<V>>> + Send + 'static {
        let (bucket, after) = (self.bucket.clone(), after.map(str::to_owned));
        let what = named(bucket.scope.shown_branch(), "a page");
        run::read(&self.pool, what, move || bucket.list(limit, after.as_deref()))
    }

    /// Every key of this branch, a page at a time, as a `Stream`. No
    /// snapshot is held between pages, so a slow loop keeps no reader, and a
    /// key written meanwhile may or may not be met. Dropped, it stops the
    /// page under way and reads no more.
    pub fn all(&self) -> All<V> {
        All {
            bucket: self.clone(),
            tokens: cancel::captured(),
            page: Vec::new().into_iter(),
            next: None,
            done: false,
            reading: None,
        }
    }

    /// The same bucket seen from the branch `owner` names under this one:
    /// `sessions.under(user_id).clear().await` signs a user out everywhere.
    pub fn under(&self, owner: impl Key) -> Bucket<V> {
        Bucket { bucket: self.bucket.under(owner), pool: Arc::clone(&self.pool) }
    }

    /// The same bucket, for its blocking calls.
    pub fn blocking(&self) -> &crate::kv::Bucket<V> {
        &self.bucket
    }

    fn put(&self, key: String, value: &V, put: Put) -> impl Future<Output = Result<Stamp>> + Send + 'static {
        write(&self.bucket, &self.pool, key, value, (WriteOptions::default(), put))
    }
}

impl Bucket<()> {
    /// Adds `key` to a bucket of keys alone, a set, and says whether it was
    /// new: `seen.add(event_id).await` is false for an event already handled.
    pub fn add(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static {
        self.create(key, &())
    }
}

impl<V: Value> From<crate::kv::Bucket<V>> for Bucket<V> {
    /// The same bucket, its calls awaited.
    fn from(bucket: crate::kv::Bucket<V>) -> Bucket<V> {
        let pool = bucket.scope.pool();
        Bucket { bucket, pool }
    }
}

impl<V> Clone for Bucket<V> {
    fn clone(&self) -> Self {
        Bucket { bucket: self.bucket.clone(), pool: Arc::clone(&self.pool) }
    }
}

impl<V> fmt::Debug for Bucket<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.bucket, f)
    }
}

impl<V> fmt::Debug for BucketBuilder<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

impl<V> Sealed for Bucket<V> {}

/// A bucket whose calls are awaited joins a transaction as the blocking one
/// does: `tx.with(&stock)` inside `store.tx`'s closure gives its blocking
/// calls there.
impl<V: Value> TxHandle for Bucket<V> {
    type InTx<'h>
        = TxBucket<'h, V>
    where
        Self: 'h;

    fn in_tx<'h>(&'h self, tx: &'h Transaction<'h>) -> TxBucket<'h, V> {
        self.bucket.in_tx(tx)
    }
}

/// A call on one key with what it asks beside its value, run by its last
/// step, awaited: `sessions.key(&token).if_version(seen).set(&next).await`.
#[must_use = "a key's call runs at its last step: set, create, take, delete or expire"]
pub struct KeyCall<'a, V> {
    call: crate::kv::KeyCall<'a, V>,
    pool: &'a Arc<Pool>,
}

impl<V: Value> KeyCall<'_, V> {
    /// The key expires `ttl` from now, for `set`, `create` and `expire`.
    pub fn ttl(self, ttl: Duration) -> Self {
        KeyCall { call: self.call.ttl(ttl), ..self }
    }

    /// The key expires at `time`, for `set`, `create` and `expire`.
    pub fn expires_at(self, time: SystemTime) -> Self {
        KeyCall { call: self.call.expires_at(time), ..self }
    }

    /// The call applies only to a live key still at `version`: a key written,
    /// expired or deleted since is a `Conflict`.
    pub fn if_version(self, version: Version) -> Self {
        KeyCall { call: self.call.if_version(version), ..self }
    }

    pub fn set(self, value: &V) -> impl Future<Output = Result<()>> + Send + 'static {
        let written = self.put(value, Put::Always);
        async move { written.await.map(drop) }
    }

    /// Writes only when the key is not there, and says whether it did.
    pub fn create(self, value: &V) -> impl Future<Output = Result<bool>> + Send + 'static {
        let written = self.put(value, Put::OnlyNew);
        async move { written.await.map(|stamp| stamp.written) }
    }

    pub fn take(self) -> impl Future<Output = Result<Option<V>>> + Send + 'static {
        let refused = self.call.refuse_expiry("take");
        let (pool, (bucket, key, options)) = (self.pool, self.call.into_parts());
        let taken = refused.map(|()| take(bucket, pool, key, options));
        async move { taken?.await }
    }

    /// Removes the key and says whether a live key was there.
    pub fn delete(self) -> impl Future<Output = Result<bool>> + Send + 'static {
        let refused = self.call.refuse_expiry("delete");
        let (pool, (bucket, key, options)) = (self.pool, self.call.into_parts());
        let removed = refused.map(|()| remove(bucket, pool, key, options));
        async move { removed?.await }
    }

    /// Gives a live key the expiry `ttl` or `expires_at` named, keeping its
    /// value and version, and says whether it was there.
    pub fn expire(self) -> impl Future<Output = Result<bool>> + Send + 'static {
        let (bucket, key, options) = self.call.into_parts();
        expire(bucket, self.pool, key, options)
    }

    fn put(self, value: &V, put: Put) -> impl Future<Output = Result<Stamp>> + Send + 'static {
        let (bucket, key, options) = self.call.into_parts();
        write(bucket, self.pool, key, value, (options, put))
    }
}

impl<V> fmt::Debug for KeyCall<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.call, f)
    }
}

/// A write of `value` under `key`, encoded now, as `put` says.
fn write<V: Value>(
    bucket: &crate::kv::Bucket<V>,
    pool: &Arc<Pool>,
    key: String,
    value: &V,
    (options, put): (WriteOptions, Put),
) -> impl Future<Output = Result<Stamp>> + Send + 'static {
    let written = bucket.encode(&key, value).map(|raw| {
        let bucket = bucket.clone();
        let what = named(bucket.scope.shown_key(&key), "a write");
        run::write(pool, what, move |answer| {
            bucket.put_raw_then(&key, (raw, options, put), move |stamp| answer.send(stamp));
        })
    });
    async move { written?.await }
}

/// A take of `key` with `options`, typed: the value decoded inside its write.
fn take<V: Value>(
    bucket: &crate::kv::Bucket<V>,
    pool: &Arc<Pool>,
    key: String,
    options: WriteOptions,
) -> impl Future<Output = Result<Option<V>>> + Send + 'static {
    let bucket = bucket.clone();
    let what = named(bucket.scope.shown_key(&key), "a take");
    run::write(pool, what, move |answer| bucket.take_then(&key, options, move |taken| answer.send(taken)))
}

fn remove<V: Value>(
    bucket: &crate::kv::Bucket<V>,
    pool: &Arc<Pool>,
    key: String,
    options: WriteOptions,
) -> impl Future<Output = Result<bool>> + Send + 'static {
    let bucket = bucket.clone();
    let what = named(bucket.scope.shown_key(&key), "a delete");
    run::write(pool, what, move |answer| bucket.remove_then(&key, options, move |found| answer.send(found)))
}

fn expire<V: Value>(
    bucket: &crate::kv::Bucket<V>,
    pool: &Arc<Pool>,
    key: String,
    options: WriteOptions,
) -> impl Future<Output = Result<bool>> + Send + 'static {
    let bucket = bucket.clone();
    let what = named(bucket.scope.shown_key(&key), "an expire");
    run::write(pool, what, move |answer| bucket.expire_then(&key, options, move |found| answer.send(found)))
}

/// Every key of a branch, a page at a time: a `Stream` of entries, which
/// ends once the last page is read, or after the error that stopped it, a
/// store closed included.
pub struct All<V> {
    bucket: Bucket<V>,
    /// The tokens around the call that made the stream, which reach each
    /// page's read on the pool.
    tokens: Captured,
    page: std::vec::IntoIter<Entry<V>>,
    next: Option<String>,
    done: bool,
    /// The page read under way, which the stream's drop stops.
    reading: Option<PageRead<V>>,
}

type PageRead<V> = Pin<Box<dyn Future<Output = Result<Page<V>>> + Send>>;

// No field is pinned: the page under way is boxed.
impl<V> Unpin for All<V> {}

impl<V: Value> futures_core::Stream for All<V> {
    type Item = Result<Entry<V>>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let all = self.get_mut();
        loop {
            if let Some(entry) = all.page.next() {
                return Poll::Ready(Some(Ok(entry)));
            }
            if all.done {
                return Poll::Ready(None);
            }
            let (bucket, tokens, after) = (&all.bucket, &all.tokens, all.next.as_deref());
            let reading =
                all.reading.get_or_insert_with(|| Box::pin(tokens.run(None, || bucket.list(PAGE_KEYS, after))));
            let Poll::Ready(read) = reading.as_mut().poll(context) else {
                return Poll::Pending;
            };
            all.reading = None;
            match read {
                Ok(page) => {
                    all.done = page.next.is_none();
                    all.next = page.next;
                    all.page = page.entries.into_iter();
                }
                Err(error) => {
                    all.done = true;
                    return Poll::Ready(Some(Err(error)));
                }
            }
        }
    }
}

impl<V> fmt::Debug for All<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "All({:?})", self.bucket)
    }
}

#[cfg(test)]
#[path = "bucket_tests.rs"]
mod tests;
