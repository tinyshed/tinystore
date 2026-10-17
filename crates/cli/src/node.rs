//! `tinystore node status`: what a node holds, read from its directory, a
//! node running meanwhile read as it stands, with the links it last wrote.
//!
//! ```text
//! tinystore node status ./data
//! file            this node  entries      epoch  applied
//! kv.db           copies     55-90        2      90
//! sql/app.db      writes     1-412        1
//!
//! running   links written 3 s ago
//! from      10.0.0.4:7400
//! follower  10.0.0.9:51234  sql/app.db 410, 2 behind
//! ```

use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use tinystore::replication::FileStatus;

pub(crate) const USAGE: &str = "usage: tinystore node status <dir>";

pub(crate) fn run(args: &[String]) -> ExitCode {
    match args {
        [command, dir] if command == "status" => status(Path::new(dir)),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn status(dir: &Path) -> ExitCode {
    let files = match tinystore::replication::status(dir) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("tinystore node status: {error}");
            return ExitCode::FAILURE;
        }
    };
    if files.is_empty() {
        println!("{} keeps no log: it is no node, or wrote nothing as one", dir.display());
        print_links(dir, &files);
        return ExitCode::SUCCESS;
    }
    let width = files.iter().map(|file| file.file.len()).max().unwrap_or(4).max(4);
    println!("{:width$}  this node  entries          epoch  applied", "file");
    for file in &files {
        println!("{}", line(file, width));
    }
    print_links(dir, &files);
    ExitCode::SUCCESS
}

/// What the node running in `dir` last wrote of its links, and how far behind
/// this node's log each follower is; nothing for a node that does not run.
fn print_links(dir: &Path, files: &[FileStatus]) {
    let links = match tinystore::replication::links(dir) {
        Ok(Some(links)) => links,
        Ok(None) => return,
        Err(error) => {
            eprintln!("tinystore node status: {error}");
            return;
        }
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    let age = now.saturating_sub(links.written) / 1000;
    let stale = if age > 15 { ": the node may have stopped without removing them" } else { "" };
    println!();
    println!("running   links written {age} s ago{stale}");
    if let Some(from) = &links.from {
        println!("from      {from}");
    }
    for follower in &links.followers {
        let held: Vec<String> = follower
            .applied
            .iter()
            .map(|(file, applied)| {
                let last = files.iter().find(|status| &status.file == file).and_then(|status| status.entries);
                match last {
                    Some((_, last)) if last > *applied => format!("{file} {applied}, {} behind", last - applied),
                    _ => format!("{file} {applied}"),
                }
            })
            .collect();
        println!("{}", format!("follower  {}  {}", follower.address, held.join(", ")).trim_end());
    }
}

/// A file's line: what this node does with it, its log's entries, their epoch
/// and where its copy stands.
fn line(file: &FileStatus, width: usize) -> String {
    let role = match (file.home, file.promoted) {
        (true, _) if file.fenced.is_some() => "fenced",
        (true, true) => "promoted",
        (true, false) => "writes",
        (false, _) if file.taking => "taking",
        (false, _) => "copies",
    };
    let entries = file.entries.map_or_else(|| "none".to_owned(), |(first, last)| format!("{first}-{last}"));
    let epoch = file.epoch.map_or_else(String::new, |epoch| epoch.to_string());
    let applied = file.applied.map_or_else(String::new, |applied| applied.to_string());
    format!("{:width$}  {role:9}  {entries:15}  {epoch:5}  {applied}", file.file).trim_end().to_owned()
}
