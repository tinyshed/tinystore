//! What the environment says of a logger's lines, each variable over its own
//! option, as in Go, Bun and Python: `LOG_LEVEL`, `LOG_CONSOLE_LEVEL`,
//! `LOG_FORMAT` and `LOG_TIME`, after a prefix and its underscore when one is
//! given. A console the code turned off stays off, and a value none of them
//! can mean is ignored and said once.
//!
//! And whether a console shows colours: a terminal, without `NO_COLOR` or a
//! `TERM` of `dumb`; `FORCE_COLOR` other than 0 makes a pipe pretty and
//! coloured, and `NO_COLOR` still wins.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use super::Format;
use super::line::Time;

/// What a logger reads its variables through, and the prefix of their names.
#[derive(Clone)]
pub(crate) struct Environment {
    pub(crate) prefix: String,
    pub(crate) lookup: fn(&str) -> Option<String>,
}

impl Default for Environment {
    fn default() -> Environment {
        Environment { prefix: String::new(), lookup: super::layer::real }
    }
}

/// What the environment said, each when it said something it can mean.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Said {
    /// slog's number of the least level kept.
    pub(crate) level: Option<i32>,
    /// slog's number of the least level printed, `LOG_CONSOLE_LEVEL`'s.
    pub(crate) printed: Option<i32>,
    pub(crate) format: Option<Format>,
    pub(crate) time: Option<Time>,
    /// Each variable that said what none can mean, with what it said and
    /// what it may say.
    pub(crate) ignored: Vec<(String, String, &'static str)>,
}

impl Environment {
    fn name(&self, variable: &str) -> String {
        if self.prefix.is_empty() { variable.to_owned() } else { format!("{}_{variable}", self.prefix) }
    }

    fn value(&self, variable: &str) -> Option<String> {
        (self.lookup)(&self.name(variable)).map(|text| text.trim().to_owned()).filter(|text| !text.is_empty())
    }

    pub(crate) fn said(&self) -> Said {
        let mut said = Said::default();
        let mut read = |variable: &str, expected: &'static str, kind: fn(&str) -> Option<()>| -> Option<String> {
            let text = self.value(variable)?;
            if kind(&text.to_lowercase()).is_none() {
                said.ignored.push((self.name(variable), text, expected));
                return None;
            }
            Some(text.to_lowercase())
        };
        let a_level: fn(&str) -> Option<()> = |text| level(text).map(drop);
        let level = read("LOG_LEVEL", "trace, debug, info, warn or error", a_level);
        let printed = read("LOG_CONSOLE_LEVEL", "trace, debug, info, warn or error", a_level);
        let format = read("LOG_FORMAT", "pretty, json or off", |text| named(&FORMATS, text).map(drop));
        let time = read("LOG_TIME", "clock, full or off", |text| named(&TIMES, text).map(drop));
        said.level = level.as_deref().and_then(self::level);
        said.printed = printed.as_deref().and_then(self::level);
        said.format = format.as_deref().and_then(|text| named(&FORMATS, text));
        said.time = time.as_deref().and_then(|text| named(&TIMES, text));
        said
    }

    /// What the environment said as the config `log` reads it: the
    /// variables' prefix, `LOG` or `APP_LOG`, and each variable that said
    /// what a logger can mean, spelled as the config spells it; the format
    /// left out of a console the code turned off, which stays off, and
    /// `LOG_CONSOLE_LEVEL`, which is one process's and no setting of the config.
    #[cfg(all(feature = "records", feature = "config"))]
    pub(crate) fn as_config(&self, said: &Said, console_off: bool) -> (String, Vec<(String, String)>) {
        let format = said.format.filter(|_| !console_off);
        let spelled = [
            ("LOG_LEVEL", said.level.and_then(|level| spelled(&LEVELS, level))),
            ("LOG_FORMAT", format.and_then(|format| spelled(&FORMATS, format))),
            ("LOG_TIME", said.time.and_then(|time| spelled(&TIMES, time))),
        ];
        let variables = spelled.into_iter().filter_map(|(name, text)| Some((self.name(name), text?.to_owned())));
        (self.name("LOG"), variables.collect())
    }
}

/// The levels a logger takes, with slog's number of each: trace below
/// debug, as Python's levels map, and below trace `tracing` has none, so
/// that a logger at trace takes every line.
pub(crate) const LEVELS: [(&str, i32); 5] = [("trace", -8), ("debug", -4), ("info", 0), ("warn", 4), ("error", 8)];
pub(crate) const FORMATS: [(&str, Format); 3] =
    [("pretty", Format::Pretty), ("json", Format::Json), ("off", Format::Off)];
pub(crate) const TIMES: [(&str, Time); 3] = [("clock", Time::Clock), ("full", Time::Full), ("off", Time::Off)];

/// slog's number of a level's name, `warning` read as `warn`.
pub(crate) fn level(text: &str) -> Option<i32> {
    named(&LEVELS, if text == "warning" { "warn" } else { text })
}

fn named<T: Copy>(table: &[(&str, T)], text: &str) -> Option<T> {
    table.iter().find(|(name, _)| *name == text).map(|(_, value)| *value)
}

/// The name a table gives a value.
#[cfg(all(feature = "records", feature = "config"))]
pub(crate) fn spelled<T: PartialEq>(table: &[(&'static str, T)], value: T) -> Option<&'static str> {
    table.iter().find(|(_, named)| *named == value).map(|(name, _)| *name)
}

/// Whether an ignored variable's value has been said already, so that the
/// next logger says it no more.
pub(crate) fn first_said(name: &str, value: &str) -> bool {
    static SAID: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut said = SAID.lock().unwrap_or_else(PoisonError::into_inner);
    said.get_or_insert_with(HashSet::new).insert(format!("{name}={value}"))
}

/// Whether a console shows colours, `terminal` saying whether its writer is
/// one:
///
/// ```text
/// NO_COLOR=1       off
/// TERM=dumb        off
/// FORCE_COLOR=1    on, a pipe or a file too
/// a terminal       on
/// anything else    off
/// ```
pub(crate) fn colors(terminal: bool, lookup: fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| lookup(name).filter(|value| !value.is_empty());
    if set("NO_COLOR").is_some() || lookup("TERM").as_deref() == Some("dumb") {
        return false;
    }
    forced(lookup) || terminal
}

/// Whether `FORCE_COLOR` asks for colours: set, and not 0.
pub(crate) fn forced(lookup: fn(&str) -> Option<String>) -> bool {
    lookup("FORCE_COLOR").is_some_and(|value| !value.is_empty() && value != "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a logger reads a variable through.
    type Lookup = fn(&str) -> Option<String>;

    fn nothing(_: &str) -> Option<String> {
        None
    }

    fn no_color(name: &str) -> Option<String> {
        (name == "NO_COLOR").then(|| "1".to_owned())
    }

    fn dumb(name: &str) -> Option<String> {
        (name == "TERM").then(|| "dumb".to_owned())
    }

    fn forced(name: &str) -> Option<String> {
        (name == "FORCE_COLOR").then(|| "1".to_owned())
    }

    fn forced_off(name: &str) -> Option<String> {
        (name == "FORCE_COLOR").then(|| "0".to_owned())
    }

    fn forced_and_no_color(name: &str) -> Option<String> {
        matches!(name, "FORCE_COLOR" | "NO_COLOR").then(|| "1".to_owned())
    }

    #[test]
    fn colours_need_a_terminal_without_no_color_and_force_color_colours_a_pipe() {
        let cases: [(bool, Lookup, bool); 7] = [
            (true, nothing, true),
            (false, nothing, false),
            (true, no_color, false),
            (true, dumb, false),
            (false, forced, true),
            (false, forced_off, false),
            (false, forced_and_no_color, false),
        ];
        for (place, (terminal, lookup, coloured)) in cases.into_iter().enumerate() {
            assert_eq!(colors(terminal, lookup), coloured, "case {place}");
        }
    }

    fn bare_and_prefixed(name: &str) -> Option<String> {
        match name {
            "LOG_LEVEL" => Some("error".to_owned()),
            "LOG_CONSOLE_LEVEL" => Some("debug".to_owned()),
            "APP_LOG_FORMAT" => Some(" JSON ".to_owned()),
            "APP_LOG_CONSOLE_LEVEL" => Some("Warning".to_owned()),
            _ => None,
        }
    }

    // contract: LOG-05
    #[test]
    fn a_logger_that_named_its_prefix_reads_its_own_variables_and_not_the_bare_ones() {
        let app = Environment { prefix: "APP".to_owned(), lookup: bare_and_prefixed };
        assert_eq!(app.said(), Said { format: Some(Format::Json), printed: Some(4), ..Said::default() });
        let bare = Environment { prefix: String::new(), lookup: bare_and_prefixed };
        assert_eq!(bare.said(), Said { level: Some(8), printed: Some(-4), ..Said::default() });
    }

    fn console_loud(name: &str) -> Option<String> {
        (name == "LOG_CONSOLE_LEVEL").then(|| "loud".to_owned())
    }

    #[test]
    fn a_log_console_level_none_can_mean_is_ignored_as_a_log_level_is() {
        let said = Environment { prefix: String::new(), lookup: console_loud }.said();
        let ignored = ("LOG_CONSOLE_LEVEL".to_owned(), "loud".to_owned(), "trace, debug, info, warn or error");
        assert_eq!(said, Said { ignored: vec![ignored], ..Said::default() });
    }
}
