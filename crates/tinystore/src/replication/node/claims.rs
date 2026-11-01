//! The catalog's say over where each file is written: the claims a node
//! makes of its files, and what it does when the catalog names another home.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{READY_WAIT, State};
use crate::replication::NODE_LOCAL;
use crate::replication::catalog::{Catalog, Claim, FILE as CATALOG};
use crate::replication::copy;
use crate::replication::log::segments_in;
use crate::replication::promote;
use crate::sync::lock;
use crate::{Error, Result, Store};

/// Where a node's claim of a file in the catalog leaves the file.
pub(crate) enum Claimed {
    /// This node writes it.
    Here,
    /// The node it takes from serves it.
    From,
    /// Its home, at this address, serves it, which the node takes it from
    /// over a direct link.
    At(String),
}

impl State {
    /// This node's catalog, opened the first time: its own on the node that is
    /// the catalog's home, a copy waited for on the others.
    pub(crate) fn catalog(&self, store: &Store) -> Result<Arc<Catalog>> {
        let mut held = lock(&self.catalog);
        if let Some(catalog) = &*held {
            return Ok(Arc::clone(catalog));
        }
        let catalog = Arc::new(Catalog::open(store)?);
        *held = Some(Arc::clone(&catalog));
        Ok(catalog)
    }

    /// Whether this node's network keeps a catalog: this node is its home,
    /// holds a copy of it, or the node it takes from serves it. Until its first
    /// link says what that node serves, a node with `from` waits for it.
    pub(crate) fn uses_catalog(&self) -> bool {
        if self.options.catalog || copy::is_copy(&self.log_dir(CATALOG)) {
            return true;
        }
        if self.options.from.is_empty() {
            return false;
        }
        let deadline = Instant::now() + READY_WAIT;
        loop {
            if let Some(served) = &*lock(&self.served) {
                return served.iter().any(|file| file == CATALOG);
            }
            if Instant::now() >= deadline || self.stopping() {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether this node holds nothing of `name` yet, neither a log of its
    /// own nor a copy, and names no list of the files it copies to say which
    /// it will.
    pub(crate) fn is_new(&self, name: &str) -> bool {
        if name == CATALOG || self.options.files.is_some() || NODE_LOCAL.contains(&name) {
            return false;
        }
        let dir = self.log_dir(name);
        !copy::is_copy(&dir)
            && !dir.join(promote::PROMOTED).exists()
            && segments_in(&dir).is_ok_and(|segments| segments.is_empty())
    }

    /// Claims `name` in the catalog for this node, which then writes it, or
    /// says where the node copies it from: the node it takes from, when that
    /// node serves it or is its home, or the home the catalog names. A node
    /// that serves nothing claims nothing: the node it takes from makes the
    /// file.
    pub(crate) fn claim(&self, store: &Store, name: &str) -> Result<Claimed> {
        let catalog = self.catalog(store)?;
        let (address, epoch) = (self.address(), self.epoch_of(name)?);
        let claim = Claim { file: name, node: &self.id, address: &address, epoch };
        let home = match catalog.home_of(name)? {
            // a copy this node promoted claims the route its older home held
            Some(home) if home.node != self.id && epoch > home.epoch && !address.is_empty() => {
                catalog.claim(store, &claim)?
            }
            Some(home) => home,
            None if address.is_empty() || self.served_by_from(name) => return Ok(Claimed::From),
            None => catalog.claim(store, &claim)?,
        };
        if home.node == self.id {
            return Ok(Claimed::Here);
        }
        let from = lock(&self.upstream_at).clone();
        if self.served_by_from(name) || home.address.is_empty() || from.as_deref() == Some(home.address.as_str()) {
            return Ok(Claimed::From);
        }
        Ok(Claimed::At(home.address))
    }

    fn served_by_from(&self, name: &str) -> bool {
        lock(&self.served).as_ref().is_some_and(|served| served.iter().any(|file| file == name))
    }

    /// Claims in the catalog each file this node already writes, as a node's
    /// files made before its network kept a catalog: a file the catalog names
    /// another home of is two homes' now, which a person settles.
    pub(super) fn claim_its_files(&self) {
        if !self.uses_catalog() {
            return;
        }
        let Some(store) = self.store() else {
            return;
        };
        let (logged, catalog) = match (self.files_logged(), self.catalog(&store)) {
            (Ok(logged), Ok(catalog)) => (logged, catalog),
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(target: "tinystore", %error, "a node could not claim its files in the catalog");
                return;
            }
        };
        let address = self.address();
        for name in logged {
            let dir = self.log_dir(&name);
            if name == CATALOG || NODE_LOCAL.contains(&name.as_str()) || copy::is_copy(&dir) || self.stopping() {
                continue;
            }
            // the claim goes to the catalog's home, whose answer is the last
            // word: this node's copy may be older than a move made while it was away
            // A claim at a guessed epoch could fence this node: one unread waits.
            let epoch = match self.epoch_of(&name) {
                Ok(epoch) => epoch,
                Err(error) => {
                    tracing::warn!(
                        target: "tinystore",
                        file = %name,
                        %error,
                        "a node could not read a file's epoch to claim it",
                    );
                    continue;
                }
            };
            let claim = Claim { file: &name, node: &self.id, address: &address, epoch };
            match catalog.claim(&store, &claim) {
                // a move away from it called off while this node was down
                Ok(home) if home.node == self.id && home.epoch > epoch => {
                    if let Err(error) = self.go_on_at(&name, home.epoch) {
                        tracing::warn!(
                            target: "tinystore",
                            file = %name,
                            %error,
                            "a node could not write a file again",
                        );
                    }
                }
                Ok(home) if home.node == self.id => {}
                Ok(home) => self.found_another_home(&name, epoch, home.epoch),
                Err(error) => {
                    tracing::debug!(target: "tinystore", file = %name, %error, "a node could not claim a file")
                }
            }
        }
    }

    /// A file this node writes whose route the catalog names another node:
    /// at a later epoch, a copy promoted while this node was away, this node
    /// is the old home and takes no more writes of it; at the same epoch two
    /// nodes wrote it before their network kept a catalog, which a person
    /// settles.
    fn found_another_home(&self, name: &str, ours: u64, theirs: u64) {
        if theirs <= ours {
            tracing::error!(
                target: "tinystore",
                file = name,
                "the catalog names another home of a file this node writes: two homes write it now"
            );
            return;
        }
        if let Err(error) = self.fence(name, theirs) {
            tracing::warn!(target: "tinystore", file = name, %error, "a node could not fence a file");
        }
    }

    /// Names this node the home of `file` in the catalog at `epoch`, once a
    /// lease made it so: nodes route the file by the catalog, and a direct link
    /// takes nothing of a node it does not name. A catalog out of reach is
    /// asked again until it answers.
    pub(crate) fn claim_taken(self: &Arc<Self>, file: &str, epoch: u64) {
        let (state, file) = (Arc::clone(self), file.to_owned());
        self.spawn("tinystore node: claim", move || {
            if !state.uses_catalog() {
                return;
            }
            while !state.stopping() {
                let Some(store) = state.store() else { return };
                let address = state.address();
                let claim = Claim { file: &file, node: &state.id, address: &address, epoch };
                match state.catalog(&store).and_then(|catalog| catalog.claim(&store, &claim)) {
                    Ok(home) if home.node == state.id => return,
                    Ok(home) => {
                        tracing::error!(
                            target: "tinystore",
                            %file,
                            epoch,
                            other = %home.node,
                            "the catalog names another home of a file this node took by its lease: it becomes that home's copy"
                        );
                        state.step_back(&store, &file, &home);
                        return;
                    }
                    Err(error) => tracing::debug!(target: "tinystore", %file, %error, "a node's claim of a file it took waits for the catalog"),
                }
                drop(store);
                state.pause(Duration::from_secs(2));
            }
        });
    }

    /// Takes no more writes of `file`, which the catalog names `home` the home
    /// of, and becomes that home's copy.
    fn step_back(self: &Arc<Self>, store: &Store, file: &str, home: &crate::replication::catalog::Home) {
        let stepped = self.fence(file, home.epoch.max(1)).and_then(|()| {
            let ours = self.point_of(file).ok_or_else(|| Error::not_found(format!("{file}: its log")))?;
            crate::replication::handover::let_go(self, store, file, &home.address, ours)
        });
        if let Err(error) = stepped {
            tracing::error!(
                target: "tinystore",
                file,
                %error,
                "a node could not become a copy of the home the catalog names",
            );
        }
    }
}
