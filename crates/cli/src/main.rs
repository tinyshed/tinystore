//! The tinystore command: the one command a person types. `serve` gives a
//! store's directory to other processes and machines over the wire protocol;
//! `status`, `logs`, `kv`, `sql`, `schema`, `search` and `mcp` read a store
//! beside whoever holds it; `sql --write`, `migrate` and `search drop` change
//! one nothing holds; `metrics ingest` writes through the server that holds
//! one, or a sidecar it starts; `stop` ends a server and `token` makes its tokens;
//! `backup` and `restore` keep a store; `node` and `witness` run the nodes of
//! a network. `help` lists them all, and docs/guides/cli/README.md is their guide.

/// `print!` for what a command answers on stdout. It gives the command's
/// `Done`, so that `?` ends the command quietly once the reader has gone, as
/// `| head` does when it has its lines, where `print!` panics.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::asked::out(format_args!($($arg)*))
    };
}

/// `println!` as `out!` is `print!`.
macro_rules! outln {
    () => {
        $crate::asked::out(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::asked::out(format_args!("{}\n", format_args!($($arg)*)))
    };
}

mod args;
mod asked;
mod backup;
mod banner;
mod client;
mod connection;
mod files;
mod help;
mod kv;
mod lines;
mod local;
mod logs;
mod mcp;
mod metrics;
mod node;
mod node_config;
#[cfg(windows)]
mod own_handles;
#[cfg(windows)]
mod owner_only;
mod reach;
mod remote;
mod search;
mod serve;
mod sql;
mod status;
mod stop;
mod token;
mod witness;

use std::process::ExitCode;

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
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("status") => status::run(rest),
        Some("logs") => logs::run(rest),
        Some("kv") => kv::run(rest),
        Some("metrics") => metrics::run(rest),
        Some("sql") => sql::run(rest),
        Some("search") => search::run(rest),
        Some("serve") => serve::run(rest),
        Some("stop") => stop::run(rest),
        Some("backup") => backup::backup(rest),
        Some("restore") => backup::restore(rest),
        Some("mcp") => mcp::run(rest),
        Some("token") => token::run(rest),
        Some("node") => node::run(rest),
        Some("witness") => witness::run(rest),
        Some("migrate") => sql::migrate(rest),
        Some("schema") => sql::schema(rest),
        Some("version" | "--version" | "-V") => asked::exit("", outln!("tinystore {}", env!("CARGO_PKG_VERSION"))),
        None => asked::exit("", help::print()),
        Some("help" | "--help" | "-h") if rest.is_empty() => asked::exit("help", help::print()),
        Some("help") => {
            let name = rest.join(" ");
            match help::command(&name).or_else(|| help::command(&rest[0])) {
                Some(command) => asked::exit("help", help::print_command(command)),
                None if rest[0] == "node" => node::run(&["--help".to_owned()]),
                None if rest[0] == "metrics" => asked::exit("help", help::print_group("metrics")),
                None => asked::end(
                    "help",
                    asked::wrong(format!("no command {name}{}", meant(&rest[0], &help::first_words()))),
                ),
            }
        }
        Some(other) => {
            let refused = asked::wrong(format!("no command {other}{}", meant(other, &help::first_words())));
            asked::end("", refused)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // contract: CLI-02
    #[test]
    fn a_misspelt_command_is_told_its_nearest_and_a_word_far_from_any_none() {
        let commands = help::first_words();
        assert_eq!(meant("bakup", &commands), "; did you mean backup?");
        assert_eq!(meant("serv", &commands), "; did you mean serve?");
        assert_eq!(meant("deploy", &commands), "");
    }
}
