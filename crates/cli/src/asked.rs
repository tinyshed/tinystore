//! What a command was asked: its words and its flags, read against the
//! flags its help names, and how it ends. A mistake in the command line is
//! one line saying what is wrong and one naming the help, exit 2; a failure
//! while running is one line, exit 1.
//!
//! ```text
//! tinystore logs ./data --levle warn
//! tinystore logs: no flag --levle; did you mean --level?
//!   tinystore help logs shows how it is called
//! ```

use std::fmt;
use std::io::{self, IsTerminal as _, Read as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use nu_ansi_term::Color;

use crate::help::{self, Command};
use crate::lines::{Terminal, paint};

/// A command's words in order and the flags it was given, each once for
/// every time it was given.
#[derive(Debug, Default)]
pub(crate) struct Asked {
    pub(crate) words: Vec<String>,
    /// `--dir`'s directory, which every command takes beside its words.
    pub(crate) dir: Option<PathBuf>,
    flags: Vec<(&'static str, String)>,
}

/// How a command ended other than done: the command line was wrong, the
/// command failed while it ran, or the reader of its stdout went away first.
#[derive(Debug)]
pub(crate) enum Refused {
    Wrong(String),
    Failed(String),
    /// `| head` closes the pipe once it has its lines: the command ends as
    /// done, saying nothing.
    ReaderGone,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Wrong(why) | Refused::Failed(why) => f.write_str(why),
            Refused::ReaderGone => f.write_str("the reader of stdout has gone"),
        }
    }
}

pub(crate) fn wrong(why: impl fmt::Display) -> Refused {
    Refused::Wrong(why.to_string())
}

pub(crate) fn failed(why: impl fmt::Display) -> Refused {
    Refused::Failed(why.to_string())
}

/// Refuses a directory that is not there: opening a store makes one, so a
/// command that reads or changes a store would otherwise work on an empty
/// one it made of a typo.
pub(crate) fn existing(dir: &std::path::Path) -> Result<(), Refused> {
    if dir.is_dir() { Ok(()) } else { Err(failed(format!("{}: no store is there", dir.display()))) }
}

impl From<tinystore::Error> for Refused {
    fn from(error: tinystore::Error) -> Refused {
        Refused::Failed(error.to_string())
    }
}

pub(crate) type Done = Result<(), Refused>;

/// A verb of a group of commands, `metrics read`: its word, its help and
/// what runs it.
pub(crate) struct Verb {
    pub(crate) word: &'static str,
    pub(crate) command: &'static Command,
    pub(crate) run: fn(Asked) -> Done,
}

/// Runs the verb of `group` that `args` names. Alone or with `--help`, the
/// group prints every verb's help; a verb it does not know is told its
/// nearest.
pub(crate) fn run_verb(group: &str, verbs: &[Verb], args: &[String]) -> ExitCode {
    if let Some(verb) = args.iter().find_map(|arg| verbs.iter().find(|verb| verb.word == arg)) {
        return run(verb.command, args, verb.run);
    }
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return exit(group, help::print_group(group));
    }
    let words: Vec<&str> = verbs.iter().map(|verb| verb.word).collect();
    let why = match args.iter().filter(|arg| !arg.starts_with('-')).nth(1) {
        Some(word) => format!("no command {group} {word}{}", crate::meant(word, &words)),
        None => format!("a verb after the directory: {}", words.join(", ")),
    };
    end(group, wrong(why))
}

/// Writes what a command answers to stdout, as `out!` and `outln!` ask.
pub(crate) fn out(text: fmt::Arguments<'_>) -> Done {
    io::stdout().write_fmt(text).map_err(unwritten)
}

/// What a command ends with when stdout takes no more: done once its reader
/// has gone, a failure otherwise.
pub(crate) fn unwritten(error: io::Error) -> Refused {
    match error.kind() {
        io::ErrorKind::BrokenPipe => Refused::ReaderGone,
        _ => failed(format!("stdout: {error}")),
    }
}

/// Says something beside a command's answer, on stderr and named after the
/// command, so that it reads apart from the answer in `2>&1`; never a panic
/// on a pipe nobody reads.
pub(crate) fn say(command: &str, message: impl fmt::Display) {
    let _ = writeln!(io::stderr(), "tinystore {command}: {message}");
}

/// Runs `command` on `args`: its help for `--help`, the line that says what
/// is wrong for a mistake, and what `run` makes of what was asked otherwise.
pub(crate) fn run(command: &'static Command, args: &[String], run: impl FnOnce(Asked) -> Done) -> ExitCode {
    match read(command, args) {
        Ok(Some(asked)) => exit(command.name, run(asked)),
        Ok(None) => exit(command.name, help::print_command(command)),
        Err(why) => end(command.name, Refused::Wrong(why)),
    }
}

/// The exit of a command that ended as `done` says.
pub(crate) fn exit(name: &str, done: Done) -> ExitCode {
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(refused) => end(name, refused),
    }
}

/// Says why a command ended and gives its exit: 2 for a command line that
/// was wrong, 1 for a failure, 0 for a reader gone. The line goes to stderr
/// as `writeln!` writes it, since `2>&1 | head` may have closed stderr too,
/// and `eprintln!` would panic.
pub(crate) fn end(name: &str, refused: Refused) -> ExitCode {
    let colours = Terminal::of_stderr().colours();
    let spelled = if name.is_empty() { "tinystore:".to_owned() } else { format!("tinystore {name}:") };
    let named = paint(colours, Color::Red.bold(), spelled);
    match refused {
        Refused::Wrong(why) => {
            let (help, does) = match name {
                "" | "help" => ("tinystore help".to_owned(), "lists the commands"),
                name => (format!("tinystore help {name}"), "shows how it is called"),
            };
            let hint = paint(colours, nu_ansi_term::Style::new().dimmed(), help);
            let _ = writeln!(io::stderr(), "{named} {why}\n  {hint} {does}");
            ExitCode::from(2)
        }
        Refused::Failed(why) => {
            let _ = writeln!(io::stderr(), "{named} {why}");
            ExitCode::FAILURE
        }
        Refused::ReaderGone => ExitCode::SUCCESS,
    }
}

/// What was asked, or none when it was the command's help.
fn read(command: &Command, args: &[String]) -> Result<Option<Asked>, String> {
    let mut asked = Asked::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--" => {
                asked.words.extend(args.by_ref().cloned());
                break;
            }
            "--dir" => asked.dir = Some(PathBuf::from(args.next().ok_or("--dir takes a directory")?)),
            flag if flag.starts_with('-') && flag.len() > 1 && flag.parse::<f64>().is_err() => {
                let (name, inline) = flag.split_once('=').map_or((flag, None), |(name, value)| (name, Some(value)));
                let Some(known) = command.flags.iter().find(|known| known.name == name) else {
                    let names: Vec<&str> = command.flags.iter().map(|known| known.name).chain(["--dir"]).collect();
                    return Err(format!("no flag {name}{}", crate::meant(name, &names)));
                };
                let value = match (known.value.is_empty(), inline) {
                    (true, None) => String::new(),
                    (true, Some(_)) => return Err(format!("{name} takes no value")),
                    (false, Some(value)) => value.to_owned(),
                    (false, None) => args.next().cloned().ok_or_else(|| format!("{name} takes {}", known.value))?,
                };
                asked.flags.push((known.name, value));
            }
            word => asked.words.push(word.to_owned()),
        }
    }
    Ok(Some(asked))
}

impl Asked {
    pub(crate) fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|(name, _)| *name == flag)
    }

    /// The flag's value, the last one given when it came twice.
    pub(crate) fn value(&self, flag: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(name, _)| *name == flag).map(|(_, value)| value.as_str())
    }

    /// Every value the flag was given, in order.
    pub(crate) fn values(&self, flag: &str) -> Vec<String> {
        self.flags.iter().filter(|(name, _)| *name == flag).map(|(_, value)| value.clone()).collect()
    }

    /// The directory of a command that reads: `--dir`, or its first word when
    /// it was given `wanted + 1` words, or the current directory; and the
    /// words after it.
    pub(crate) fn dir_or_here(&self, wanted: usize) -> Result<(PathBuf, Vec<String>), Refused> {
        match (&self.dir, self.words.len()) {
            (Some(dir), _) => Ok((dir.clone(), self.words.clone())),
            (None, count) if count == wanted + 1 => Ok((PathBuf::from(&self.words[0]), self.words[1..].to_vec())),
            (None, _) => Ok((PathBuf::from("."), self.words.clone())),
        }
    }

    /// The directory before a group's verb and the words after it, as
    /// `metrics ./data read cpu` gives them. A verb that only reads takes the
    /// current directory when none comes before it.
    pub(crate) fn placed(&self, group: &str, verb: &str, reads: bool) -> Result<(PathBuf, Vec<String>), Refused> {
        let at = self.words.iter().position(|word| word == verb).unwrap_or_default();
        let after = self.words.get(at + 1..).unwrap_or_default().to_vec();
        match (&self.dir, at) {
            (Some(_), 1) => Err(wrong("a directory given twice: --dir, and a word before the verb")),
            (Some(dir), 0) => Ok((dir.clone(), after)),
            (None, 1) => Ok((PathBuf::from(&self.words[0]), after)),
            (None, 0) if reads => Ok((PathBuf::from("."), after)),
            (None, 0) => Err(wrong(format!("a store's directory before the verb: tinystore {group} ./data {verb}"))),
            _ => Err(wrong(format!("one directory before {verb}, not {}", self.words[..at].join(" ")))),
        }
    }

    /// A count a flag gives, `otherwise` when it is not given.
    pub(crate) fn count(&self, flag: &str, otherwise: usize) -> Result<usize, Refused> {
        self.value(flag)
            .map_or(Ok(otherwise), |given| given.parse().map_err(|_| wrong(format!("{flag} {given}: a count"))))
    }

    /// The directory of a command that changes one: `--dir`, or its first word.
    pub(crate) fn dir_needed(&self, command: &str) -> Result<(PathBuf, Vec<String>), Refused> {
        match (&self.dir, self.words.split_first()) {
            (Some(dir), _) => Ok((dir.clone(), self.words.clone())),
            (None, Some((dir, rest))) => Ok((PathBuf::from(dir), rest.to_vec())),
            (None, None) => Err(wrong(format!("a store's directory: tinystore {command} <dir>"))),
        }
    }
}

/// JSON a command was given as a word, or on stdin for `-`, written again
/// on one line; `what` names it in the refusal of anything else.
pub(crate) fn json_given(given: &str, what: &str) -> Result<String, Refused> {
    let text = if given == "-" {
        if io::stdin().is_terminal() {
            return Err(wrong(format!("- reads {what} on stdin, and stdin is a terminal")));
        }
        let mut text = String::new();
        io::stdin().read_to_string(&mut text).map_err(|error| failed(format!("stdin: {error}")))?;
        text
    } else {
        given.to_owned()
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| wrong(format!("{what} is JSON: {error}")))?;
    Ok(value.to_string())
}

/// A span as `--since` and `--idle` take it: a number and its unit.
///
/// ```text
/// 30000ms → 30 s     30s → 30 s     5m → 300 s     2d → 172,800 s
/// ```
pub(crate) fn span(text: &str) -> Result<std::time::Duration, String> {
    let digits = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let number: u64 = number.parse().map_err(|_| format!("the span {text}: a number and a unit, as 30s"))?;
    let millis = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "" if number == 0 => 0,
        _ => return Err(format!("the span {text}: a unit of ms, s, m, h or d")),
    };
    Ok(std::time::Duration::from_millis(number.saturating_mul(millis)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn words_and_flags_are_read_against_the_commands_help() {
        let asked = read(&help::LOGS, &args("./data -f --level warn -n=5")).unwrap().unwrap();
        assert_eq!(asked.words, ["./data"]);
        assert!(asked.has("-f"));
        assert_eq!((asked.value("--level"), asked.value("-n")), (Some("warn"), Some("5")));
        assert_eq!(read(&help::LOGS, &args("--levle warn")).unwrap_err(), "no flag --levle; did you mean --level?");
        assert_eq!(read(&help::LOGS, &args("--level")).unwrap_err(), "--level takes <level>");
        assert_eq!(read(&help::LOGS, &args("-f=1")).unwrap_err(), "-f takes no value");
        assert!(read(&help::LOGS, &args("./data --help")).unwrap().is_none(), "help");
        let negative = read(&help::KV_GET, &args("get counts -1")).unwrap().unwrap();
        assert_eq!(negative.words, ["get", "counts", "-1"], "a negative number is a word");
    }

    #[test]
    fn a_reading_commands_directory_is_its_first_word_dir_or_the_current_one() {
        let asked = |text: &str| read(&help::STATUS, &args(text)).unwrap().unwrap();
        assert_eq!(asked("./data").dir_or_here(0).unwrap().0, PathBuf::from("./data"));
        assert_eq!(asked("").dir_or_here(0).unwrap().0, PathBuf::from("."));
        assert_eq!(asked("--dir d").dir_or_here(0).unwrap().0, PathBuf::from("d"));
        assert!(asked("").dir_needed("stop").is_err());
    }

    #[test]
    fn spans_read_as_people_write_them() {
        assert_eq!(span("30000ms").unwrap(), std::time::Duration::from_secs(30));
        assert_eq!(span("2d").unwrap(), std::time::Duration::from_secs(172_800));
        assert!(span("30").is_err(), "a unit");
    }
}
