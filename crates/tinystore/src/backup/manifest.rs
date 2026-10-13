//! A backup's manifest: the format it is written in, when it was made, and
//! each file's path, engine, size and SHA-256, which a restore checks every
//! byte against.
//!
//! ```text
//! {"format":1,"created":1791547200000,"keyLeftOut":false,
//!  "files":[{"name":"kv.db","engine":"kv","size":8192,"sha256":"9f86d0…"}]}
//! ```

use serde_json::{Map, Value, json};

use super::Backup;

/// The manifest's own name in the zip, which no file of a store's takes.
pub(super) const MANIFEST: &str = "manifest.json";
/// The format a backup is written in; a restore refuses one it does not
/// know, from a later version.
const FORMAT: u64 = 1;
/// The longest manifest a restore reads: a few hundred bytes a file.
pub(super) const MOST_BYTES: u64 = 64 << 20;

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
        format!("{manifest:#}\n")
    }

    /// The manifest a backup's zip holds, or why it is none.
    pub(super) fn read(text: &[u8]) -> Result<Manifest, String> {
        let value: Value = serde_json::from_slice(text).map_err(|error| format!("its manifest is no JSON: {error}"))?;
        let map = value.as_object().ok_or("its manifest is no object")?;
        match map.get("format").and_then(Value::as_u64) {
            Some(FORMAT) => {}
            Some(format) => return Err(format!("format {format}, where this version reads {FORMAT}")),
            None => return Err("its manifest names no format".to_owned()),
        }
        let created = map.get("created").and_then(Value::as_i64).ok_or("its manifest has no time")?;
        let key_left_out = map.get("keyLeftOut").and_then(Value::as_bool).unwrap_or(false);
        let files = map.get("files").and_then(Value::as_array).ok_or("its manifest lists no files")?;
        let files = files.iter().map(listed).collect::<Result<Vec<_>, String>>()?;
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
    const KNOWN: [&str; 9] = ["kv", "jobs", "sql", "blobs", "records", "metrics", "program", "key", "other"];
    KNOWN.into_iter().find(|known| *known == name).unwrap_or("other")
}
