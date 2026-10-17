//! `tinystore node`: what a node holds, and a stopped node's files made its
//! own or made copies again.
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
//!
//! tinystore node promote ./data --from us.internal:7444    asks us.internal:7444 first
//! tinystore node demote ./data
//! ```
//!
//! A promotion is the one step that can split a file for good: no node can
//! tell a dead home from a cut cable, so a person promotes, and the nodes the
//! copy took its files from are asked first whether they still serve them.

use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tinystore::ErrorKind;
use tinystore::replication::{FileStatus, Node};

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;
use crate::node_config::NodeConfig;

pub(crate) fn run(args: &[String]) -> ExitCode {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("status") => asked::run(&help::NODE_STATUS, rest, status),
        Some("promote") => asked::run(&help::NODE_PROMOTE, rest, promote),
        Some("demote") => asked::run(&help::NODE_DEMOTE, rest, demote),
        Some("--help" | "-h") | None => {
            for command in [&help::NODE_STATUS, &help::NODE_PROMOTE, &help::NODE_DEMOTE] {
                help::print_command(command);
                println!();
            }
            ExitCode::SUCCESS
        }
        Some(other) => asked::end(
            "node",
            wrong(format!("no command node {other}{}", crate::meant(other, &["status", "promote", "demote"]))),
        ),
    }
}

fn status(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_or_here(0)?;
    if !rest.is_empty() {
        return Err(wrong(format!("one directory, not {} too", rest.join(" "))));
    }
    let files = tinystore::replication::status(&dir)?;
    let links = tinystore::replication::links(&dir)?;
    if asked.has("--json") {
        let shown: Vec<serde_json::Value> = files
            .iter()
            .map(|file| {
                json!({
                    "file": file.file, "home": file.home, "entries": file.entries, "epoch": file.epoch,
                    "applied": file.applied, "taking": file.taking, "promoted": file.promoted, "fenced": file.fenced,
                })
            })
            .collect();
        let links = links.map(|links| {
            let followers: Vec<serde_json::Value> = links
                .followers
                .iter()
                .map(|follower| json!({ "address": follower.address, "applied": follower.applied }))
                .collect();
            json!({ "written": links.written, "from": links.from, "followers": followers, "unheld": links.unheld })
        });
        println!("{}", json!({ "files": shown, "links": links }));
        return Ok(());
    }
    if files.is_empty() {
        println!("{} keeps no log: it is no node, or wrote nothing as one", dir.display());
    } else {
        let width = files.iter().map(|file| file.file.len()).max().unwrap_or(4).max(4);
        println!("{:width$}  this node  entries          epoch  applied", "file");
        for file in &files {
            println!("{}", line(file, width));
        }
    }
    print_links(&dir, &files);
    Ok(())
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
    if links.unheld > 0 {
        println!("unheld    {} synchronous commits answered with no copy holding them", links.unheld);
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

fn promote(asked: Asked) -> Done {
    let (dir, files) = asked.dir_needed("node promote")?;
    if !asked.has("--force") {
        refuse_a_second_home(&dir, &files, &asked)?;
    }
    let promoted = tinystore::replication::promote(&dir, &files).map_err(|error| stopped_first(&dir, error))?;
    if promoted.is_empty() {
        return Err(failed(format!("{} holds no copy", dir.display())));
    }
    for one in promoted {
        println!("● {}  this node's own from commit {}, epoch {}", one.file, one.sequence + 1, one.epoch);
    }
    Ok(())
}

/// Asks each node `--from` names which files it serves, and refuses when one
/// still serves a file being promoted: its home is alive, or a copy of it.
fn refuse_a_second_home(dir: &Path, files: &[String], asked: &Asked) -> Done {
    let from = asked.values("--from");
    if from.is_empty() {
        eprintln!(
            "no node asked first: --from names the nodes {} took its files from, so that a home still alive is found",
            dir.display()
        );
        return Ok(());
    }
    let config =
        asked.value("--node-config").map(|file| NodeConfig::read(Path::new(file))).transpose().map_err(wrong)?;
    let config = config.unwrap_or_default();
    let node = Node {
        token: config.token().map_err(failed)?,
        tls: config.node_tls(),
        insecure: config.insecure,
        ..Node::default()
    };
    let promoting: Vec<String> = if files.is_empty() {
        tinystore::replication::status(dir)?.into_iter().filter(|file| !file.home).map(|file| file.file).collect()
    } else {
        files.to_vec()
    };
    for address in &from {
        let Some(served) = tinystore::replication::served_at(dir, node.clone(), address)? else {
            eprintln!("{address} does not answer");
            continue;
        };
        let alive: Vec<&String> = promoting.iter().filter(|file| served.contains(file)).collect();
        if !alive.is_empty() {
            let alive: Vec<&str> = alive.iter().map(|file| file.as_str()).collect();
            return Err(failed(format!(
                "{address} still serves {}: promoting would make a second home\nstop that node first, or promote with \
                 --force if it is known to take no more writes",
                alive.join(", ")
            )));
        }
    }
    Ok(())
}

fn demote(asked: Asked) -> Done {
    let (dir, files) = asked.dir_needed("node demote")?;
    let demoted = tinystore::replication::demote(&dir, &files).map_err(|error| stopped_first(&dir, error))?;
    if demoted.is_empty() {
        return Err(failed(format!("{} writes no file to make a copy", dir.display())));
    }
    for one in demoted {
        match one.last {
            Some((sequence, epoch)) => println!(
                "● {}  a copy again; its log stood at commit {sequence}, epoch {epoch}, and what its new home never \
                 had of it is lost",
                one.file
            ),
            None => println!("● {}  a copy again", one.file),
        }
    }
    println!("start the node with --from its new home to take each file again");
    Ok(())
}

/// A node running holds its directory: it is stopped first.
fn stopped_first(dir: &Path, error: tinystore::Error) -> asked::Refused {
    if error.kind() == ErrorKind::InUse {
        return failed(format!(
            "{error}\nstop the node that holds {} first: tinystore stop {}",
            dir.display(),
            dir.display()
        ));
    }
    error.into()
}
