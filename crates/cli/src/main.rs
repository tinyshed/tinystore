//! The tinystore command. `serve` gives a store's directory to other
//! processes over the wire protocol: a private child's stdin and stdout, or a
//! local socket or named pipe that `SERVE` names, for a sidecar or a person.
//! `backup` writes a store's directory as one checked zip, and `restore` puts
//! one back into an empty directory.

mod args;
mod backup;
mod connection;
mod lines;
mod local;
mod remote;
mod serve;

use std::process::ExitCode;

const USAGE: &str = "usage: tinystore serve [<dir> | --dir <dir>] [--local | --stdio] [--log <file>] [--idle <span>] \
                     [--clock <time>] [--listen tls://<host>:<port> --tls-cert <pem> --tls-key <pem> --tokens <file>] \
                     [--memory <size>]
       tinystore backup <dir> <file.zip> [--include <name>]... [--include-encryption-key]
       tinystore restore <file.zip> <dir>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve::run(&args[1..]),
        Some("backup") => backup::backup(&args[1..]),
        Some("restore") => backup::restore(&args[1..]),
        Some("version" | "--version") => {
            println!("tinystore {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
