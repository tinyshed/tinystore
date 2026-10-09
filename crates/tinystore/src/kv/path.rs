//! Where a key lives: its branch's owners and the key, in one byte string a
//! branch's keys share as a prefix, so that a branch is one range of the
//! table and a clear removes the branches under it with it.
//!
//! ```text
//! of("tenant-7").of(42), "iPhone" → 01 tenant-7 00 · 01 42 00 · 02 iPhone
//! a 00 inside a name              → 00 FF
//! ```
//!
//! An owner ends with 00, so the byte after a branch's prefix is 01 or 02, and
//! every path under a prefix lies in `[prefix, prefix 03)`.

use std::borrow::Cow;

use crate::{Error, Result};

/// The longest path a key may have, its owners included, as the file keeps it.
pub(crate) const MAX_PATH: usize = 1 << 10;

const OWNER: u8 = 0x01;
const KEY: u8 = 0x02;
const PAST: u8 = 0x03;
const END: u8 = 0x00;
const ESCAPE: u8 = 0xff;

/// What names a key or an owner: text, or an integer spelled in decimal, so
/// that `42` and `"42"` are one key.
pub trait Key {
    fn text(&self) -> Cow<'_, str>;
}

impl Key for str {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
}

impl Key for String {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
}

impl<K: Key + ?Sized> Key for &K {
    fn text(&self) -> Cow<'_, str> {
        (**self).text()
    }
}

macro_rules! integer_keys {
    ($($integer:ty),*) => {
        $(impl Key for $integer {
            fn text(&self) -> Cow<'_, str> {
                Cow::Owned(self.to_string())
            }
        })*
    };
}

integer_keys!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize);

/// A branch: the owners above its keys, as a prefix of their paths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Branch {
    prefix: Vec<u8>,
    owners: Vec<String>,
}

impl Branch {
    /// The branch under this one that `owner` names.
    pub(crate) fn of(&self, owner: &str) -> Result<Branch> {
        if owner.is_empty() {
            return Err(Error::invalid(format!("{}: an empty owner", self.shown())));
        }
        let mut prefix = self.prefix.clone();
        prefix.push(OWNER);
        escape(&mut prefix, owner);
        prefix.push(END);
        let mut owners = self.owners.clone();
        owners.push(owner.to_owned());
        let branch = Branch { prefix, owners };
        branch.check_length(branch.prefix.len())?;
        Ok(branch)
    }

    /// The path of `key` in this branch.
    pub(crate) fn path(&self, key: &str) -> Result<Vec<u8>> {
        if key.is_empty() {
            return Err(Error::invalid(format!("{}: an empty key", self.shown())));
        }
        let mut path = self.keys_from();
        escape(&mut path, key);
        self.check_length(path.len())?;
        Ok(path)
    }

    /// The key a path of this branch holds.
    pub(crate) fn key_of(&self, path: &[u8]) -> Result<String> {
        let escaped = path
            .strip_prefix(self.keys_from().as_slice())
            .ok_or_else(|| Error::corrupt(format!("{}: a path outside its branch", self.shown())))?;
        let mut key = Vec::with_capacity(escaped.len());
        let mut bytes = escaped.iter();
        while let Some(&byte) = bytes.next() {
            key.push(byte);
            if byte == END && bytes.next() != Some(&ESCAPE) {
                return Err(Error::corrupt(format!("{}: a key with an unescaped 00", self.shown())));
            }
        }
        String::from_utf8(key).map_err(|_| Error::corrupt(format!("{}: a key that is not UTF-8", self.shown())))
    }

    pub(crate) fn prefix(&self) -> &[u8] {
        &self.prefix
    }

    /// Every path under this branch, its own keys and those of the branches
    /// under it: `[prefix, prefix 03)`.
    pub(crate) fn everything(&self) -> (Vec<u8>, Vec<u8>) {
        let mut past = self.prefix.clone();
        past.push(PAST);
        (self.prefix.clone(), past)
    }

    /// This branch's own keys, after `after` when a page continues:
    /// `(prefix 02 after, prefix 03)`, the lower bound excluded.
    pub(crate) fn own_keys_after(&self, after: Option<&str>) -> (Vec<u8>, Vec<u8>) {
        let mut from = self.keys_from();
        if let Some(after) = after {
            escape(&mut from, after);
        }
        let mut past = self.prefix.clone();
        past.push(PAST);
        (from, past)
    }

    /// The owners and a key as an error names them: `tenant-7/42/iPhone`.
    pub(crate) fn shown_key(&self, key: &str) -> String {
        let mut names = self.owners.clone();
        names.push(key.to_owned());
        format!("key {:?}", names.join("/"))
    }

    pub(crate) fn shown(&self) -> String {
        if self.owners.is_empty() {
            return "its root".to_owned();
        }
        format!("branch {:?}", self.owners.join("/"))
    }

    fn keys_from(&self) -> Vec<u8> {
        let mut path = self.prefix.clone();
        path.push(KEY);
        path
    }

    fn check_length(&self, length: usize) -> Result<()> {
        if length > MAX_PATH {
            return Err(Error::invalid(format!(
                "{}: a path of {length} bytes, over the {MAX_PATH} its owners and key may take",
                self.shown()
            )));
        }
        Ok(())
    }
}

fn escape(path: &mut Vec<u8>, name: &str) {
    for &byte in name.as_bytes() {
        path.push(byte);
        if byte == END {
            path.push(ESCAPE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_keeps_its_names_apart() {
        let branch = Branch::default().of("tenant-7").unwrap().of(&42.text()).unwrap();
        let path = branch.path("iPhone").unwrap();
        assert_eq!(path, b"\x01tenant-7\x00\x0142\x00\x02iPhone");
        assert_eq!(branch.key_of(&path).unwrap(), "iPhone");
    }

    #[test]
    fn a_zero_byte_inside_a_name_is_escaped_and_read_back() {
        let branch = Branch::default().of("a\0b").unwrap();
        let path = branch.path("c\0d").unwrap();
        assert_eq!(path, b"\x01a\x00\xffb\x00\x02c\x00\xffd");
        assert_eq!(branch.key_of(&path).unwrap(), "c\0d");
    }

    #[test]
    fn an_owner_that_starts_like_another_is_not_under_it() {
        let a = Branch::default().of("a").unwrap();
        let a_zero = Branch::default().of("a\0").unwrap();
        let (from, past) = a.everything();
        let path = a_zero.path("k").unwrap();
        assert!(path.starts_with(&from), "it shares the prefix's bytes");
        assert!(!(path.as_slice() >= from.as_slice() && path.as_slice() < past.as_slice()));
    }

    #[test]
    fn an_integer_key_is_its_decimal_spelling() {
        let branch = Branch::default();
        assert_eq!(branch.path(&42u8.text()).unwrap(), branch.path("42").unwrap());
        assert_eq!(branch.path(&(-1i64).text()).unwrap(), branch.path("-1").unwrap());
    }

    #[test]
    fn an_empty_or_too_long_key_is_refused() {
        let branch = Branch::default();
        assert!(branch.path("").is_err());
        assert!(branch.of("").is_err());
        assert!(branch.path(&"k".repeat(MAX_PATH)).is_err());
        assert!(branch.path(&"k".repeat(MAX_PATH - 1)).is_ok());
    }

    #[test]
    fn an_error_names_the_owners_and_the_key() {
        let branch = Branch::default().of("user-1").unwrap();
        assert_eq!(branch.shown_key("token"), r#"key "user-1/token""#);
        assert_eq!(branch.shown(), r#"branch "user-1""#);
        assert_eq!(Branch::default().shown(), "its root");
    }
}
