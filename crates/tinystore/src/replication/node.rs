//! A node as it runs: the logs of the files it writes, the copies it holds,
//! and the threads of its links, which the store starts at open and stops
//! first at close.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::catalog::{Catalog, Claim, FILE as CATALOG};
use super::copy::{self, Copy};
use super::entry::Point;
use super::follow::{Direct, Upstream, Written};
use super::links::Follower;
use super::log::{Acks, Bounds, Log, Notify, segments_in};
use super::snapshot::Source;
use super::{DIR, NODE_LOCAL, Node};
use crate::sqlite::{File, Observe, Readers, Switch};
use crate::store::WeakStore;
use crate::sync::lock;
use crate::{Error, ErrorKind, Result, Store};

/// How long opening a file a node copies waits for its first copy.
pub(crate) const READY_WAIT: Duration = Duration::from_secs(30);
/// How long a snapshot taken again waits for the reads of the copy it
/// replaces to end.
const PAUSE_AT_MOST: Duration = Duration::from_secs(5);

pub(crate) struct State {
    pub(crate) dir: PathBuf,
    pub(crate) options: Node,
    /// The store the node is, which runs the writes other nodes pass on.
    store: Mutex<Option<WeakStore>>,
    bounds: Bounds,
    pub(crate) notify: Arc<Notify>,
    /// What a synchronous home's commits wait for.
    pub(crate) acks: Arc<Acks>,
    /// The logs of the files this node writes that are open now.
    logs: Mutex<HashMap<String, Weak<Log>>>,
    /// The copies this node holds whole.
    copies: Mutex<HashMap<String, Arc<Copy>>>,
    copied: Condvar,
    /// An open waits out a change of its file's route, and the change the
    /// opens begun before it, so that an engine's file opens on one side of
    /// the change: before it, as a file the change switches or pauses, or
    /// after it, by the route it leaves.
    routes: Mutex<Routes>,
    rerouted: Condvar,
    pub(crate) upstream: Upstream,
    pub(crate) stopping: AtomicBool,
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// Every link open, shut down at stop so that its threads stop reading.
    links: Mutex<Vec<TcpStream>>,
    pub(crate) listening: Mutex<Option<SocketAddr>>,
    /// The ops of this node's writes that commute, queued for their home and
    /// opened with the first.
    queue: Mutex<Option<Arc<super::queue::Queue>>>,
    /// The TLS its links speak, made from its options.
    pub(crate) tls: super::transport::Tls,
    /// How many snapshots this node began taking, the first copies and those
    /// taken again.
    pub(crate) snapshots: std::sync::atomic::AtomicU64,
    /// The commits of an old home that the nodes taking from this one held
    /// past the point its promotion went on from, which they lost.
    pub(crate) lost: std::sync::atomic::AtomicU64,
    /// A file is being opened that no copy holds yet: the link asks for the
    /// files served now rather than at its next turn.
    pub(crate) wanted: AtomicBool,
    /// What the node holds of each engine's file it opened, by the file's
    /// name: a snapshot taken again closes their readers while it takes the
    /// file's place, and a move of the file's home changes their mode.
    opened: Mutex<HashMap<String, Vec<Switch>>>,
    /// The nodes taking from this one now, by their link: what `log/links`
    /// says of them.
    pub(crate) followers: Mutex<HashMap<u64, Follower>>,
    next_follower: std::sync::atomic::AtomicU64,
    /// The address of the node this one takes from, while a link reaches it.
    pub(crate) upstream_at: Mutex<Option<String>>,
    /// The node's id, made once and kept in `log/node`, which the catalog and
    /// its home's count of its queued ops know it by.
    pub(crate) id: String,
    /// The files the node it takes from serves, as its last welcome or list
    /// said; none until a link said.
    pub(crate) served: Mutex<Option<Vec<String>>>,
    catalog: super::catalog::Held,
    /// The direct links to the homes of files `from` does not serve, by the
    /// address each reaches.
    direct: Mutex<HashMap<String, Arc<Direct>>>,
    /// The checks of copies a program asked for, by file.
    verifying: Mutex<HashMap<String, Verifying>>,
    /// The leases a witness granted this node of copies it takes over, until
    /// when, for their logs to open holding them.
    granted: Mutex<HashMap<String, Instant>>,
    /// The epoch each file this node writes was last seen at, for a judged
    /// node's renewal of a file no engine holds open.
    epochs: Mutex<HashMap<String, u64>>,
    /// The leases this node keeps, when it is a witness.
    pub(crate) witness: Option<super::witness::Witness>,
}

/// Where a check of a copy stands.
enum Verifying {
    Asked,
    Sent,
    Done(Result<u64>),
}

/// The file in a copy's log directory that names the node a direct link
/// takes it from, so that the link is made again when the node starts.
pub(super) const DIRECT: &str = "from";

/// The files whose route may be changing now, by their name: a takeover, a
/// hand-over or a snapshot taken again changes how an engine's file is reached.
#[derive(Default)]
struct Routes {
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

/// A node taking from this one, kept among the followers until its link ends.
pub(crate) struct Following {
    state: Arc<State>,
    id: u64,
    node: String,
    address: String,
}

impl Following {
    /// The node's id, as its hello said.
    pub(crate) fn node(&self) -> &str {
        &self.node
    }

    pub(crate) fn address(&self) -> &str {
        &self.address
    }

    /// The follower holds `file` up to entry `sequence`.
    pub(crate) fn applied(&self, file: &str, sequence: u64) {
        if let Some(follower) = lock(&self.state.followers).get_mut(&self.id) {
            follower.applied.insert(file.to_owned(), sequence);
        }
    }
}

impl Drop for Following {
    fn drop(&mut self) {
        lock(&self.state.followers).remove(&self.id);
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
    pub(crate) fn new(dir: &Path, options: Node, keep_free: u64) -> Result<State> {
        let tls = super::transport::Tls::of(&options)?;
        let acks = Arc::new(Acks::waiting_for(options.synchronous_with.clone()));
        Ok(State {
            id: super::queue::node_id(&dir.join(DIR))?,
            acks,
            served: Mutex::default(),
            catalog: Mutex::default(),
            direct: Mutex::default(),
            verifying: Mutex::default(),
            granted: Mutex::default(),
            epochs: Mutex::default(),
            witness: if options.witness { Some(super::witness::Witness::open(dir)?) } else { None },
            queue: Mutex::default(),
            tls,
            dir: dir.to_owned(),
            options,
            store: Mutex::default(),
            bounds: Bounds { keep_free, ..Bounds::default() },
            notify: Arc::default(),
            logs: Mutex::default(),
            copies: Mutex::default(),
            copied: Condvar::new(),
            routes: Mutex::default(),
            rerouted: Condvar::new(),
            upstream: Upstream::default(),
            stopping: AtomicBool::new(false),
            threads: Mutex::default(),
            links: Mutex::default(),
            listening: Mutex::default(),
            snapshots: std::sync::atomic::AtomicU64::new(0),
            lost: std::sync::atomic::AtomicU64::new(0),
            wanted: AtomicBool::new(false),
            opened: Mutex::default(),
            followers: Mutex::default(),
            next_follower: std::sync::atomic::AtomicU64::new(0),
            upstream_at: Mutex::default(),
        })
    }

    /// Starts the node's links: a server where it serves, and the link to the
    /// node it takes its files from.
    pub(crate) fn start(self: &Arc<Self>, store: &Store) -> Result<()> {
        if self.options.failover.is_some() && self.options.synchronous && self.options.synchronous_with.is_empty() {
            return Err(Error::invalid(
                "a synchronous node a witness judges names the copies its commits wait for, synchronous_with: only \
                 they hold every commit it answered, and the witness gives its lease to one of them",
            ));
        }
        *lock(&self.store) = Some(store.downgrade());
        // the catalog's home serves it from its first link on, so that no node
        // takes a network that keeps one for a network that keeps none
        if self.options.catalog {
            self.catalog(store)?;
        }
        if let Some(address) = &self.options.serve {
            super::serve::start(self, address)?;
        }
        let direct = self.files_taken_directly()?;
        if !self.options.from.is_empty() || !direct.is_empty() {
            self.open_copies_held(store)?;
        }
        if !self.options.from.is_empty() {
            super::follow::start(self, store);
        }
        for (name, address) in direct {
            self.take_directly(store, &name, &address)?;
        }
        let writing = Arc::clone(self);
        self.spawn("tinystore node: links", move || super::links::keep_written(&writing));
        if let Some(witness) = &self.options.failover {
            super::judge::start(self, store, witness.clone());
        }
        if self.options.catalog || !self.options.from.is_empty() {
            let claiming = Arc::clone(self);
            self.spawn("tinystore node: catalog", move || claiming.claim_its_files());
        }
        // writes queued before the store last closed are sent from the start
        if self.dir.join(DIR).join(super::queue::FILE).exists() {
            self.queue()?;
        }
        Ok(())
    }

    /// Stops every link and waits for its threads; a thread that is the
    /// caller, as one whose store's last handle it held, is left to end.
    pub(crate) fn stop(&self) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        for log in lock(&self.logs).values().filter_map(Weak::upgrade) {
            log.stop_waiting();
        }
        if let Some(mut address) = *lock(&self.listening) {
            // wakes the accepting thread, which then sees the node stopping; a
            // server on every interface is reached on loopback, since Windows
            // refuses a connection to 0.0.0.0 and the thread would sleep on
            if address.ip().is_unspecified() {
                address.set_ip(match address {
                    std::net::SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
                    std::net::SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
                });
            }
            let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
        }
        for link in lock(&self.links).drain(..) {
            let _ = link.shutdown(std::net::Shutdown::Both);
        }
        self.upstream.stop();
        for direct in lock(&self.direct).values() {
            direct.upstream.stop();
        }
        self.notify.appended();
        let threads: Vec<_> = lock(&self.threads).drain(..).collect();
        for thread in threads {
            if thread.thread().id() != thread::current().id() {
                let _ = thread.join();
            }
        }
        lock(&self.copies).clear();
        if let Some(queue) = lock(&self.queue).take() {
            queue.close();
        }
        if let Some(catalog) = lock(&self.catalog).take() {
            catalog.close();
        }
    }

    /// Where other nodes reach this one: the address it was given, or the one
    /// its server listens on; empty for a node that serves nothing.
    pub(crate) fn address(&self) -> String {
        match &self.options.advertise {
            Some(address) => address.clone(),
            None => lock(&self.listening).map(|at| at.to_string()).unwrap_or_default(),
        }
    }

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
            && !dir.join(super::promote::PROMOTED).exists()
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

    /// The epoch of this node's log of `name`: its last entry's, or the
    /// point a promotion left; 1 for a file it holds no log of.
    fn epoch_of(&self, name: &str) -> Result<u64> {
        let dir = self.log_dir(name);
        let last = super::log::Log::last_in(&dir)?.map(|point| point.epoch);
        let promoted = super::promote::promoted_at(&dir)?.map(|point| point.epoch);
        Ok(last.max(promoted).unwrap_or(1))
    }

    fn served_by_from(&self, name: &str) -> bool {
        lock(&self.served).as_ref().is_some_and(|served| served.iter().any(|file| file == name))
    }

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
                    super::follow::start_direct(self, store, Arc::clone(&direct));
                    direct
                }
            }
        };
        direct.add(name);
        Ok(())
    }

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
        super::promote::promote_one(name.to_owned(), &self.dir.join(name), &self.log_dir(name), at_least)?;
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

    /// Takes writes of `name` again, after a move that did not finish.
    /// Writes `name` again at `epoch`, the catalog naming this node its home
    /// there once a move away from it was called off: its log goes on at the
    /// epoch, and its fence goes.
    pub(crate) fn go_on_at(&self, name: &str, epoch: u64) -> Result<()> {
        match lock(&self.logs).get(name).and_then(Weak::upgrade) {
            Some(log) => log.go_on_at(epoch)?,
            None => Log::go_on_at_in(&self.log_dir(name), epoch)?,
        }
        self.unfence(name)
    }

    pub(crate) fn unfence(&self, name: &str) -> Result<()> {
        match lock(&self.logs).get(name).and_then(Weak::upgrade) {
            Some(log) => log.unfence(),
            None => Log::unfence_in(&self.log_dir(name)),
        }
    }

    /// Runs `read` with where this node's own file `name` stands, held still:
    /// by its writer while one is open; with none, nothing writes it.
    pub(crate) fn holding_home<T>(&self, name: &str, read: impl FnOnce(Point) -> Result<T>) -> Result<T> {
        if let Some(log) = lock(&self.logs).get(name).and_then(Weak::upgrade) {
            return log.holding(read);
        }
        let point = self.point_of(name).ok_or_else(|| {
            Error::new(ErrorKind::Unavailable, format!("{name}: this node holds no log of it it can read now"))
        })?;
        read(point)
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

    /// Refuses every write of `name` from now on, and across restarts: this
    /// node is its old home, and another writes it at `epoch`.
    pub(crate) fn fence(&self, name: &str, epoch: u64) -> Result<()> {
        tracing::error!(
            target: "tinystore",
            file = name,
            epoch,
            "another node writes a file this node wrote, at a later epoch: this node takes no more writes of it"
        );
        match lock(&self.logs).get(name).and_then(Weak::upgrade) {
            Some(log) => log.fence(epoch),
            None => Log::fence_in(&self.log_dir(name), epoch),
        }
    }

    /// Whether a direct link takes `name`, rather than the link to `from`.
    pub(crate) fn follows_directly(&self, name: &str) -> bool {
        lock(&self.direct).values().any(|direct| direct.follows(name))
    }

    /// Passes a write of `file` on toward its home: over the direct link that
    /// takes the file, or the link to `from`.
    pub(crate) fn write_toward(&self, file: &str, op: Vec<u8>, at_most: Duration) -> Result<Written> {
        let direct = lock(&self.direct).values().find(|direct| direct.follows(file)).cloned();
        match direct {
            Some(direct) => direct.upstream.write(op, at_most),
            None => self.upstream.write(op, at_most),
        }
    }

    /// The copies a direct link takes, each with the address it reaches, as
    /// their log directories keep them.
    fn files_taken_directly(&self) -> Result<Vec<(String, String)>> {
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

    /// Claims in the catalog each file this node already writes, as a node's
    /// files made before its network kept a catalog: a file the catalog names
    /// another home of is two homes' now, which a person settles.
    fn claim_its_files(&self) {
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

    /// This node's queue of writes that commute, opened and its sender started
    /// the first time.
    pub(crate) fn queue(self: &Arc<Self>) -> Result<Arc<super::queue::Queue>> {
        let mut held = lock(&self.queue);
        if let Some(queue) = &*held {
            return Ok(Arc::clone(queue));
        }
        let queue = Arc::new(super::queue::Queue::open(&self.dir, self.id.clone())?);
        *held = Some(Arc::clone(&queue));
        let (state, sending) = (Arc::clone(self), Arc::clone(&queue));
        self.spawn("tinystore node: queue", move || super::queue::send(&state, &sending));
        Ok(queue)
    }

    /// The store the node is, while it is open.
    pub(crate) fn store(&self) -> Option<Store> {
        lock(&self.store).as_ref().and_then(WeakStore::upgrade)
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// What this node first says on a link, naming itself at `address`, or
    /// by no address when it only asks.
    pub(crate) fn hello(&self, address: String) -> super::message::Message {
        super::message::Message::Hello {
            version: super::message::VERSION,
            token: self.options.token.clone().unwrap_or_default(),
            node: self.id.clone(),
            address,
        }
    }

    /// Sleeps `span`, waking within 50 ms once the node stops.
    pub(crate) fn pause(&self, span: Duration) {
        let until = Instant::now() + span;
        while Instant::now() < until && !self.stopping() {
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub(crate) fn spawn(&self, name: &str, work: impl FnOnce() + Send + 'static) {
        // each line a node's threads log names the node, for a process or a log that holds several
        let inside = tracing::Span::current().metadata().is_some_and(|span| span.name() == "node");
        let span = if inside {
            tracing::Span::none()
        } else {
            tracing::info_span!(target: "tinystore", "node", node = %self.id)
        };
        let thread = thread::Builder::new().name(name.to_owned()).spawn(move || span.in_scope(work));
        match thread {
            Ok(thread) => {
                // A thread that ended needs no join at stop: a copy forwarding
                // writes starts one a write, which would otherwise pile up.
                let mut threads = lock(&self.threads);
                threads.retain(|kept| !kept.is_finished());
                threads.push(thread);
            }
            Err(error) => tracing::warn!(target: "tinystore", %error, "a node could not start a thread of its links"),
        }
    }

    /// Keeps a link to shut down at stop.
    pub(crate) fn keep_link(&self, link: &TcpStream) {
        if let Ok(link) = link.try_clone() {
            let mut links = lock(&self.links);
            links.retain(|kept| kept.peer_addr().is_ok());
            links.push(link);
        }
    }

    /// The log of the file `name` this node writes, opened for its writer.
    pub(crate) fn open_log(&self, name: &str) -> Result<Arc<dyn Observe>> {
        let mut log = Log::open(self.log_dir(name), self.bounds, Arc::clone(&self.notify))?
            .followed(name, Arc::clone(&self.acks));
        // the catalog's home moves only by a person's hand, never by a lease
        if self.options.failover.is_some() && name != CATALOG {
            log = log.judged(lock(&self.granted).remove(name).unwrap_or_else(Instant::now));
        }
        if self.options.synchronous || !self.options.synchronous_with.is_empty() {
            log = log.synchronous();
        }
        let log = Arc::new(log);
        lock(&self.logs).insert(name.to_owned(), Arc::downgrade(&log));
        Ok(log)
    }

    pub(crate) fn log_dir(&self, name: &str) -> PathBuf {
        self.dir.join(DIR).join(name)
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
            || dir.join(super::promote::PROMOTED).exists()
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
                if let Err(error) = super::forward::make(self, name) {
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

    /// Keeps the node `node`, which linked from `address`, among the
    /// followers.
    pub(crate) fn following(self: &Arc<Self>, node: String, address: String) -> Following {
        let id = self.next_follower.fetch_add(1, Ordering::Relaxed);
        lock(&self.followers).insert(id, Follower { address: address.clone(), applied: Default::default() });
        Following { state: Arc::clone(self), id, node, address }
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

    pub(crate) fn bounds(&self) -> Bounds {
        self.bounds
    }

    pub(crate) fn drop_copy(&self, name: &str) {
        lock(&self.copies).remove(name);
    }

    /// Opens the copies whole on disk, so that their engines read them while
    /// no link reaches their home.
    fn open_copies_held(&self, store: &Store) -> Result<()> {
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

    /// Every file with a directory in `log/`, and each whose log is open with
    /// no entry yet, for a judged node's turn: a file born here asks its
    /// witness for its first lease before its first commit.
    pub(crate) fn files_logged_now(&self) -> Result<Vec<String>> {
        let mut files = self.files_logged()?;
        for name in lock(&self.logs).keys() {
            if !files.contains(name) {
                files.push(name.clone());
            }
        }
        Ok(files)
    }

    /// Keeps the lease a witness granted of the copy `file` until its log
    /// opens, as the copy is taken over.
    pub(crate) fn granted(&self, file: &str, until: Instant) {
        lock(&self.granted).insert(file.to_owned(), until);
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
    fn step_back(self: &Arc<Self>, store: &Store, file: &str, home: &super::catalog::Home) {
        let stepped = self.fence(file, home.epoch.max(1)).and_then(|()| {
            let ours = self.point_of(file).ok_or_else(|| Error::not_found(format!("{file}: its log")))?;
            super::handover::let_go(self, store, file, &home.address, ours)
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

    /// The epoch this node's open log of `file` writes at.
    pub(crate) fn epoch_written(&self, file: &str) -> Option<u64> {
        lock(&self.logs).get(file).and_then(Weak::upgrade).map(|log| log.epoch())
    }

    /// The epoch this node writes `file` at, taking no writer: a write that
    /// holds one may wait for the lease this epoch renews. A log no engine
    /// holds open is read once and remembered, its epoch moving only while
    /// open.
    pub(crate) fn epoch_held(&self, file: &str) -> Option<u64> {
        if let Some(epoch) = self.epoch_written(file) {
            lock(&self.epochs).insert(file.to_owned(), epoch);
            return Some(epoch);
        }
        if let Some(&epoch) = lock(&self.epochs).get(file) {
            return Some(epoch);
        }
        // the epoch a move or a promotion left, which no entry may name yet, read
        // from the log's files rather than through the writer
        if !self.log_dir(file).exists() {
            return None;
        }
        let epoch = self.epoch_of(file).ok()?;
        lock(&self.epochs).insert(file.to_owned(), epoch);
        Some(epoch)
    }

    /// Keeps the lease of `file` this node writes on to `until`.
    pub(crate) fn lease_until(&self, file: &str, until: Instant) {
        if let Some(log) = lock(&self.logs).get(file).and_then(Weak::upgrade) {
            log.lease_until(until);
        }
    }

    /// Every file with a directory in `log/`, by its name in the store's.
    fn files_logged(&self) -> Result<Vec<String>> {
        files_logged_in(&self.dir.join(DIR))
    }

    /// What a snapshot of the file `name` reads, if this node serves it.
    pub(crate) fn source(&self, name: &str) -> Option<Box<dyn Source>> {
        if let Some(copy) = self.copy(name) {
            return Some(Box::new(CopySource(copy)));
        }
        let dir = self.log_dir(name);
        if copy::is_copy(&dir) || !dir.exists() {
            return None;
        }
        let log = lock(&self.logs).get(name).and_then(Weak::upgrade);
        Some(Box::new(HomeSource { path: self.dir.join(name), dir, log }))
    }

    /// Where the log of `name` stands, for a write's caller to wait for.
    pub(crate) fn point_of(&self, name: &str) -> Option<Point> {
        self.source(name).and_then(|source| source.stable().ok())
    }
}

/// Every file with a directory in `logs`, a store's `log/`, by its name in
/// the store's directory.
pub(crate) fn files_logged_in(logs: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    walk(logs, "", &mut names)?;
    names.sort();
    Ok(names)
}

/// Collects the names of the files whose log directories lie under `dir`: a
/// directory holding a segment, a copy's mark or a promotion's point is one.
fn walk(dir: &Path, prefix: &str, names: &mut Vec<String>) -> Result<()> {
    let listed = match fs::read_dir(dir) {
        Ok(listed) => listed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::io(dir.display().to_string(), error)),
    };
    for found in listed {
        let found = found.map_err(|error| Error::io(dir.display().to_string(), error))?;
        let Ok(name) = found.file_name().into_string() else { continue };
        if !found.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = found.path();
        let joined = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        // a promoted copy whose log kept no entry since its snapshot holds one too
        let holds_a_log =
            copy::is_copy(&path) || !segments_in(&path)?.is_empty() || path.join(super::promote::PROMOTED).exists();
        if holds_a_log {
            names.push(joined);
        } else {
            walk(&path, &joined, names)?;
        }
    }
    Ok(())
}

/// A file this node writes, its log open with its writer or closed.
struct HomeSource {
    path: PathBuf,
    dir: PathBuf,
    log: Option<Arc<Log>>,
}

impl Source for HomeSource {
    fn path(&self) -> &Path {
        &self.path
    }

    fn stable(&self) -> Result<Point> {
        if let Some(log) = &self.log {
            return Ok(log.stable());
        }
        // With no writer open, a WAL that holds frames may hold commits a
        // process that died made and its log never took: they reach the log
        // when the file's engine opens it.
        if fs::metadata(crate::sqlite::wal_of(&self.path)).is_ok_and(|wal| wal.len() > 0) {
            return Err(Error::new(
                ErrorKind::Unavailable,
                "its WAL holds commits its log may lack until its engine opens it",
            ));
        }
        Log::last_in(&self.dir)?.ok_or_else(|| Error::new(ErrorKind::Unavailable, "its log holds no entry yet"))
    }
}

/// A copy this node holds whole.
struct CopySource(Arc<Copy>);

impl Source for CopySource {
    fn path(&self) -> &Path {
        self.0.path()
    }

    fn stable(&self) -> Result<Point> {
        Ok(self.0.point())
    }
}
