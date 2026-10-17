//! A write on a copy of blobs.db, passed to the file's home: the set by its
//! name and bounds, the call, and a file's bytes whole, run there on the same
//! set. A copy's upload gathers its bytes in memory, a bound at most, since
//! the ids its own files would take are its home's to give.
//!
//! ```text
//! name, ttl and max_file_size     the set's, each a u8 saying it is there
//! u8 call    0 put, 1 create, 2 delete, 3 copy, 4 rename, 5 expire
//! put, create      path, options, bytes
//! delete           path, if_match
//! copy, rename     from, to, options
//! expire           path, milliseconds
//! ```

use std::time::Duration;

use super::files::{Files, Options};
use crate::replication::{In, Out};
use crate::{Error, ErrorKind, Result, Store};

/// blobs' file, as a node names it.
pub(crate) const FILE: &str = "blobs/blobs.db";
/// The bytes a copy's upload gathers at most, before its home takes them.
pub(crate) const GATHERED: u64 = 256 << 20;

/// A call on a copy's set of files.
pub(crate) enum Call<'a> {
    Put { path: &'a str, options: &'a Options, bytes: &'a [u8], create: bool },
    Delete { path: &'a str, if_match: Option<&'a str> },
    Copy { from: &'a str, to: &'a str, options: &'a Options, rename: bool },
    Expire { path: &'a str, after: Duration },
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
    let mut out = Out(vec![crate::replication::BLOBS]);
    out.text(set.name);
    optional_long(&mut out, set.ttl.map(millis));
    optional_long(&mut out, set.max_file_size);
    match call {
        Call::Put { path, options, bytes, create } => {
            out.byte(u8::from(*create));
            out.text(path);
            options_out(&mut out, options);
            out.bytes(bytes);
        }
        Call::Delete { path, if_match } => {
            out.byte(2);
            out.text(path);
            optional_text(&mut out, *if_match);
        }
        Call::Copy { from, to, options, rename } => {
            out.byte(3 + u8::from(*rename));
            out.text(from);
            out.text(to);
            options_out(&mut out, options);
        }
        Call::Expire { path, after } => {
            out.byte(5);
            out.text(path);
            out.long(millis(*after));
        }
    }
    let answer = crate::replication::pass_on(store, FILE, out.0)?;
    In { bytes: &answer, at: 0 }.byte().map(|did| did == 1)
}

/// Runs at the home a call another node passed on, and says whether it wrote
/// or found a file.
pub(crate) fn run(store: &Store, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut at = In { bytes, at: 0 };
    let name = at.text()?;
    let ttl = optional_long_in(&mut at)?.map(Duration::from_millis);
    let max_file_size = optional_long_in(&mut at)?;
    let mut builder = store.files(&name);
    if let Some(ttl) = ttl {
        builder = builder.ttl(ttl);
    }
    if let Some(most) = max_file_size {
        builder = builder.max_file_size(most);
    }
    let files: Files = builder.open()?;
    let did = match at.byte()? {
        call @ (0 | 1) => {
            let path = at.text()?;
            let options = options_in(&mut at)?;
            let body = at.bytes()?;
            files.call(&path, options).written(body, call == 1)?.is_some()
        }
        2 => {
            let path = at.text()?;
            let if_match = optional_text_in(&mut at)?;
            let mut call = files.key(&path);
            if let Some(etag) = if_match {
                call = call.if_match(etag);
            }
            call.delete()?;
            true
        }
        call @ (3 | 4) => {
            let (from, to) = (at.text()?, at.text()?);
            let options = options_in(&mut at)?;
            let call_on = files.call(&to, options);
            if call == 4 {
                call_on.rename_from(&from)?
            } else {
                call_on.copy_from(&from)?
            };
            true
        }
        5 => {
            let path = at.text()?;
            files.expire(&path, Duration::from_millis(at.long()?))?
        }
        _ => return Err(corrupt("a call")),
    };
    if !at.done() {
        return Err(corrupt("a call that holds more than its fields"));
    }
    Ok(vec![u8::from(did)])
}

fn options_out(out: &mut Out, options: &Options) {
    optional_text(out, options.content_type.as_deref());
    out.byte(u8::from(options.meta_given));
    out.count(options.meta.len());
    for (name, value) in &options.meta {
        out.text(name);
        out.text(value);
    }
    optional_long(out, options.ttl.map(millis));
    optional_text(out, options.if_match.as_deref());
    optional_long(out, options.size);
}

fn options_in(at: &mut In<'_>) -> Result<Options> {
    let content_type = optional_text_in(at)?;
    let meta_given = at.byte()? == 1;
    let mut meta = std::collections::BTreeMap::new();
    for _ in 0..at.count()? {
        meta.insert(at.text()?, at.text()?);
    }
    let ttl = optional_long_in(at)?.map(Duration::from_millis);
    let if_match = optional_text_in(at)?;
    let size = optional_long_in(at)?;
    Ok(Options { content_type, meta, meta_given, ttl, if_match, size })
}

fn optional_text(out: &mut Out, text: Option<&str>) {
    out.byte(u8::from(text.is_some()));
    out.text(text.unwrap_or(""));
}

fn optional_text_in(at: &mut In<'_>) -> Result<Option<String>> {
    let there = at.byte()? == 1;
    let text = at.text()?;
    Ok(there.then_some(text))
}

fn optional_long(out: &mut Out, value: Option<u64>) {
    out.byte(u8::from(value.is_some()));
    out.long(value.unwrap_or(0));
}

fn optional_long_in(at: &mut In<'_>) -> Result<Option<u64>> {
    let there = at.byte()? == 1;
    let value = at.long()?;
    Ok(there.then_some(value))
}

fn millis(span: Duration) -> u64 {
    u64::try_from(span.as_millis()).unwrap_or(u64::MAX)
}

fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a blobs write passed on: {what}"))
}
