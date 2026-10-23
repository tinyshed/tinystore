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
use crate::files::{self, StoreFile};
use crate::help;
use crate::lines::{Terminal, paint};

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
    let report = Report::of(&dir)?;
    if asked.has("--json") {
        println!("{}", report.json());
    } else {
        print!("{}", report.text(Terminal::of_stdout().colours()));
    }
    Ok(())
}

/// What `status --json` prints of a directory, for the MCP's tool too.
pub(crate) fn json_of(dir: &Path) -> Result<serde_json::Value, asked::Refused> {
    Ok(Report::of(dir)?.json())
}

/// What `status` found in a directory.
struct Report {
    dir: String,
    served: Served,
    files: Vec<(StoreFile, Option<FileStatus>)>,
    node: bool,
    links: Option<Links>,
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
    fn of(dir: &Path) -> Result<Report, asked::Refused> {
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
        let files = files::of(dir)
            .into_iter()
            .map(|file| {
                let status = logged.iter().find(|status| status.file == file.file).cloned();
                (file, status)
            })
            .collect();
        let links = tinystore::replication::links(dir)?;
        Ok(Report { dir: dir.display().to_string(), served, files, node, links })
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
        let sizes: Vec<String> = self.files.iter().map(|(file, _)| files::bytes(file.bytes)).collect();
        let total = files::bytes(self.files.iter().map(|(file, _)| file.bytes).sum());
        let size_width = sizes.iter().map(String::len).chain([total.len()]).max().unwrap_or(0);
        for ((file, status), size) in self.files.iter().zip(&sizes) {
            let role = if self.node { role(file, status.as_ref(), self.links.as_ref()) } else { String::new() };
            text.push_str(&format!("  {:name_width$}  {size:>size_width$}", file.name));
            if !role.is_empty() {
                text.push_str(&format!("   {}", paint(dim, &role)));
            }
            text.push('\n');
        }
        text.push_str(&format!("  {}\n", paint(dim, &"─".repeat(name_width + 2 + size_width))));
        text.push_str(&format!("  {:name_width$}  {total:>size_width$}\n", "total"));
        if let Some(node) = self.node_line() {
            text.push_str(&format!("\n  {} {}\n", paint(bold, "node"), node));
        }
        text
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
        })
    }
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
    use tinystore::{Options, Store};

    // contract: SEC-05
    #[test]
    fn a_store_no_server_holds_shows_its_files_and_says_it_is_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        store.bucket::<String>("sessions").open().unwrap().set("42", &"x".to_owned()).unwrap();
        store.close().unwrap();
        let report = Report::of(dir.path()).unwrap();
        let text = report.text(false);
        assert!(text.contains("○") && text.contains("not served"), "{text}");
        assert!(text.lines().any(|line| line.trim_start().starts_with("kv ")), "{text}");
        assert!(text.contains("total"), "{text}");
        assert_eq!(report.json()["served"], serde_json::Value::Null);
        assert_eq!(report.json()["files"][0]["name"], "kv");
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
