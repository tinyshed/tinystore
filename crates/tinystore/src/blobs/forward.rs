//! A write on a copy of blobs.db, passed to the file's home: the set by its
//! name and bounds, the call, and a file's bytes whole, run there on the same
//! set. A copy's upload gathers its bytes in memory, a bound at most, since
//! the ids its own files would take are its home's to give.
//!
//! ```text
//! name, ttl and max_file_size     the set's, each a u8 saying it is there
//! u8 call    0 put, 1 create, 2 delete, 3 copy, 4 rename, 5 expire, 6 clear,
//!            7 use, 8 ceiling, 9 trim
//! put, create      path, options, bytes
//! delete           path, if_match
//! copy, rename     from, to, options
//! expire           path, milliseconds
//! clear            the folder's whole prefix, users/42/, or nothing for the set
//! use              path, the revision its read saw, when, the term it renews to
//! ceiling          max_size, when_full, and u8 0 any, 1 none, 2 this ceiling and it
//! trim             to; answered with the files removed and their bytes
//! ```

use std::time::Duration;

use super::cache::{Ceiling, Expect, Trim, WhenFull};
use super::files::{Files, Options};
use super::uses::Use;
use crate::replication::{In, Out};
use crate::{Error, ErrorKind, Result, Store};

pub(crate) use super::engine::FILE_IN_STORE as FILE;
/// The bytes a copy's upload gathers at most, before its home takes them.
pub(crate) const GATHERED: u64 = 256 << 20;

/// A call on a copy's set of files.
pub(crate) enum Call<'a> {
    Put { path: &'a str, options: &'a Options, bytes: &'a [u8], create: bool },
    Delete { path: &'a str, if_match: Option<&'a str> },
    Copy { from: &'a str, to: &'a str, options: &'a Options, rename: bool },
    Expire { path: &'a str, after: Duration },
    Clear { folder: &'a str },
    Use { path: &'a str, used: &'a Use },
    Ceiling { wanted: Option<Ceiling>, expect: Expect },
    Trim { to: u64 },
}

/// The set a call is on, as its home opens it.
pub(crate) struct Set<'a> {
    pub(crate) name: &'a str,
    pub(crate) ttl: Option<Duration>,
    pub(crate) max_file_size: Option<u64>,
}

/// Passes `call` to the home of the copy blobs.db is, and says whether it
/// wrote or found a file.
pub(crate) fn pass_on(store: &Store, set: &Set<'_>, call: &Call<'_>) -> Result<bool> {
    let answer = pass_on_answered(store, set, call)?;
    In { bytes: &answer, at: 0 }.byte().map(|did| did == 1)
}

/// Passes `call` on as [`pass_on`] does, and hands back the home's answer.
fn pass_on_answered(store: &Store, set: &Set<'_>, call: &Call<'_>) -> Result<Vec<u8>> {
    crate::replication::pass_on(store, FILE, request(set, call))
}

/// What a copy passes its home for `call` on `set`.
fn request(set: &Set<'_>, call: &Call<'_>) -> Vec<u8> {
    let mut out = Out(vec![crate::replication::BLOBS]);
    out.text(set.name).maybe_long(set.ttl.map(millis)).maybe_long(set.max_file_size);
    match call {
        Call::Put { path, options, bytes, create } => {
            options_out(out.byte(u8::from(*create)).text(path), options).bytes(bytes)
        }
        Call::Delete { path, if_match } => out.byte(2).text(path).maybe_text(*if_match),
        Call::Copy { from, to, options, rename } => {
            options_out(out.byte(3 + u8::from(*rename)).text(from).text(to), options)
        }
        Call::Expire { path, after } => out.byte(5).text(path).span(*after),
        Call::Clear { folder } => out.byte(6).text(folder),
        Call::Use { path, used } => out
            .byte(7)
            .text(path)
            .long(used.revision.cast_unsigned())
            .long(used.used.cast_unsigned())
            .maybe_long(used.until.map(i64::cast_unsigned)),
        Call::Ceiling { wanted, expect } => {
            out.byte(8)
                .maybe_long(wanted.map(|ceiling| ceiling.max_size))
                .maybe_text(wanted.map(|ceiling| ceiling.when_full.as_str()));
            match expect {
                Expect::Any => out.byte(0),
                Expect::Absent => out.byte(1),
                Expect::Is(current) => out.byte(2).long(*current),
            }
        }
        Call::Trim { to } => out.byte(9).long(*to),
    };
    out.0
}

/// Passes a copy's trim to its home, and says what it removed there.
pub(crate) fn pass_trim(store: &Store, set: &Set<'_>, to: u64) -> Result<Trim> {
    let answer = pass_on_answered(store, set, &Call::Trim { to })?;
    let mut at = In { bytes: &answer, at: 0 };
    at.byte()?;
    Ok(Trim { evictions: at.long()?, freed: at.long()? })
}

/// Passes a copy's uses to its home, which records them, and waits for no
/// answer: a use that does not reach it is asked again by the file's next
/// read. Says how many it passed.
pub(crate) fn pass_uses(store: &Store, uses: Vec<((i64, String), Use)>) -> Result<usize> {
    if uses.is_empty() {
        return Ok(0);
    }
    let requests: Vec<Vec<u8>> = uses
        .iter()
        .map(|((_, path), used)| {
            let set = Set { name: &used.name, ttl: None, max_file_size: None };
            request(&set, &Call::Use { path, used })
        })
        .collect();
    crate::replication::pass_on_all_then(store, FILE, &requests, |_| {})?;
    Ok(requests.len())
}

/// Runs at the home a call another node passed on, and says whether it wrote
/// or found a file.
pub(crate) fn run(store: &Store, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut at = In { bytes, at: 0 };
    let files = open_set(store, &mut at)?;
    let answer = run_call(files, &mut at)?;
    if !at.done() {
        return Err(corrupt("a call that holds more than its fields"));
    }
    Ok(answer)
}

/// Opens the set a call names, with the bounds it was opened with.
fn open_set(store: &Store, at: &mut In<'_>) -> Result<Files> {
    let name = at.text()?;
    let ttl = at.maybe_long()?.map(Duration::from_millis);
    let max_file_size = at.maybe_long()?;
    let mut builder = store.files(&name);
    if let Some(ttl) = ttl {
        builder = builder.ttl(ttl);
    }
    if let Some(most) = max_file_size {
        builder = builder.max_file_size(most);
    }
    builder.open()
}

fn run_call(files: Files, at: &mut In<'_>) -> Result<Vec<u8>> {
    let did = match at.byte()? {
        call @ (0 | 1) => {
            let path = at.text()?;
            let options = options_in(at)?;
            let body = at.bytes()?;
            files.call(&path, options).written(body, call == 1)?.is_some()
        }
        2 => delete(&files, at)?,
        call @ (3 | 4) => copy(&files, at, call == 4)?,
        5 => {
            let path = at.text()?;
            files.expire(&path, at.span()?)?
        }
        6 => clear(files, at)?,
        7 => {
            let path = at.text()?;
            let (revision, used) = (at.long()?.cast_signed(), at.long()?.cast_signed());
            let until = at.maybe_long()?.map(u64::cast_signed);
            files.use_passed_on(&path, &Use { name: std::sync::Arc::clone(&files.name), revision, used, until })?
        }
        8 => {
            let max_size = at.maybe_long()?;
            let when_full = at.maybe_text()?.map(|text| WhenFull::of(&text)).transpose()?;
            let expect = match at.byte()? {
                0 => Expect::Any,
                1 => Expect::Absent,
                2 => Expect::Is(at.long()?),
                _ => return Err(corrupt("a ceiling's expectation")),
            };
            let wanted = max_size.map(|max_size| Ceiling { max_size, when_full: when_full.unwrap_or_default() });
            files.change_ceiling(wanted, expect)?
        }
        9 => {
            let trim = files.trimmed(at.long()?, None)?;
            let mut out = Out(vec![1]);
            out.long(trim.evictions).long(trim.freed);
            return Ok(out.0);
        }
        _ => return Err(corrupt("a call")),
    };
    Ok(vec![u8::from(did)])
}

fn delete(files: &Files, at: &mut In<'_>) -> Result<bool> {
    let path = at.text()?;
    let if_match = at.maybe_text()?;
    let mut call = files.key(&path);
    if let Some(etag) = if_match {
        call = call.if_match(etag);
    }
    call.delete()?;
    Ok(true)
}

fn copy(files: &Files, at: &mut In<'_>, rename: bool) -> Result<bool> {
    let (from, to) = (at.text()?, at.text()?);
    let options = options_in(at)?;
    let call_on = files.call(&to, options);
    if rename {
        call_on.rename_from(&from)?
    } else {
        call_on.copy_from(&from)?
    };
    Ok(true)
}

fn clear(files: Files, at: &mut In<'_>) -> Result<bool> {
    let prefix = at.text()?;
    let folder =
        prefix.split('/').filter(|segment| !segment.is_empty()).fold(files, |in_it, segment| in_it.folder(segment));
    folder.clear()?;
    Ok(true)
}

fn options_out<'a>(out: &'a mut Out, options: &Options) -> &'a mut Out {
    out.maybe_text(options.content_type.as_deref()).byte(u8::from(options.meta_given));
    out.count(options.meta.len());
    for (name, value) in &options.meta {
        out.text(name).text(value);
    }
    out.maybe_long(options.ttl.map(millis)).maybe_text(options.if_match.as_deref()).maybe_long(options.size)
}

fn options_in(at: &mut In<'_>) -> Result<Options> {
    let content_type = at.maybe_text()?;
    let meta_given = at.byte()? == 1;
    let mut meta = std::collections::BTreeMap::new();
    for _ in 0..at.count()? {
        meta.insert(at.text()?, at.text()?);
    }
    let ttl = at.maybe_long()?.map(Duration::from_millis);
    let if_match = at.maybe_text()?;
    let size = at.maybe_long()?;
    Ok(Options { content_type, meta, meta_given, ttl, if_match, size, ..Options::default() })
}

fn millis(span: Duration) -> u64 {
    crate::clock::millis(span) as u64
}

fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a blobs write passed on: {what}"))
}
