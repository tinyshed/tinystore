//! A node's server: the nodes that take files from it connect here, and
//! each link gets a thread that reads what the other node asks and one that
//! sends it each file's entries as its log grows.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::entry::{self, Kind, MAGIC, Point};
use super::log::{segment_path, segments_in};
use super::message::{Frames, LeaseAsk, LeaseHolder, Message, VERSION, plain_name, timed_out};
use super::node::{Following, State, lock};
use super::pack::Packer;
use super::snapshot::{self, Part};
use super::transport::{self, Reading, Writing};
use crate::{Error, ErrorKind, Result};

/// How long a link may say nothing before its other end pings it.
pub(crate) const PING_EVERY: Duration = Duration::from_secs(5);
/// How long a link may stay silent before it is taken for dead.
pub(crate) const SILENT_AT_MOST: Duration = Duration::from_secs(30);
/// The bytes of entries a link sends of one file before it turns to the next.
const TURN: usize = 8 << 20;

/// Listens at `address`, a thread accepting the links of other nodes.
pub(crate) fn start(state: &Arc<State>, address: &str) -> Result<()> {
    let listener = TcpListener::bind(address).map_err(|error| Error::io(format!("node: serve {address}"), error))?;
    let bound = listener.local_addr().map_err(|error| Error::io(format!("node: serve {address}"), error))?;
    *lock(&state.listening) = Some(bound);
    tracing::info!(target: "tinystore", address = %bound, "a node serves its files");
    let accepting = Arc::clone(state);
    state.spawn("tinystore node: serve", move || accept(&accepting, &listener));
    Ok(())
}

fn accept(state: &Arc<State>, listener: &TcpListener) {
    for stream in listener.incoming() {
        if state.stopping() {
            return;
        }
        let Ok(stream) = stream else { continue };
        let peer = stream.peer_addr().ok();
        state.keep_link(&stream);
        let serving = Arc::clone(state);
        state.spawn("tinystore node: link", move || {
            if let Err(error) = serve_link(&serving, stream)
                && !serving.stopping()
            {
                tracing::info!(target: "tinystore", peer = ?peer, %error, "a node's link ended");
            }
        });
    }
}

/// What a link sends, shared by its threads.
pub(crate) struct Out {
    stream: Mutex<(Writing, Option<Packer>)>,
    last: Mutex<Instant>,
}

impl Out {
    pub(crate) fn new(stream: Writing) -> Out {
        Out { stream: Mutex::new((stream, None)), last: Mutex::new(Instant::now()) }
    }

    pub(crate) fn send(&self, message: &Message) -> Result<()> {
        let bytes = message.encode();
        let mut held = lock(&self.stream);
        let (stream, packer) = &mut *held;
        let sent = match packer {
            Some(packer) => packer.pack(&bytes).and_then(|packed| stream.write_all(&packed)),
            None => stream.write_all(&bytes),
        };
        sent.map_err(|error| Error::io("a link", error))?;
        *lock(&self.last) = Instant::now();
        Ok(())
    }

    /// Sends what follows through the link's stream.
    pub(crate) fn pack(&self) -> Result<()> {
        let packer = Packer::new().map_err(|error| Error::io("a link's stream", error))?;
        lock(&self.stream).1 = Some(packer);
        Ok(())
    }

    pub(crate) fn idle_for(&self) -> Duration {
        lock(&self.last).elapsed()
    }
}

/// The files a link follows, and the follows it has not begun yet.
#[derive(Default)]
struct Asked {
    follows: Vec<(String, Point)>,
    ended: bool,
    /// The node at the link's other end, among the node's followers.
    following: Option<Following>,
}

fn serve_link(state: &Arc<State>, stream: TcpStream) -> Result<()> {
    let address = stream.peer_addr().map_or_else(|_| "an unknown address".to_owned(), |at| at.to_string());
    let (mut reading, writing) = transport::accept(state, stream)?;
    reading.set_timeout(Duration::from_secs(1))?;
    writing.set_timeout(SILENT_AT_MOST)?;
    let out = Arc::new(Out::new(writing));
    let mut frames = Frames::default();
    let hello = read_one(state, &mut reading, &mut frames, SILENT_AT_MOST)?;
    let Some(Message::Hello { version, token, node, address: said }) = hello else {
        return Err(Error::invalid("a link began with no hello"));
    };
    if version != VERSION {
        out.send(&Message::Refused { why: format!("this node speaks the link's version {VERSION}, not {version}") })?;
        return Ok(());
    }
    if !token_admits(state.options.token.as_deref(), &token) {
        out.send(&Message::Refused { why: "the token is not this node's".to_owned() })?;
        return Ok(());
    }
    out.send(&Message::Welcome { files: state.files_served()? })?;
    out.pack()?;
    frames.unpack_from_here()?;
    // a node is known by the address it said, or where its link comes from
    let address = if said.is_empty() { address } else { said };
    let asked = Arc::new(Mutex::new(Asked { following: Some(state.following(node, address)), ..Asked::default() }));
    let sending = (Arc::clone(state), Arc::clone(&out), Arc::clone(&asked));
    state.spawn("tinystore node: send", move || {
        let (state, out, asked) = sending;
        if let Err(error) = send_files(&state, &out, &asked)
            && !state.stopping()
        {
            tracing::info!(target: "tinystore", %error, "a node stopped sending on a link");
        }
        lock(&asked).ended = true;
    });
    let mut following = false;
    let ended = read_requests(state, &mut reading, &mut frames, &out, &asked, &mut following);
    let mut asked_last = lock(&asked);
    asked_last.ended = true;
    if following && let Some(gone) = asked_last.following.take() {
        state.acks.unlinked(gone.node());
    }
    drop(asked_last);
    state.notify.appended();
    ended
}

/// Reads what the other node asks until the link ends.
fn read_requests(
    state: &Arc<State>,
    reading: &mut Reading,
    frames: &mut Frames,
    out: &Arc<Out>,
    asked: &Arc<Mutex<Asked>>,
    following: &mut bool,
) -> Result<()> {
    loop {
        let Some(message) = read_one(state, reading, frames, SILENT_AT_MOST)? else {
            return Ok(());
        };
        if lock(asked).ended {
            return Ok(());
        }
        match message {
            Message::Follow { file, at } => {
                if !plain_name(&file) {
                    return Err(Error::invalid(format!("a link asked for {file:?}, which is no file's name")));
                }
                let mut held = lock(asked);
                held.follows.push((file, at));
                if !*following && let Some(follower) = &held.following {
                    *following = true;
                    state.acks.linked(follower.node(), follower.address());
                }
                drop(held);
                state.notify.appended();
            }
            Message::List => out.send(&Message::Listed { files: state.files_served()? })?,
            Message::Write { id, op } => {
                let (state, out) = (Arc::clone(state), Arc::clone(out));
                state.clone().spawn("tinystore node: write", move || {
                    let written = super::forward::run(&state, &op);
                    let _ = out.send(&written.into_message(id));
                });
            }
            Message::Ping => {}
            Message::Lease { node, address, asks } => {
                let Some(witness) = &state.witness else {
                    return Err(Error::invalid("a lease asked of a node that is no witness"));
                };
                let mut holders = Vec::with_capacity(asks.len());
                for asked in asks {
                    let LeaseAsk { file, epoch, position, holding, synchronous } = asked;
                    let (node, address, synchronous) = (&node, &address, &synchronous);
                    let ask = super::witness::Ask { file: &file, node, address, epoch, position, holding, synchronous };
                    if let Some(holder) = witness.ask(&ask)? {
                        holders.push(LeaseHolder { file, holder });
                    }
                }
                if !holders.is_empty() {
                    out.send(&Message::Leased { holders })?;
                }
            }
            Message::Verify { file } => {
                let (state, out) = (Arc::clone(state), Arc::clone(out));
                state.clone().spawn("tinystore node: verify", move || {
                    let answer = match super::verify::here(&state, &file) {
                        Ok((at, sum)) => Message::Verified { file, at, sum: sum.to_vec(), why: String::new() },
                        Err(error) => {
                            Message::Verified { file, at: Point::default(), sum: Vec::new(), why: error.to_string() }
                        }
                    };
                    let _ = out.send(&answer);
                });
            }
            Message::Applied { file, sequence } => {
                if let Some(following) = &lock(asked).following {
                    state.acks.held(following.node(), &file, sequence);
                    following.applied(&file, sequence);
                }
            }
            #[cfg(feature = "blobs")]
            Message::Fetch { path } => {
                if !crate::blobs::is_object(&path) {
                    return Err(Error::invalid(format!("a link asked for {path:?}, which is no file blobs keeps")));
                }
                let (state, out) = (Arc::clone(state), Arc::clone(out));
                state.clone().spawn("tinystore node: fetch", move || {
                    if let Err(error) = send_object(&state, &out, &path) {
                        let _ = out.send(&Message::Failed { file: path, kind: error.kind(), why: error.to_string() });
                    }
                });
            }
            other => return Err(Error::invalid(format!("a link said {other:?}, which a server is not asked"))),
        }
    }
}

/// Sends a file blobs keeps, in pieces of a mebibyte.
#[cfg(feature = "blobs")]
fn send_object(state: &State, out: &Out, path: &str) -> Result<()> {
    let full = state.dir.join(path);
    let mut file = fs::File::open(&full).map_err(|error| Error::io(full.display().to_string(), error))?;
    let mut offset = 0;
    loop {
        let mut piece = vec![0; 1 << 20];
        let mut filled = 0;
        while filled < piece.len() {
            let read = file.read(&mut piece[filled..]).map_err(|error| Error::io(full.display().to_string(), error))?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        piece.truncate(filled);
        let last = filled < 1 << 20;
        out.send(&Message::Object { path: path.to_owned(), offset, bytes: piece, last })?;
        if last {
            return Ok(());
        }
        offset += filled as u64;
    }
}

/// Sends the files of blobs this node holds past those sent on the link
/// already, before the entries of blobs.db that name them: the link carries
/// them in order, so a copy holds a file before the commit that names it. The
/// files older than the link's first entries the copy fetches itself.
#[cfg(feature = "blobs")]
fn send_objects_first(state: &State, out: &Out, sent: &mut Option<i64>) -> Result<()> {
    let Some(after) = *sent else {
        *sent = Some(crate::blobs::objects_after(&state.dir, 0)?.last().map_or(0, |&(id, _)| id));
        return Ok(());
    };
    for (id, path) in crate::blobs::objects_after(&state.dir, after)? {
        send_object(state, out, &path)?;
        *sent = Some(id);
    }
    Ok(())
}

/// Reads the next message, `None` once the link ended; a link silent for
/// `at_most` is an error.
fn read_one(state: &State, reading: &mut Reading, frames: &mut Frames, at_most: Duration) -> Result<Option<Message>> {
    let began = Instant::now();
    loop {
        if let Some(message) = frames.next()? {
            return Ok(Some(message));
        }
        if state.stopping() {
            return Ok(None);
        }
        match frames.fill(reading) {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(error) if timed_out(&error) => {
                if began.elapsed() > at_most {
                    return Err(Error::new(ErrorKind::Unavailable, "a link said nothing for too long"));
                }
            }
            Err(error) => return Err(Error::io("a link", error)),
        }
    }
}

/// Compares the token a node gave with this node's in time that does not
/// depend on where they differ.
fn token_admits(expected: Option<&str>, given: &str) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let (expected, given) = (expected.as_bytes(), given.as_bytes());
    let mut differs = expected.len() ^ given.len();
    for (index, byte) in expected.iter().enumerate() {
        differs |= usize::from(byte ^ given.get(index).copied().unwrap_or(0));
    }
    differs == 0
}

/// Where a link stands in sending one file.
enum Sending {
    /// Asked from `at`: where to begin is found in the log.
    Asked(Point),
    Snapshot,
    /// The entries after `last`, read from the segment that begins at
    /// `segment`, `offset` bytes in.
    Entries {
        last: Point,
        segment: u64,
        offset: u64,
    },
    /// The file could not be sent; asked again from `at` once `until` passes.
    Later {
        at: Point,
        until: Instant,
    },
}

struct Followed {
    file: String,
    sending: Sending,
    /// The newest file of blobs sent on this link before the commits naming
    /// it, for blobs.db; none until its entries began.
    #[cfg(feature = "blobs")]
    objects: Option<i64>,
}

/// Sends each file followed: a snapshot where needed, then its entries as
/// its log grows, a turn of each file at a time.
fn send_files(state: &Arc<State>, out: &Out, asked: &Mutex<Asked>) -> Result<()> {
    let mut followed: Vec<Followed> = Vec::new();
    let mut listed = state.notify.served();
    loop {
        let seen = state.notify.seen();
        // a file this node began to serve is told at once, not at the next ask
        if state.notify.served() != listed {
            listed = state.notify.served();
            out.send(&Message::Listed { files: state.files_served()? })?;
        }
        {
            let mut asked = lock(asked);
            if asked.ended || state.stopping() {
                return Ok(());
            }
            for (file, at) in asked.follows.drain(..) {
                followed.retain(|known| known.file != file);
                followed.push(Followed {
                    file,
                    sending: Sending::Asked(at),
                    #[cfg(feature = "blobs")]
                    objects: None,
                });
            }
        }
        let mut sent = false;
        for one in &mut followed {
            sent |= send_some(state, out, one)?;
        }
        if !sent {
            if out.idle_for() >= PING_EVERY {
                out.send(&Message::Ping)?;
            }
            state.notify.wait_past(seen, Duration::from_millis(500));
        }
    }
}

/// Sends what is next of one file; whether it sent anything.
fn send_some(state: &State, out: &Out, followed: &mut Followed) -> Result<bool> {
    let file = followed.file.clone();
    let dir = state.log_dir(&file);
    loop {
        match followed.sending {
            Sending::Later { at, until } => {
                if Instant::now() < until {
                    return Ok(false);
                }
                followed.sending = Sending::Asked(at);
            }
            Sending::Asked(at) => {
                followed.sending = match begin_at(state, &file, &dir, at) {
                    Ok(sending) => sending,
                    Err(error) => later(out, &file, at, &error)?,
                };
            }
            Sending::Snapshot => {
                followed.sending = match send_snapshot(state, out, &file) {
                    Ok(sending) => sending,
                    Err(error) if error.kind() == ErrorKind::Io && error.what() == "a link" => return Err(error),
                    Err(error) => later(out, &file, Point::default(), &error)?,
                };
                return Ok(true);
            }
            Sending::Entries { last, segment, offset } => {
                #[cfg(feature = "blobs")]
                if file == crate::blobs::FILE_IN_STORE {
                    send_objects_first(state, out, &mut followed.objects)?;
                }
                let (sending, sent) = send_entries(out, &file, &dir, last, segment, offset)?;
                followed.sending = sending;
                return Ok(sent);
            }
        }
    }
}

/// Tells the other node the file cannot be sent now, and asks again later.
fn later(out: &Out, file: &str, at: Point, error: &Error) -> Result<Sending> {
    out.send(&Message::Failed { file: file.to_owned(), kind: error.kind(), why: error.to_string() })?;
    Ok(Sending::Later { at, until: Instant::now() + Duration::from_secs(2) })
}

/// Where to begin sending `file` to a node that holds it up to `at`.
fn begin_at(state: &State, file: &str, dir: &std::path::Path, at: Point) -> Result<Sending> {
    let sending = where_to_begin(state, file, dir, at)?;
    if matches!(sending, Sending::Snapshot) && at.sequence > 0 {
        check_epochs(state, file, dir, at)?;
    }
    Ok(sending)
}

/// A node that applied entries of `file` takes no snapshot of it from a node
/// holding an older epoch than it has seen: that node's file is an old home's,
/// come back without its demotion. What a node held of the old home past the
/// point a promotion went on from, the new home counts as lost.
fn check_epochs(state: &State, file: &str, dir: &std::path::Path, at: Point) -> Result<()> {
    let Some(source) = state.source(file) else {
        return Ok(());
    };
    let held = source.stable()?;
    if at.epoch > held.epoch {
        // this node writes the file still: it is the old home, which learns here
        if state.copy(file).is_none() && !super::copy::is_copy(dir) {
            state.fence(file, at.epoch)?;
        }
        return Err(Error::new(
            ErrorKind::Conflict,
            format!(
                "{file}: the node that asks has applied epoch {} of it, and this node holds epoch {}: an old home's file",
                at.epoch, held.epoch
            ),
        ));
    }
    if let Some(promoted) = super::promote::promoted_at(dir)?
        && at.epoch < promoted.epoch
        && at.sequence > promoted.sequence
    {
        let lost = at.sequence - promoted.sequence;
        state.lost.fetch_add(lost, Ordering::Relaxed);
        tracing::warn!(
            target: "tinystore",
            file,
            lost,
            epoch = at.epoch,
            "a node held commits of the old home that this home never had: they are lost, and it takes a snapshot"
        );
    }
    Ok(())
}

/// Where to begin by the entries this node keeps of `file`.
fn where_to_begin(state: &State, file: &str, dir: &std::path::Path, at: Point) -> Result<Sending> {
    let Some(source) = state.source(file) else {
        return Err(Error::not_found(format!("{file}: this node holds no such file")));
    };
    if at.sequence == 0 {
        return Ok(Sending::Snapshot);
    }
    let segments = segments_in(dir)?;
    let Some(&segment) = segments.iter().rev().find(|&&first| first <= at.sequence + 1) else {
        // a log that kept no entry since its snapshot, or since a promotion,
        // stands where the other node does: its next entry is the one it needs
        let stable = source.stable()?;
        if stable == at {
            return Ok(Sending::Entries { last: at, segment: at.sequence + 1, offset: MAGIC.len() as u64 });
        }
        if stable.sequence < at.sequence && state.copy(file).is_some() {
            return Err(Error::new(ErrorKind::Unavailable, "this copy is behind the node that asks"));
        }
        return Ok(Sending::Snapshot);
    };
    let bytes = read_from(&segment_path(dir, segment), 0)?;
    let mut offset = MAGIC.len();
    while let Some((entry, used)) = entry::decode(&bytes[offset..]).unwrap_or(None) {
        if entry.sequence == at.sequence {
            if entry.check != at.check {
                return Ok(Sending::Snapshot);
            }
            return Ok(Sending::Entries { last: at, segment, offset: (offset + used) as u64 });
        }
        if entry.sequence == at.sequence + 1 {
            if entry.previous != at.check {
                return Ok(Sending::Snapshot);
            }
            return Ok(Sending::Entries { last: at, segment, offset: offset as u64 });
        }
        offset += used;
    }
    let stable = source.stable()?;
    if stable == at {
        return Ok(Sending::Entries { last: at, segment, offset: offset as u64 });
    }
    if stable.sequence < at.sequence && state.copy(file).is_some() {
        return Err(Error::new(ErrorKind::Unavailable, "this copy is behind the node that asks"));
    }
    Ok(Sending::Snapshot)
}

fn send_snapshot(state: &State, out: &Out, file: &str) -> Result<Sending> {
    let source = state.source(file).ok_or_else(|| Error::not_found(format!("{file}: this node holds no such file")))?;
    let at = snapshot::read(source.as_ref(), |part| {
        let file = file.to_owned();
        out.send(&match part {
            Part::Begin { page_size, at } => Message::SnapshotBegin { file, page_size, at },
            Part::Pages(pages) => Message::SnapshotPages { file, pages },
            Part::End { ready_at } => Message::SnapshotEnd { file, ready_at },
        })
    })?;
    let dir = state.log_dir(file);
    let segment = segments_in(&dir)?.into_iter().rev().find(|&first| first <= at.sequence + 1);
    match segment {
        Some(segment) => Ok(Sending::Entries { last: at, segment, offset: MAGIC.len() as u64 }),
        None => Ok(Sending::Entries { last: at, segment: at.sequence + 1, offset: MAGIC.len() as u64 }),
    }
}

/// Sends the entries after `last` that the log holds now, a turn's worth.
fn send_entries(
    out: &Out,
    file: &str,
    dir: &std::path::Path,
    mut last: Point,
    mut segment: u64,
    mut offset: u64,
) -> Result<(Sending, bool)> {
    let mut sent = 0;
    loop {
        let path = segment_path(dir, segment);
        if !path.exists() {
            // Pruned, or the log started again: read from the segment that
            // holds the next entry, if one does.
            let segments = segments_in(dir)?;
            match segments.iter().rev().find(|&&first| first <= last.sequence + 1) {
                Some(&first) if first != segment => (segment, offset) = (first, MAGIC.len() as u64),
                _ if segments.iter().any(|&first| first > last.sequence + 1) => {
                    return Ok((Sending::Snapshot, sent > 0));
                }
                _ => return Ok((Sending::Entries { last, segment, offset }, sent > 0)),
            }
            continue;
        }
        let newer = segments_in(dir)?.into_iter().find(|&first| first > segment);
        let bytes = read_from(&path, offset)?;
        let mut at = 0;
        while let Some((entry, used)) = entry::decode(&bytes[at..])? {
            if entry.sequence > last.sequence {
                let restarts = entry.kind == Kind::Gap || (entry.kind == Kind::Begin && entry.size > 1);
                if !entry.follows(&last) || restarts {
                    return Ok((Sending::Snapshot, sent > 0));
                }
                out.send(&Message::Entry { file: file.to_owned(), bytes: bytes[at..at + used].to_vec() })?;
                last = entry.point();
                sent += used;
            }
            at += used;
            if sent >= TURN {
                return Ok((Sending::Entries { last, segment, offset: offset + at as u64 }, true));
            }
        }
        offset += at as u64;
        // A segment is appended to no more once a newer one began, so with
        // the newer seen before the read, this one was read to its end.
        match newer {
            Some(next) if next == last.sequence + 1 => (segment, offset) = (next, MAGIC.len() as u64),
            Some(_) => return Ok((Sending::Snapshot, sent > 0)),
            None => return Ok((Sending::Entries { last, segment, offset }, sent > 0)),
        }
    }
}

/// The bytes of a segment from `offset` to its end as it stands.
fn read_from(path: &PathBuf, offset: u64) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path).map_err(|error| Error::io(path.display().to_string(), error))?;
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_to_end(&mut bytes))
        .map_err(|error| Error::io(path.display().to_string(), error))?;
    Ok(bytes)
}
