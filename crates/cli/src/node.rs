//! `tinystore node status`: what a node holds, read from its directory, a
//! node running meanwhile read as it stands.
//!
//! ```text
//! tinystore node status ./data
//! file            this node  entries      epoch  applied
//! kv.db           copies     55-90        2      90
//! sql/app.db      writes     1-412        1
//! ```

use std::path::Path;
use std::process::ExitCode;

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
        return ExitCode::SUCCESS;
    }
    let width = files.iter().map(|file| file.file.len()).max().unwrap_or(4).max(4);
    println!("{:width$}  this node  entries          epoch  applied", "file");
    for file in &files {
        println!("{}", line(file, width));
    }
    ExitCode::SUCCESS
}

/// A file's line: what this node does with it, its log's entries, their epoch
/// and where its copy stands.
fn line(file: &FileStatus, width: usize) -> String {
    let role = match (file.home, file.promoted) {
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
