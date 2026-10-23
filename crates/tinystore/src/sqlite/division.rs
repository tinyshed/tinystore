//! What a file is made of, page by page, as `dbstat` divides it by b-tree:
//! what the measurements of an engine's file on disk print.

use std::path::Path;

use rusqlite::Connection;

use super::{File, sql_error, wal_of};

/// A file's pages: each b-tree's, the free ones, and the rest, the pointer
/// map of a file that gives pages back.
#[derive(Debug)]
pub(crate) struct Division {
    pub(crate) page_size: u64,
    pub(crate) pages: u64,
    pub(crate) free: u64,
    pub(crate) pointer_map: u64,
    /// Each b-tree, largest first.
    pub(crate) trees: Vec<Tree>,
    pub(crate) file_bytes: u64,
    pub(crate) wal_bytes: u64,
}

#[derive(Debug)]
pub(crate) struct Tree {
    pub(crate) name: String,
    pub(crate) pages: u64,
    /// The bytes its pages hold: cells and their headers, what is not unused.
    pub(crate) used: u64,
    /// The bytes of its rows alone.
    pub(crate) payload: u64,
}

const TREES: &str = "select name, count(*), sum(pgsize - unused), sum(payload) from dbstat
    group by name order by count(*) desc, name";

/// Makes the file at `path` as a store of v0.1.0 made its engines' files:
/// without `auto_vacuum`, which a file with a table keeps.
pub(crate) fn made_as_before(path: &Path, page_size: u32) {
    let file = File::open(path, super::Config { page_size, ..super::Config::default() }).unwrap();
    let made = |tx: &super::Tx<'_>| tx.execute_batch("create table made_before (x); drop table made_before");
    file.transaction(|tx| made(tx).map_err(|error| sql_error("make it", error))).unwrap();
    file.close().unwrap();
}

/// What the passes of a measurement took: the median and the slowest of the
/// first `live`, those of the load, and those after it in all.
pub(crate) fn passes(passes: &[std::time::Duration], live: usize) -> String {
    let (during, after) = passes.split_at(live.min(passes.len()));
    let mut sorted = during.to_vec();
    sorted.sort();
    let (median, slowest) = (sorted.get(sorted.len() / 2), sorted.last());
    let after: std::time::Duration = after.iter().sum();
    format!(
        "{} passes under load, {median:?} the median, {slowest:?} the slowest; {after:?} the passes after",
        during.len()
    )
}

/// The bytes this process has written through the system so far, where the
/// system says: `wchar` of Linux's `/proc/self/io`.
pub(crate) fn written() -> Option<u64> {
    let io = std::fs::read_to_string("/proc/self/io").ok()?;
    io.lines().find_map(|line| line.strip_prefix("wchar: ")).and_then(|bytes| bytes.trim().parse().ok())
}

impl Division {
    /// The division of `file` as its readers see it now.
    pub(crate) fn of(file: &File) -> Division {
        Division::at(file.path())
    }

    /// The division of the file at `path`, read through a connection of its
    /// own, as a reader of the store sees it.
    pub(crate) fn at(path: &Path) -> Division {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        let connection = Connection::open_with_flags(path, flags).unwrap();
        let read = |connection: &Connection| -> rusqlite::Result<((i64, i64, i64), Vec<Tree>)> {
            let one = |sql: &str| connection.query_row(sql, [], |row| row.get::<_, i64>(0));
            let counts = (one("pragma page_size")?, one("pragma page_count")?, one("pragma freelist_count")?);
            let mut select = connection.prepare(TREES)?;
            let rows = select.query_map([], |row| {
                Ok(Tree {
                    name: row.get(0)?,
                    pages: row.get::<_, i64>(1)?.unsigned_abs(),
                    used: row.get::<_, i64>(2)?.unsigned_abs(),
                    payload: row.get::<_, i64>(3)?.unsigned_abs(),
                })
            })?;
            Ok((counts, rows.collect::<rusqlite::Result<Vec<Tree>>>()?))
        };
        let ((page_size, pages, free), trees) = read(&connection).unwrap();
        let (page_size, pages, free) = (page_size.unsigned_abs(), pages.unsigned_abs(), free.unsigned_abs());
        let in_trees: u64 = trees.iter().map(|tree| tree.pages).sum();
        let size = |path: &Path| std::fs::metadata(path).map_or(0, |found| found.len());
        Division {
            page_size,
            pages,
            free,
            pointer_map: pages.saturating_sub(free + in_trees),
            trees,
            file_bytes: size(path),
            wal_bytes: size(&wal_of(path)),
        }
    }

    pub(crate) fn bytes(&self, pages: u64) -> u64 {
        pages * self.page_size
    }

    /// Prints the division, `label` above it, the b-trees past `shown` summed
    /// as one line.
    pub(crate) fn print(&self, label: &str, shown: usize) {
        let kib = |bytes: u64| bytes as f64 / 1024.0;
        println!(
            "{label}: {:.0} KiB in its pages, {:.0} KiB of file and {:.0} KiB of WAL on disk",
            kib(self.bytes(self.pages)),
            kib(self.file_bytes),
            kib(self.wal_bytes)
        );
        println!("  {:<24} {:>10} {:>10} {:>10}", "b-tree", "pages KiB", "used KiB", "rows KiB");
        for tree in self.trees.iter().take(shown) {
            let (pages, used, rows) = (kib(self.bytes(tree.pages)), kib(tree.used), kib(tree.payload));
            println!("  {:<24} {pages:>10.1} {used:>10.1} {rows:>10.1}", tree.name);
        }
        let rest = &self.trees[self.trees.len().min(shown)..];
        if !rest.is_empty() {
            let pages = kib(self.bytes(rest.iter().map(|tree| tree.pages).sum()));
            let used = kib(rest.iter().map(|tree| tree.used).sum());
            let rows = kib(rest.iter().map(|tree| tree.payload).sum());
            println!("  {:<24} {pages:>10.1} {used:>10.1} {rows:>10.1}", format!("{} others", rest.len()));
        }
        println!("  {:<24} {:>10.1}", "free pages", kib(self.bytes(self.free)));
        println!("  {:<24} {:>10.1}", "pointer map", kib(self.bytes(self.pointer_map)));
    }
}
