//! The tinystore command. `serve` gives a store's directory to other
//! processes over the wire protocol: a private child's stdin and stdout, or a
//! local socket or named pipe that `SERVE` names, for a sidecar or a person.

mod args;
mod connection;
mod local;
mod serve;

use std::process::ExitCode;

const USAGE: &str = "usage: tinystore serve [<dir> | --dir <dir>] [--local | --stdio] [--log <file>] [--idle <span>] \
                     [--clock <time>]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve::run(&args[1..]),
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
