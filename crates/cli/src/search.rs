//! `tinystore search list` and `tinystore search drop`: the indexes a
//! store's `search.db` holds, and one of them dropped with every document
//! it holds. An index no program opens any more is kept until it is dropped
//! here or by `store.drop_index`, since another version may open it again.
//!
//! ```text
//! tinystore search list ./data
//! tinystore search drop ./data posts
//! ```

use std::path::Path;
use std::process::ExitCode;

use tinystore::search::{Listed, Status};
use tinystore::{ErrorKind, Options, Store};

pub(crate) const USAGE: &str = "usage: tinystore search list <dir>
       tinystore search drop <dir> <name>";

pub(crate) fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("list") => match &args[1..] {
            [dir] => list(Path::new(dir)),
            _ => usage(),
        },
        Some("drop") => match &args[1..] {
            [dir, name] => drop(Path::new(dir), name),
            _ => usage(),
        },
        Some(other) => {
            eprintln!("tinystore search: no command {other}{}\n{USAGE}", crate::meant(other, &["list", "drop"]));
            ExitCode::from(2)
        }
        None => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// The store in `dir`, which no server or program may hold meanwhile.
fn opened(command: &str, dir: &Path) -> Result<Store, ExitCode> {
    match Store::open(dir, Options { background: false, ..Options::default() }) {
        Ok(store) => Ok(store),
        Err(error) if error.kind() == ErrorKind::InUse => {
            eprintln!("tinystore search {command}: {error}\na program or a server holds {}", dir.display());
            Err(ExitCode::FAILURE)
        }
        Err(error) => Err(failed(command, &error)),
    }
}

fn list(dir: &Path) -> ExitCode {
    let store = match opened("list", dir) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let listed = tinystore::search::list(&store).and_then(|listed| store.close().map(|()| listed));
    match listed {
        Ok(listed) if listed.is_empty() => {
            println!("● {}  holds no index", dir.display());
            ExitCode::SUCCESS
        }
        Ok(listed) => {
            println!("● {}  {} indexes", dir.display(), listed.len());
            for index in &listed {
                println!("  {}", line(index));
            }
            ExitCode::SUCCESS
        }
        Err(error) => failed("list", &error),
    }
}

/// An index as a line of the list: its name, documents, vectors, when it was
/// last opened, and a rebuild under way.
fn line(index: &Listed) -> String {
    let vectors = if index.vectors > 0 { format!(", {} vectors", index.vectors) } else { String::new() };
    let opened = index
        .opened
        .and_then(|at| jiff::Timestamp::try_from(at).ok())
        .map_or_else(|| "never opened".to_owned(), |at| format!("opened {}", at.strftime("%Y-%m-%d %H:%M:%S UTC")));
    let status = match &index.status {
        Status::Ready => String::new(),
        Status::Rebuilding { done } => format!(" · rebuilding, {:.0} % read", done * 100.0),
        Status::Failed { error } => format!(" · its rebuild failed: {error}"),
    };
    format!("{}  {} documents{vectors} · {opened}{status}", index.name, index.documents)
}

fn drop(dir: &Path, name: &str) -> ExitCode {
    let store = match opened("drop", dir) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let dropped = store.drop_index(name).and_then(|dropped| {
        tinystore::search::maintain(&store)?;
        store.close().map(|()| dropped)
    });
    match dropped {
        Ok(true) => {
            println!("● {}  dropped {name}", dir.display());
            ExitCode::SUCCESS
        }
        Ok(false) => {
            eprintln!("tinystore search drop: {} holds no index {name}", dir.display());
            ExitCode::FAILURE
        }
        Err(error) => failed("drop", &error),
    }
}

fn failed(command: &str, error: &tinystore::Error) -> ExitCode {
    eprintln!("tinystore search {command}: {error}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_says_an_index_its_documents_and_a_rebuild_under_way() {
        let listed = Listed {
            name: "posts".to_owned(),
            documents: 12,
            vectors: 0,
            opened: None,
            status: Status::Rebuilding { done: 0.42 },
        };
        assert_eq!(line(&listed), "posts  12 documents · never opened · rebuilding, 42 % read");
    }

    #[test]
    fn list_and_drop_read_and_drop_a_stores_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let posts = store.index::<String>("posts").open().unwrap();
        posts.set("a", &"hello".to_owned()).unwrap();
        store.close().unwrap();
        assert_eq!(list(dir.path()), ExitCode::SUCCESS);
        assert_eq!(drop(dir.path(), "posts"), ExitCode::SUCCESS);
        assert_eq!(drop(dir.path(), "posts"), ExitCode::FAILURE);
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        assert!(tinystore::search::list(&store).unwrap().is_empty());
    }
}
