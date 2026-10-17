//! The tinystore command. `serve` gives a store's directory to other
//! processes over the wire protocol: a private child's stdin and stdout, or a
//! local socket or named pipe that `SERVE` names, for a sidecar or a person.
//! `backup` writes a store's directory as one checked zip, and `restore` puts
//! one back into an empty directory. `search` lists a store's indexes and
//! drops one.

mod args;
mod backup;
mod connection;
mod lines;
mod local;
mod node;
mod promote;
mod remote;
mod search;
mod serve;

use std::process::ExitCode;

const USAGE: &str = "usage: tinystore serve [<dir> | --dir <dir>] [--local | --stdio] [--log <file>] [--idle <span>] \
                     [--clock <time>] [--listen tls://<host>:<port> --tls-cert <pem> --tls-key <pem> --tokens <file>] \
                     [--memory <size>] [--node] [--node-listen <host>:<port>] [--from <host>:<port>]...                      [--node-files <file>,...] [--node-token-file <file>] [--node-synchronous]                      [--node-tls-cert <pem> --node-tls-key <pem>] [--node-tls-roots <pem>]
       tinystore backup <dir> <file.zip> [--include <name>]... [--include-encryption-key]
       tinystore restore <file.zip> <dir>
       tinystore search list <dir>
       tinystore search drop <dir> <name>
       tinystore promote <dir> [<file>...]
       tinystore demote <dir> [<file>...]
       tinystore node status <dir>";

/// The commands, for a misspelt one's nearest.
const COMMANDS: [&str; 8] = ["serve", "backup", "restore", "search", "promote", "demote", "node", "version"];

/// What follows a refusal when a person may have misspelt a command or a
/// flag: the nearest of `names`, one edit away for three to six letters and
/// two for more. It is only said: no word is read as another.
pub(crate) fn meant(given: &str, names: &[&str]) -> String {
    let edits = match given.chars().count() {
        0..3 => 0,
        3..7 => 1,
        _ => 2,
    };
    tinystore::typos::Dictionary::new(names)
        .nearest(given, edits)
        .map_or_else(String::new, |near| format!("; did you mean {}?", near.word))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve::run(&args[1..]),
        Some("backup") => backup::backup(&args[1..]),
        Some("restore") => backup::restore(&args[1..]),
        Some("search") => search::run(&args[1..]),
        Some("promote") => promote::run(&args[1..]),
        Some("demote") => promote::demote(&args[1..]),
        Some("node") => node::run(&args[1..]),
        Some("version" | "--version") => {
            println!("tinystore {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        None | Some("help" | "--help" | "-h") => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Some(other) => {
            eprintln!("tinystore: no command {other}{}\n{USAGE}", meant(other, &COMMANDS));
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_misspelt_command_is_told_its_nearest_and_a_word_far_from_any_none() {
        assert_eq!(meant("bakup", &COMMANDS), "; did you mean backup?");
        assert_eq!(meant("serv", &COMMANDS), "; did you mean serve?");
        assert_eq!(meant("deploy", &COMMANDS), "");
    }
}
