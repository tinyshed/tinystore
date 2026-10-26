//! `tinystore files`: a store's sets of files from a shell. `put` and
//! `delete` write through the server that holds the directory, or a sidecar
//! they start; `get` and `list` read beside whoever holds it. `get` writes
//! to a path and never to stdout, since PowerShell's `>` writes a program's
//! bytes back as text.
//!
//! ```text
//! tinystore files ./data put avatars users/42/avatar.png ./avatar.png
//! tinystore files ./data get avatars users/42/avatar.png ./avatar.png
//! tinystore files ./data list avatars users/42
//! tinystore files ./data delete avatars users/42/avatar.png
//! ```

use std::io::{self, IsTerminal as _, Read as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use serde_json::json;
use tinystore::blobs::{FileInfo, Files};
use tinystore::{ErrorKind, Store};

use crate::asked::{self, Asked, Done, Refused, Verb, failed, wrong};
use crate::on_disk::bytes;
use crate::{help, reach, window};

const VERBS: [Verb; 4] = [
    Verb { word: "put", command: &help::FILES_PUT, run: put },
    Verb { word: "get", command: &help::FILES_GET, run: get },
    Verb { word: "list", command: &help::FILES_LIST, run: list },
    Verb { word: "delete", command: &help::FILES_DELETE, run: delete },
];

/// The files a page of `list` holds unless `--limit` says.
const PAGE: usize = 100;
/// A file's type by its extension, for one `put` gives none; any other is
/// written as bytes.
const TYPES: [(&str, &str); 16] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("pdf", "application/pdf"),
    ("json", "application/json"),
    ("zip", "application/zip"),
    ("txt", "text/plain; charset=utf-8"),
    ("md", "text/markdown; charset=utf-8"),
    ("csv", "text/csv; charset=utf-8"),
    ("html", "text/html; charset=utf-8"),
    ("css", "text/css; charset=utf-8"),
    ("js", "text/javascript; charset=utf-8"),
    ("mp4", "video/mp4"),
];

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run_verb("files", &VERBS, args)
}

/// Writes a local file, or stdin's bytes for `-`, at a path of the set,
/// through the server that holds the directory.
fn put(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("files", "put", false)?;
    let [set, path, source] = &rest[..] else {
        return Err(wrong("put takes a set, a path and a local file: put avatars users/42/avatar.png ./avatar.png"));
    };
    reach::store_at(&dir)?;
    let content_type = asked.value("--type").map(str::to_owned).or_else(|| type_of(path).map(str::to_owned));
    let source = Source::of(source)?;
    let (named, at) = (set.clone(), path.clone());
    let info = reach::write(&dir, "files put", move |client| {
        let files = client.files(&named)?;
        let (mut bytes, size) = source.open()?;
        client.put(files, &at, (&mut *bytes, size), content_type.as_deref())
    })?;
    outln!("put {path} in files {set}, {} {}", bytes(info.size), info.content_type)
}

/// Where `put` reads a file's bytes: a local file, opened again for each
/// attempt, or stdin, read once before the first.
#[derive(Clone)]
enum Source {
    File(PathBuf),
    Stdin(Arc<Vec<u8>>),
}

impl Source {
    fn of(given: &str) -> Result<Source, Refused> {
        if given != "-" {
            let path = PathBuf::from(given);
            if !path.is_file() {
                return Err(failed(format!("{given}: no file to put")));
            }
            return Ok(Source::File(path));
        }
        if io::stdin().is_terminal() {
            return Err(wrong("- reads the file's bytes on stdin, and stdin is a terminal"));
        }
        let mut read = Vec::new();
        io::stdin().read_to_end(&mut read).map_err(|error| failed(format!("stdin: {error}")))?;
        Ok(Source::Stdin(Arc::new(read)))
    }

    fn open(&self) -> tinystore::Result<(Box<dyn io::Read + '_>, u64)> {
        match self {
            Source::File(path) => {
                let failed = |error| tinystore::Error::io(path.display().to_string(), error);
                let file = std::fs::File::open(path).map_err(failed)?;
                let size = file.metadata().map_err(failed)?.len();
                Ok((Box::new(file), size))
            }
            Source::Stdin(bytes) => Ok((Box::new(bytes.as_slice()), bytes.len() as u64)),
        }
    }
}

/// Writes the file at a path of the set to a local path, read beside whoever
/// holds the directory: beside the local path first and renamed once whole,
/// and never over a file there unless `--force`.
fn get(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("files", "get", true)?;
    let [set, path, target] = &rest[..] else {
        return Err(wrong("get takes a set, a path and where to write it: get avatars users/42/avatar.png ./a.png"));
    };
    let target = PathBuf::from(target);
    if target.exists() && !asked.has("--force") {
        return Err(failed(format!("{}: a file is there; --force writes over it", target.display())));
    }
    let store = Store::open_beside(&dir).map_err(failed)?;
    let Some(mut stored) = opened(&store, set)?.get(path)? else {
        return Err(failed(format!("files {set} holds no {path}")));
    };
    let size = stored.info().size;
    let beside = beside_of(&target);
    let written = std::fs::File::create(&beside).and_then(|mut file| {
        io::copy(&mut stored, &mut file)?;
        file.sync_all()
    });
    if let Err(error) = written.and_then(|()| std::fs::rename(&beside, &target)) {
        let _ = std::fs::remove_file(&beside);
        return Err(failed(format!("{}: {error}", target.display())));
    }
    outln!("wrote {}, {}", target.display(), bytes(size))
}

/// Where `get` writes before it renames: beside the target, so that the
/// rename stays on its disk.
fn beside_of(target: &Path) -> PathBuf {
    let name = target.file_name().map_or_else(|| "file".into(), |name| name.to_string_lossy().into_owned());
    target.with_file_name(format!(".{name}.tinystore-{}", std::process::id()))
}

fn list(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("files", "list", true)?;
    let (Some(set), prefix, None) = (rest.first(), rest.get(1), rest.get(2)) else {
        return Err(wrong("list takes a set and a path's prefix at most: list avatars users/42"));
    };
    asked::existing(&dir)?;
    let store = Store::open_beside(&dir).map_err(failed)?;
    let files = opened(&store, set)?;
    let mut listing = files.list().prefix(prefix.cloned().unwrap_or_default()).limit(asked.count("--limit", PAGE)?);
    if let Some(after) = asked.value("--after") {
        listing = listing.after(after);
    }
    let page = listing.page()?;
    if asked.has("--json") {
        for info in &page.files {
            outln!("{}", shown(info))?;
        }
        return page.next.map_or(Ok(()), |next| outln!("{}", json!({ "next": next })));
    }
    if page.files.is_empty() {
        return outln!(
            "files {set} holds no file{}",
            prefix.map_or_else(String::new, |prefix| format!(" under {prefix}"))
        );
    }
    let width = page.files.iter().map(|info| info.path.chars().count()).max().unwrap_or(0).min(60);
    for info in &page.files {
        let modified = window::clock(info.last_modified, true);
        outln!("{:width$}  {:>9}  {modified}  {}", info.path, bytes(info.size), info.content_type)?;
    }
    if let Some(next) = page.next {
        asked::say("files list", format!("more files; --after {next} reads on"));
    }
    Ok(())
}

/// Deletes the file at a path of the set through the server that holds the
/// directory.
fn delete(asked: Asked) -> Done {
    let (dir, rest) = asked.placed("files", "delete", false)?;
    let [set, path] = &rest[..] else {
        return Err(wrong("delete takes a set and a path: delete avatars users/42/avatar.png"));
    };
    reach::store_at(&dir)?;
    let (named, at) = (set.clone(), path.clone());
    let found = reach::write(&dir, "files delete", move |client| {
        let files = client.files(&named)?;
        client.delete_file(files, &at)
    })?;
    if found { outln!("deleted {path} from files {set}") } else { outln!("files {set} holds no {path}") }
}

/// The set `name` beside whoever holds the store; one never written to is
/// said as none, since a read beside makes nothing.
fn opened(store: &Store, name: &str) -> Result<Files, Refused> {
    let missing = || failed(format!("{} holds no files {name}", store.dir().display()));
    if !store.dir().join("blobs").exists() {
        return Err(missing());
    }
    match store.files(name).open() {
        Ok(files) => Ok(files),
        // a set is made by its first open, which a store opened beside cannot write
        Err(error) if error.kind() == ErrorKind::NotFound || error.to_string().contains("readonly") => Err(missing()),
        Err(error) => Err(failed(error)),
    }
}

/// A file as JSON: its times whole and UTC.
fn shown(info: &FileInfo) -> serde_json::Value {
    json!({
        "path": info.path,
        "size": info.size,
        "etag": info.etag,
        "type": info.content_type,
        "modified": window::utc(info.last_modified),
        "expires": info.expires.map(window::utc),
    })
}

/// The type a path's extension names, among the common ones.
fn type_of(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    TYPES.iter().find(|(known, _)| *known == extension).map(|(_, named)| *named)
}
