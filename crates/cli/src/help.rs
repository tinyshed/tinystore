//! What `tinystore` alone, `tinystore help` and `--help` print: the commands
//! by what a person does with them, and each command's flags and examples,
//! coloured on a terminal and plain in a pipe. Help is no error, so it goes to
//! stdout and exits 0. The parser reads its flags from the same table, so a
//! flag the help names is a flag the command takes.

use nu_ansi_term::{Color, Style};

use crate::lines::{Terminal, paint};

/// A command as help shows it and the parser reads it.
pub(crate) struct Command {
    /// `status`, or `node promote` for a command of a group.
    pub(crate) name: &'static str,
    pub(crate) takes: &'static str,
    pub(crate) does: &'static str,
    pub(crate) flags: &'static [Flag],
    pub(crate) examples: &'static [&'static str],
    /// What a person needs beyond the flags, a sentence or two.
    pub(crate) more: &'static str,
}

/// A flag: its name, the value it takes, `""` for a switch, what it means,
/// and the heading it is shown under, `""` for none and `HIDDEN` for a flag
/// only the SDKs pass.
pub(crate) struct Flag {
    pub(crate) name: &'static str,
    pub(crate) value: &'static str,
    pub(crate) means: &'static str,
    pub(crate) heading: &'static str,
}

/// The heading of a flag that only the SDKs pass, which help leaves out.
pub(crate) const HIDDEN: &str = "hidden";

const fn flag(name: &'static str, value: &'static str, means: &'static str) -> Flag {
    Flag { name, value, means, heading: "" }
}

const fn under(heading: &'static str, name: &'static str, value: &'static str, means: &'static str) -> Flag {
    Flag { name, value, means, heading }
}

const JSON: Flag = flag("--json", "", "one JSON document, for scripts");

pub(crate) const STATUS: Command = Command {
    name: "status",
    takes: "[dir]",
    does: "what the store holds, who serves it, how its node stands",
    flags: &[JSON],
    examples: &["tinystore status ./data", "tinystore status ./data --json"],
    more: "A directory no server holds says so and exits 0.",
};

pub(crate) const LOGS: Command = Command {
    name: "logs",
    takes: "[dir]",
    does: "the application's logs, and new ones as they arrive with -f",
    flags: &[
        flag("-f", "", "follow new lines as they come, until Ctrl+C"),
        flag("-n", "<count>", "how many of the last lines to print first; 20 unless --since"),
        flag("--level", "<level>", "trace, debug, info, warn or error, and above"),
        flag("--since", "<span>", "lines from the last span, as 30s, 15m, 1h or 2d"),
        flag("--grep", "<text>", "lines that hold the text, in any case"),
        flag("--stream", "<name>", "one stream, as api"),
        flag("--json", "", "one JSON object a line"),
    ],
    examples: &["tinystore logs ./data -f", "tinystore logs ./data --since 1h --level warn"],
    more: "It reads beside the program or server that holds the directory, and writes nothing.",
};

pub(crate) const KV: Command = Command {
    name: "kv",
    takes: "[dir] get <bucket> <key>",
    does: "one value; list <bucket> [branch] for a bucket's keys",
    flags: &[flag("--json", "", "each value with its key and expiry, as JSON")],
    examples: &[
        "tinystore kv ./data get sessions 42",
        "tinystore kv ./data list sessions",
        "tinystore kv ./data list sessions 42",
    ],
    more: "It reads beside the program or server that holds the directory, and an encrypted bucket's values with the \
           key the directory holds.",
};

pub(crate) const SQL: Command = Command {
    name: "sql",
    takes: "[dir] <db> \"<query>\"",
    does: "a query's rows; reads only unless --write",
    flags: &[flag("--write", "", "let the statement write: needs the directory no program holds"), JSON],
    examples: &[
        "tinystore sql ./data app \"select count(*) from users\"",
        "tinystore sql ./data app \"delete from users where id = 7\" --write",
    ],
    more: "Without --write it reads beside the program or server that holds the directory.",
};

pub(crate) const SEARCH: Command = Command {
    name: "search",
    takes: "list [dir]",
    does: "the store's search indexes; drop <dir> <name> drops one",
    flags: &[],
    examples: &["tinystore search list ./data", "tinystore search drop ./data posts"],
    more: "An index no program opens any more is kept until it is dropped, since another version may open it again.",
};

pub(crate) const SERVE: Command = Command {
    name: "serve",
    takes: "<dir>",
    does: "share the store with programs here and elsewhere, until Ctrl+C",
    flags: &[
        under("local", "--stdio", "", "one client on stdin and stdout, for a program that starts the server itself"),
        under("remote", "--listen", "tls://<host>:<port>", "serve other machines over TLS"),
        under("remote", "--tls-cert", "<pem>", "the certificate chain it answers with"),
        under("remote", "--tls-key", "<pem>", "its private key"),
        under("remote", "--tokens", "<file>", "who may connect: admin, data and read tokens, a line each"),
        under("node", "--node-listen", "<host>:<port>", "where other nodes copy from this one"),
        under("node", "--from", "<host>:<port>", "a node it copies from; again for the next to try"),
        under("node", "--node-config", "<file.toml>", "the rest of what the node is"),
        under("limits", "--memory", "<size>", "what the engines may hold at once, as 512MiB; 1GiB with --listen"),
        under("limits", "--idle", "<span>", "stop once no client has been connected this long; never by default"),
        under("store", "--durability", "full|os", "how far a commit goes before it returns; each engine's own"),
        under("store", "--keep-free", "<size>", "the disk the store keeps free; 1GiB by default"),
        under("store", "--encryption-key-file", "<file>", "the store's key, kept apart from its data"),
        under("output", "--log-file", "<file>", "JSON lines to a file as well"),
        under("output", "--quiet", "", "the banner, warnings and errors only"),
        under(HIDDEN, "--local", "", "a sidecar an SDK started"),
        under(HIDDEN, "--clock", "<time>", "a private server's test clock"),
    ],
    examples: &[
        "tinystore serve ./data",
        "tinystore serve ./data --listen tls://0.0.0.0:7443 --tls-cert cert.pem --tls-key key.pem --tokens tokens.txt",
        "tinystore serve ./data --node-listen 0.0.0.0:7444 --from us.internal:7444 --node-config node.toml",
    ],
    more: "Programs on this machine find it through <dir>/server/SERVE. A node's rarer settings live in its \
           --node-config file: files, token-file, catalog, advertise, synchronous, synchronous-with, failover, \
           insecure and [tls] cert, key and roots.",
};

pub(crate) const STOP: Command = Command {
    name: "stop",
    takes: "<dir>",
    does: "stop the server of a directory, its work finished",
    flags: &[],
    examples: &["tinystore stop ./data"],
    more: "The clients are told to leave and the streams running finish. With no server it says so and exits 1.",
};

pub(crate) const BACKUP: Command = Command {
    name: "backup",
    takes: "<dir> <zip>",
    does: "the whole store in one checked zip, while it runs",
    flags: &[
        flag("--include", "<name>", "a file of the program's own in the directory; again for more"),
        flag("--include-encryption-key", "", "the store's key too, which a backup leaves out by default"),
        flag("--encryption-key-file", "<file>", "where the store's key is, when not in the directory"),
    ],
    examples: &["tinystore backup ./data backup.zip"],
    more: "It reads beside a server or program that holds the directory, and alone when none does.",
};

pub(crate) const RESTORE: Command = Command {
    name: "restore",
    takes: "<zip> <dir>",
    does: "a backup into an empty directory",
    flags: &[],
    examples: &["tinystore restore backup.zip ./data2"],
    more: "Every byte is checked against the backup's manifest, or nothing is left behind.",
};

pub(crate) const MCP: Command = Command {
    name: "mcp",
    takes: "[dir]",
    does: "let an AI agent read the store",
    flags: &[],
    examples: &["claude mcp add tinystore -- tinystore mcp ./data"],
    more: "The Model Context Protocol on stdin and stdout, with tools that only read: status, logs, kv_get, kv_list, \
           jobs_list, sql_query and search_list.",
};

pub(crate) const TOKEN: Command = Command {
    name: "token",
    takes: "new [admin|data|read]",
    does: "a token line for serve's --tokens",
    flags: &[],
    examples: &["tinystore token new admin >> tokens.txt"],
    more: "admin may do anything, data reads and writes the store, read only reads it.",
};

pub(crate) const NODE_STATUS: Command = Command {
    name: "node status",
    takes: "[dir]",
    does: "the node's files, its links and how far behind its copies are",
    flags: &[JSON],
    examples: &["tinystore node status ./data"],
    more: "",
};

pub(crate) const NODE_PROMOTE: Command = Command {
    name: "node promote",
    takes: "<dir> [file]",
    does: "make a copy the home, once its old home is gone",
    flags: &[
        flag("--from", "<host>:<port>", "a node the copy took its files from, asked first; again for more"),
        flag("--node-config", "<file.toml>", "the node's settings, for the token and TLS that reach them"),
        flag("--force", "", "promote though a node it took from still answers"),
    ],
    examples: &["tinystore node promote ./data", "tinystore node promote ./data sql/app.db --from us.internal:7444"],
    more: "The node must be stopped. Each node named by --from is asked first: one that still serves the file would \
           make a second home, so the promotion is refused unless --force.",
};

pub(crate) const NODE_DEMOTE: Command = Command {
    name: "node demote",
    takes: "<dir> [file]",
    does: "make an old home's files copies again",
    flags: &[],
    examples: &["tinystore node demote ./data"],
    more: "The node must be stopped. Start it again with --from its new home to take each file again.",
};

pub(crate) const WITNESS: Command = Command {
    name: "witness",
    takes: "<dir> --listen <host>:<port>",
    does: "decide which node writes a file when its home goes quiet",
    flags: &[
        flag("--listen", "<host>:<port>", "where nodes reach it"),
        flag("--token-file", "<file>", "the token nodes say to it"),
        flag("--tls-cert", "<pem>", "the certificate chain it answers with"),
        flag("--tls-key", "<pem>", "its private key"),
        flag("--insecure", "", "plain TCP beyond this machine, which anyone on the way reads"),
        flag("--quiet", "", "warnings and errors only"),
    ],
    examples: &["tinystore witness ./witness --listen 0.0.0.0:7445 --token-file token"],
    more: "A witness keeps each file's lease and no data; nodes name it as failover in their node config.",
};

pub(crate) const MIGRATE: Command = Command {
    name: "migrate",
    takes: "<dir> <db> <migrations>",
    does: "apply a database's migration files",
    flags: &[],
    examples: &["tinystore migrate ./data app ./migrations"],
    more: "It applies the files the database has not applied, in one transaction, and refuses one that applied an \
           edited, renamed or missing file. No program may hold the directory meanwhile.",
};

pub(crate) const SCHEMA: Command = Command {
    name: "schema",
    takes: "<dir> [db]",
    does: "a database's tables, columns and indexes",
    flags: &[JSON],
    examples: &["tinystore schema ./data app", "tinystore schema ./data"],
    more: "Without a database it lists the store's databases.",
};

/// The commands by what a person does with them.
pub(crate) const GROUPS: [(&str, &[&Command]); 4] = [
    ("Look", &[&STATUS, &LOGS, &KV, &SQL, &SEARCH]),
    ("Run", &[&SERVE, &STOP, &BACKUP, &RESTORE, &MCP, &TOKEN]),
    ("Nodes", &[&NODE_STATUS, &NODE_PROMOTE, &NODE_DEMOTE, &WITNESS]),
    ("Develop", &[&MIGRATE, &SCHEMA]),
];

/// Every command by its name, for `help <command>` and a misspelt one.
pub(crate) fn command(name: &str) -> Option<&'static Command> {
    GROUPS.iter().flat_map(|(_, commands)| commands.iter().copied()).find(|command| command.name == name)
}

/// The words a command may begin with, for a misspelt one's nearest.
pub(crate) fn first_words() -> Vec<&'static str> {
    let mut words: Vec<&str> = GROUPS
        .iter()
        .flat_map(|(_, commands)| commands.iter())
        .map(|command| command.name.split(' ').next().unwrap_or(command.name))
        .collect();
    words.extend(["help", "version"]);
    words.dedup();
    words
}

/// The help, coloured when `colours`.
pub(crate) fn text(colours: bool) -> String {
    let paint = |style: Style, text: &str| paint(colours, style, text);
    let (bold, dim) = (Style::new().bold(), Style::new().dimmed());
    let (named, takes) = (Color::Green.bold(), Color::Cyan.normal());
    let mut help = format!("{} {}\n", paint(bold, "tinystore"), paint(dim, env!("CARGO_PKG_VERSION")));
    let rows = || GROUPS.iter().flat_map(|(_, commands)| commands.iter());
    let spelled = |command: &Command| {
        let (name, args) = split_name(command);
        name.len() + 1 + args.len()
    };
    let width = rows().map(|command| spelled(command)).max().unwrap_or(0);
    for (group, commands) in GROUPS {
        help.push_str(&format!("\n{}\n", paint(bold, group)));
        for command in commands {
            let (name, args) = split_name(command);
            let pad = " ".repeat(width - spelled(command) + 2);
            help.push_str(&format!("  {} {}{pad}{}\n", paint(named, &name), paint(takes, &args), command.does));
        }
    }
    help.push_str(&format!(
        "\n{}\n",
        paint(dim, "tinystore help <command> · --version · --json for scripts · docs: tinyshed.org/tinystore")
    ));
    help
}

/// The name as the list shows it: a group's command with its word after it
/// among what it takes, `node` then `status [dir]`, so that the columns line up.
fn split_name(command: &Command) -> (String, String) {
    match command.name.split_once(' ') {
        Some((group, word)) => (group.to_owned(), format!("{word} {}", command.takes)),
        None => (command.name.to_owned(), command.takes.to_owned()),
    }
}

/// One command's help: how it is called, its flags under their headings, and
/// an example of each use.
pub(crate) fn of_command(colours: bool, command: &Command) -> String {
    let paint = |style: Style, text: &str| paint(colours, style, text);
    let (bold, dim, named, takes) =
        (Style::new().bold(), Style::new().dimmed(), Color::Green.bold(), Color::Cyan.normal());
    let mut help = format!(
        "{} {} {}\n{}\n",
        paint(bold, "tinystore"),
        paint(named, command.name),
        paint(takes, command.takes),
        command.does
    );
    let shown: Vec<&Flag> = command.flags.iter().filter(|flag| flag.heading != HIDDEN).collect();
    let width = shown.iter().map(|flag| flag.name.len() + 1 + flag.value.len()).max().unwrap_or(0);
    let mut heading = None;
    for (at, flag) in shown.iter().enumerate() {
        if at == 0 || heading != Some(flag.heading) {
            help.push('\n');
            if !flag.heading.is_empty() {
                help.push_str(&format!("{}\n", paint(bold, flag.heading)));
            }
            heading = Some(flag.heading);
        }
        let spelled =
            if flag.value.is_empty() { flag.name.to_owned() } else { format!("{} {}", flag.name, flag.value) };
        let pad = " ".repeat(width - spelled.len() + 2);
        help.push_str(&format!("  {}{pad}{}\n", paint(takes, &spelled), flag.means));
    }
    if !command.more.is_empty() {
        help.push_str(&format!("\n{}\n", wrapped(command.more, 80)));
    }
    help.push('\n');
    for example in command.examples {
        help.push_str(&format!("  {}\n", paint(dim, example)));
    }
    help
}

/// Text broken into lines of at most `width` columns, at spaces.
fn wrapped(text: &str, width: usize) -> String {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let line = lines.last_mut().expect("one line at least");
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(word.to_owned());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    lines.join("\n")
}

/// Prints the help to stdout, coloured on a terminal.
pub(crate) fn print() {
    print!("{}", text(Terminal::of_stdout().colours()));
}

/// Prints one command's help to stdout, coloured on a terminal.
pub(crate) fn print_command(command: &Command) {
    print!("{}", of_command(Terminal::of_stdout().colours(), command));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_help_groups_every_command_and_lines_up_what_each_does() {
        let help = text(false);
        for group in ["Look", "Run", "Nodes", "Develop"] {
            assert!(help.lines().any(|line| line == group), "{group}:\n{help}");
        }
        let rows: Vec<&str> = help.lines().filter(|line| line.starts_with("  ")).collect();
        let starts: Vec<usize> = rows
            .iter()
            .map(|row| {
                let command =
                    GROUPS.iter().flat_map(|(_, commands)| commands.iter()).find(|command| row.ends_with(command.does));
                row.len() - command.map_or(0, |command| command.does.len())
            })
            .collect();
        assert!(starts.windows(2).all(|pair| pair[0] == pair[1]), "what each does starts in one column:\n{help}");
        assert!(!help.contains('\u{1b}'), "no colour without a terminal");
        assert!(text(true).contains('\u{1b}'));
    }

    #[test]
    fn a_commands_help_shows_its_flags_but_the_sdks_own() {
        let help = of_command(false, &SERVE);
        assert!(help.starts_with("tinystore serve <dir>\n"), "{help}");
        assert!(help.contains("\nremote\n  --listen tls://<host>:<port>"), "{help}");
        assert!(help.contains("--node-config <file.toml>"), "{help}");
        assert!(!help.contains("--clock") && !help.contains("--local"), "{help}");
        assert!(help.contains("  tinystore serve ./data\n"), "{help}");
        assert_eq!(command("node promote").map(|command| command.name), Some("node promote"));
        assert!(first_words().contains(&"node") && first_words().contains(&"witness"));
    }

    #[test]
    fn a_long_line_breaks_at_spaces_within_its_width() {
        assert_eq!(wrapped("one two three four", 9), "one two\nthree\nfour");
    }
}
