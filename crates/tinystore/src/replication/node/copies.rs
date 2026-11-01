//! The copies a node holds of other nodes' files: taken over the link to
//! `from` or a direct one, waited for, kept whole, checked, and the writes
//! passed on toward their home.

use std::collections::HashSet;
use std::fs;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use super::{DIRECT, READY_WAIT, State};
use crate::replication::NODE_LOCAL;
use crate::replication::catalog::FILE as CATALOG;
use crate::replication::copy::{self, Copy};
use crate::replication::follow::{Direct, Written};
use crate::replication::log::segments_in;
use crate::replication::promote;
use crate::sync::lock;
use crate::{Error, ErrorKind, Result, Store};

/// Where a check of a copy stands.
pub(super) enum Verifying {
    Asked,
    Sent,
    Done(Result<u64>),
}

impl State {
    /// Takes `name` over a direct link to the node at `address`, made the
    /// first time and kept in the copy's log directory for the next start.
    pub(crate) fn take_directly(self: &Arc<Self>, store: &Store, name: &str, address: &str) -> Result<()> {
        let dir = self.log_dir(name);
        fs::create_dir_all(&dir).map_err(|error| Error::io(dir.display().to_string(), error))?;
        fs::write(dir.join(DIRECT), address).map_err(|error| Error::io(dir.display().to_string(), error))?;
        let direct = {
            let mut directs = lock(&self.direct);
            match directs.get(address) {
                Some(direct) => Arc::clone(direct),
                None => {
                    let direct = Arc::new(Direct::new(address));
                    directs.insert(address.to_owned(), Arc::clone(&direct));
                    crate::replication::follow::start_direct(self, store, Arc::clone(&direct));
                    direct
                }
            }
        };
        direct.add(name);
        Ok(())
    }

    /// Asks the link that takes `file` to check this node's copy of it.
    pub(crate) fn ask_to_verify(&self, file: &str) {
        lock(&self.verifying).insert(file.to_owned(), Verifying::Asked);
    }

    /// What checking the copy of `file` found, once it did, let go of.
    pub(crate) fn verified(&self, file: &str) -> Option<Result<u64>> {
        let mut verifying = lock(&self.verifying);
        match verifying.get(file) {
            Some(Verifying::Done(_)) => match verifying.remove(file) {
                Some(Verifying::Done(done)) => Some(done),
                _ => None,
            },
            _ => None,
        }
    }

    pub(crate) fn forget_verifying(&self, file: &str) {
        lock(&self.verifying).remove(file);
    }

    /// The files whose check a program asked for and no link sent yet, now
    /// marked sent.
    pub(crate) fn to_verify(&self, held: impl Fn(&str) -> bool) -> Vec<String> {
        let mut verifying = lock(&self.verifying);
        let asked: Vec<String> = verifying
            .iter()
            .filter(|(file, state)| matches!(state, Verifying::Asked) && held(file))
            .map(|(file, _)| file.clone())
            .collect();
        for file in &asked {
            verifying.insert(file.clone(), Verifying::Sent);
        }
        asked
    }

    /// Keeps what checking the copy of `file` found.
    pub(crate) fn verification(&self, file: &str, done: Result<u64>) {
        lock(&self.verifying).insert(file.to_owned(), Verifying::Done(done));
    }

    /// Has the link to `from` ask for the files served at its next turn.
    pub(crate) fn ask_for_what_is_served(&self) {
        self.wanted.store(true, Ordering::SeqCst);
    }

    /// Whether a direct link takes `name`, rather than the link to `from`.
    pub(crate) fn follows_directly(&self, name: &str) -> bool {
        lock(&self.direct).values().any(|direct| direct.follows(name))
    }

    /// Passes a write of `file` on toward its home: over the direct link that
    /// takes the file, or the link to `from`.
    pub(crate) fn write_toward(&self, file: &str, op: Vec<u8>, at_most: Duration) -> Result<Written> {
        crate::polled::may_wait(|| format!("{file}: a write passed on to its home"))?;
        let direct = lock(&self.direct).values().find(|direct| direct.follows(file)).cloned();
        match direct {
            Some(direct) => direct.upstream.write(op, at_most),
            None => self.upstream.write(op, at_most),
        }
    }

    /// Passes a write of `file` on toward its home as `write_toward` does,
    /// over a link up now, and hands its answer to `answered`.
    #[cfg(feature = "kv")]
    pub(crate) fn write_toward_then(
        &self,
        file: &str,
        op: Vec<u8>,
        answered: crate::replication::follow::Answered,
    ) -> Result<()> {
        let direct = lock(&self.direct).values().find(|direct| direct.follows(file)).cloned();
        match direct {
            Some(direct) => direct.upstream.write_then(op, answered),
            None => self.upstream.write_then(op, answered),
        }
    }

    /// The copies a direct link takes, each with the address it reaches, as
    /// their log directories keep them.
    pub(super) fn files_taken_directly(&self) -> Result<Vec<(String, String)>> {
        let mut taken = Vec::new();
        for name in self.files_logged()? {
            let dir = self.log_dir(&name);
            if let Ok(address) = fs::read_to_string(dir.join(DIRECT))
                && copy::is_copy(&dir)
            {
                taken.push((name, address.trim().to_owned()));
            }
        }
        Ok(taken)
    }

    /// Whether this node takes the file `name` from another rather than
    /// writing it: every file but its own records and metrics when it copies
    /// all, or those it names.
    pub(crate) fn takes(&self, name: &str) -> bool {
        if self.options.from.is_empty() {
            return false;
        }
        if name == CATALOG {
            return !self.options.catalog;
        }
        // a file whose log is this node's own, as a promoted copy's, it writes
        let dir = self.log_dir(name);
        if !copy::is_copy(&dir) && segments_in(&dir).is_ok_and(|segments| !segments.is_empty())
            || dir.join(promote::PROMOTED).exists()
        {
            return false;
        }
        match &self.options.files {
            Some(files) => files.iter().any(|file| file == name),
            None => !NODE_LOCAL.contains(&name),
        }
    }

    /// Waits until the copy of `name` is whole, `READY_WAIT` at most.
    pub(crate) fn wait_ready(&self, name: &str) -> Result<()> {
        let began = Instant::now();
        let deadline = began + READY_WAIT;
        let mut asked_to_make = false;
        let mut copies = lock(&self.copies);
        while !copies.contains_key(name) {
            // a file its home never made would never come: ask the home to make it
            if !asked_to_make && began.elapsed() > Duration::from_millis(500) {
                asked_to_make = true;
                drop(copies);
                if let Err(error) = crate::replication::forward::make(self, name) {
                    tracing::debug!(
                        target: "tinystore",
                        file = name,
                        %error,
                        "a node could not ask its home to make a file",
                    );
                }
                copies = lock(&self.copies);
                continue;
            }
            let now = Instant::now();
            if now >= deadline || self.stopping() {
                return Err(Error::new(
                    ErrorKind::Unavailable,
                    format!(
                        "{name}: no copy of it has come from {} yet: its home has not made it, or no link reaches it",
                        self.options.from.join(", ")
                    ),
                ));
            }
            self.wanted.store(true, Ordering::SeqCst);
            let turn = (deadline - now).min(Duration::from_millis(250));
            copies = self.copied.wait_timeout(copies, turn).unwrap_or_else(PoisonError::into_inner).0;
        }
        Ok(())
    }

    pub(crate) fn copy(&self, name: &str) -> Option<Arc<Copy>> {
        lock(&self.copies).get(name).cloned()
    }

    /// Keeps a copy that became whole, for its engines and the nodes that
    /// copy it.
    pub(crate) fn keep_copy(&self, name: &str, copy: Arc<Copy>) {
        lock(&self.copies).insert(name.to_owned(), copy);
        self.copied.notify_all();
        self.notify.served_changed();
        self.notify.appended();
    }

    pub(crate) fn drop_copy(&self, name: &str) {
        lock(&self.copies).remove(name);
    }

    /// Opens the copies whole on disk, so that their engines read them while
    /// no link reaches their home.
    pub(super) fn open_copies_held(&self, store: &Store) -> Result<()> {
        for name in self.files_logged()? {
            if copy::is_ready(&self.log_dir(&name), &self.dir.join(&name)) {
                match Copy::open(store, &name) {
                    Ok(copy) => self.keep_copy(&name, Arc::new(copy)),
                    Err(error) if error.kind() == ErrorKind::Corrupt => self.take_again(&name, &error)?,
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    /// Makes a copy whose position or log no longer reads one the link takes
    /// again from a snapshot, as it takes a first copy, rather than a store
    /// that does not open: a copy is only ever its home's file again.
    fn take_again(&self, name: &str, error: &Error) -> Result<()> {
        let position = self.log_dir(name).join(copy::Position::FILE);
        fs::remove_file(&position).map_err(|error| Error::io(position.display().to_string(), error))?;
        tracing::warn!(target: "tinystore", file = name, %error, "a copy starts again from a snapshot");
        Ok(())
    }

    /// The files this node serves: those it writes that keep a log, and its
    /// copies that are whole.
    pub(crate) fn files_served(&self) -> Result<Vec<String>> {
        let copies = lock(&self.copies).keys().cloned().collect::<HashSet<_>>();
        let mut served = Vec::new();
        for name in self.files_logged()? {
            let dir = self.log_dir(&name);
            if copies.contains(&name) || (!copy::is_copy(&dir) && !segments_in(&dir)?.is_empty()) {
                served.push(name);
            }
        }
        Ok(served)
    }
}
