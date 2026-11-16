//! blobs over the protocol: files by handle, the server holding nothing
//! between calls, as S3 does. A get answers what the file carries, and all
//! its bytes when they fit one message; a read streams a file's bytes, the
//! whole file checked or a range, on the ETag a get gave, as pieces within
//! the client's credit. An upload's pieces come as the client's DATA,
//! written in order as they come, the credit of each given back once it is
//! written, and its last DATA publishes the file.
//!
//! A directory's handle takes the reads of a set of files, its bytes from
//! the disk, and its own calls: a scan, its changes and a forget. A
//! connection that came with a token names no path of the server's machine:
//! it reaches the folders the server was given alone (BLOB-15).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use super::codec::{self, Map, Message};
use super::protocol::{
    BlobsAt, BlobsChange, BlobsChanges, BlobsDelete, BlobsDirectory, BlobsExpire, BlobsFolder, BlobsFound, BlobsGot,
    BlobsHead, BlobsInfo, BlobsList, BlobsMove, BlobsOpen, BlobsPage, BlobsPiece, BlobsRead, BlobsScanned, BlobsSince,
    BlobsUsage, BlobsWrite, BlobsWritten, Empty, Failure, Handle, method,
};
use super::{Lent, Link, Route};
use crate::blobs::{Change, Directory, FileCall, FileInfo, Files, StoredFile, Upload};
use crate::sync::lock;
use crate::{Error, ErrorKind, Store, unix_millis};

use super::Answered;

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::BLOBS_READ => Route::Read,
        method::BLOBS_UPLOAD => Route::Upload,
        _ => Route::Worker,
    }
}

/// What a connection opened, by handle, and its streams of a file's bytes.
#[derive(Default)]
pub(crate) struct Handles {
    open: super::ByHandle<Held>,
    reads: Arc<Mutex<HashMap<u32, Arc<Getting>>>>,
    uploads: Arc<Mutex<HashMap<u32, Arc<Uploading>>>>,
    /// The connection came with a token, and names no path (BLOB-15).
    token: bool,
    /// The folders the server was given, by their directory's name.
    given: Option<Arc<BTreeMap<String, PathBuf>>>,
}

/// What a handle names: a set of files, or a directory, which no call writes.
#[derive(Clone)]
enum Held {
    Files(Files),
    Directory(Directory),
}

impl Handles {
    /// The handles of a connection, as its host admits it and the folders it was given.
    pub(crate) fn of(connect: &crate::pipe::Connect) -> Handles {
        Handles { token: connect.admit.is_some(), given: connect.directories.clone(), ..Handles::default() }
    }

    /// The files or the directory a handle names, in the folder of these segments.
    fn held(&self, handle: u64, folder: &[String]) -> Result<Held, Failure> {
        let held = self.open.get(handle, || format!("handle {handle} names nothing this connection opened"))?;
        Ok(match held {
            Held::Files(files) => {
                Held::Files(folder.iter().fold(files, |files, segment| files.folder(segment.as_str())))
            }
            Held::Directory(directory) => {
                Held::Directory(folder.iter().fold(directory, |directory, segment| directory.folder(segment.as_str())))
            }
        })
    }

    /// The files a handle names, in the folder of these segments; a
    /// directory's handle is refused, since the store never writes one.
    fn at(&self, handle: u64, folder: &[String]) -> Result<Files, Failure> {
        match self.held(handle, folder)? {
            Held::Files(files) => Ok(files),
            Held::Directory(directory) => Err(Failure::invalid(format!(
                "handle {handle} names {directory:?}, which the store lists and reads and never writes"
            ))),
        }
    }

    /// The directory a handle names, as its own calls take it.
    fn directory_at(&self, handle: u64) -> Result<Directory, Failure> {
        match self.held(handle, &[])? {
            Held::Directory(directory) => Ok(directory),
            Held::Files(_) => Err(Failure::invalid(format!("handle {handle} names files, not a directory"))),
        }
    }

    /// Credit the client granted a read's stream.
    pub(crate) fn grant(&self, stream: u32, credit: u32) {
        let getting = lock(&self.reads).get(&stream).cloned();
        if let Some(getting) = getting {
            getting.grant(credit);
        }
    }

    /// The upload a client's DATA on `stream` is a piece of.
    pub(crate) fn uploading(&self, stream: u32) -> Option<Arc<Uploading>> {
        lock(&self.uploads).get(&stream).cloned()
    }

    /// Ends a read or an upload the client cancelled; says whether there was one.
    pub(crate) fn cancel(&self, stream: u32) -> bool {
        // out of its map before it ends, which takes the map's lock again:
        // an `if let` on the lock would hold it and wait for itself
        let getting = lock(&self.reads).remove(&stream);
        if let Some(getting) = getting {
            getting.end(Err(Failure::cancelled("the client cancelled its read")));
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
        let reads: Vec<_> = lock(&self.reads).drain().map(|(_, getting)| getting).collect();
        for getting in reads {
            getting.drop_file();
        }
        let uploads: Vec<_> = lock(&self.uploads).drain().map(|(_, uploading)| uploading).collect();
        for uploading in uploads {
            uploading.cancel();
        }
    }
}

/// Answers a call of one message: everything but a read and an upload. A
/// read connection's is `read_only`.
pub(crate) fn call(
    store: &Store,
    handles: &Handles,
    lent: Lent<'_>,
    called: u16,
    body: &[u8],
    (max_body, read_only): (usize, bool),
) -> Answered {
    match called {
        method::BLOBS_OPEN => open(store, handles, lent, BlobsOpen::decode(body)?, read_only),
        method::BLOBS_PUT => put(handles, BlobsWrite::decode(body)?),
        method::BLOBS_GET => get(handles, &BlobsAt::decode(body)?, max_body),
        method::BLOBS_HEAD => head(handles, &BlobsAt::decode(body)?),
        method::BLOBS_DELETE => delete(handles, BlobsDelete::decode(body)?),
        method::BLOBS_COPY | method::BLOBS_RENAME => moved(handles, called, BlobsMove::decode(body)?),
        method::BLOBS_EXPIRE => expire(handles, &BlobsExpire::decode(body)?),
        method::BLOBS_LIST => list(handles, BlobsList::decode(body)?),
        method::BLOBS_USAGE => usage(handles, &BlobsFolder::decode(body)?),
        method::BLOBS_CLEAR => clear(handles, &BlobsFolder::decode(body)?),
        method::BLOBS_DIRECTORY => directory(store, handles, BlobsDirectory::decode(body)?, read_only),
        method::BLOBS_SCAN => scan(handles, &BlobsFolder::decode(body)?),
        method::BLOBS_CHANGES => changes(handles, BlobsSince::decode(body)?),
        method::BLOBS_FORGET => forget(handles, &BlobsFolder::decode(body)?),
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

/// Answers a call made inside a database's transaction, on its stream: a
/// write of files kept in the database's file commits with its rows.
#[cfg(feature = "sql")]
pub(crate) fn call_in(handles: &Handles, tx: &crate::Transaction<'_>, called: u16, body: &[u8]) -> Answered {
    use crate::TxHandle as _;
    match called {
        method::BLOBS_PUT => {
            let mut asked = BlobsWrite::decode(body)?;
            let files = handles.at(asked.handle, &asked.folder)?;
            let bytes = std::mem::take(&mut asked.bytes);
            let call = options(files.in_tx(tx).key(&asked.path), &asked);
            let info = if asked.create {
                call.create(&bytes)?.then(|| files.in_tx(tx).head(&asked.path)).transpose()?.flatten()
            } else {
                Some(call.put(&bytes)?)
            };
            Ok(BlobsWritten { info: info.map(info_of) }.encode())
        }
        method::BLOBS_HEAD => {
            let asked = BlobsAt::decode(body)?;
            let info = handles.at(asked.handle, &asked.folder)?.in_tx(tx).head(&asked.path)?;
            Ok(BlobsHead { info: info.map(info_of) }.encode())
        }
        method::BLOBS_DELETE => {
            let asked = BlobsDelete::decode(body)?;
            let files = handles.at(asked.handle, &asked.folder)?;
            let mut call = files.in_tx(tx).key(&asked.path);
            if let Some(etag) = asked.if_match {
                call = call.if_match(etag);
            }
            call.delete()?;
            Ok(Empty {}.encode())
        }
        method::BLOBS_COPY | method::BLOBS_RENAME => {
            let asked = BlobsMove::decode(body)?;
            let files = handles.at(asked.handle, &asked.folder)?;
            let mut call = files.in_tx(tx).key(&asked.to);
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
        other => Err(Failure::invalid(format!(
            "method {other:#06x} does not run inside a transaction: files join one with put, head, delete, copy \
             and rename"
        ))),
    }
}

/// Opens a set of files as the call says; a connection that only reads opens
/// only one the store holds, found first, so that the open finds it as that
/// did and makes nothing.
fn open(store: &Store, handles: &Handles, lent: Lent<'_>, asked: BlobsOpen, read_only: bool) -> Answered {
    let mut files = match asked.database {
        #[cfg(feature = "sql")]
        Some(database) => {
            let shared = lent(database)?;
            if read_only {
                crate::blobs::set_in_file_as_it_is(&shared, &asked.name)?;
            }
            crate::blobs::FilesBuilder::in_file(shared, &asked.name)
        }
        #[cfg(not(feature = "sql"))]
        Some(database) => {
            let _ = lent;
            return Err(Failure::invalid(format!("database {database}: this server keeps no databases")));
        }
        None => {
            if read_only {
                crate::blobs::set_as_it_is(store, &asked.name)?;
            }
            store.files(&asked.name)
        }
    };
    if let Some(ttl) = asked.ttl {
        files = files.ttl(Duration::from_millis(ttl));
    }
    if let Some(most) = asked.max_file_size {
        files = files.max_file_size(most);
    }
    let files = files.open()?;
    let handle = handles.open.keep(Held::Files(files));
    Ok(Handle { handle }.encode())
}

/// Opens a directory: on the path a connection of this machine names, or on
/// the folder the server was given under the name. One that came with a
/// token names no path, and opens only what the server was given; one that
/// only reads opens only a directory the store holds, and scans nothing.
fn directory(store: &Store, handles: &Handles, asked: BlobsDirectory, read_only: bool) -> Answered {
    let name = asked.name.as_str();
    let refused = |why: &str| Failure::invalid(format!("directory {name}: {why}"));
    let given = handles.given.as_ref().and_then(|given| given.get(name)).cloned();
    let path = match (asked.path, handles.token) {
        (Some(_), true) => return Err(refused("a connection that came with a token names no path of the server's")),
        (Some(path), false) => Some(PathBuf::from(path)),
        (None, true) if given.is_none() => return Err(refused("the server was given no directory of the name")),
        (None, _) => given,
    };
    if read_only {
        if asked.every.is_some() {
            return Err(refused("a scan on a connection that only reads"));
        }
        crate::blobs::directory_as_it_is(store, name)?;
    }
    let mut opening = store.directory(name).ignore(asked.ignore).growing(asked.growing);
    if let Some(path) = path {
        opening = opening.path(path);
    }
    if let Some(every) = asked.every {
        opening = opening.every(Duration::from_millis(every));
    }
    let handle = handles.open.keep(Held::Directory(opening.open()?));
    Ok(Handle { handle }.encode())
}

fn scan(handles: &Handles, asked: &BlobsFolder) -> Answered {
    let directory = handles.directory_at(asked.handle)?;
    let at = asked.folder.iter().fold(directory, |directory, segment| directory.folder(segment.as_str()));
    let scan = at.scan()?;
    let scanned = BlobsScanned {
        added: scan.added,
        changed: scan.changed,
        removed: scan.removed,
        skipped: scan.skipped,
        count: scan.count,
        size: scan.size,
    };
    Ok(scanned.encode())
}

fn changes(handles: &Handles, asked: BlobsSince) -> Answered {
    let directory = handles.directory_at(asked.handle)?;
    let mut call = directory.changes();
    if let Some(after) = asked.after {
        call = call.after(after);
    }
    let page = call.page()?;
    let told = |change: Change| match change {
        Change::Added => "added",
        Change::Changed => "changed",
        Change::Removed => "removed",
    };
    let items = page
        .items
        .into_iter()
        .map(|item| BlobsChange { path: item.path, change: told(item.change).to_owned(), info: item.info.map(info_of) })
        .collect();
    Ok(BlobsChanges { items, next: page.next }.encode())
}

fn forget(handles: &Handles, asked: &BlobsFolder) -> Answered {
    let directory = handles.directory_at(asked.handle)?;
    asked.folder.iter().fold(directory, |directory, segment| directory.folder(segment.as_str())).forget()?;
    Ok(Empty {}.encode())
}

fn put(handles: &Handles, mut asked: BlobsWrite) -> Answered {
    let files = handles.at(asked.handle, &asked.folder)?;
    let bytes = std::mem::take(&mut asked.bytes);
    let info = options(files.key(&asked.path), &asked).written(&bytes, asked.create)?;
    Ok(BlobsWritten { info: info.map(info_of) }.encode())
}

fn get(handles: &Handles, asked: &BlobsAt, max_body: usize) -> Answered {
    let Some(mut file) = handles.held(asked.handle, &asked.folder)?.get(&asked.path)? else {
        return Ok(BlobsGot::default().encode());
    };
    let info = info_of(file.info().clone());
    // half a body, the rest room for what it carries; a larger file waits for its read
    if info.size > (max_body / 2) as u64 {
        return Ok(BlobsGot { info: Some(info), bytes: Vec::new() }.encode());
    }
    got_whole(info, &mut file)
}

/// A get's answer with all the file's bytes, read straight into the message
/// that carries them: `BlobsGot { info, bytes }.encode()`, as the test beside
/// it checks, without the file's bytes held twice. A mebibyte's answer copied
/// from bytes of their own cost a fresh mebibyte and its copy (issue #10).
fn got_whole(info: BlobsInfo, file: &mut StoredFile) -> Answered {
    let size = usize::try_from(info.size).map_err(|_| Failure::internal("a file past this machine's memory"))?;
    // as `BlobsGot::size` reserves: a map's head, two fields' numbers, a bin's head of five
    let mut out = Vec::with_capacity(3 + 1 + info.size() + 1 + 5 + size);
    let mut map = Map::open(&mut out);
    map.field(&mut out, 1);
    codec::write_message(&mut out, &info);
    if size > 0 {
        map.field(&mut out, 2);
        codec::write_bin_in_place(&mut out, size, |bytes| file.read_whole(bytes))?;
    }
    map.close(&mut out);
    Ok(out)
}

fn head(handles: &Handles, asked: &BlobsAt) -> Answered {
    let info = handles.held(asked.handle, &asked.folder)?.head(&asked.path)?;
    Ok(BlobsHead { info: info.map(info_of) }.encode())
}

fn delete(handles: &Handles, asked: BlobsDelete) -> Answered {
    let files = handles.at(asked.handle, &asked.folder)?;
    let mut call = files.key(&asked.path);
    if let Some(etag) = asked.if_match {
        call = call.if_match(etag);
    }
    call.delete()?;
    Ok(Empty {}.encode())
}

/// A copy or a rename, as `called` says: the file at `from`, at `to`.
fn moved(handles: &Handles, called: u16, asked: BlobsMove) -> Answered {
    let files = handles.at(asked.handle, &asked.folder)?;
    let mut call = files.key(&asked.to);
    if let Some(etag) = asked.if_match {
        call = call.if_match(etag);
    }
    let info = if called == method::BLOBS_COPY { call.copy_from(&asked.from)? } else { call.rename_from(&asked.from)? };
    Ok(info_of(info).encode())
}

fn expire(handles: &Handles, asked: &BlobsExpire) -> Answered {
    let files = handles.at(asked.handle, &asked.folder)?;
    let found = files.expire(&asked.path, Duration::from_millis(asked.after))?;
    Ok(BlobsFound { found }.encode())
}

fn list(handles: &Handles, asked: BlobsList) -> Answered {
    let limit = asked.limit.map(|limit| usize::try_from(limit).unwrap_or(usize::MAX));
    let page = match handles.held(asked.handle, &asked.folder)? {
        Held::Files(files) => {
            let mut list = files.list().prefix(asked.prefix);
            if let Some(after) = asked.after {
                list = list.after(after);
            }
            if let Some(limit) = limit {
                list = list.limit(limit);
            }
            list.page()?
        }
        Held::Directory(directory) => {
            let mut list = directory.list().prefix(asked.prefix);
            if let Some(after) = asked.after {
                list = list.after(after);
            }
            if let Some(limit) = limit {
                list = list.limit(limit);
            }
            list.page()?
        }
    };
    Ok(BlobsPage { files: page.files.into_iter().map(info_of).collect(), next: page.next }.encode())
}

fn usage(handles: &Handles, asked: &BlobsFolder) -> Answered {
    let usage = match handles.held(asked.handle, &asked.folder)? {
        Held::Files(files) => files.usage()?,
        Held::Directory(directory) => directory.usage()?,
    };
    Ok(BlobsUsage { count: usage.count, size: usage.size }.encode())
}

fn clear(handles: &Handles, asked: &BlobsFolder) -> Answered {
    handles.at(asked.handle, &asked.folder)?.clear()?;
    Ok(Empty {}.encode())
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
    super::part_bound(link.max_body, link.credit, 16)
}

/// How a read's answer goes: whole in the RESPONSE, or the RESPONSE's piece
/// and then the rest within the client's credit.
pub(crate) enum Reading {
    Whole(Vec<u8>),
    Begun(Vec<u8>, Arc<Getting>),
}

/// Reads a file's bytes: the whole file, checked, or a range, which is not.
/// The file is opened here and held until the read ends, so that the bytes
/// are those of the ETag it was asked on, through a replace.
///
/// A growing directory's file holds to no version: the read goes on to the
/// size the file has as each piece is read, and each piece brings it.
pub(crate) fn read(handles: &Handles, stream: u32, body: &[u8], link: &Link) -> Result<Reading, Failure> {
    let asked = BlobsRead::decode(body)?;
    let held = handles.held(asked.handle, &asked.folder)?;
    let found = held.get(&asked.path)?;
    let growing = found.as_ref().is_some_and(StoredFile::is_growing);
    let mut file = match (found, &asked.if_match) {
        (Some(file), Some(etag)) if !growing && !file.matches(etag) => return Err(held.changed(&asked.path)),
        (None, Some(_)) => return Err(held.changed(&asked.path)),
        (None, None) => return Err(held.fail(&asked.path, Error::not_found("no file is there"))),
        (Some(file), _) => file,
    };
    let size = file.info().size;
    let (from, left) = if growing {
        (asked.offset.unwrap_or(0), asked.length.unwrap_or(u64::MAX))
    } else {
        let from = asked.offset.unwrap_or(0).min(size);
        (from, asked.length.map_or(size - from, |length| length.min(size - from)))
    };
    file.seek(SeekFrom::Start(from)).map_err(|error| Error::io("blobs: seek a file", error))?;
    let piece = piece_bound(link);
    let first = bound(piece, left);
    let (response, read) = read_piece(&mut file, first)?;
    let left = left - read as u64;
    if left == 0 || read < first {
        return Ok(Reading::Whole(response));
    }
    let state = GetState { file: Some(file), left, credit: link.credit, busy: false, ended: false };
    let getting = Arc::new(Getting {
        stream,
        piece,
        state: Mutex::new(state),
        link: link.clone(),
        reads: Arc::downgrade(&handles.reads),
    });
    lock(&handles.reads).insert(stream, Arc::clone(&getting));
    Ok(Reading::Begun(response, getting))
}

impl Held {
    fn get(&self, path: &str) -> crate::Result<Option<StoredFile>> {
        match self {
            Held::Files(files) => files.get(path),
            Held::Directory(directory) => directory.get(path),
        }
    }

    fn head(&self, path: &str) -> crate::Result<Option<FileInfo>> {
        match self {
            Held::Files(files) => files.head(path),
            Held::Directory(directory) => directory.head(path),
        }
    }

    /// A read on an ETag the path no longer holds, as a write's `ifMatch` is.
    fn changed(&self, path: &str) -> Failure {
        self.fail(path, Error::new(ErrorKind::Conflict, "the path holds no file with the ETag given"))
    }

    fn fail(&self, path: &str, error: Error) -> Failure {
        match self {
            Held::Files(files) => files.fail(path, error).into(),
            Held::Directory(directory) => directory.fail(path, error).into(),
        }
    }
}

/// The bytes the next piece reads: a piece's, or fewer when the read has
/// fewer left.
fn bound(piece: usize, left: u64) -> usize {
    usize::try_from(left).map_or(piece, |left| left.min(piece))
}

/// The next `length` bytes of a file as a piece's message, read straight
/// into it: `BlobsPiece { bytes }.encode()`, as the test beside it checks.
/// The read's last bytes wait until the whole file's SHA-256 is checked.
/// And the bytes it read: `length`, or of a growing file fewer at its end.
fn read_piece(file: &mut StoredFile, length: usize) -> Result<(Vec<u8>, usize), Failure> {
    if file.is_growing() {
        return growing_piece(file, length);
    }
    // as `BlobsPiece::size` reserves: a map's head, a field's number, a bin's head of five
    let mut out = Vec::with_capacity(3 + 1 + 5 + length);
    let mut map = Map::open(&mut out);
    if length > 0 {
        map.field(&mut out, 1);
        codec::write_bin_in_place(&mut out, length, |bytes| fill(file, bytes))?;
    }
    map.close(&mut out);
    Ok((out, length))
}

/// Up to `length` bytes of a growing file, as many as it holds now, with the
/// file's size and time as they were read.
fn growing_piece(file: &mut StoredFile, length: usize) -> Result<(Vec<u8>, usize), Failure> {
    let mut bytes = vec![0; length];
    let mut filled = 0;
    while filled < length {
        match file.read(&mut bytes[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) => return Err(Failure::from(read_error(error))),
        }
    }
    bytes.truncate(filled);
    let info = file.info();
    let piece = BlobsPiece { bytes, size: Some(info.size), last_modified: Some(unix_millis(info.last_modified)) };
    Ok((piece.encode(), filled))
}

/// Fills `bytes` from where the file's next read begins; the file reads to
/// its end, so that a read short of it is the store's fault.
fn fill(file: &mut StoredFile, bytes: &mut [u8]) -> Result<(), Failure> {
    let mut filled = 0;
    while filled < bytes.len() {
        match file.read(&mut bytes[filled..]) {
            Ok(0) => return Err(Failure::internal("a read of a file ended before its piece")),
            Ok(read) => filled += read,
            Err(error) => return Err(Failure::from(read_error(error))),
        }
    }
    Ok(())
}

/// The store's error a read carried, or an I/O one.
fn read_error(error: std::io::Error) -> crate::Error {
    // An OS error carries no inner error, and `into_inner` would drop it whole.
    if !error.get_ref().is_some_and(|inner| inner.is::<crate::Error>()) {
        return crate::Error::io("blobs: read a file", error);
    }
    match error.into_inner().map(|inner| inner.downcast::<crate::Error>()) {
        Some(Ok(error)) => *error,
        _ => crate::Error::internal("blobs: a read failed"),
    }
}

/// A read's bytes going out a piece at a time within the client's credit.
pub(crate) struct Getting {
    stream: u32,
    piece: usize,
    state: Mutex<GetState>,
    link: Link,
    reads: Weak<Mutex<HashMap<u32, Arc<Getting>>>>,
}

struct GetState {
    /// The file, here between two runs of the pieces that go.
    file: Option<StoredFile>,
    /// The read's bytes still to go.
    left: u64,
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
            let (mut file, left) = {
                let mut state = lock(&self.state);
                match state.file.take() {
                    Some(file) if !state.ended && state.credit >= room => (file, state.left),
                    file => {
                        state.file = file;
                        state.busy = false;
                        return;
                    }
                }
            };
            let length = bound(self.piece, left);
            let (body, read) = match read_piece(&mut file, length) {
                Ok(piece) => piece,
                Err(failure) => return self.end(Err(failure)),
            };
            let left = left - read as u64;
            // a growing file's read ends at the end it has now
            let ended = left == 0 || read < length;
            if read > 0 {
                // sent under the lock a cancel's end takes, as records' and
                // metrics' are: a piece read while a cancel ended the read
                // never goes after the stream's last frame
                let mut state = lock(&self.state);
                if state.ended {
                    return;
                }
                // the message's bytes, which the client gives back, and not the room:
                // counted apart, the two drift until the stream waits for good
                state.credit -= body.len() as u64;
                (self.link.send)(body);
            }
            if ended {
                return self.end(Ok(Empty {}.encode()));
            }
            let mut state = lock(&self.state);
            state.file = Some(file);
            state.left = left;
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
        if let Some(reads) = self.reads.upgrade() {
            lock(&reads).remove(&self.stream);
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

#[cfg(test)]
#[path = "blobs_tests.rs"]
mod tests;
