//! The marks a Rust declaration puts on its settings, as types: a secret, a
//! setting with no default, a setting that changes while the program runs.
//!
//! A mark serializes as its value, and a secret as `***`, wherever the
//! program sends it. While the core reads a declaration, or compares two
//! values of one, a mark serializes as a map of one key the core reads
//! instead, so that the marks reach the core through the program's own serde
//! derive and need no derive of their own.

use std::cell::Cell;
use std::fmt;
use std::ops::{Deref, DerefMut};

use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The keys of the maps the marks write while the core reads, each after a
/// NUL so that no key of a program's own can be one.
pub(super) const SECRET: &str = "\0tinystore:secret";
pub(super) const SECRET_REQUIRED: &str = "\0tinystore:secret required";
pub(super) const REQUIRED: &str = "\0tinystore:required";
pub(super) const LIVE: &str = "\0tinystore:live";

thread_local! {
    static READING: Cell<bool> = const { Cell::new(false) };
}

/// `value` as the core reads it: each mark as its map, each secret with its
/// text.
pub(super) fn marked<T: Serialize>(value: &T) -> serde_json::Result<serde_json::Value> {
    let _reading = Reading(READING.replace(true));
    serde_json::to_value(value)
}

/// Puts back what the thread was doing when a read began, a panic too.
struct Reading(bool);

impl Drop for Reading {
    fn drop(&mut self) {
        READING.set(self.0);
    }
}

fn mark<S: Serializer, T: Serialize>(serializer: S, key: &str, value: &T) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(1))?;
    map.serialize_entry(key, value)?;
    map.end()
}

/// A setting that prints as `***` wherever it goes, `Debug`, `Display`,
/// serde and the logs, and whose text `reveal` gives. A secret is never kept,
/// so that only a file or a variable gives it.
///
/// `Secret::default()` holds no value, and a layer must give it;
/// `Secret::new` gives it a default.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T> {
    value: T,
    required: bool,
}

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Secret { value, required: false }
    }

    /// The secret's value, where the program hands it to what needs it.
    pub fn reveal(&self) -> &T {
        &self.value
    }
}

impl<T: Default> Default for Secret<T> {
    fn default() -> Self {
        Secret { value: T::default(), required: true }
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl<T: Serialize> Serialize for Secret<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match (READING.get(), self.required) {
            (true, true) => mark(serializer, SECRET_REQUIRED, &self.value),
            (true, false) => mark(serializer, SECRET, &self.value),
            (false, _) => serializer.serialize_str("***"),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Secret::new)
    }
}

/// A setting with no default: a layer must give it, or the config does not
/// open. It reads as its value.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Required<T>(T);

impl<T> Required<T> {
    pub fn new(value: T) -> Self {
        Required(value)
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

/// A setting that may change while the program runs: its change is kept in
/// the store, goes over every layer until it is reset, and reaches every
/// process. It reads as its value.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Live<T>(T);

impl<T> Live<T> {
    pub fn new(value: T) -> Self {
        Live(value)
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

/// What `Required` and `Live` share: they read as their value and serialize
/// as it, but for the core.
macro_rules! transparent {
    ($mark:ident, $key:expr) => {
        impl<T> Deref for $mark<T> {
            type Target = T;

            fn deref(&self) -> &T {
                &self.0
            }
        }

        impl<T> DerefMut for $mark<T> {
            fn deref_mut(&mut self) -> &mut T {
                &mut self.0
            }
        }

        impl<T: fmt::Debug> fmt::Debug for $mark<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<T: fmt::Display> fmt::Display for $mark<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<T: Serialize> Serialize for $mark<T> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                if READING.get() { mark(serializer, $key, &self.0) } else { self.0.serialize(serializer) }
            }
        }

        impl<'de, T: Deserialize<'de>> Deserialize<'de> for $mark<T> {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                T::deserialize(deserializer).map($mark)
            }
        }
    };
}

transparent!(Required, REQUIRED);
transparent!(Live, LIVE);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[derive(Serialize, Deserialize, Default)]
    struct Db {
        url: Secret<String>,
        token: Secret<String>,
        region: Required<String>,
        pool: Live<u32>,
    }

    // contract: CFG-06, SEC-05
    #[test]
    fn a_mark_is_its_value_to_the_program_and_a_map_to_the_core() {
        let db = Db { token: Secret::new("t".into()), pool: Live::new(10), ..Db::default() };
        let shown = json!({ "url": "***", "token": "***", "region": "", "pool": 10 });
        assert_eq!(serde_json::to_value(&db).unwrap(), shown);
        assert_eq!(format!("{:?} {}", db.token, db.token), "*** ***");
        let read = json!({
            "url": { SECRET_REQUIRED: "" },
            "token": { SECRET: "t" },
            "region": { REQUIRED: "" },
            "pool": { LIVE: 10 },
        });
        assert_eq!(marked(&db).unwrap(), read);
        assert_eq!(serde_json::to_value(&db).unwrap(), shown, "a read leaves the thread as it was");
    }

    #[test]
    fn a_marked_setting_reads_back_as_its_value() {
        let db: Db = serde_json::from_value(json!({ "url": "u", "token": "t", "region": "eu", "pool": 3 })).unwrap();
        assert_eq!((db.url.reveal().as_str(), db.region.as_str(), *db.pool), ("u", "eu", 3));
    }
}
