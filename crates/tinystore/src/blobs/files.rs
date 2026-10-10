//! A set of files by path, as a program opens it: `store.files("avatars")`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{OptionalExtension, params};

use super::change::{Change, Condition, Content, Place, Version, WhenThere, hidden};
use super::engine::Blobs;
use super::info::{FileInfo, FileRow, OCTETS, file_columns, matches, meta_text};
use super::paths::Folder;
use super::read::StoredFile;
use super::upload::Upload;
use crate::sqlite::sql_error;
use crate::{Error, ErrorKind, Key, Result, Store};

/// A content type's bytes at most.
const MAX_TYPE: usize = 256;
/// Meta's names and values together, at most.
const MAX_META: usize = 2 << 10;

/// What opens a set of files: its name, and the term and the bound of the
/// files written in it.
#[derive(Debug)]
pub struct FilesBuilder<'s> {
    store: &'s Store,
    name: String,
    ttl: Option<Duration>,
    max_file_size: Option<u64>,
}

impl<'s> FilesBuilder<'s> {
    pub(crate) fn new(store: &'s Store, name: &str) -> Self {
        FilesBuilder { store, name: name.to_owned(), ttl: None, max_file_size: None }
    }

    /// The term of every file written without its own, counted from its
    /// write; not a ceiling.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// The largest file a write may leave: one past it is refused and leaves
    /// nothing, and nothing stored is ever removed to make room.
    pub fn max_file_size(mut self, bytes: u64) -> Self {
        self.max_file_size = Some(bytes);
        self
    }

    pub fn open(self) -> Result<Files> {
        check_name(&self.name)?;
        let blobs = Blobs::of(self.store)?;
        let set = blobs.set_id(&self.name)?;
        Ok(Files {
            blobs,
            name: Arc::from(self.name),
            set,
            folder: Folder::default(),
            refused: None,
            ttl: self.ttl,
            max_file_size: self.max_file_size,
        })
    }
}

/// A set of files by path, or a folder of it: bytes written whole and
/// replaced whole, each with a type, an ETag and a few strings of the
/// program's.
#[derive(Clone)]
pub struct Files {
    pub(crate) blobs: Arc<Blobs>,
    name: Arc<str>,
    pub(crate) set: i64,
    pub(crate) folder: Folder,
    /// A folder that is not one, refused by the first call that uses it.
    refused: Option<Arc<Error>>,
    ttl: Option<Duration>,
    pub(crate) max_file_size: Option<u64>,
}

impl fmt::Debug for Files {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Files({}, {:?})", self.name, self.folder.prefix())
    }
}

impl Files {
    /// The folder in this one that `segment` names: `folder("users").folder(42)`
    /// is `users/42`, and its files' paths are its segments and then theirs.
    pub fn folder(&self, segment: impl Key) -> Files {
        let mut folder = self.clone();
        if folder.refused.is_none() {
            match self.folder.folder(&segment.text()) {
                Ok(inner) => folder.folder = inner,
                Err(error) => folder.refused = Some(Arc::new(self.fail(self.folder.prefix(), error))),
            }
        }
        folder
    }

    /// A call on one path, its options before its verb:
    /// `files.key("a.md").if_match(&etag).put(&text)`.
    pub fn key(&self, path: &str) -> FileCall<'_> {
        FileCall { files: self, path: path.to_owned(), options: Options::default() }
    }

    /// Writes the file and replaces whatever the path held, at the moment it
    /// returns.
    pub fn put(&self, path: &str, bytes: impl AsRef<[u8]>) -> Result<FileInfo> {
        self.key(path).put(bytes)
    }

    /// Writes the file only when the path holds none; says whether it did.
    pub fn create(&self, path: &str, bytes: impl AsRef<[u8]>) -> Result<bool> {
        self.key(path).create(bytes)
    }

    /// A file written a piece at a time, which appears at its `commit`.
    pub fn upload(&self, path: &str) -> Result<Upload> {
        self.key(path).upload()
    }

    /// The file at the path and the bytes it reads, which stay what they were
    /// now through a replace, a delete or an expiry; none when there is none.
    pub fn get(&self, path: &str) -> Result<Option<StoredFile>> {
        let whole = self.whole(path)?;
        StoredFile::open(self, &whole).map_err(|error| self.fail(&whole, error))
    }

    /// What the file at the path carries, none of its bytes.
    pub fn head(&self, path: &str) -> Result<Option<FileInfo>> {
        let whole = self.whole(path)?;
        self.head_of(&whole).map_err(|error| self.fail(&whole, error))
    }

    /// Removes the file at the path; nothing to remove is not an error.
    pub fn delete(&self, path: &str) -> Result<()> {
        self.key(path).delete()
    }

    /// Writes the file at `from` at `to` too, sharing its bytes: no byte is
    /// copied, and the two are two files from then on. A file at `to` is a
    /// conflict, unless `if_match` names the version to replace.
    pub fn copy(&self, from: &str, to: &str) -> Result<FileInfo> {
        self.key(to).copy_from(from)
    }

    /// Moves the file at `from` to `to` in one write, as `copy` writes it.
    pub fn rename(&self, from: &str, to: &str) -> Result<FileInfo> {
        self.key(to).rename_from(from)
    }

    /// Makes the file at the path expire `after` from now; says whether there
    /// was one.
    pub fn expire(&self, path: &str, after: Duration) -> Result<bool> {
        let whole = self.whole(path)?;
        self.expire_after(&whole, after).map_err(|error| self.fail(&whole, error))
    }

    pub(crate) fn whole(&self, path: &str) -> Result<String> {
        self.usable()?;
        self.folder.path(path).map_err(|error| self.fail(&format!("{}{path}", self.folder.prefix()), error))
    }

    /// Why a folder is not one, refused at the first call that uses it.
    pub(crate) fn usable(&self) -> Result<()> {
        self.refused.as_ref().map_or(Ok(()), |refused| Err(refused.duplicate()))
    }

    /// An error said of the files and the path it is about:
    /// `files avatars: "users/42/1.png": conflict`.
    pub(crate) fn fail(&self, path: &str, error: Error) -> Error {
        error.within(format!("{path:?}")).within(format!("files {}", self.name))
    }

    pub(crate) fn describe(&self) -> String {
        format!("files {}", self.name)
    }

    pub(crate) fn expiry(&self, ttl: Option<Duration>, now: i64) -> Option<i64> {
        ttl.or(self.ttl).map(|ttl| now.saturating_add(i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX)))
    }

    fn head_of(&self, whole: &str) -> Result<Option<FileInfo>> {
        const SELECT_HEAD: &str = concat!(
            "select ",
            file_columns!(),
            " from files as f where f.set_id = ?1 and f.path = ?2 and (f.expires is null or f.expires > ?3) and not ",
            hidden!()
        );
        let now = self.blobs.now();
        let row = self.blobs.file.read(|connection| {
            connection
                .prepare_cached(SELECT_HEAD)
                .and_then(|mut statement| statement.query_row(params![self.set, whole, now], FileRow::read).optional())
                .map_err(|error| sql_error("blobs: a file's head", error))
        })?;
        row.map(|row| row.info(&self.folder)).transpose()
    }

    /// Publishes new bytes at the path when `condition` holds against what it
    /// holds; says what it wrote, or nothing when a `create` found a file.
    pub(crate) fn publish(
        &self,
        whole: &str,
        options: &Options,
        content: Content,
        condition: Condition,
    ) -> Result<Option<FileInfo>> {
        let blobs = Arc::clone(&self.blobs);
        let files = self.clone();
        let (path, content_type, meta, ttl) =
            (whole.to_owned(), options.content_type(), meta_text(&options.meta), options.ttl);
        let bytes = match &content.place {
            Place::Inline(body) => body.len(),
            Place::File | Place::Shared => 0,
        };
        let (info, freed) = self.blobs.file.write(bytes, move |tx| {
            let mut change = Change::new(tx, &blobs);
            let version =
                Version { content, content_type, meta, modified: change.now, expires: files.expiry(ttl, change.now) };
            if !change.write(files.set, &path, &version, &condition)? {
                return Ok((None, Vec::new()));
            }
            Ok((Some(version.info(&files.folder, &path)?), change.freed))
        })?;
        self.blobs.let_files_go(&freed);
        Ok(info)
    }

    fn copy_to(&self, from: &str, to: &str, options: &Options, rename: bool) -> Result<FileInfo> {
        if from == to {
            return Err(Error::invalid("a copy or a rename onto its own path"));
        }
        let blobs = Arc::clone(&self.blobs);
        let files = self.clone();
        let (from, to, options) = (from.to_owned(), to.to_owned(), options.clone());
        let condition = Condition { if_match: options.if_match.clone(), when_there: WhenThere::Conflict };
        let (info, freed) = self.blobs.file.write(0, move |tx| {
            let mut change = Change::new(tx, &blobs);
            let source =
                change.current(files.set, &from)?.filter(|current| current.live(change.now)).ok_or_else(|| {
                    Error::not_found(format!("no file at {from:?} to {}", if rename { "rename" } else { "copy" }))
                })?;
            let version = Version {
                content: Content { id: source.content, size: source.size, sha256: source.sha256, place: Place::Shared },
                content_type: options.content_type.clone().unwrap_or(source.content_type),
                meta: if options.meta_given { meta_text(&options.meta) } else { source.meta },
                modified: change.now,
                expires: files.expiry(options.ttl, change.now),
            };
            change.write(files.set, &to, &version, &condition)?;
            if rename {
                change.remove(files.set, &from)?;
            }
            Ok((version.info(&files.folder, &to)?, change.freed))
        })?;
        self.blobs.let_files_go(&freed);
        Ok(info)
    }

    fn delete_at(&self, whole: &str, if_match: Option<String>) -> Result<()> {
        let blobs = Arc::clone(&self.blobs);
        let (set, path) = (self.set, whole.to_owned());
        let freed = self.blobs.file.write(0, move |tx| {
            let mut change = Change::new(tx, &blobs);
            if let Some(etag) = &if_match {
                let current = change.current(set, &path)?.filter(|current| current.live(change.now));
                if !current.is_some_and(|current| matches(etag, &current.sha256)) {
                    return Err(Error::new(ErrorKind::Conflict, "the path holds no file with the ETag given"));
                }
            }
            change.remove(set, &path)?;
            Ok(change.freed)
        })?;
        self.blobs.let_files_go(&freed);
        Ok(())
    }

    fn expire_after(&self, whole: &str, after: Duration) -> Result<bool> {
        let blobs = Arc::clone(&self.blobs);
        let (set, path) = (self.set, whole.to_owned());
        let after = i64::try_from(after.as_millis()).unwrap_or(i64::MAX);
        self.blobs.file.write(0, move |tx| {
            let change = Change::new(tx, &blobs);
            if !change.current(set, &path)?.is_some_and(|current| current.live(change.now)) {
                return Ok(false);
            }
            let expires = change.now.saturating_add(after);
            change.execute(
                "update files set expires = ?3 where set_id = ?1 and path = ?2",
                params![set, path, expires],
            )?;
            Ok(true)
        })
    }
}

/// A write's options, as a [`FileCall`] gathers them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Options {
    pub(crate) content_type: Option<String>,
    pub(crate) meta: BTreeMap<String, String>,
    pub(crate) meta_given: bool,
    pub(crate) ttl: Option<Duration>,
    pub(crate) if_match: Option<String>,
    /// How long the body is, as a `Content-Length` says it.
    pub(crate) size: Option<u64>,
}

impl Options {
    fn content_type(&self) -> String {
        self.content_type.clone().unwrap_or_else(|| OCTETS.to_owned())
    }

    fn check(&self) -> Result<()> {
        if self.content_type.as_ref().is_some_and(|content_type| content_type.len() > MAX_TYPE) {
            return Err(Error::invalid(format!("a content type over {MAX_TYPE} bytes")));
        }
        let meta: usize = self.meta.iter().map(|(name, value)| name.len() + value.len()).sum();
        if meta > MAX_META {
            return Err(Error::invalid(format!("meta of {meta} bytes, over 2 KiB")));
        }
        Ok(())
    }
}

/// One path and the options of a call on it, which ends in its verb.
#[derive(Debug)]
#[must_use = "a call does nothing until its verb"]
pub struct FileCall<'f> {
    files: &'f Files,
    path: String,
    options: Options,
}

impl FileCall<'_> {
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.options.content_type = Some(content_type.into());
        self
    }

    /// A string of the program's the file carries, such as the name it was
    /// uploaded as.
    pub fn meta(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.meta.insert(name.into(), value.into());
        self.options.meta_given = true;
        self
    }

    /// The file's own term, from its write.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.options.ttl = Some(ttl);
        self
    }

    /// Writes only over the file with this ETag: one that changed, expired or
    /// went since is a conflict, and nothing is written.
    pub fn if_match(mut self, etag: impl Into<String>) -> Self {
        self.options.if_match = Some(etag.into());
        self
    }

    /// How long the body is; one longer or shorter is refused and leaves
    /// nothing.
    pub fn size(mut self, bytes: u64) -> Self {
        self.options.size = Some(bytes);
        self
    }

    pub fn put(self, bytes: impl AsRef<[u8]>) -> Result<FileInfo> {
        let condition = Condition { if_match: self.options.if_match.clone(), when_there: WhenThere::Replace };
        self.write_all(bytes.as_ref(), condition)?.ok_or_else(|| Error::internal("a put that wrote nothing"))
    }

    /// Writes only when the path holds no file; says whether it did.
    pub fn create(self, bytes: impl AsRef<[u8]>) -> Result<bool> {
        let condition = Condition { if_match: self.options.if_match.clone(), when_there: WhenThere::Skip };
        self.write_all(bytes.as_ref(), condition).map(|info| info.is_some())
    }

    /// A file written a piece at a time, which appears at its `commit`.
    pub fn upload(self) -> Result<Upload> {
        let whole = self.files.whole(&self.path)?;
        self.options.check().map_err(|error| self.files.fail(&whole, error))?;
        Upload::start(self.files.clone(), whole, self.options)
    }

    pub fn delete(self) -> Result<()> {
        let whole = self.files.whole(&self.path)?;
        self.files.delete_at(&whole, self.options.if_match).map_err(|error| self.files.fail(&whole, error))
    }

    /// Writes the file at `from` here too, sharing its bytes.
    pub fn copy_from(self, from: &str) -> Result<FileInfo> {
        self.copied(from, false)
    }

    /// Moves the file at `from` here in one write.
    pub fn rename_from(self, from: &str) -> Result<FileInfo> {
        self.copied(from, true)
    }

    fn copied(self, from: &str, rename: bool) -> Result<FileInfo> {
        let (source, target) = (self.files.whole(from)?, self.files.whole(&self.path)?);
        self.options.check().map_err(|error| self.files.fail(&target, error))?;
        self.files.copy_to(&source, &target, &self.options, rename).map_err(|error| self.files.fail(&target, error))
    }

    /// Writes the whole file, or with `create` only where no file is; says
    /// what it wrote, nothing when a create found a file. The wire's put.
    pub(crate) fn written(self, bytes: &[u8], create: bool) -> Result<Option<FileInfo>> {
        let when_there = if create { WhenThere::Skip } else { WhenThere::Replace };
        let condition = Condition { if_match: self.options.if_match.clone(), when_there };
        self.write_all(bytes, condition)
    }

    fn write_all(self, bytes: &[u8], condition: Condition) -> Result<Option<FileInfo>> {
        let whole = self.files.whole(&self.path)?;
        self.options.check().map_err(|error| self.files.fail(&whole, error))?;
        let mut upload = Upload::start(self.files.clone(), whole, self.options)?;
        upload.take(bytes)?;
        upload.finish(condition)
    }
}

/// A set's name, as a file's is: short and plain.
fn check_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if first && rest && name.len() <= 64 {
        return Ok(());
    }
    Err(Error::invalid(format!("a name is [a-z0-9][a-z0-9_-]{{0,63}}, not {name:?}")))
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
