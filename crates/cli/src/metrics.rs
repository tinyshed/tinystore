//! `tinystore metrics`: a store's series from a shell. `ingest` writes the
//! text an exporter serves, read whole from stdin, through the server that
//! holds the directory or a sidecar it starts: every series of the text, or
//! none.
//!
//! ```text
//! curl -s localhost:9100/metrics | tinystore metrics ./data ingest --label instance=web-1
//! tinystore metrics ./data ingest --keep 30d < scrape.txt
//! ```

use std::io::{IsTerminal as _, Read as _};
use std::process::ExitCode;
use std::time::SystemTime;

use tinystore::metrics::Labels;

use crate::asked::{self, Asked, Done, Refused, failed, wrong};
use crate::help::{self, Command};
use crate::reach;

/// A verb of `metrics`: its word, its help and what runs it.
struct Verb {
    word: &'static str,
    command: &'static Command,
    run: fn(Asked) -> Done,
}

const VERBS: [Verb; 1] = [Verb { word: "ingest", command: &help::METRICS_INGEST, run: ingest }];

pub(crate) fn run(args: &[String]) -> ExitCode {
    let Some((at, verb)) = args.iter().enumerate().find_map(|(at, arg)| Some((at, verb_named(arg)?))) else {
        return given_no_verb(args);
    };
    let rest: Vec<String> = [&args[..at], &args[at + 1..]].concat();
    asked::run(verb.command, &rest, verb.run)
}

fn verb_named(word: &str) -> Option<&'static Verb> {
    VERBS.iter().find(|verb| verb.word == word)
}

/// `metrics` alone or with `--help`: every metrics command's help; with a
/// verb it does not know, that verb's nearest.
fn given_no_verb(args: &[String]) -> ExitCode {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return asked::exit("metrics", help::print_group("metrics"));
    }
    let verbs: Vec<&str> = VERBS.iter().map(|verb| verb.word).collect();
    let why = match args.iter().filter(|arg| !arg.starts_with('-')).nth(1) {
        Some(word) => format!("no command metrics {word}{}", crate::meant(word, &verbs)),
        None => format!("a verb after the directory: {}", verbs.join(", ")),
    };
    asked::end("metrics", wrong(why))
}

/// Writes the text on stdin, read to its end: every series of it or none.
fn ingest(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_needed("metrics <dir> ingest")?;
    if let Some(word) = rest.first() {
        return Err(wrong(format!("{word}: ingest reads Prometheus text on stdin and takes no words")));
    }
    let labels = labels(&asked.values("--label"))?;
    let keep = asked.value("--keep").map(asked::span).transpose().map_err(wrong)?;
    if std::io::stdin().is_terminal() {
        return Err(wrong(
            "it reads Prometheus text on stdin, and stdin is a terminal\n  curl -s localhost:9100/metrics | \
             tinystore metrics ./data ingest",
        ));
    }
    let mut text = Vec::new();
    std::io::stdin().read_to_end(&mut text).map_err(|error| failed(format!("stdin: {error}")))?;
    let samples = samples_in(&text);
    // the time of a sample the text gives none, the same if it is sent again
    let at = SystemTime::now();
    reach::write(&dir, "metrics ingest", move |client| client.ingest_prometheus(&text, &labels, at, keep))?;
    let plural = if samples == 1 { "" } else { "s" };
    outln!("wrote {samples} sample{plural} to {}", dir.display())
}

/// `--label`'s pairs, `instance=web-1`.
fn labels(given: &[String]) -> Result<Labels, Refused> {
    given
        .iter()
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) if !name.is_empty() => Ok((name.to_owned(), value.to_owned())),
            _ => Err(wrong(format!("--label {pair}: a name and its value, as instance=web-1"))),
        })
        .collect()
}

/// A Prometheus text's samples: a line each that is neither empty nor a
/// comment.
fn samples_in(text: &[u8]) -> usize {
    text.split(|byte| *byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_texts_samples_are_its_lines_that_are_no_comment() {
        let text = b"# HELP rtt round trip\n# TYPE rtt gauge\nrtt{target=\"poland\"} 23.1\n\n  rtt 1 1793014320000\r\n";
        assert_eq!(samples_in(text), 2);
    }

    #[test]
    fn a_label_is_a_name_and_its_value() {
        let given = labels(&["instance=web-1".to_owned(), "empty=".to_owned()]).unwrap();
        assert_eq!(given["instance"], "web-1");
        assert_eq!(given["empty"], "");
        assert!(labels(&["=web-1".to_owned()]).is_err());
        assert!(labels(&["web-1".to_owned()]).is_err());
    }
}
