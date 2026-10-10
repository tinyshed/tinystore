//! One write's statements in its transaction: what the path holds, the
//! condition checked against it, the row written, and the names a content
//! gains and loses. A content left without names goes: an inline one here,
//! a file's after the commit.

use rusqlite::{OptionalExtension, Params, params};

use super::engine::Blobs;
use super::info::{FileInfo, matches, meta_of};
use super::paths::Folder;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, ErrorKind, Result};

/// Whether a marked clear hid a file aliased `f`: its revision is the mark's
/// or older, under the mark's folder.
macro_rules! hidden {
    () => {
        "exists (select 1 from cleared as c where c.set_id = f.set_id and f.revision <= c.revision
            and substr(f.path, 1, length(c.prefix)) = c.prefix)"
    };
}
pub(crate) use hidden;

/// The bytes a write names: new ones, kept inline or in their file, or the
/// ones another file names, which a copy shares.
pub(crate) struct Content {
    pub(crate) id: i64,
    pub(crate) size: i64,
    pub(crate) sha256: Vec<u8>,
    pub(crate) place: Place,
}

pub(crate) enum Place {
    Inline(Vec<u8>),
    File,
    Shared,
}

/// A file's row as a write makes it.
pub(crate) struct Version {
    pub(crate) content: Content,
    pub(crate) content_type: String,
    pub(crate) meta: Option<String>,
    pub(crate) modified: i64,
    pub(crate) expires: Option<i64>,
}

impl Version {
    pub(crate) fn info(&self, folder: &Folder, path: &str) -> Result<FileInfo> {
        Ok(FileInfo {
            path: folder.relative(path).to_owned(),
            size: u64::try_from(self.content.size).unwrap_or(0),
            etag: super::info::etag_of(&self.content.sha256),
            content_type: self.content_type.clone(),
            last_modified: Blobs::time(self.modified),
            expires: self.expires.map(Blobs::time),
            meta: meta_of(self.meta.as_deref())?,
        })
    }
}

/// A path's row as a write finds it, hidden when a marked clear hid it.
pub(crate) struct Current {
    pub(crate) content: i64,
    pub(crate) size: i64,
    pub(crate) sha256: Vec<u8>,
    pub(crate) content_type: String,
    pub(crate) expires: Option<i64>,
    pub(crate) meta: Option<String>,
    hidden: bool,
}

impl Current {
    pub(crate) fn live(&self, now: i64) -> bool {
        !self.hidden && self.expires.is_none_or(|expires| expires > now)
    }
}

/// What a file already at the path does to a write that names no ETag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WhenThere {
    /// `put` and an upload's `commit` write the path.
    #[default]
    Replace,
    /// `create` writes nothing, and says so.
    Skip,
    /// `copy` and `rename` never replace a file by surprise.
    Conflict,
}

/// A write's condition on what the path holds.
#[derive(Clone, Debug, Default)]
pub(crate) struct Condition {
    /// The ETag of the file the write may replace, and that file only.
    pub(crate) if_match: Option<String>,
    pub(crate) when_there: WhenThere,
}

impl Condition {
    /// Whether the write goes ahead against what the path holds.
    fn holds(&self, existing: Option<&Current>, now: i64) -> Result<bool> {
        let live = existing.filter(|current| current.live(now));
        if let Some(etag) = &self.if_match {
            return match live {
                Some(current) if matches(etag, &current.sha256) => Ok(true),
                _ => Err(Error::new(ErrorKind::Conflict, "the path holds no file with the ETag given")),
            };
        }
        match (live, self.when_there) {
            (None, _) | (Some(_), WhenThere::Replace) => Ok(true),
            (Some(_), WhenThere::Skip) => Ok(false),
            (Some(_), WhenThere::Conflict) => Err(Error::new(ErrorKind::Conflict, "the path holds a file")),
        }
    }
}

const SELECT_CURRENT: &str = concat!(
    "select f.content, f.size, f.sha256, f.type, f.expires, f.meta, ",
    hidden!(),
    " from files as f where f.set_id = ?1 and f.path = ?2"
);
const INSERT_CONTENT: &str = "insert into contents (id, names, size, sha256, inline) values (?1, 1, ?2, ?3, ?4)";
const INSERT_BODY: &str = "insert into bodies (id, bytes) values (?1, ?2)";
const NAME_CONTENT: &str = "update contents set names = names + 1 where id = ?1 returning names";
const UPSERT_FILE: &str =
    "insert into files (set_id, path, revision, content, size, sha256, type, modified, expires, meta)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
    on conflict (set_id, path) do update set revision = excluded.revision, content = excluded.content,
        size = excluded.size, sha256 = excluded.sha256, type = excluded.type, modified = excluded.modified,
        expires = excluded.expires, meta = excluded.meta";
const DELETE_FILE: &str = "delete from files where set_id = ?1 and path = ?2 returning content";
const UNNAME_CONTENT: &str = "update contents set names = names - ?2 where id = ?1 returning names, inline";
const DROP_CONTENT: &str = "delete from contents where id = ?1";
const DROP_BODY: &str = "delete from bodies where id = ?1";

/// One write's transaction, and the files of the contents it left without
/// names, which go once it commits.
pub(crate) struct Change<'t, 'c> {
    tx: &'t Tx<'c>,
    blobs: &'t Blobs,
    /// The store's clock in the transaction, unix milliseconds.
    pub(crate) now: i64,
    pub(crate) freed: Vec<i64>,
}

impl<'t, 'c> Change<'t, 'c> {
    pub(crate) fn new(tx: &'t Tx<'c>, blobs: &'t Blobs) -> Self {
        Change { tx, blobs, now: blobs.now(), freed: Vec::new() }
    }

    pub(crate) fn current(&self, set: i64, path: &str) -> Result<Option<Current>> {
        self.tx
            .prepare_cached(SELECT_CURRENT)
            .and_then(|mut statement| {
                statement
                    .query_row(params![set, path], |row| {
                        Ok(Current {
                            content: row.get(0)?,
                            size: row.get(1)?,
                            sha256: row.get(2)?,
                            content_type: row.get(3)?,
                            expires: row.get(4)?,
                            meta: row.get(5)?,
                            hidden: row.get(6)?,
                        })
                    })
                    .optional()
            })
            .map_err(|error| sql_error("blobs: a file's row", error))
    }

    /// Writes `version` at the path when `condition` holds against what it
    /// holds, and takes a name from the content it named; says whether it
    /// wrote.
    pub(crate) fn write(&mut self, set: i64, path: &str, version: &Version, condition: &Condition) -> Result<bool> {
        let existing = self.current(set, path)?;
        if !condition.holds(existing.as_ref(), self.now)? {
            return Ok(false);
        }
        let revision = self.blobs.next_revision(self.tx)?;
        self.name(&version.content)?;
        let content = &version.content;
        self.execute(
            UPSERT_FILE,
            params![
                set,
                path,
                revision,
                content.id,
                content.size,
                content.sha256,
                version.content_type,
                version.modified,
                version.expires,
                version.meta
            ],
        )?;
        if let Some(existing) = existing {
            self.release(existing.content, 1)?;
        }
        Ok(true)
    }

    /// Gives a content the name a write makes: new bytes are written, the
    /// body with them when inline, and a shared content counts one more.
    fn name(&mut self, content: &Content) -> Result<()> {
        match &content.place {
            Place::Shared => {
                let names: Option<i64> = self
                    .tx
                    .prepare_cached(NAME_CONTENT)
                    .and_then(|mut statement| statement.query_row([content.id], |row| row.get(0)).optional())
                    .map_err(|error| sql_error("blobs: name a content", error))?;
                names
                    .map(drop)
                    .ok_or_else(|| Error::corrupt(format!("content {}, which a file names, is missing", content.id)))
            }
            Place::Inline(body) => {
                self.execute(INSERT_CONTENT, params![content.id, content.size, content.sha256, true])?;
                self.execute(INSERT_BODY, params![content.id, body])
            }
            Place::File => self.execute(INSERT_CONTENT, params![content.id, content.size, content.sha256, false]),
        }
    }

    /// Deletes a path's row, whatever it holds, and takes a name from its
    /// content; says whether there was one.
    pub(crate) fn remove(&mut self, set: i64, path: &str) -> Result<bool> {
        let content: Option<i64> = self
            .tx
            .prepare_cached(DELETE_FILE)
            .and_then(|mut statement| statement.query_row(params![set, path], |row| row.get(0)).optional())
            .map_err(|error| sql_error("blobs: delete a file's row", error))?;
        match content {
            Some(id) => self.release(id, 1).map(|()| true),
            None => Ok(false),
        }
    }

    /// Takes `count` names from a content: one left without names goes, an
    /// inline one here with its bytes, a file's once the write has committed.
    pub(crate) fn release(&mut self, id: i64, count: i64) -> Result<()> {
        let left: Option<(i64, bool)> = self
            .tx
            .prepare_cached(UNNAME_CONTENT)
            .and_then(|mut statement| {
                statement.query_row(params![id, count], |row| Ok((row.get(0)?, row.get(1)?))).optional()
            })
            .map_err(|error| sql_error("blobs: unname a content", error))?;
        match left {
            None => Err(Error::corrupt(format!("content {id}, which a file named, is missing"))),
            Some((names, _)) if names < 0 => Err(Error::corrupt(format!("content {id} lost more names than it had"))),
            Some((names, _)) if names > 0 => Ok(()),
            Some((_, false)) => {
                self.freed.push(id);
                Ok(())
            }
            Some((_, true)) => {
                self.execute(DROP_BODY, [id])?;
                self.execute(DROP_CONTENT, [id])
            }
        }
    }

    /// Takes a name from the content of each file a batch deleted, a
    /// statement a content however many of its files the batch held.
    pub(crate) fn release_all(&mut self, mut contents: Vec<i64>) -> Result<()> {
        contents.sort_unstable();
        for run in contents.chunk_by(|a, b| a == b) {
            self.release(run[0], i64::try_from(run.len()).unwrap_or(i64::MAX))?;
        }
        Ok(())
    }

    /// Runs a delete that returns the content of each file it deleted.
    pub(crate) fn deleted(&self, sql: &str, parameters: impl Params) -> Result<Vec<i64>> {
        let mut statement = self.tx.prepare_cached(sql).map_err(|error| sql_error("blobs: delete files", error))?;
        let rows = statement
            .query_map(parameters, |row| row.get(0))
            .map_err(|error| sql_error("blobs: delete files", error))?;
        rows.collect::<rusqlite::Result<Vec<i64>>>().map_err(|error| sql_error("blobs: delete files", error))
    }

    pub(crate) fn execute(&self, sql: &str, parameters: impl Params) -> Result<()> {
        self.tx
            .prepare_cached(sql)
            .and_then(|mut statement| statement.execute(parameters))
            .map(drop)
            .map_err(|error| sql_error("blobs: a write", error))
    }

    pub(crate) fn next_revision(&self) -> Result<i64> {
        self.blobs.next_revision(self.tx)
    }
}
