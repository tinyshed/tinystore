//! What every kind of kv handle shares: the engine, the row of its name in
//! the engine's file, what the name holds, and the branch the handle sees.

use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use rusqlite::Connection;

use super::engine::Kv;
use super::path::{Branch, Key};
use crate::engine::Home;
use crate::sqlite::Tx;
use crate::{Error, Result};

/// What a name in kv's file holds. A name keeps its kind: a name that holds
/// counters never opens as a bucket of values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Values,
    /// Values sealed with the store's encryption key.
    Encrypted,
    Counters,
    RateLimit,
    Quota,
    Once,
}

impl Kind {
    /// The kind as the name's row keeps it.
    pub(crate) fn role(self) -> &'static str {
        match self {
            Kind::Values => "values",
            Kind::Encrypted => "encrypted values",
            Kind::Counters => "counters",
            Kind::RateLimit => "rate limit",
            Kind::Quota => "quota",
            Kind::Once => "once",
        }
    }

    /// What an error calls a handle of this kind: `kv counters hits`.
    fn shown(self) -> &'static str {
        match self {
            Kind::Values | Kind::Encrypted => "bucket",
            Kind::Counters => "counters",
            Kind::RateLimit => "rate limit",
            Kind::Quota => "quota",
            Kind::Once => "once",
        }
    }
}

/// A handle's place in its engine's file, cheap to clone.
#[derive(Clone)]
pub(crate) struct Scope {
    pub(crate) kv: Arc<Kv>,
    pub(crate) id: i64,
    name: Arc<str>,
    kind: Kind,
    /// An invalid owner given to `of` fails the branch's calls, not `of`.
    branch: std::result::Result<Branch, String>,
}

impl Scope {
    /// Opens `name` as `kind` where `home` says, making its row the first time.
    pub(crate) fn open(home: &Home, name: &str, kind: Kind) -> Result<Scope> {
        let lent_by = match home {
            Home::Store(_) => None,
            Home::Shared(shared) => Some(shared.owner()),
        };
        let shown = || described(lent_by, kind, name);
        check_name(name).map_err(|error| error.within(shown()))?;
        let kv = Kv::at(home).map_err(|error| error.within(shown()))?;
        let id = kv.name(name, kind).map_err(|error| error.within(shown()))?;
        Ok(Scope { kv, id, name: name.into(), kind, branch: Ok(Branch::default()) })
    }

    /// The same name seen from the branch `owner` names under this one.
    pub(crate) fn under(&self, owner: impl Key) -> Scope {
        let branch = match &self.branch {
            Ok(branch) => branch.under(&owner.text()).map_err(|error| error.to_string()),
            Err(invalid) => Err(invalid.clone()),
        };
        Scope { branch, ..self.clone() }
    }

    pub(crate) fn branch(&self) -> Result<&Branch> {
        self.branch.as_ref().map_err(|invalid| Error::invalid(invalid.clone()).within(self.shown()))
    }

    pub(crate) fn path(&self, key: &str) -> Result<Vec<u8>> {
        self.branch()?.path(key).map_err(|error| error.within(self.shown()))
    }

    /// The store's time, in unix milliseconds.
    pub(crate) fn now(&self) -> i64 {
        self.kv.now()
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The handle as an error names it: `kv counters hits`.
    pub(crate) fn shown(&self) -> String {
        described(self.kv.lent_by(), self.kind, &self.name)
    }

    pub(crate) fn shown_key(&self, key: &str) -> String {
        match &self.branch {
            Ok(branch) => format!("{}: {}", self.shown(), branch.shown_key(key)),
            Err(_) => self.shown(),
        }
    }

    pub(crate) fn shown_branch(&self) -> String {
        match &self.branch {
            Ok(branch) => format!("{}: {}", self.shown(), branch.shown()),
            Err(_) => self.shown(),
        }
    }

    /// Runs a write on the file's writer in a commit it may share, naming the
    /// key in its errors.
    pub(crate) fn write<T: Send + 'static>(
        &self,
        key: &str,
        bytes: usize,
        work: impl FnOnce(&Tx<'_>, &AtomicI64) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let revision = self.kv.revision();
        self.kv.file().write(bytes, move |tx| work(tx, &revision)).map_err(|error| error.within(self.shown_key(key)))
    }

    /// Queues a write on the file's writer and returns; `done` gets its answer
    /// on the thread that commits it.
    pub(crate) fn submit<T: Send + 'static>(
        &self,
        key: &str,
        bytes: usize,
        work: impl FnOnce(&Tx<'_>, &AtomicI64) -> Result<T> + Send + 'static,
        done: impl FnOnce(Result<T>) + Send + 'static,
    ) {
        let revision = self.kv.revision();
        let shown = self.shown_key(key);
        let answered = move |answer: Result<T>| done(answer.map_err(|error| error.within(shown)));
        self.kv.file().submit(bytes, move |tx| work(tx, &revision), answered);
    }

    /// Runs a read on a reader, in one snapshot, naming the key in its errors.
    pub(crate) fn read<T>(&self, key: &str, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.kv.file().read(read).map_err(|error| error.within(self.shown_key(key)))
    }
}

impl std::fmt::Debug for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.shown())
    }
}

/// A handle as an error names it: `kv counters hits`, or `sql app: kv bucket
/// sessions` for one kept in a database's file.
fn described(lent_by: Option<&str>, kind: Kind, name: &str) -> String {
    match lent_by {
        Some(owner) => format!("{owner}: kv {} {name}", kind.shown()),
        None => format!("kv {} {name}", kind.shown()),
    }
}

fn check_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    let first_fits = bytes.first().is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let rest_fits =
        bytes.iter().all(|&byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-');
    if first_fits && rest_fits && bytes.len() <= 64 {
        return Ok(());
    }
    Err(Error::invalid("a name is a lowercase letter or digit, then up to 63 of them, '_' and '-'"))
}
