//! A change of a file's route, a takeover or a snapshot taken again, and the
//! engines' opens of the file it waits out, the readers it pauses and the
//! files it switches.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::{READY_WAIT, State};
use crate::replication::copy::Copy;
use crate::replication::promote;
use crate::sqlite::{File, Readers, Switch};
use crate::sync::lock;
use crate::{Error, ErrorKind, Result, Store};

/// How long a snapshot taken again waits for the reads of the copy it
/// replaces to end.
const PAUSE_AT_MOST: Duration = Duration::from_secs(5);

/// The files whose route may be changing now, by their name: a takeover, a
/// hand-over or a snapshot taken again changes how an engine's file is reached.
#[derive(Default)]
pub(super) struct Routes {
    /// How many engines are opening each.
    opening: HashMap<String, usize>,
    /// How many changes of each are under way, which do not wait for one another.
    rerouting: HashMap<String, usize>,
}

/// An engine's open of a file under way, which a change of the file's route
/// waits for.
pub(crate) struct Opening<'a> {
    state: &'a State,
    name: String,
}

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        release(&mut lock(&self.state.routes).opening, &self.name);
        self.state.rerouted.notify_all();
    }
}

/// A change of a file's route under way, which an engine's open of it waits
/// out.
pub(crate) struct Rerouting<'a> {
    state: &'a State,
    name: String,
}

impl Drop for Rerouting<'_> {
    fn drop(&mut self) {
        release(&mut lock(&self.state.routes).rerouting, &self.name);
        self.state.rerouted.notify_all();
    }
}

/// Counts one fewer of `name` under way, forgetting it at none.
fn release(under_way: &mut HashMap<String, usize>, name: &str) {
    if let Some(count) = under_way.get_mut(name) {
        *count -= 1;
        if *count == 0 {
            under_way.remove(name);
        }
    }
}

/// The readers of a copy's files held closed while a snapshot takes the
/// file's place, open to reads again once it drops.
pub(crate) struct Paused(Vec<Arc<Readers>>);

impl Drop for Paused {
    fn drop(&mut self) {
        self.0.iter().for_each(|readers| readers.resume());
    }
}

impl State {
    /// Makes the copy `name` this running node's own at the next epoch, or at
    /// `at_least` when that is later, as a claim or a lease names it: the
    /// engines' opens of it under way ended, its readers paused, its applier
    /// closed once no link holds it, its log going on after the last entry it
    /// applied, and each engine's file of it given a writer.
    ///
    /// An open between letting go of the copy and keeping the promotion's
    /// point would wait for a copy that never comes, so opens wait it out.
    pub(crate) fn take_over_at(
        self: &Arc<Self>,
        store: &Store,
        name: &str,
        copy: Arc<Copy>,
        at_least: u64,
    ) -> Result<()> {
        let _rerouting = self.wait_to_reroute(name)?;
        let paused = self.pause_readers(name)?;
        if let Some(direct) = lock(&self.direct).values().find(|direct| direct.follows(name)) {
            direct.remove(name);
        }
        self.drop_copy(name);
        let deadline = Instant::now() + PAUSE_AT_MOST;
        while Arc::strong_count(&copy) > 1 {
            if Instant::now() >= deadline {
                self.keep_copy(name, copy);
                return Err(Error::new(ErrorKind::Unavailable, format!("{name}: a link still applies its copy")));
            }
            thread::sleep(Duration::from_millis(10));
        }
        drop(copy);
        promote::promote_one(name.to_owned(), &self.dir.join(name), &self.log_dir(name), at_least)?;
        let log = self.open_log(name)?;
        self.switch_each(name, |switch| switch.become_home(Arc::clone(&log)))?;
        drop(paused);
        store.applied(name);
        Ok(())
    }

    /// Begins an engine's open of `name` once no change of its route is under
    /// way, `READY_WAIT` at most: until the open ends, no change begins.
    pub(crate) fn wait_to_open(&self, name: &str) -> Result<Opening<'_>> {
        let deadline = Instant::now() + READY_WAIT;
        let mut routes = lock(&self.routes);
        while routes.rerouting.contains_key(name) {
            let now = Instant::now();
            if now >= deadline || self.stopping() {
                return Err(Error::new(ErrorKind::Unavailable, format!("{name}: its route changes still")));
            }
            let turn = (deadline - now).min(Duration::from_millis(250));
            routes = self.rerouted.wait_timeout(routes, turn).unwrap_or_else(PoisonError::into_inner).0;
        }
        *routes.opening.entry(name.to_owned()).or_default() += 1;
        Ok(Opening { state: self, name: name.to_owned() })
    }

    /// Begins a change of the route of `name` once the engines' opens of it
    /// under way end, `PAUSE_AT_MOST` at most; an open begun meanwhile waits
    /// for it.
    ///
    /// A thread inside `Store::open_file` of `name` must not call it, nor wait
    /// for one that does: the change would wait out the bound for that open.
    pub(crate) fn wait_to_reroute(&self, name: &str) -> Result<Rerouting<'_>> {
        let deadline = Instant::now() + PAUSE_AT_MOST;
        let mut routes = lock(&self.routes);
        *routes.rerouting.entry(name.to_owned()).or_default() += 1;
        while routes.opening.contains_key(name) {
            let now = Instant::now();
            if now >= deadline {
                release(&mut routes.rerouting, name);
                self.rerouted.notify_all();
                return Err(Error::new(ErrorKind::Unavailable, format!("{name}: an engine still opens it")));
            }
            routes = self.rerouted.wait_timeout(routes, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
        }
        Ok(Rerouting { state: self, name: name.to_owned() })
    }

    /// Keeps what the node holds of an engine's file `name` it opened.
    pub(crate) fn opened(&self, name: &str, file: File) -> File {
        let mut held = lock(&self.opened);
        let switches = held.entry(name.to_owned()).or_default();
        switches.retain(Switch::is_open);
        switches.push(file.switch());
        file
    }

    /// Runs `change` on each engine's file of `name` open on this node.
    pub(crate) fn switch_each(&self, name: &str, change: impl Fn(&Switch) -> Result<()>) -> Result<()> {
        let held = lock(&self.opened);
        for switch in held.get(name).into_iter().flatten().filter(|switch| switch.is_open()) {
            change(switch)?;
        }
        Ok(())
    }

    /// Closes the readers of every engine's file open on `name`, and holds
    /// its reads waiting until what it gives back drops.
    pub(crate) fn pause_readers(&self, name: &str) -> Result<Paused> {
        let open: Vec<Arc<Readers>> = lock(&self.opened)
            .get(name)
            .map(|switches| switches.iter().filter_map(Switch::readers).collect())
            .unwrap_or_default();
        let mut paused = Paused(Vec::new());
        for readers in open {
            readers.pause(PAUSE_AT_MOST)?;
            paused.0.push(readers);
        }
        Ok(paused)
    }
}
