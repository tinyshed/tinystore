//! blobs over protocol 2: files by handle. A get answers what the file
//! carries and its first bytes, and the rest goes as pieces within the
//! client's credit, read from the file as credit comes and checked as it is
//! read. An upload's pieces come as the client's DATA, written in order as
//! they come, the credit of each given back once it is written, and its last
//! DATA publishes the file.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use super::Route;
use super::codec::Message;
use super::protocol::{
    BlobsAt, BlobsDelete, BlobsExpire, BlobsFolder, BlobsFound, BlobsGot, BlobsHead, BlobsInfo, BlobsList, BlobsMove,
    BlobsOpen, BlobsPage, BlobsPiece, BlobsUsage, BlobsWrite, BlobsWritten, Empty, Failure, Handle, method,
};
use crate::blobs::{FileCall, FileInfo, Files, StoredFile, Upload};
use crate::{Store, unix_millis};

pub(crate) type Answered = Result<Vec<u8>, Failure>;
pub(crate) type Sender = Arc<dyn Fn(Vec<u8>) + Send + Sync>;
pub(crate) type Finisher = Arc<dyn Fn(Answered) + Send + Sync>;
pub(crate) type Granter = Arc<dyn Fn(u32) + Send + Sync>;
pub(crate) type Spawn = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::BLOBS_GET => Route::Get,
        method::BLOBS_UPLOAD => Route::Upload,
        _ => Route::Worker,
    }
}

/// What a stream of a file's bytes needs of its session.
#[derive(Clone)]
pub(crate) struct Link {
    /// A DATA on the stream.
    pub(crate) send: Sender,
    /// The stream's last frame.
    pub(crate) finish: Finisher,
    /// Credit given back to the client for DATA it sent on the stream.
    pub(crate) grant: Granter,
    /// Work on the session's workers, where a file is read or written.
    pub(crate) spawn: Spawn,
    /// The DATA bytes the client takes before it grants more.
    pub(crate) credit: u64,
    pub(crate) max_body: usize,
}

/// What a connection opened, by handle, and its streams of a file's bytes.
#[derive(Default)]
pub(crate) struct Handles {
    open: Mutex<HashMap<u64, Files>>,
    last: AtomicU64,
    gets: Arc<Mutex<HashMap<u32, Arc<Getting>>>>,
    uploads: Arc<Mutex<HashMap<u32, Arc<Uploading>>>>,
}

impl Handles {
    /// The files a handle names, in the folder of these segments.
    fn at(&self, handle: u64, folder: &[String]) -> Result<Files, Failure> {
        let files = lock(&self.open)
            .get(&handle)
            .cloned()
            .ok_or_else(|| Failure::invalid(format!("handle {handle} names no files this connection opened")))?;
        Ok(folder.iter().fold(files, |files, segment| files.folder(segment.as_str())))
    }

    /// Credit the client granted a get's stream.
    pub(crate) fn grant(&self, stream: u32, credit: u32) {
        let getting = lock(&self.gets).get(&stream).cloned();
        if let Some(getting) = getting {
            getting.grant(credit);
        }
    }

    /// The upload a client's DATA on `stream` is a piece of.
    pub(crate) fn uploading(&self, stream: u32) -> Option<Arc<Uploading>> {
        lock(&self.uploads).get(&stream).cloned()
    }

    /// Ends a get or an upload the client cancelled; says whether there was one.
    pub(crate) fn cancel(&self, stream: u32) -> bool {
        // out of its map before it ends, which takes the map's lock again:
        // an `if let` on the lock would hold it and wait for itself
        let getting = lock(&self.gets).remove(&stream);
        if let Some(getting) = getting {
            getting.end(Err(Failure::cancelled("the client cancelled its get")));
            return true;
        }
        let uploading = lock(&self.uploads).remove(&stream);
        if let Some(uploading) = uploading {
            uploading.cancel();
            return true;
        }
        false
    }

    /// Lets go of every stream at the session's end: a file read is closed,
    /// an upload's bytes removed.
    pub(crate) fn end(&self) {
        let gets: Vec<_> = lock(&self.gets).drain().map(|(_, getting)| getting).collect();
        for getting in gets {
            getting.drop_file();
        }
        let uploads: Vec<_> = lock(&self.uploads).drain().map(|(_, uploading)| uploading).collect();
        for uploading in uploads {
            uploading.cancel();
        }
    }
}

/// Answers a call of one message: everything but a get and an upload.
pub(crate) fn call(store: &Store, handles: &Handles, called: u16, body: &[u8]) -> Answered {
    match called {
        method::BLOBS_OPEN => {
            let asked = BlobsOpen::decode(body)?;
            let mut files = store.files(&asked.name);
            if let Some(ttl) = asked.ttl {
                files = files.ttl(Duration::from_millis(ttl));
            }
            if let Some(most) = asked.max_file_size {
                files = files.max_file_size(most);
            }
            let files = files.open()?;
            let handle = handles.last.fetch_add(1, Ordering::Relaxed) + 1;
            lock(&handles.open).insert(handle, files);
            Ok(Handle { handle }.encode())
        }
        method::BLOBS_PUT => {
            let mut asked = BlobsWrite::decode(body)?;
            let files = handles.at(asked.handle, &asked.folder)?;
            let bytes = std::mem::take(&mut asked.bytes);
            let info = options(files.key(&asked.path), &asked).written(&bytes, asked.create)?;
            Ok(BlobsWritten { info: info.map(info_of) }.encode())
        }
        method::BLOBS_HEAD => {
            let asked = BlobsAt::decode(body)?;
            let info = handles.at(asked.handle, &asked.folder)?.head(&asked.path)?;
            Ok(BlobsHead { info: info.map(info_of) }.encode())
        }
        method::BLOBS_DELETE => {
            let asked = BlobsDelete::decode(body)?;
            let files = handles.at(asked.handle, &asked.folder)?;
            let mut call = files.key(&asked.path);
            if let Some(etag) = asked.if_match {
                call = call.if_match(etag);
            }
            call.delete()?;
            Ok(Empty {}.encode())
        }
        method::BLOBS_COPY | method::BLOBS_RENAME => {
            let asked = BlobsMove::decode(body)?;
            let files = handles.at(asked.handle, &asked.folder)?;
            let mut call = files.key(&asked.to);
            if let Some(etag) = asked.if_match {
                call = call.if_match(etag);
            }
            let info = if called == method::BLOBS_COPY {
                call.copy_from(&asked.from)?
            } else {
                call.rename_from(&asked.from)?
            };
            Ok(info_of(info).encode())
        }
        method::BLOBS_EXPIRE => {
            let asked = BlobsExpire::decode(body)?;
            let files = handles.at(asked.handle, &asked.folder)?;
            let found = files.expire(&asked.path, Duration::from_millis(asked.after))?;
            Ok(BlobsFound { found }.encode())
        }
        method::BLOBS_LIST => {
            let asked = BlobsList::decode(body)?;
            let files = handles.at(asked.handle, &asked.folder)?;
            let mut list = files.list().prefix(asked.prefix);
            if let Some(after) = asked.after {
                list = list.after(after);
            }
            if let Some(limit) = asked.limit {
                list = list.limit(usize::try_from(limit).unwrap_or(usize::MAX));
            }
            let page = list.page()?;
            Ok(BlobsPage { files: page.files.into_iter().map(info_of).collect(), next: page.next }.encode())
        }
        method::BLOBS_USAGE => {
            let asked = BlobsFolder::decode(body)?;
            let usage = handles.at(asked.handle, &asked.folder)?.usage()?;
            Ok(BlobsUsage { count: usage.count, size: usage.size }.encode())
        }
        method::BLOBS_CLEAR => {
            let asked = BlobsFolder::decode(body)?;
            handles.at(asked.handle, &asked.folder)?.clear()?;
            Ok(Empty {}.encode())
        }
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

/// A write's options from its message.
fn options<'f>(mut call: FileCall<'f>, asked: &BlobsWrite) -> FileCall<'f> {
    if let Some(content_type) = &asked.content_type {
        call = call.content_type(content_type.clone());
    }
    for (name, value) in asked.meta.iter().flatten() {
        call = call.meta(name.clone(), value.clone());
    }
    if let Some(ttl) = asked.ttl {
        call = call.ttl(Duration::from_millis(ttl));
    }
    if let Some(etag) = &asked.if_match {
        call = call.if_match(etag.clone());
    }
    if let Some(size) = asked.size {
        call = call.size(size);
    }
    call
}

fn info_of(info: FileInfo) -> BlobsInfo {
    BlobsInfo {
        path: info.path,
        size: info.size,
        etag: info.etag,
        content_type: info.content_type,
        last_modified: unix_millis(info.last_modified),
        expires: info.expires.map(unix_millis),
        meta: (!info.meta.is_empty()).then_some(info.meta),
    }
}

/// The largest piece of a file a message carries: half of what the client
/// takes at once, so that one grant lets the next piece go while it reads
/// the last, less the piece's own few bytes.
fn piece_bound(link: &Link) -> usize {
    let credit = usize::try_from(link.credit).unwrap_or(usize::MAX);
    (link.max_body.min(credit) / 2).saturating_sub(16).max(1)
}

/// How a get's answer goes: whole in the RESPONSE, or the RESPONSE and then
/// the rest of the file within the client's credit.
pub(crate) enum Got {
    Whole(Vec<u8>),
    Begun(Vec<u8>, Arc<Getting>),
}

/// Answers a get: what the file carries and its first bytes, and the rest to
/// come when they do not fit.
pub(crate) fn get(handles: &Handles, stream: u32, body: &[u8], link: &Link) -> Result<Got, Failure> {
    let asked = BlobsAt::decode(body)?;
    let files = handles.at(asked.handle, &asked.folder)?;
    let Some(mut file) = files.get(&asked.path)? else {
        return Ok(Got::Whole(BlobsGot { info: None, bytes: Vec::new() }.encode()));
    };
    let piece = piece_bound(link);
    let first = read_piece(&mut file, piece)?;
    let finished = first.len() < piece || file.info().size == first.len() as u64;
    let response = BlobsGot { info: Some(info_of(file.info().clone())), bytes: first }.encode();
    if finished {
        return Ok(Got::Whole(response));
    }
    let state = GetState { file: Some(file), credit: link.credit, busy: false, ended: false };
    let getting = Arc::new(Getting {
        stream,
        piece,
        state: Mutex::new(state),
        link: link.clone(),
        gets: Arc::downgrade(&handles.gets),
    });
    lock(&handles.gets).insert(stream, Arc::clone(&getting));
    Ok(Got::Begun(response, getting))
}

/// Reads the next piece of a file, at most `bound` bytes: fewer only at its
/// end, whose read is the one its check holds back on.
fn read_piece(file: &mut StoredFile, bound: usize) -> Result<Vec<u8>, Failure> {
    let mut piece = vec![0; bound];
    let mut filled = 0;
    while filled < bound {
        match file.read(&mut piece[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) => return Err(Failure::from(read_error(error))),
        }
    }
    piece.truncate(filled);
    Ok(piece)
}

/// The store's error a read carried, or an I/O one.
fn read_error(error: std::io::Error) -> crate::Error {
    match error.into_inner().map(|inner| inner.downcast::<crate::Error>()) {
        Some(Ok(error)) => *error,
        Some(Err(inner)) => crate::Error::io("blobs: read a file", std::io::Error::other(inner)),
        None => crate::Error::internal("blobs: a read failed"),
    }
}

/// A get's file going out a piece at a time within the client's credit.
pub(crate) struct Getting {
    stream: u32,
    piece: usize,
    state: Mutex<GetState>,
    link: Link,
    gets: Weak<Mutex<HashMap<u32, Arc<Getting>>>>,
}

struct GetState {
    /// The file, here between two runs of the pieces that go.
    file: Option<StoredFile>,
    credit: u64,
    /// A worker sends pieces now.
    busy: bool,
    ended: bool,
}

impl Getting {
    fn grant(self: &Arc<Self>, credit: u32) {
        lock(&self.state).credit += u64::from(credit);
        self.pump();
    }

    /// Sends what the credit takes, on a worker, unless one does already.
    pub(crate) fn pump(self: &Arc<Self>) {
        {
            let mut state = lock(&self.state);
            if state.busy || state.ended {
                return;
            }
            state.busy = true;
        }
        let getting = Arc::clone(self);
        (self.link.spawn)(Box::new(move || getting.send_pieces()));
    }

    fn send_pieces(&self) {
        // a piece's message: its bytes and a few of its own
        let room = (self.piece + 16) as u64;
        loop {
            let mut file = {
                let mut state = lock(&self.state);
                match state.file.take() {
                    Some(file) if !state.ended && state.credit >= room => file,
                    file => {
                        state.file = file;
                        state.busy = false;
                        return;
                    }
                }
            };
            match read_piece(&mut file, self.piece) {
                Err(failure) => return self.end(Err(failure)),
                Ok(bytes) => {
                    let last = bytes.len() < self.piece;
                    if !bytes.is_empty() {
                        let body = BlobsPiece { bytes }.encode();
                        // the message's bytes, which the client gives back, and not the room:
                        // counted apart, the two drift until the stream waits for good
                        lock(&self.state).credit -= body.len() as u64;
                        (self.link.send)(body);
                    }
                    if last {
                        return self.end(Ok(Empty {}.encode()));
                    }
                }
            }
            lock(&self.state).file = Some(file);
        }
    }

    fn end(&self, last: Answered) {
        {
            let mut state = lock(&self.state);
            if std::mem::replace(&mut state.ended, true) {
                return;
            }
            state.file = None;
        }
        if let Some(gets) = self.gets.upgrade() {
            lock(&gets).remove(&self.stream);
        }
        (self.link.finish)(last);
    }

    fn drop_file(&self) {
        let mut state = lock(&self.state);
        state.ended = true;
        state.file = None;
    }
}

/// Begins an upload: its file to write and the client's pieces to come.
pub(crate) fn upload(handles: &Handles, stream: u32, body: &[u8], link: &Link) -> Result<(), Failure> {
    let asked = BlobsWrite::decode(body)?;
    let files = handles.at(asked.handle, &asked.folder)?;
    let upload = options(files.key(&asked.path), &asked).upload()?;
    let state =
        UploadState { upload: Some(upload), pieces: VecDeque::new(), busy: false, cancelled: false, ended: false };
    let uploading = Arc::new(Uploading {
        stream,
        create: asked.create,
        state: Mutex::new(state),
        link: link.clone(),
        uploads: Arc::downgrade(&handles.uploads),
    });
    lock(&handles.uploads).insert(stream, uploading);
    Ok(())
}

/// An upload under way: the client's pieces, written in order on a worker.
pub(crate) struct Uploading {
    stream: u32,
    /// Its last DATA writes only where no file is.
    create: bool,
    state: Mutex<UploadState>,
    link: Link,
    uploads: Weak<Mutex<HashMap<u32, Arc<Uploading>>>>,
}

struct UploadState {
    /// The upload, here while no worker writes its pieces.
    upload: Option<Upload>,
    /// The client's DATA not written yet, and whether each was its last.
    pieces: VecDeque<(Vec<u8>, bool)>,
    /// A worker holds the upload and writes the pieces.
    busy: bool,
    /// The client cancelled: whoever holds the upload lets it go, and then
    /// answers, so that `cancelled` means its bytes are gone.
    cancelled: bool,
    ended: bool,
}

impl Uploading {
    /// A DATA the client sent, its last when `last`: queued, and written in
    /// the order it came on a worker.
    pub(crate) fn piece(self: &Arc<Self>, body: Vec<u8>, last: bool) {
        {
            let mut state = lock(&self.state);
            if state.ended || state.cancelled {
                return;
            }
            state.pieces.push_back((body, last));
            if std::mem::replace(&mut state.busy, true) {
                return;
            }
        }
        let uploading = Arc::clone(self);
        (self.link.spawn)(Box::new(move || uploading.write_pieces()));
    }

    /// Ends the upload with its bytes removed: here, or by the worker that
    /// writes a piece once it is written. A last piece the worker took still
    /// publishes, and the answer says so.
    fn cancel(&self) {
        let upload = {
            let mut state = lock(&self.state);
            state.cancelled = true;
            if state.busy {
                return;
            }
            state.upload.take()
        };
        drop(upload);
        self.end(Err(Failure::cancelled("the client cancelled its upload")));
    }

    fn write_pieces(&self) {
        let Some(mut upload) = lock(&self.state).upload.take() else { return };
        loop {
            let (body, last) = {
                let mut state = lock(&self.state);
                if state.cancelled {
                    drop(state);
                    drop(upload);
                    return self.end(Err(Failure::cancelled("the client cancelled its upload")));
                }
                let Some(next) = state.pieces.pop_front() else {
                    state.upload = Some(upload);
                    state.busy = false;
                    return;
                };
                next
            };
            let length = u32::try_from(body.len()).unwrap_or(u32::MAX);
            let written = if body.is_empty() {
                Ok(())
            } else {
                BlobsPiece::decode(&body).and_then(|piece| upload.take(&piece.bytes).map_err(Failure::from))
            };
            match (written, last) {
                (Err(failure), _) => {
                    drop(upload);
                    return self.end(Err(failure));
                }
                (Ok(()), true) => {
                    let published = upload.publish(self.create).map_err(Failure::from);
                    return self.end(published.map(|info| BlobsWritten { info: info.map(info_of) }.encode()));
                }
                (Ok(()), false) => (self.link.grant)(length),
            }
        }
    }

    fn end(&self, last: Answered) {
        {
            let mut state = lock(&self.state);
            if std::mem::replace(&mut state.ended, true) {
                return;
            }
            state.pieces.clear();
        }
        if let Some(uploads) = self.uploads.upgrade() {
            lock(&uploads).remove(&self.stream);
        }
        (self.link.finish)(last);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "blobs_tests.rs"]
mod tests;
