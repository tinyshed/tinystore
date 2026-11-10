//! A backup's manifest: the format it is written in, when it was made, and
//! each file's path, engine, size and SHA-256, which a restore checks every
//! byte against.
//!
//! ```text
//! {"format":1,"created":1791547200000,"keyLeftOut":false,
//!  "files":[{"name":"kv.db","engine":"kv","size":8192,"sha256":"9f86d0…"}]}
//! ```

use std::cell::Cell;
use std::fmt;
use std::io::{self, Read};

use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::error::Category;
use serde_json::{Map, Value, json};

use super::Backup;

/// The manifest's own name in the zip, which no file of a store's takes.
pub(super) const MANIFEST: &str = "manifest.json";
/// The format a backup is written in; a restore refuses one it does not
/// know, from a later version.
const FORMAT: u64 = 1;
/// The bytes a file of the manifest takes at most, as any other field of it:
/// a zip's longest name with each of its bytes escaped, 6 × 65,535, beside
/// an engine, a size and a SHA-256. A backup lists a file in about 120 bytes
/// beside its name.
const MOST_FIELD_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Manifest {
    /// Unix milliseconds by the store's clock.
    pub(super) created: i64,
    pub(super) files: Vec<Listed>,
    pub(super) key_left_out: bool,
}

/// A file of the backup as the manifest lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Listed {
    pub(super) name: String,
    /// `kv`, `sql`, `blobs`…, `program` for a file of the program's and
    /// `key` for the store's encryption key.
    pub(super) engine: &'static str,
    pub(super) size: u64,
    pub(super) sha256: String,
}

impl Manifest {
    pub(super) fn to_json(&self) -> String {
        let files: Vec<Value> = self
            .files
            .iter()
            .map(|file| json!({ "name": file.name, "engine": file.engine, "size": file.size, "sha256": file.sha256 }))
            .collect();
        let manifest =
            json!({ "format": FORMAT, "created": self.created, "keyLeftOut": self.key_left_out, "files": files });
        // without the spaces a person reads it with, as every release's restore
        // reads it alike: an older one, which reads 64 MiB at most, reads more files
        format!("{manifest}\n")
    }

    /// The manifest a backup's zip holds, or why it is none, read as its
    /// bytes come rather than held whole: it lists no more than `most_files`,
    /// and no field of it passes [`MOST_FIELD_BYTES`].
    pub(super) fn read(text: impl Read, most_files: usize) -> Result<Manifest, String> {
        let since = Cell::new(0);
        let bytes = io::BufReader::new(Counted { bytes: text, since: &since });
        let mut json = serde_json::Deserializer::from_reader(bytes);
        let read =
            Fields { since: &since, most_files }.deserialize(&mut json).and_then(|read| json.end().map(|()| read));
        let Unchecked { fields: map, files } = read.map_err(unread)?;
        match map.get("format").and_then(Value::as_u64) {
            Some(FORMAT) => {}
            Some(format) => return Err(format!("format {format}, where this version reads {FORMAT}")),
            None => return Err("its manifest names no format".to_owned()),
        }
        let created = map.get("created").and_then(Value::as_i64).ok_or("its manifest has no time")?;
        let key_left_out = map.get("keyLeftOut").and_then(Value::as_bool).unwrap_or(false);
        let files = files.ok_or("its manifest lists no files")??;
        Ok(Manifest { created, files, key_left_out })
    }

    pub(super) fn summary(&self) -> Backup {
        Backup {
            files: self.files.len(),
            bytes: self.files.iter().map(|file| file.size).sum(),
            key_left_out: self.key_left_out,
        }
    }
}

/// The manifest's object as it reads, before its fields are checked: the
/// fields a restore reads beside its files, and its files, or why one of
/// them is none.
struct Unchecked {
    fields: Map<String, Value>,
    files: Option<Listing>,
}

type Listing = Result<Vec<Listed>, String>;

/// Reads the manifest's object, each field counted from where it starts.
struct Fields<'a> {
    since: &'a Cell<u64>,
    most_files: usize,
}

impl<'de> DeserializeSeed<'de> for Fields<'_> {
    type Value = Unchecked;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Unchecked, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Fields<'_> {
    type Value = Unchecked;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a backup's manifest")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unchecked, A::Error> {
        let mut read = Unchecked { fields: Map::new(), files: None };
        loop {
            self.since.set(0);
            let Some(key) = map.next_key::<String>()? else { return Ok(read) };
            if key == "files" {
                read.files = Some(map.next_value_seed(Files { since: self.since, most: self.most_files })?);
            } else if ["format", "created", "keyLeftOut"].contains(&key.as_str()) {
                read.fields.insert(key, map.next_value::<Value>()?);
            } else {
                // a field of a later version's, which this one reads past
                map.next_value::<IgnoredAny>()?;
            }
        }
    }
}

/// Reads the manifest's files one at a time, each counted from where it
/// starts. The first that is not one is kept and those after it read past:
/// the format, checked once the whole object is read, is said before it,
/// since a later version's files need not be this one's.
struct Files<'a> {
    since: &'a Cell<u64>,
    most: usize,
}

impl<'de> DeserializeSeed<'de> for Files<'_> {
    type Value = Listing;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Listing, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Files<'_> {
    type Value = Listing;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the list of a backup's files")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Listing, A::Error> {
        let (mut files, mut wrong, mut count) = (Vec::new(), None, 0);
        loop {
            self.since.set(0);
            let Some(value) = seq.next_element::<Value>()? else {
                return Ok(match wrong {
                    Some(why) => Err(why),
                    None => Ok(files),
                });
            };
            count += 1;
            if count > self.most {
                return Err(de::Error::custom(format!("it lists more files than the {} its zip holds", self.most)));
            }
            if wrong.is_some() {
                continue;
            }
            match listed(&value) {
                Ok(file) => files.push(file),
                Err(why) => wrong = Some(why),
            }
        }
    }
}

/// The manifest's bytes, counted from where the field being read started:
/// past [`MOST_FIELD_BYTES`] the read fails, so that no field is held whole
/// however long it is. They are read a buffer at a time, and counted so.
struct Counted<'a, R> {
    bytes: R,
    since: &'a Cell<u64>,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.bytes.read(buffer)?;
        let since = self.since.get() + read as u64;
        if since > MOST_FIELD_BYTES {
            return Err(io::Error::other(format!("a field of its manifest past {MOST_FIELD_BYTES} bytes")));
        }
        self.since.set(since);
        Ok(read)
    }
}

/// Why the manifest's bytes are no manifest: a read that failed, the zip's
/// check of them among its causes, as the read said it; text that is no
/// JSON; or JSON that is no manifest.
fn unread(error: serde_json::Error) -> String {
    match error.classify() {
        Category::Io => error.to_string(),
        Category::Data => format!("its manifest is not one: {error}"),
        _ => format!("its manifest is no JSON: {error}"),
    }
}

fn listed(value: &Value) -> Result<Listed, String> {
    let map: &Map<String, Value> = value.as_object().ok_or("its manifest lists a file that is no object")?;
    let text =
        |field: &str| map.get(field).and_then(Value::as_str).ok_or(format!("a file of its manifest has no {field}"));
    let name = text("name")?.to_owned();
    let size = map.get("size").and_then(Value::as_u64).ok_or(format!("{name}: no size in its manifest"))?;
    let sha256 = text("sha256")?.to_owned();
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{name}: a SHA-256 in its manifest that is not one"));
    }
    let engine = engine(text("engine")?);
    Ok(Listed { name, engine, size, sha256: sha256.to_ascii_lowercase() })
}

/// An engine's name as the backup writes it; one this version does not
/// know is kept as `other`, its file restored all the same.
fn engine(name: &str) -> &'static str {
    const KNOWN: [&str; 10] = ["kv", "jobs", "sql", "blobs", "records", "metrics", "search", "program", "key", "other"];
    KNOWN.into_iter().find(|known| *known == name).unwrap_or("other")
}
