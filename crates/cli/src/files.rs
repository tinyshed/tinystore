//! The files a store's directory holds and what they weigh, as `serve`'s
//! banner and `status` show them: each engine's file with its WAL, the
//! application's databases, and blobs with their bytes.

use std::path::Path;

/// One engine's file of a store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoreFile {
    /// As a person names it: `kv`, `sql/app`, `blobs`.
    pub(crate) name: String,
    /// As replication names it: `kv.db`, `sql/app.db`, `blobs/blobs.db`.
    pub(crate) file: String,
    pub(crate) bytes: u64,
}

/// The engines' files in the order a person reads them: what the program
/// keeps first, then its logs and metrics.
pub(crate) fn of(store: &Path) -> Vec<StoreFile> {
    let mut files = Vec::new();
    let mut add = |name: &str, file: &str, bytes: Option<u64>| {
        if let Some(bytes) = bytes {
            files.push(StoreFile { name: name.to_owned(), file: file.to_owned(), bytes });
        }
    };
    for (name, file) in [("kv", "kv.db"), ("jobs", "jobs.db"), ("search", "search.db")] {
        add(name, file, size_of(&store.join(file)));
    }
    add("blobs", "blobs/blobs.db", tree_size(&store.join("blobs")));
    for database in databases(store) {
        add(
            &format!("sql/{database}"),
            &format!("sql/{database}.db"),
            size_of(&store.join(format!("sql/{database}.db"))),
        );
    }
    for (name, file) in [("records", "records.db"), ("metrics", "metrics.db"), ("catalog", "catalog.db")] {
        add(name, file, size_of(&store.join(file)));
    }
    files
}

/// The application's databases by name, `sql/<name>.db`, in order.
pub(crate) fn databases(store: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(store.join("sql")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.strip_suffix(".db").map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// A SQLite file's bytes with its WAL's; none when there is no file.
fn size_of(file: &Path) -> Option<u64> {
    let size = std::fs::metadata(file).ok()?.len();
    let mut wal = file.as_os_str().to_owned();
    wal.push("-wal");
    Some(size + std::fs::metadata(&wal).map_or(0, |wal| wal.len()))
}

/// What a directory's files weigh together; none when there is no directory.
fn tree_size(dir: &Path) -> Option<u64> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut total = 0;
    for entry in entries.filter_map(Result::ok) {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            total += tree_size(&entry.path()).unwrap_or(0);
        } else if let Ok(metadata) = entry.metadata() {
            total += metadata.len();
        }
    }
    Some(total)
}

/// A size as a person reads it: `512 B`, `36 KiB`, `1.2 MiB`.
pub(crate) fn bytes(size: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if size < 1024 {
        return format!("{size} B");
    }
    let mut value = size as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 { format!("{value:.1} {}", UNITS[unit]) } else { format!("{value:.0} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_reads_as_a_person_reads_one() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(36 * 1024), "36 KiB");
        assert_eq!(bytes(1_258_291), "1.2 MiB");
    }

    #[test]
    fn a_stores_files_are_its_engines_with_their_wals() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("kv.db"), [0; 100]).unwrap();
        std::fs::write(dir.path().join("kv.db-wal"), [0; 20]).unwrap();
        std::fs::create_dir_all(dir.path().join("sql")).unwrap();
        std::fs::write(dir.path().join("sql/app.db"), [0; 10]).unwrap();
        std::fs::write(dir.path().join("sql/app.db-wal"), [0; 1]).unwrap();
        std::fs::create_dir_all(dir.path().join("blobs/objects")).unwrap();
        std::fs::write(dir.path().join("blobs/objects/ab"), [0; 5]).unwrap();
        let found: Vec<(String, u64)> = of(dir.path()).into_iter().map(|file| (file.name, file.bytes)).collect();
        assert_eq!(found, [("kv".to_owned(), 120), ("blobs".to_owned(), 5), ("sql/app".to_owned(), 11)]);
        assert_eq!(databases(dir.path()), ["app"]);
    }
}
