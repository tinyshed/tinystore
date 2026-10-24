use std::fmt;
use std::time::{Duration, SystemTime};

use super::bucket::{Bucket, Put, Version, Work, WriteOptions};
#[cfg(feature = "replication")]
use super::forward;
use super::tx;
use super::value::Value;
use crate::{Error, Result, Transaction};

/// A call on one key with what it asks beside its value, run by its last step:
/// `sessions.key(&token).if_version(seen).set(&next)`.
#[must_use = "a key's call runs at its last step: set, create, take, delete or expire"]
pub struct KeyCall<'a, V> {
    bucket: &'a Bucket<V>,
    tx: Option<&'a Transaction<'a>>,
    key: String,
    options: WriteOptions,
}

impl<'a, V: Value> KeyCall<'a, V> {
    pub(crate) fn new(bucket: &'a Bucket<V>, tx: Option<&'a Transaction<'a>>, key: String) -> Self {
        KeyCall { bucket, tx, key, options: WriteOptions::default() }
    }

    /// The key expires `ttl` from now, for `set`, `create` and `expire`.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.options = self.options.ttl(ttl);
        self
    }

    /// The key expires at `time`, for `set`, `create` and `expire`.
    pub fn expires_at(mut self, time: SystemTime) -> Self {
        self.options = self.options.expires_at(time);
        self
    }

    /// The call applies only to a live key still at `version`: a key written,
    /// expired or deleted since is a `Conflict`.
    pub fn if_version(mut self, version: Version) -> Self {
        self.options = self.options.if_version(version);
        self
    }

    pub fn set(self, value: &V) -> Result<()> {
        self.put(value, Put::Always).map(|_| ())
    }

    /// Writes only when the key is not there, and says whether it did.
    pub fn create(self, value: &V) -> Result<bool> {
        self.put(value, Put::OnlyNew)
    }

    pub fn take(self) -> Result<Option<V>> {
        self.refuse_expiry("take")?;
        #[cfg(feature = "replication")]
        if self.tx.is_none() && self.bucket.is_copy() {
            let cell = self.bucket.take_at_home(&self.key, self.options)?;
            return cell.map(|cell| super::bucket::decode::<V>(&cell.raw)).transpose();
        }
        let work = self.bucket.take_work(&self.key, self.options)?;
        self.run(0, work)
    }

    /// Removes the key and says whether a live key was there.
    pub fn delete(self) -> Result<bool> {
        self.refuse_expiry("delete")?;
        #[cfg(feature = "replication")]
        if self.tx.is_none() && self.bucket.is_copy() {
            return forward::found(&self.bucket.pass_on(&self.key, forward::Call::Remove(self.options))?);
        }
        let work = self.bucket.remove_work(&self.key, self.options)?;
        self.run(0, work)
    }

    /// Gives a live key the expiry `ttl` or `expires_at` named, keeping its
    /// value and version, and says whether it was there.
    pub fn expire(self) -> Result<bool> {
        #[cfg(feature = "replication")]
        if self.tx.is_none() && self.bucket.is_copy() {
            return forward::found(&self.bucket.pass_on(&self.key, forward::Call::Expire(self.options))?);
        }
        let work = self.bucket.expire_work(&self.key, self.options)?;
        self.run(0, work)
    }

    fn put(self, value: &V, put: Put) -> Result<bool> {
        let raw = self.bucket.encode(&self.key, value)?;
        #[cfg(feature = "replication")]
        if self.tx.is_none() && self.bucket.is_copy() {
            let call = forward::Call::Put { raw, put, options: self.options };
            return Ok(forward::stamp(&self.bucket.pass_on(&self.key, call)?)?.written);
        }
        let (bytes, work) = self.bucket.put_work(&self.key, raw, self.options, put)?;
        Ok(self.run(bytes, work)?.written)
    }

    /// Runs the call in the transaction it was made in, or in a commit it
    /// may share.
    fn run<T: Send + 'static>(&self, bytes: usize, work: impl Work<T>) -> Result<T> {
        match self.tx {
            Some(within) => tx::call_on_key(within, &self.bucket.scope, &self.key, work),
            None => self.bucket.scope.write(&self.key, bytes, work),
        }
    }

    /// The bucket, the key and the options of a call the async API runs, by
    /// its own last step.
    #[cfg(feature = "async")]
    pub(crate) fn into_parts(self) -> (&'a Bucket<V>, String, WriteOptions) {
        (self.bucket, self.key, self.options)
    }

    /// Refuses a ttl or a time on a call that writes no value, a take or a
    /// delete, before anything is queued.
    pub(crate) fn refuse_expiry(&self, verb: &str) -> Result<()> {
        if self.options.expires.is_none() {
            return Ok(());
        }
        let refused = Error::invalid(format!("a ttl or a time on a {verb}, which writes no value"));
        Err(refused.within(self.bucket.scope.shown_key(&self.key)))
    }
}

impl<V> fmt::Debug for KeyCall<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyCall({}, {:?})", self.bucket.scope.shown_key(&self.key), self.options)
    }
}
