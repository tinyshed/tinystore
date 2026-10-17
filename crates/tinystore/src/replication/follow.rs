//! A node's link to the node it takes its files from: the first address of
//! its `from` that answers, its files followed from where each copy stands,
//! snapshots put together and entries applied as they come, the link made
//! again when it breaks, and the writes this node passes on over it.

use std::collections::{HashMap, HashSet};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::copy::{self, Copy};
use super::entry::{self, Point};
use super::message::{Frames, Message, VERSION, plain_name, timed_out};
use super::node::{State, lock};
use super::serve::{Out, PING_EVERY, SILENT_AT_MOST};
use super::snapshot::Taking;
use crate::store::WeakStore;
use crate::{Error, ErrorKind, Result, Store};

/// How often a node that copies every file asks for the files served.
const LIST_EVERY: Duration = Duration::from_secs(5);
/// How long a write passed on waits for a link to its home.
const LINK_WAIT: Duration = Duration::from_secs(10);

/// A write passed on toward the home and what came back.
pub(crate) struct Written {
    pub(crate) outcome: std::result::Result<Vec<u8>, (ErrorKind, String)>,
    pub(crate) waits: Vec<(String, u64)>,
}

/// The link to the node this one takes its files from, as the threads that
/// pass writes on see it.
#[derive(Default)]
pub(crate) struct Upstream {
    out: Mutex<Option<Arc<Out>>>,
    linked: Condvar,
    next_id: AtomicU64,
    waiting: Mutex<HashMap<u64, Sender<Written>>>,
}

impl Upstream {
    /// Passes `op` on to the node this one takes its files from, and waits
    /// for what it answers.
    pub(crate) fn write(&self, op: Vec<u8>, at_most: Duration) -> Result<Written> {
        let out = self.wait_for_link()?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (answer, answered): (Sender<Written>, Receiver<Written>) = mpsc::channel();
        lock(&self.waiting).insert(id, answer);
        if let Err(error) = out.send(&Message::Write { id, op }) {
            lock(&self.waiting).remove(&id);
            return Err(Error::new(ErrorKind::Unavailable, "the link to the home broke before the write left")
                .with_source(error));
        }
        answered.recv_timeout(at_most).map_err(|_| {
            lock(&self.waiting).remove(&id);
            Error::new(ErrorKind::OutcomeUnknown, "the link to the home broke or went silent before the write answered")
        })
    }

    fn wait_for_link(&self) -> Result<Arc<Out>> {
        let deadline = Instant::now() + LINK_WAIT;
        let mut out = lock(&self.out);
        loop {
            if let Some(out) = &*out {
                return Ok(Arc::clone(out));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::new(ErrorKind::Unavailable, "no link reaches the home"));
            }
            out = self.linked.wait_timeout(out, deadline - now).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        }
    }

    fn linked(&self, out: Option<Arc<Out>>) {
        let ended = out.is_none();
        *lock(&self.out) = out;
        self.linked.notify_all();
        if ended {
            // a write whose answer the broken link would have carried: its
            // sender is dropped, and its caller hears the outcome unknown
            lock(&self.waiting).clear();
        }
    }

    fn answered(&self, id: u64, written: Written) {
        if let Some(answer) = lock(&self.waiting).remove(&id) {
            let _ = answer.send(written);
        }
    }

    pub(crate) fn stop(&self) {
        self.linked(None);
    }
}

/// Starts the thread that keeps the link to the node's `from`.
pub(crate) fn start(state: &Arc<State>, store: &Store) {
    let (state, store) = (Arc::clone(state), store.downgrade());
    state.clone().spawn("tinystore node: from", move || keep_linked(&state, &store));
}

fn keep_linked(state: &Arc<State>, store: &WeakStore) {
    let mut index = 0;
    let mut pause = Duration::from_millis(200);
    let mut said = None;
    while !state.stopping() {
        let address = state.options.from[index % state.options.from.len()].clone();
        let began = Instant::now();
        match follow(state, store, &address) {
            Ok(()) => {}
            Err(error) => {
                if said.as_deref() != Some(error.to_string().as_str()) && !state.stopping() {
                    tracing::warn!(target: "tinystore", from = %address, %error, "a node's link to the node it copies");
                    said = Some(error.to_string());
                }
            }
        }
        state.upstream.linked(None);
        *lock(&state.upstream_at) = None;
        if began.elapsed() > Duration::from_secs(10) {
            pause = Duration::from_millis(200);
        } else {
            index += 1;
            pause = (pause * 2).min(Duration::from_secs(5));
        }
        let until = Instant::now() + pause;
        while Instant::now() < until && !state.stopping() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// One file a link follows: a snapshot put together, or a copy whole.
enum Held {
    Taking(Box<Taking>),
    Whole(Arc<Copy>),
    /// Asked for, nothing come yet.
    Asked,
}

/// Follows the files over a link to `address` until it breaks.
fn follow(state: &Arc<State>, store: &WeakStore, address: &str) -> Result<()> {
    let stream = connect(address)?;
    state.keep_link(&stream);
    let (mut reading, writing) = super::transport::connect(state, address, stream)?;
    reading.set_timeout(Duration::from_millis(250))?;
    writing.set_timeout(SILENT_AT_MOST)?;
    let out = Arc::new(Out::new(writing));
    out.send(&Message::Hello { version: VERSION, token: state.options.token.clone().unwrap_or_default() })?;
    let mut link = Link {
        state,
        store,
        out: Arc::clone(&out),
        held: HashMap::new(),
        retries: Vec::new(),
        said: HashMap::new(),
        #[cfg(feature = "blobs")]
        fetching: Fetching::default(),
    };
    let mut frames = Frames::default();
    let mut heard = Instant::now();
    let mut listed = Instant::now();
    let mut welcomed = false;
    loop {
        if state.stopping() {
            return Ok(());
        }
        while let Some(message) = frames.next()? {
            heard = Instant::now();
            if !welcomed {
                match message {
                    Message::Welcome { files } => {
                        welcomed = true;
                        frames.unpack_from_here()?;
                        out.pack()?;
                        state.upstream.linked(Some(Arc::clone(&out)));
                        *lock(&state.upstream_at) = Some(address.to_owned());
                        *lock(&state.served) = Some(files.clone());
                        tracing::info!(target: "tinystore", from = %address, "a node is linked to the node it copies");
                        link.follow_new(&files)?;
                        continue;
                    }
                    Message::Refused { why } => return Err(Error::new(ErrorKind::Invalid, format!("refused: {why}"))),
                    other => return Err(Error::invalid(format!("a link began with {other:?}, not a welcome"))),
                }
            }
            link.take(message)?;
        }
        if welcomed {
            link.say_applied()?;
        }
        match frames.fill(&mut reading) {
            Ok(true) => {}
            Ok(false) => return Err(Error::new(ErrorKind::Unavailable, "the node closed the link")),
            Err(error) if timed_out(&error) => {}
            Err(error) => return Err(Error::io("a link", error)),
        }
        if heard.elapsed() > SILENT_AT_MOST {
            return Err(Error::new(ErrorKind::Unavailable, "the node said nothing for too long"));
        }
        if welcomed {
            link.retry_due()?;
            #[cfg(feature = "blobs")]
            link.fetch_due()?;
            let wanted = state.wanted.swap(false, Ordering::SeqCst);
            if wanted || (state.options.files.is_none() && listed.elapsed() > LIST_EVERY) {
                out.send(&Message::List)?;
                listed = Instant::now();
            } else if out.idle_for() >= PING_EVERY {
                out.send(&Message::Ping)?;
            }
        }
    }
}

fn connect(address: &str) -> Result<TcpStream> {
    let what = || format!("node: from {address}");
    let addresses = address.to_socket_addrs().map_err(|error| Error::io(what(), error))?;
    let mut last = None;
    for resolved in addresses {
        match TcpStream::connect_timeout(&resolved, Duration::from_secs(5)) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = Some(error),
        }
    }
    Err(Error::io(what(), last.unwrap_or_else(|| std::io::Error::other("the address resolves to nothing"))))
}

/// One link's files and what it does with each message.
struct Link<'a> {
    state: &'a Arc<State>,
    store: &'a WeakStore,
    out: Arc<Out>,
    held: HashMap<String, Held>,
    /// Files whose sending failed, asked for again once their time comes.
    retries: Vec<(String, Instant)>,
    /// The entry of each copy this link last said it holds.
    said: HashMap<String, u64>,
    /// The files blobs keeps beside blobs.db, fetched once a copy names them.
    #[cfg(feature = "blobs")]
    fetching: Fetching,
}

/// The files of blobs a link fetches: those under way, each with the file
/// its pieces are written to, and when to look for missing ones next.
#[cfg(feature = "blobs")]
#[derive(Default)]
struct Fetching {
    under_way: HashMap<String, Option<std::fs::File>>,
    due: bool,
    not_before: Option<Instant>,
}

impl Link<'_> {
    /// Follows each file served that this node takes and does not follow yet.
    fn follow_new(&mut self, served: &[String]) -> Result<()> {
        let mut wanted: HashSet<&str> =
            served.iter().map(String::as_str).filter(|name| self.state.takes(name)).collect();
        if let Some(files) = &self.state.options.files {
            wanted.extend(files.iter().map(String::as_str));
        }
        let mut wanted: Vec<&str> = wanted.into_iter().filter(|name| plain_name(name)).collect();
        wanted.sort_unstable();
        for name in wanted {
            if !self.held.contains_key(name) {
                self.ask(name)?;
            }
        }
        Ok(())
    }

    /// Asks for `name` from where this node's copy of it stands.
    fn ask(&mut self, name: &str) -> Result<()> {
        let at = match self.state.copy(name) {
            Some(copy) => {
                let at = copy.point();
                self.held.insert(name.to_owned(), Held::Whole(copy));
                self.blobs_changed(name);
                at
            }
            None => {
                self.held.insert(name.to_owned(), Held::Asked);
                Point::default()
            }
        };
        self.out.send(&Message::Follow { file: name.to_owned(), at })
    }

    /// Looks for the files of blobs a copy of blobs.db names once it changed.
    fn blobs_changed(&mut self, file: &str) {
        #[cfg(feature = "blobs")]
        if file == crate::blobs::FILE_IN_STORE {
            self.fetching.due = true;
        }
        let _ = file;
    }

    /// Asks for each file of blobs the copy names and this node lacks.
    #[cfg(feature = "blobs")]
    fn fetch_due(&mut self) -> Result<()> {
        let now = Instant::now();
        if !self.fetching.due || self.fetching.not_before.is_some_and(|not_before| now < not_before) {
            return Ok(());
        }
        (self.fetching.due, self.fetching.not_before) = (false, Some(now + Duration::from_millis(300)));
        for path in crate::blobs::objects_missing(&self.state.dir)? {
            if !self.fetching.under_way.contains_key(&path) {
                self.out.send(&Message::Fetch { path: path.clone() })?;
                self.fetching.under_way.insert(path, None);
            }
        }
        Ok(())
    }

    /// Writes a piece of a file of blobs, and gives the file its name once
    /// whole: blobs never writes it again, so a name means its bytes.
    #[cfg(feature = "blobs")]
    fn object(&mut self, path: &str, offset: u64, bytes: &[u8], last: bool) -> Result<()> {
        use std::io::Write;
        let Some(written) = self.fetching.under_way.get_mut(path) else {
            return Ok(());
        };
        let target = self.state.dir.join(path);
        let mut temp = target.as_os_str().to_owned();
        temp.push(".part");
        let temp = std::path::PathBuf::from(temp);
        let io = |error| Error::io(temp.display().to_string(), error);
        if offset == 0 {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            *written = Some(std::fs::File::create(&temp).map_err(io)?);
        }
        let Some(file) = written.as_mut() else {
            return Err(Error::corrupt(format!("{path}: a piece of a file whose first never came")));
        };
        file.write_all(bytes).map_err(io)?;
        if last {
            file.sync_all().map_err(io)?;
            self.fetching.under_way.remove(path);
            std::fs::rename(&temp, &target).map_err(io)?;
        }
        Ok(())
    }

    /// Tells the other node the entry each copy holds now, where it moved:
    /// what a synchronous home's commits wait for.
    fn say_applied(&mut self) -> Result<()> {
        for (file, held) in &self.held {
            if let Held::Whole(copy) = held {
                let applied = copy.applied();
                if self.said.get(file).is_none_or(|said| *said < applied) {
                    self.out.send(&Message::Applied { file: file.clone(), sequence: applied })?;
                    self.said.insert(file.clone(), applied);
                }
            }
        }
        Ok(())
    }

    fn retry_due(&mut self) -> Result<()> {
        let now = Instant::now();
        let due: Vec<String> = self.retries.iter().filter(|(_, at)| *at <= now).map(|(name, _)| name.clone()).collect();
        self.retries.retain(|(_, at)| *at > now);
        for name in due {
            self.ask(&name)?;
        }
        Ok(())
    }

    fn take(&mut self, message: Message) -> Result<()> {
        match message {
            Message::SnapshotBegin { file, page_size, at } => self.begin(&file, page_size, at),
            Message::SnapshotPages { file, pages } => match self.held.get_mut(&file) {
                Some(Held::Taking(taking)) => taking.pages(&pages),
                _ => Err(Error::invalid(format!("{file}: pages of no snapshot under way"))),
            },
            Message::SnapshotEnd { file, ready_at } => {
                if let Some(Held::Taking(taking)) = self.held.get_mut(&file) {
                    taking.end(ready_at);
                }
                self.finish_when_whole(&file)
            }
            Message::Entry { file, bytes } => self.apply(&file, &bytes),
            #[cfg(feature = "blobs")]
            Message::Failed { file, .. } if crate::blobs::is_object(&file) => {
                self.fetching.under_way.remove(&file);
                (self.fetching.due, self.fetching.not_before) = (true, Some(Instant::now() + Duration::from_secs(2)));
                Ok(())
            }
            #[cfg(feature = "blobs")]
            Message::Object { path, offset, bytes, last } => self.object(&path, offset, &bytes, last),
            // a node holding an old home's file: the link goes on to the next node
            Message::Failed { kind: ErrorKind::Conflict, why, .. } => Err(Error::new(ErrorKind::Conflict, why)),
            Message::Failed { file, kind, why } => {
                tracing::debug!(target: "tinystore", %file, %kind, %why, "a node's file could not be sent yet");
                self.held.remove(&file);
                self.retries.push((file, Instant::now() + Duration::from_secs(2)));
                Ok(())
            }
            Message::Written { id, outcome, waits } => {
                self.state.upstream.answered(id, Written { outcome, waits });
                Ok(())
            }
            Message::Listed { files } => {
                *lock(&self.state.served) = Some(files.clone());
                self.follow_new(&files)
            }
            Message::Ping => Ok(()),
            other => Err(Error::invalid(format!("a link said {other:?}, which a follower is not sent"))),
        }
    }

    /// Starts putting a snapshot together beside the copy held before, which
    /// its readers read until the snapshot takes its place.
    fn begin(&mut self, file: &str, page_size: u32, at: Point) -> Result<()> {
        if !plain_name(file) {
            return Err(Error::invalid(format!("a link sent {file:?}, which is no file's name")));
        }
        let applied = self.epoch_applied(file)?;
        if at.epoch < applied {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "{file}: a snapshot at epoch {} from a node, and this copy applied epoch {applied}: an old home's file",
                    at.epoch
                ),
            ));
        }
        self.state.snapshots.fetch_add(1, Ordering::Relaxed);
        let dir = self.state.log_dir(file);
        let taking = Box::new(Taking::begin(&dir, &self.state.dir.join(file), page_size, at)?);
        self.held.insert(file.to_owned(), Held::Taking(taking));
        Ok(())
    }

    /// The epoch of the last entry this node's copy of `file` applied, kept
    /// until a snapshot replaces the copy.
    fn epoch_applied(&self, file: &str) -> Result<u64> {
        let kept = copy::Position::read_point(&self.state.log_dir(file).join(copy::Position::FILE))?;
        Ok(kept.map_or(0, |point| point.epoch))
    }

    fn apply(&mut self, file: &str, bytes: &[u8]) -> Result<()> {
        let Some((entry, _)) = entry::decode(bytes)? else {
            return Err(Error::corrupt(format!("{file}: an entry cut short")));
        };
        let applied = match self.held.get_mut(file) {
            Some(Held::Taking(taking)) => taking.apply(&entry),
            Some(Held::Whole(copy)) => copy.apply(&entry, Some(bytes)),
            Some(Held::Asked) | None => return Ok(()),
        };
        match applied {
            Ok(()) => {
                self.blobs_changed(file);
                self.finish_when_whole(file)
            }
            Err(error) if matches!(error.kind(), ErrorKind::Conflict | ErrorKind::Invalid) => {
                tracing::warn!(target: "tinystore", %file, %error, "a copy starts again from a snapshot");
                self.held.insert(file.to_owned(), Held::Asked);
                self.out.send(&Message::Follow { file: file.to_owned(), at: Point::default() })
            }
            Err(error) => Err(error),
        }
    }

    /// Gives a snapshot whose pages are all one commit's the copy's name,
    /// and opens it as the copy its engines read.
    fn finish_when_whole(&mut self, file: &str) -> Result<()> {
        let whole = matches!(self.held.get(file), Some(Held::Taking(taking)) if taking.whole());
        if !whole {
            return Ok(());
        }
        let store = self.store.upgrade().ok_or_else(|| Error::closed("the node's store"))?;
        // the engines reading the copy held before let go of its file, and the
        // copy its writer, so that the snapshot takes its name: an open file
        // keeps its name on Windows, and elsewhere a reader would go on
        // reading the file it replaced
        let paused = self.state.pause_readers(file)?;
        let Some(Held::Taking(taking)) = self.held.remove(file) else {
            return Ok(());
        };
        self.state.drop_copy(file);
        if let Err(error) = (*taking).finish() {
            if copy::is_ready(&self.state.log_dir(file), &self.state.dir.join(file)) {
                self.state.keep_copy(file, Arc::new(Copy::open(&store, file)?));
            }
            return Err(error);
        }
        let copy = Arc::new(Copy::open(&store, file)?);
        self.state.keep_copy(file, Arc::clone(&copy));
        drop(paused);
        // what an engine kept of a copy taken again is read again
        store.applied(file);
        drop(store);
        self.held.insert(file.to_owned(), Held::Whole(copy));
        tracing::info!(target: "tinystore", %file, "a node holds a whole copy of a file");
        self.blobs_changed(file);
        Ok(())
    }
}
