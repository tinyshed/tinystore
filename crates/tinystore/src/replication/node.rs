//! A node as it runs: the logs of the files it writes, the copies it holds,
//! and the threads of its links, which the store starts at open and stops
//! first at close.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::copy::Copy;
use super::follow::{Direct, Upstream};
use super::links::Follower;
use super::log::{Acks, Bounds, Log, Notify};
use super::{DIR, Node};
use crate::sqlite::Switch;
use crate::store::WeakStore;
use crate::sync::lock;
use crate::{Error, Result, Store};

mod claims;
mod copies;
mod logs;
mod routes;

pub(crate) use claims::Claimed;
use copies::Verifying;
pub(crate) use logs::files_logged_in;
use routes::Routes;

/// How long opening a file a node copies waits for its first copy.
pub(crate) const READY_WAIT: Duration = Duration::from_secs(30);

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

/// The file in a copy's log directory that names the node a direct link
/// takes it from, so that the link is made again when the node starts.
pub(super) const DIRECT: &str = "from";

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
        let _ = self.acks.probe.set(Arc::clone(store.probe()));
        let sampler: Weak<dyn crate::probe::Sample> = Arc::downgrade(self) as Weak<State>;
        store.probe().sampled(sampler);
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

    /// Keeps the node `node`, which linked from `address`, among the
    /// followers.
    pub(crate) fn following(self: &Arc<Self>, node: String, address: String) -> Following {
        let id = self.next_follower.fetch_add(1, Ordering::Relaxed);
        lock(&self.followers).insert(id, Follower { address: address.clone(), applied: Default::default() });
        Following { state: Arc::clone(self), id, node, address }
    }

    pub(crate) fn bounds(&self) -> Bounds {
        self.bounds
    }
}
