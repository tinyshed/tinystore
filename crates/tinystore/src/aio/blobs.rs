//! The async API's handles of the Blobs engine, beside the blocking ones of
//! [`crate::blobs`]: the same sets, folders, calls on a path, lists, files
//! read and uploads, with the same verbs. Their answers are the blocking
//! API's own, [`FileInfo`], [`Page`](crate::blobs::Page) and
//! [`Usage`](crate::blobs::Usage). Opening a set stays a plain call; every
//! call that reads, writes or waits is awaited. Maintenance stays blocking,
//! [`crate::blobs::maintain`]`(store.blocking())`, since only a test of a
//! store opened without background work runs it.
//!
//! ```no_run
//! # async fn example(png: Vec<u8>) -> tinystore::Result<()> {
//! use tinystore::Options;
//! use tinystore::aio::Store;
//!
//! let store = Store::open("data", Options::default())?;
//! let avatars = store.files("avatars").max_file_size(5 << 20).open()?; // opening stays a plain call
//! let info = avatars.key("42.png").content_type("image/png").put(png).await?; // the bytes are moved in
//! let avatar = avatars.get("42.png").await?.expect("just written");
//! assert_eq!(avatar.info().etag, info.etag);
//! let bytes = avatar.read_all().await?; // checked against its SHA-256
//! # Ok(())
//! # }
//! ```
//!
//! # Where the work runs
//!
//! | A call                                        | Runs                                                                   | Its future dropped once polled                   |
//! |-----------------------------------------------|------------------------------------------------------------------------|--------------------------------------------------|
//! | `head`                                        | in the first poll when a reader is idle, else on the pool              | answered in that poll, or stops like any read    |
//! | `get`                                         | as `head`, a file on disk opened on the pool                           | answered in that poll, or stops like any read    |
//! | `usage`, a list's `page`, `all`'s pages       | on the store's pool                                                    | stops; its reader goes back                      |
//! | `delete`, `copy`, `rename`, `expire`, `clear` | submitted in the first poll, committed on the pool                     | commits; its answer is let go                    |
//! | `put`, `create` up to 16 KiB                  | memory waited for on no thread, then submitted as `delete` is          | commits, its wait for memory included            |
//! | `put`, `create` past 16 KiB                   | memory waited for on no thread, then written and committed on the pool | commits, its wait for memory included            |
//! | `upload`                                      | memory waited for on no thread                                         | leaves the line, holds nothing                   |
//! | an upload's `write`                           | in the poll while the file may stay inline, then on the pool           | its piece is written; the next call waits for it |
//! | an upload's `commit` up to 16 KiB             | submitted in the poll                                                  | commits; unpolled, the upload aborts             |
//! | an upload's `commit` past 16 KiB              | written and committed on the pool, after its writes                    | commits; unpolled, the upload aborts             |
//! | a file's `read_all`, `read_at`, its `Stream`  | in the poll for a file kept inline, else on the pool                   | stops between pieces of 256 KiB                  |
//!
//! None of them commits, syncs the disk or waits on the thread that polls
//! its future (STORE-10). A file on disk opens, is read and is written on
//! the store's pool: a put of more than 16 KiB writes its bytes there, and
//! commits there too, one pool thread held until its group's commit ends.
//! So does a put or a commit kept inline that finds the block of content ids
//! spent, as the first after an open does and then one in a thousand:
//! reserving the next block is a commit of its own. The files a write freed
//! are removed on the pool after its commit. A write is lazy until its first
//! poll, and once polled it commits whether or not anyone still awaits it
//! (DUR-11). On a node whose blobs.db is a copy, a write passes to its home
//! in a blocking round trip, which waits on a thread of its own rather than
//! the pool's, as every call passed on to a home does.

use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::Store;
use super::oneshot::{Answer, pending};
use super::run;
use crate::blobs::{CallOptions, FileInfo, Freed};
use crate::polled::{self, Lead};
use crate::pool::Pool;
use crate::sync::lock;
use crate::{Error, Key, Result};

mod list;
mod read;
mod upload;

pub use list::{All, List};
pub use read::StoredFile;
pub use upload::Upload;

impl Store {
    /// The set of files `name`, made the first time it opens, as
    /// [`crate::Store::files`] opens it.
    pub fn files(&self, name: &str) -> FilesBuilder<'_> {
        FilesBuilder { builder: self.blocking().files(name), pool: self.pool() }
    }
}

/// What opens a set of files, as [`crate::blobs::FilesBuilder`] does.
#[derive(Debug)]
pub struct FilesBuilder<'s> {
    builder: crate::blobs::FilesBuilder<'s>,
    pool: &'s Arc<Pool>,
}

impl<'s> FilesBuilder<'s> {
    /// The set of files `builder` opens, its calls run on `pool`.
    #[cfg(feature = "sql")]
    pub(crate) fn new(builder: crate::blobs::FilesBuilder<'s>, pool: &'s Arc<Pool>) -> Self {
        FilesBuilder { builder, pool }
    }

    /// The term of every file written without its own, counted from its
    /// write; not a ceiling.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.builder = self.builder.ttl(ttl);
        self
    }

    /// The largest file a write may leave: one past it is refused and leaves
    /// nothing, and nothing stored is ever removed to make room.
    pub fn max_file_size(mut self, bytes: u64) -> Self {
        self.builder = self.builder.max_file_size(bytes);
        self
    }

    /// Opens the set, a plain call made once at start: a first open may
    /// migrate blobs.db.
    pub fn open(self) -> Result<Files> {
        Ok(Files { blocking: self.builder.open()?, pool: Arc::clone(self.pool) })
    }
}

/// A set of files by path, or a folder of it, whose calls are awaited: the
/// same set as [`crate::blobs::Files`], and the same set when one is made
/// from the other.
#[derive(Clone)]
pub struct Files {
    blocking: crate::blobs::Files,
    pool: Arc<Pool>,
}

impl fmt::Debug for Files {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.blocking, f)
    }
}

impl From<crate::blobs::Files> for Files {
    /// The same files, their calls awaited: a clone of the handle.
    fn from(blocking: crate::blobs::Files) -> Files {
        let pool = Arc::clone(&blocking.blobs.pool);
        Files { blocking, pool }
    }
}

impl Files {
    /// The same files, for their blocking calls.
    pub fn blocking(&self) -> &crate::blobs::Files {
        &self.blocking
    }

    /// The folder in this one that `segment` names: `folder("users").folder(42)`
    /// is `users/42`, and its files' paths are its segments and then theirs.
    pub fn folder(&self, segment: impl Key) -> Files {
        Files { blocking: self.blocking.folder(segment), pool: Arc::clone(&self.pool) }
    }

    /// A call on one path, its options before its verb:
    /// `files.key("a.md").if_match(&etag).put(text)`.
    pub fn key(&self, path: &str) -> FileCall<'_> {
        FileCall { call: self.blocking.key(path), pool: &self.pool }
    }

    /// Writes the file and replaces whatever the path held, as
    /// [`FileCall::put`] does.
    pub fn put(
        &self,
        path: &str,
        bytes: impl AsRef<[u8]> + Send + 'static,
    ) -> impl Future<Output = Result<FileInfo>> + Send + 'static {
        self.key(path).put(bytes)
    }

    /// Writes the file only when the path holds none; says whether it did.
    pub fn create(
        &self,
        path: &str,
        bytes: impl AsRef<[u8]> + Send + 'static,
    ) -> impl Future<Output = Result<bool>> + Send + 'static {
        self.key(path).create(bytes)
    }

    /// A file written a piece at a time, which appears at its `commit`, as
    /// [`FileCall::upload`] begins it.
    pub fn upload(&self, path: &str) -> impl Future<Output = Result<Upload>> + Send + 'static {
        self.key(path).upload()
    }

    /// The file at the path and the bytes it reads, which stay what they were
    /// now through a replace, a delete or an expiry; none when there is none.
    ///
    /// Its row is read in the first poll when a reader is idle, and a file
    /// kept inline is answered there; a file on disk opens on the store's
    /// pool. Dropped, it stops, and a file it opened is closed.
    pub fn get(&self, path: &str) -> impl Future<Output = Result<Option<StoredFile>>> + Send + 'static {
        let (files, path, pool) = (self.blocking.clone(), path.to_owned(), Arc::clone(&self.pool));
        let opened = run::point(&self.pool, describe(&files, &path, "a get"), move || files.get(&path));
        async move { Ok(opened.await?.map(|file| StoredFile::new(file, pool))) }
    }

    /// What the file at the path carries, none of its bytes.
    pub fn head(&self, path: &str) -> impl Future<Output = Result<Option<FileInfo>>> + Send + 'static {
        let (files, path) = (self.blocking.clone(), path.to_owned());
        run::point(&self.pool, describe(&files, &path, "a head"), move || files.head(&path))
    }

    /// Removes the file at the path; nothing to remove is not an error.
    pub fn delete(&self, path: &str) -> impl Future<Output = Result<()>> + Send + 'static {
        self.key(path).delete()
    }

    /// Writes the file at `from` at `to` too, sharing its bytes, as
    /// [`crate::blobs::Files::copy`] does: a file at `to` is a conflict.
    pub fn copy(&self, from: &str, to: &str) -> impl Future<Output = Result<FileInfo>> + Send + 'static {
        self.key(to).copy_from(from)
    }

    /// Moves the file at `from` to `to` in one write, as `copy` writes it.
    pub fn rename(&self, from: &str, to: &str) -> impl Future<Output = Result<FileInfo>> + Send + 'static {
        self.key(to).rename_from(from)
    }

    /// Makes the file at the path expire `after` from now; says whether there
    /// was one.
    pub fn expire(&self, path: &str, after: Duration) -> impl Future<Output = Result<bool>> + Send + 'static {
        let owned = Owned { path: path.to_owned(), ..self.owned() };
        write(
            owned,
            "an expiry",
            move |owned, done| owned.files.expire_then(&owned.path, after, done),
            move |owned| owned.files.expire(&owned.path, after),
        )
    }

    /// The files and the pool, a call on no path yet.
    fn owned(&self) -> Owned {
        Owned {
            files: self.blocking.clone(),
            path: String::new(),
            options: CallOptions::default(),
            pool: Arc::clone(&self.pool),
        }
    }
}

/// One path and the options of a call on it, which ends in its verb, as
/// [`crate::blobs::FileCall`] gathers them.
#[derive(Debug)]
#[must_use = "a call does nothing until its verb, and its verb nothing until it is awaited"]
pub struct FileCall<'f> {
    call: crate::blobs::FileCall<'f>,
    pool: &'f Arc<Pool>,
}

impl FileCall<'_> {
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.call = self.call.content_type(content_type);
        self
    }

    /// A string of the program's the file carries, such as the name it was
    /// uploaded as.
    pub fn meta(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.call = self.call.meta(name, value);
        self
    }

    /// The file's own term, from its write.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.call = self.call.ttl(ttl);
        self
    }

    /// Writes only over the file with this ETag: one that changed, expired or
    /// went since is a conflict, and nothing is written.
    pub fn if_match(mut self, etag: impl Into<String>) -> Self {
        self.call = self.call.if_match(etag);
        self
    }

    /// How long the body is; one longer or shorter is refused and leaves
    /// nothing.
    pub fn size(mut self, bytes: u64) -> Self {
        self.call = self.call.size(bytes);
        self
    }

    /// Writes the file and replaces whatever the path held, at the moment its
    /// future is ready. The bytes are owned, since past 16 KiB a thread of
    /// the store's pool writes them to disk while the future may be gone.
    pub fn put(
        self,
        bytes: impl AsRef<[u8]> + Send + 'static,
    ) -> impl Future<Output = Result<FileInfo>> + Send + 'static {
        let written = self.written(bytes, false);
        async move { written.await?.ok_or_else(|| Error::internal("a put that wrote nothing")) }
    }

    /// Writes the file only when the path holds no file; says whether it did.
    pub fn create(
        self,
        bytes: impl AsRef<[u8]> + Send + 'static,
    ) -> impl Future<Output = Result<bool>> + Send + 'static {
        let written = self.written(bytes, true);
        async move { written.await.map(|info| info.is_some()) }
    }

    /// A file written a piece at a time, which appears at its `commit`. It
    /// waits on no thread for the memory an upload holds, and dropped while
    /// it waits it leaves the line and holds nothing.
    pub fn upload(self) -> impl Future<Output = Result<Upload>> + Send + 'static {
        let owned = self.owned();
        async move {
            let Owned { files, path, options, pool } = owned;
            let (answer, started) = pending(describe(&files, &path, "an upload"));
            let _waiting = files.call(&path, options).upload_then(move |upload| answer.send(upload));
            let upload = started.await?;
            Ok(Upload::new(upload, files, path, pool))
        }
    }

    pub fn delete(self) -> impl Future<Output = Result<()>> + Send + 'static {
        write(self.owned(), "a delete", |owned, done| owned.call().delete_then(done), |owned| owned.call().delete())
    }

    /// Writes the file at `from` here too, sharing its bytes.
    pub fn copy_from(self, from: &str) -> impl Future<Output = Result<FileInfo>> + Send + 'static {
        self.copied(from, false)
    }

    /// Moves the file at `from` here in one write.
    pub fn rename_from(self, from: &str) -> impl Future<Output = Result<FileInfo>> + Send + 'static {
        self.copied(from, true)
    }

    fn copied(self, from: &str, rename: bool) -> impl Future<Output = Result<FileInfo>> + Send + 'static {
        let (source, again) = (from.to_owned(), from.to_owned());
        write(
            self.owned(),
            if rename { "a rename" } else { "a copy" },
            move |owned, done| owned.call().copied_then(&source, rename, done),
            move |owned| if rename { owned.call().rename_from(&again) } else { owned.call().copy_from(&again) },
        )
    }

    /// Writes the whole file, or with `create` only where no file is: the
    /// upload's memory waited for on no thread, then the bytes taken and
    /// submitted where they stay inline, or written to disk and committed on
    /// the store's pool. Once polled it commits whether or not its future is
    /// still awaited, its wait for memory included (DUR-11).
    fn written(
        self,
        bytes: impl AsRef<[u8]> + Send + 'static,
        create: bool,
    ) -> impl Future<Output = Result<Option<FileInfo>>> + Send + 'static {
        let owned = self.owned();
        let lead = run::lead_on(&owned.pool);
        async move {
            let what = owned.what(if create { "a create" } else { "a put" });
            if passed_home(&owned.files) {
                return run::passed_on(what, move || owned.call().written(bytes.as_ref(), create)).await;
            }
            let (answer, answered) = pending(what);
            polled::leading_on(&lead.clone(), || Putting { owned, lead, create, answer }.start(bytes));
            answered.await
        }
    }

    fn owned(self) -> Owned {
        let (files, path, options) = self.call.into_parts();
        Owned { files: files.clone(), path, options, pool: Arc::clone(self.pool) }
    }
}

/// A call on one path, owned by its future until its first poll makes the
/// call again: the files, the path, the options, and the store's pool.
struct Owned {
    files: crate::blobs::Files,
    path: String,
    options: CallOptions,
    pool: Arc<Pool>,
}

impl Owned {
    /// The blocking call, made again where the awaited one runs.
    fn call(&self) -> crate::blobs::FileCall<'_> {
        self.files.call(&self.path, self.options.clone())
    }

    /// What its errors call it: `files avatars: "users/42/1.png": a put`.
    fn what(&self, verb: &str) -> String {
        describe(&self.files, &self.path, verb)
    }
}

/// What an awaited call's errors call it: `files avatars: "users/42/1.png": a put`.
fn describe(files: &crate::blobs::Files, path: &str, verb: &str) -> String {
    format!("{}: {verb}", label(files, path))
}

/// The file at `path` as the blocking calls' errors name it, the folder's
/// path before its own: `files avatars: "users/42/1.png"`; the set alone for
/// a call on its root.
fn label(files: &crate::blobs::Files, path: &str) -> String {
    match format!("{}{path}", files.folder.prefix()) {
        whole if whole.is_empty() => files.describe(),
        whole => format!("{}: {whole:?}", files.describe()),
    }
}

/// A put whose memory it waits for: what takes over once it is held, on
/// whichever thread gives it back.
struct Putting {
    owned: Owned,
    lead: Lead,
    create: bool,
    answer: Answer<Option<FileInfo>>,
}

impl Putting {
    /// Waits for the memory an upload holds, on no thread, and keeps the
    /// waiter in line whatever becomes of the future: the waiter goes with
    /// the closure that is given the memory, once it is given.
    fn start(self, bytes: impl AsRef<[u8]> + Send + 'static) {
        let (files, path, options) = (self.owned.files.clone(), self.owned.path.clone(), self.owned.options.clone());
        let kept = Arc::new(Mutex::new(None));
        let waiter = Arc::clone(&kept);
        let waiting = files.call(&path, options).upload_then(move |upload| {
            let _answered = waiter;
            self.publish(upload, bytes);
        });
        *lock(&kept) = waiting;
    }

    /// Takes `bytes` into the upload that holds the put's memory and
    /// publishes it: submitted where they stay inline and a content id is
    /// free now, the next block of ids or the disk on the store's pool.
    fn publish(self, upload: Result<crate::blobs::Upload>, bytes: impl AsRef<[u8]> + Send + 'static) {
        let Putting { owned, lead, create, answer } = self;
        let mut upload = match upload {
            Ok(upload) => upload,
            Err(error) => return answer.send(Err(error)),
        };
        if !upload.stays_in_memory(bytes.as_ref().len()) {
            return on_the_pool(&owned.pool, answer, move || {
                upload.take(bytes.as_ref())?;
                upload.publish(create)
            });
        }
        if let Err(error) = upload.take(bytes.as_ref()) {
            return answer.send(Err(error));
        }
        if upload.inline_id().is_none() {
            return on_the_pool(&owned.pool, answer, move || upload.publish(create));
        }
        let done = answering(&owned.files, &owned.pool, answer);
        polled::leading_on(&lead, || upload.publish_inline_then(create, done));
    }
}

/// What a write submitted calls once its commit ends: its answer, given at
/// once when it freed nothing, else once the files it freed are removed.
type Answering<T> = Box<dyn FnOnce(Result<(T, Freed)>) + Send>;

/// Gives `answer` what a write submitted answered. The files it freed are
/// removed on the store's pool first, never on the thread that answers its
/// commit: an unlink waits while a snapshot links files, and the commits
/// behind this one wait for that thread. They are removed as its store
/// closes too, since the write committed.
fn answering<T: Send + 'static>(files: &crate::blobs::Files, pool: &Arc<Pool>, answer: Answer<T>) -> Answering<T> {
    let (files, pool) = (files.clone(), Arc::clone(pool));
    Box::new(move |written| match written {
        Ok((value, freed)) if !freed.is_empty() => pool.run_always(Box::new(move || {
            files.remove_freed(&freed);
            answer.send(Ok(value));
        })),
        written => answer.send(written.map(|(value, _)| value)),
    })
}

/// A write as an awaited call makes it: `submit` in its first poll, which
/// submits it to blobs.db's writer, its commit led on the pool; or on a node
/// whose blobs.db is a copy, `on_a_copy`, the blocking call, on a thread of
/// its own while the home answers.
fn write<T: Send + 'static>(
    owned: Owned,
    verb: &str,
    submit: impl FnOnce(&Owned, Answering<T>) + Send + 'static,
    on_a_copy: impl FnOnce(&Owned) -> Result<T> + Send + 'static,
) -> impl Future<Output = Result<T>> + Send + 'static {
    let what = owned.what(verb);
    async move {
        if passed_home(&owned.files) {
            return run::passed_on(what, move || on_a_copy(&owned)).await;
        }
        let pool = Arc::clone(&owned.pool);
        run::write(&pool, what, move |answer| {
            let done = answering(&owned.files, &owned.pool, answer);
            submit(&owned, done);
        })
        .await
    }
}

/// Whether the set's blobs.db is a copy, whose home takes its writes in a
/// blocking round trip.
fn passed_home(files: &crate::blobs::Files) -> bool {
    #[cfg(feature = "replication")]
    return files.is_copy();
    #[cfg(not(feature = "replication"))]
    {
        let _ = files;
        false
    }
}

/// Runs `work`, the blocking part of a write that was polled, on the store's
/// pool, and gives `answer` what it returns, or its panic. A pool that
/// stopped as its store closes drops it unrun, which answers `Closed`.
fn on_the_pool<T: Send + 'static>(
    pool: &Arc<Pool>,
    answer: Answer<T>,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) {
    pool.run(Box::new(move || match catch_unwind(AssertUnwindSafe(work)) {
        Ok(outcome) => answer.send(outcome),
        Err(payload) => answer.panicked(payload),
    }));
}

#[cfg(feature = "sql")]
impl crate::transaction::sealed::Sealed for Files {}

/// A set of files whose calls are awaited joins a database's transaction as
/// the blocking one does: `tx.with(&packages)` inside `db.tx`'s closure gives
/// its blocking calls there.
#[cfg(feature = "sql")]
impl crate::TxHandle for Files {
    type InTx<'h> = crate::blobs::TxFiles<'h>;

    fn in_tx<'h>(&'h self, tx: &'h crate::Transaction<'h>) -> crate::blobs::TxFiles<'h> {
        crate::TxHandle::in_tx(&self.blocking, tx)
    }
}

#[cfg(test)]
#[path = "blobs_tests.rs"]
mod tests;
