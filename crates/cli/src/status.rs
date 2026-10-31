//! `tinystore status`: what a store's directory holds, which server serves it,
//! and, for a node, which files it writes and which it copies. It reads the
//! store's files as they stand, and says HELLO on the server's endpoint to
//! learn whether a server still answers there.
//!
//! ```text
//! ● ./data  served by tinystore 0.1.0 · pid 37420 · up 12m · durability os
//!
//!   kv           36 KiB   home
//!   sql/app     1.2 MiB   copy · at commit 412
//!   records      24 KiB   this node's own
//!   ───────────────────
//!   total       1.3 MiB
//!
//!   node  from us.internal:7444 · 2 nodes copy from it
//! ```

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nu_ansi_term::{Color, Style};
use serde_json::json;
use tinystore::replication::{FileStatus, Links};

use crate::asked::{self, Asked, Done, failed};
use crate::client;
use crate::help;
use crate::lines::{Terminal, paint};
use crate::names::{Holding, Holdings, Kept, Names};
use crate::on_disk::{self, StoreFile};
use crate::window;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::STATUS, args, status)
}

fn status(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_or_here(0)?;
    if !rest.is_empty() {
        return Err(asked::wrong(format!("one directory, not {} too", rest.join(" "))));
    }
    if !dir.is_dir() {
        return Err(failed(format!("{}: no such directory", dir.display())));
    }
    let report = Report::of(&dir, asked.has("--names"))?;
    if asked.has("--json") {
        outln!("{}", report.json())?;
    } else {
        out!("{}", report.text(Terminal::of_stdout().colours()))?;
    }
    Ok(())
}

/// What `status --json` prints of a directory, for the MCP's tool too.
pub(crate) fn json_of(dir: &Path) -> Result<serde_json::Value, asked::Refused> {
    Ok(Report::of(dir, false)?.json())
}

/// What `status` found in a directory.
struct Report {
    dir: String,
    served: Served,
    files: Vec<(StoreFile, Option<FileStatus>)>,
    node: bool,
    links: Option<Links>,
    names: Named,
}

/// The store's names, with what each holds when `--names` asked.
enum Named {
    Listed(Names),
    Held(Holdings),
}

enum Served {
    By {
        server: String,
        pid: u64,
        since: SystemTime,
        durability: Option<String>,
    },
    /// `SERVE` names a server that does not answer: it died, or is not this
    /// store's.
    Gone {
        why: String,
    },
    No,
}

impl Report {
    fn of(dir: &Path, holdings: bool) -> Result<Report, asked::Refused> {
        let served = match client::found(dir).map_err(failed)? {
            None => Served::No,
            Some(found) => match found.connect() {
                Ok(connection) => Served::By {
                    server: connection.server,
                    pid: found.pid,
                    since: found.since,
                    durability: connection.durability,
                },
                Err(why) => Served::Gone { why },
            },
        };
        let logged = tinystore::replication::status(dir)?;
        let node = dir.join("log").is_dir();
        let files = on_disk::of(dir)
            .into_iter()
            .map(|file| {
                let status = logged.iter().find(|status| status.file == file.file).cloned();
                (file, status)
            })
            .collect();
        let links = tinystore::replication::links(dir)?;
        let names = if holdings { Named::Held(Holdings::of(dir)) } else { Named::Listed(Names::of(dir)) };
        Ok(Report { dir: dir.display().to_string(), served, files, node, links, names })
    }

    fn text(&self, colours: bool) -> String {
        let paint = |style: Style, text: &str| paint(colours, style, text);
        let (bold, dim) = (Style::new().bold(), Style::new().dimmed());
        let mut text = match &self.served {
            Served::By { server, pid, since, durability } => format!(
                "{} {}  {}\n",
                paint(Color::Green.bold(), "●"),
                paint(bold, &self.dir),
                paint(
                    dim,
                    &format!(
                        "served by tinystore {server} · pid {pid} · up {} · {}",
                        age(*since),
                        durability_of(durability.as_deref())
                    )
                )
            ),
            Served::Gone { why } => format!(
                "{} {}  {}\n  {}\n",
                paint(Color::Yellow.normal(), "○"),
                paint(bold, &self.dir),
                paint(Color::Yellow.normal(), "not served: server/SERVE names a server that does not answer"),
                paint(dim, why)
            ),
            Served::No => format!("{} {}  {}\n", paint(dim, "○"), paint(bold, &self.dir), paint(dim, "not served")),
        };
        text.push('\n');
        if self.files.is_empty() {
            text.push_str(&format!(
                "  {}\n",
                paint(dim, "no files yet: each engine makes its file the first time a program opens it")
            ));
            return text;
        }
        let name_width = self.files.iter().map(|(file, _)| file.name.len()).max().unwrap_or(0).max(5);
        let sizes: Vec<String> = self.files.iter().map(|(file, _)| on_disk::bytes(file.bytes)).collect();
        let total = on_disk::bytes(self.files.iter().map(|(file, _)| file.bytes).sum());
        let size_width = sizes.iter().map(String::len).chain([total.len()]).max().unwrap_or(0);
        for ((file, status), size) in self.files.iter().zip(&sizes) {
            let role = if self.node { role(file, status.as_ref(), self.links.as_ref()) } else { String::new() };
            text.push_str(&format!("  {:name_width$}  {size:>size_width$}", file.name));
            if !role.is_empty() {
                text.push_str(&format!("   {}", paint(dim, &role)));
            }
            text.push('\n');
            text.push_str(&self.names_under(&file.name, colours));
        }
        text.push_str(&format!("  {}\n", paint(dim, &"─".repeat(name_width + 2 + size_width))));
        text.push_str(&format!("  {:name_width$}  {total:>size_width$}\n", "total"));
        if let Some(node) = self.node_line() {
            text.push_str(&format!("\n  {} {}\n", paint(bold, "node"), node));
        }
        text
    }

    /// The names `--names` shows under an engine's file, a line each, their
    /// columns lined up with every other file's names.
    fn names_under(&self, file: &str, colours: bool) -> String {
        let Named::Held(holdings) = &self.names else { return String::new() };
        let columns = Columns::of(holdings, self.files.iter().map(|(file, _)| file.name.as_str()));
        let (names, counts) = holdings.of_file(file);
        names.iter().map(|holding| columns.row(holding, counts, colours)).collect()
    }

    /// What the node's links say while it runs.
    fn node_line(&self) -> Option<String> {
        if !self.node {
            return None;
        }
        let Some(links) = &self.links else {
            return Some("not running: its links show while it runs".to_owned());
        };
        let mut said = Vec::new();
        if let Some(from) = &links.from {
            said.push(format!("from {from}"));
        }
        said.push(match links.followers.len() {
            0 => "no node copies from it".to_owned(),
            1 => "1 node copies from it".to_owned(),
            count => format!("{count} nodes copy from it"),
        });
        if links.unheld > 0 {
            said.push(format!("{} synchronous commits no copy held", links.unheld));
        }
        let age = now_millis().saturating_sub(links.written) / 1000;
        if age > 15 {
            said.push(format!("its links are {age} s old: it may have stopped without removing them"));
        }
        Some(said.join(" · "))
    }

    fn json(&self) -> serde_json::Value {
        let served = match &self.served {
            Served::By { server, pid, since, durability } => json!({
                "server": server,
                "pid": pid,
                "since": since.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64),
                "durability": durability,
            }),
            Served::Gone { why } => json!({ "gone": why }),
            Served::No => serde_json::Value::Null,
        };
        let files: Vec<serde_json::Value> = self
            .files
            .iter()
            .map(|(file, status)| {
                let mut shown = json!({ "name": file.name, "file": file.file, "bytes": file.bytes });
                if let Some(status) = status {
                    shown["home"] = json!(status.home);
                    shown["applied"] = json!(status.applied);
                    shown["epoch"] = json!(status.epoch);
                }
                shown
            })
            .collect();
        let links = self.links.as_ref().map(|links| {
            let followers: Vec<serde_json::Value> = links
                .followers
                .iter()
                .map(|follower| json!({ "address": follower.address, "applied": follower.applied }))
                .collect();
            json!({ "from": links.from, "followers": followers, "unheld": links.unheld, "written": links.written })
        });
        json!({
            "dir": self.dir,
            "served": served,
            "files": files,
            "bytes": self.files.iter().map(|(file, _)| file.bytes).sum::<u64>(),
            "node": self.node.then(|| json!({ "links": links })),
            "names": match &self.names {
                Named::Listed(names) => names.json(),
                Named::Held(holdings) => holdings.json(),
            },
        })
    }
}

/// The widths that line up the names `--names` shows, under every file:
///
/// ```text
///     sessions        500 keys       2.0 MiB
///     hits             12 keys        96 B     counters
///     audit          2000 events      30 KiB   keep 400d   2026-10-31 … 2026-10-31
///     client-errors     1 event      2.0 KiB   keep 90d    2026-10-02 … 2026-10-31
///     courses        3000 documents  930 KiB
/// ```
#[derive(Default)]
struct Columns {
    name: usize,
    count: usize,
    unit: usize,
    bytes: usize,
    keep: usize,
}

impl Columns {
    fn of<'a>(holdings: &Holdings, files: impl Iterator<Item = &'a str>) -> Columns {
        let mut columns = Columns::default();
        for file in files {
            let (names, counts) = holdings.of_file(file);
            for holding in names {
                let count = holding.count.unwrap_or_default();
                columns.name = columns.name.max(holding.name.chars().count());
                columns.count = columns.count.max(count.to_string().len());
                columns.unit = columns.unit.max(unit_of(count, counts).len());
                columns.bytes = columns.bytes.max(holding.bytes.map_or(0, |bytes| on_disk::bytes(bytes).len()));
                columns.keep = columns.keep.max(holding.kept.as_ref().map_or(0, |kept| keep_of(kept).len()));
            }
        }
        columns
    }

    /// A name's line: its count, its bytes, and a bucket's role or what a
    /// stream or a metric keeps.
    fn row(&self, holding: &Holding, counts: &str, colours: bool) -> String {
        let Columns { name: name_width, count: count_width, unit: unit_width, bytes: bytes_width, keep: keep_width } =
            *self;
        let count = holding.count.unwrap_or_default();
        let bytes = holding.bytes.map_or_else(String::new, on_disk::bytes);
        let row = format!(
            "    {:name_width$}  {count:>count_width$} {:unit_width$}  {bytes:>bytes_width$}",
            holding.name,
            unit_of(count, counts)
        );
        let role = holding.role.as_deref().filter(|role| *role != "values");
        let said = match (&holding.kept, role) {
            (Some(kept), _) => {
                let (oldest, newest) = (window::date(kept.oldest), window::date(kept.newest));
                format!("{:keep_width$}   {oldest} … {newest}", keep_of(kept))
            }
            (None, Some(role)) => role.to_owned(),
            (None, None) => return row + "\n",
        };
        row + "   " + &paint(colours, Style::new().dimmed(), &said) + "\n"
    }
}

fn keep_of(kept: &Kept) -> String {
    format!("keep {}", window::span_text(kept.keep))
}

/// What a count counts, as a person says it after the number: `key`, `keys`, `series`.
fn unit_of(count: u64, what: &str) -> String {
    if count == 1 || what.ends_with('s') { what.to_owned() } else { format!("{what}s") }
}

/// What a node does with a file: writes it, copies it, or keeps its own.
fn role(file: &StoreFile, status: Option<&FileStatus>, links: Option<&Links>) -> String {
    let Some(status) = status else {
        return match file.name.as_str() {
            "records" | "metrics" => "this node's own".to_owned(),
            _ => String::new(),
        };
    };
    if let Some(epoch) = status.fenced {
        return format!("fenced: another node writes it at epoch {epoch}");
    }
    if status.home {
        return if status.promoted { "home · promoted".to_owned() } else { "home".to_owned() };
    }
    let from = links.and_then(|links| links.from.as_deref()).map_or_else(String::new, |from| format!(" of {from}"));
    match status.applied {
        _ if status.taking => format!("copy{from} · taking a snapshot"),
        Some(applied) => format!("copy{from} · at commit {applied}"),
        None => format!("copy{from} · waiting for its first snapshot"),
    }
}

/// How far the server's commits go: what it was started with, or each
/// engine's own when it was started with neither.
fn durability_of(started: Option<&str>) -> String {
    started.map_or_else(
        || "durability not set: full, os for records and metrics".to_owned(),
        |started| format!("durability {started}"),
    )
}

/// How long ago `since` was, as a person says it: `40s`, `12m`, `3h 5m`, `2d 4h`.
fn age(since: SystemTime) -> String {
    let seconds = SystemTime::now().duration_since(since).unwrap_or(Duration::ZERO).as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{}d {}h", seconds / 86_400, seconds % 86_400 / 3600),
    }
}

fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinystore::records::NewEvent;
    use tinystore::{Options, Store};

    // contract: SEC-05
    #[test]
    fn a_store_no_server_holds_shows_its_files_and_says_it_is_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        store.bucket::<String>("sessions").open().unwrap().set("42", &"x".to_owned()).unwrap();
        store.close().unwrap();
        let report = Report::of(dir.path(), false).unwrap();
        let text = report.text(false);
        assert!(text.contains("○") && text.contains("not served"), "{text}");
        assert!(text.lines().any(|line| line.trim_start().starts_with("kv ")), "{text}");
        assert!(text.contains("total"), "{text}");
        assert_eq!(report.json()["served"], serde_json::Value::Null);
        assert_eq!(report.json()["files"][0]["name"], "kv");
    }

    /// The issue's store: two buckets of very different weight, two streams
    /// and an index of 3000 courses.
    fn a_store_of_students(dir: &Path) {
        let store = Store::open(dir, Options { background: false, ..Options::default() }).unwrap();
        let sessions = store.bucket::<String>("sessions").open().unwrap();
        for i in 0..500 {
            sessions.set(format!("s{i}").as_str(), &"x".repeat(4000)).unwrap();
        }
        let pins = store.bucket::<Vec<u32>>("pins").open().unwrap();
        for i in 0..50 {
            pins.set(format!("u{i}").as_str(), &vec![2518, 4293]).unwrap();
        }
        let audit = store.events("audit").keep(Duration::from_secs(400 * 86_400)).open().unwrap();
        audit.append_all((0..2000).map(|user| NewEvent::new("signed_in").fields(&json!({ "userId": user })))).unwrap();
        let errors = store.events("client-errors").keep(Duration::from_secs(90 * 86_400)).open().unwrap();
        errors.append("client.query", &json!({ "message": "x".repeat(2000) })).unwrap();
        let courses = store.index::<serde_json::Value>("courses").field("name", 1.0).open().unwrap();
        for from in (0..3000u64).step_by(1000) {
            let names = (from..from + 1000).map(|i| json!({ "name": format!("Курс {i}: линейная алгебра") }));
            let values: Vec<serde_json::Value> = names.collect();
            courses.set_many((from..).zip(&values)).unwrap();
        }
        // packed, as a program's background work packs each thousand documents set
        tinystore::search::maintain(&store).unwrap();
        store.close().unwrap();
    }

    #[test]
    fn names_say_what_each_bucket_stream_and_index_holds_the_heaviest_first() {
        let dir = tempfile::tempdir().unwrap();
        a_store_of_students(dir.path());
        let report = Report::of(dir.path(), true).unwrap();
        let names = &report.json()["names"];
        let shown = |list: &str, field: &str| -> Vec<serde_json::Value> {
            names[list].as_array().unwrap().iter().map(|held| held[field].clone()).collect()
        };
        assert_eq!(shown("buckets", "name"), [json!("sessions"), json!("pins")]);
        assert_eq!(shown("buckets", "count"), [json!(500), json!(50)]);
        assert_eq!(shown("streams", "name"), [json!("audit"), json!("client-errors")]);
        assert_eq!(shown("streams", "count"), [json!(2000), json!(1)]);
        assert_eq!(shown("indexes", "count"), [json!(3000)]);
        for list in ["buckets", "streams"] {
            let bytes: Vec<u64> = shown(list, "bytes").iter().map(|bytes| bytes.as_u64().unwrap()).collect();
            assert!(bytes[0] > bytes[1], "{list}: {bytes:?}, the heaviest first");
        }
        let keys = |list: &str| -> Vec<String> { names[list][0].as_object().unwrap().keys().cloned().collect() };
        assert_eq!(keys("buckets"), ["bytes", "count", "name", "role"]);
        assert_eq!(keys("streams"), ["bytes", "count", "keep", "name", "newest", "oldest"]);
        assert_eq!(keys("indexes"), ["bytes", "count", "name"]);
        assert_eq!(names["streams"][0]["keep"], "400d");

        let text = report.text(false);
        let lines: Vec<&str> = text.lines().collect();
        let at = |start: &str| lines.iter().position(|line| line.trim_start().starts_with(start)).unwrap();
        assert!(at("kv ") < at("sessions") && at("sessions") < at("pins") && at("pins") < at("search "), "{text}");
        assert!(lines[at("sessions")].contains(" 500 keys "), "{text}");
        assert!(lines[at("audit")].contains("2000 events") && lines[at("audit")].contains("keep 400d"), "{text}");
        assert!(lines[at("client-errors")].contains("   1 event "), "{text}");
        assert!(lines[at("courses")].contains("3000 documents"), "{text}");
    }

    #[test]
    fn a_status_without_names_lists_them_as_before() {
        let dir = tempfile::tempdir().unwrap();
        a_store_of_students(dir.path());
        let report = Report::of(dir.path(), false).unwrap();
        assert_eq!(report.json()["names"]["streams"], json!(["audit", "client-errors"]));
        assert_eq!(report.json()["names"]["buckets"][0], json!({ "name": "pins", "role": "values" }));
        assert!(!report.text(false).contains("sessions"));
    }

    #[test]
    fn an_age_reads_as_a_person_says_it() {
        let ago = |seconds| age(SystemTime::now() - Duration::from_secs(seconds));
        assert_eq!(ago(40), "40s");
        assert_eq!(ago(12 * 60 + 5), "12m");
        assert_eq!(ago(3 * 3600 + 5 * 60), "3h 5m");
        assert_eq!(ago(2 * 86_400 + 4 * 3600), "2d 4h");
    }

    // contract: SEC-05
    #[test]
    fn a_nodes_file_says_whether_it_writes_or_copies_it() {
        let kv = StoreFile { name: "kv".to_owned(), file: "kv.db".to_owned(), bytes: 1 };
        let status = FileStatus {
            file: "kv.db".to_owned(),
            home: false,
            entries: None,
            epoch: Some(2),
            applied: Some(90),
            taking: false,
            promoted: false,
            fenced: None,
        };
        let links = Links { from: Some("us:7444".to_owned()), ..Links::default() };
        assert_eq!(role(&kv, Some(&status), Some(&links)), "copy of us:7444 · at commit 90");
        assert_eq!(role(&kv, Some(&FileStatus { home: true, ..status.clone() }), None), "home");
        let records = StoreFile { name: "records".to_owned(), ..kv };
        assert_eq!(role(&records, None, None), "this node's own");
    }
}
