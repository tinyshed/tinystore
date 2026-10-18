//! `tinystore search list` and `tinystore search drop`: the indexes a
//! store's `search.db` holds, and one of them dropped with every document
//! it holds. An index no program opens any more is kept until it is dropped
//! here or by `store.drop_index`, since another version may open it again.
//!
//! ```text
//! tinystore search list ./data         beside a program or server that holds it
//! tinystore search drop ./data posts   with no program holding it
//! ```

use std::path::Path;
use std::process::ExitCode;

use tinystore::search::{Listed, Status};
use tinystore::{ErrorKind, Options, Store};

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::SEARCH, args, search)
}

fn search(asked: Asked) -> Done {
    let at = asked.words.iter().position(|word| word == "list" || word == "drop");
    let (verb, around) = match at {
        Some(at @ (0 | 1)) => {
            let mut around = asked.words.clone();
            let verb = around.remove(at);
            (verb, around)
        }
        _ => return Err(wrong("list [dir] or drop <dir> <name>")),
    };
    match (verb.as_str(), &around[..], &asked.dir) {
        ("list", [], dir) => list(dir.as_deref().unwrap_or(Path::new("."))),
        ("list", [dir], None) => list(Path::new(dir)),
        ("drop", [name], Some(dir)) => drop_index(dir, name),
        ("drop", [dir, name], None) => drop_index(Path::new(dir), name),
        ("drop", ..) => Err(wrong("drop takes a directory and an index: search drop ./data posts")),
        _ => Err(wrong("list takes a directory at most: search list ./data")),
    }
}

fn list(dir: &Path) -> Done {
    let store = Store::open_beside(dir).map_err(failed)?;
    let listed = match tinystore::search::list(&store) {
        Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
        listed => listed?,
    };
    if listed.is_empty() {
        println!("● {}  holds no index", dir.display());
        return Ok(());
    }
    let indexes = if listed.len() == 1 { "1 index".to_owned() } else { format!("{} indexes", listed.len()) };
    println!("● {}  {indexes}", dir.display());
    for index in &listed {
        println!("  {}", line(index));
    }
    Ok(())
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

/// Drops an index, in a store that no server or program may hold meanwhile.
fn drop_index(dir: &Path, name: &str) -> Done {
    asked::existing(dir)?;
    let store = match Store::open(dir, Options { background: false, ..Options::default() }) {
        Ok(store) => store,
        Err(error) if error.kind() == ErrorKind::InUse => {
            return Err(failed(format!("{error}\na program or a server holds {}: drop it through it", dir.display())));
        }
        Err(error) => return Err(error.into()),
    };
    let dropped = store.drop_index(name).and_then(|dropped| {
        tinystore::search::maintain(&store)?;
        store.close().map(|()| dropped)
    })?;
    if !dropped {
        return Err(failed(format!("{} holds no index {name}", dir.display())));
    }
    println!("● {}  dropped {name}", dir.display());
    Ok(())
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
    fn list_reads_beside_and_drop_drops_a_stores_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        let posts = store.index::<String>("posts").open().unwrap();
        posts.set("a", &"hello".to_owned()).unwrap();
        assert!(list(dir.path()).is_ok(), "beside the program that holds it");
        store.close().unwrap();
        assert!(drop_index(dir.path(), "posts").is_ok());
        assert!(drop_index(dir.path(), "posts").is_err());
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        assert!(tinystore::search::list(&store).unwrap().is_empty());
    }
}
