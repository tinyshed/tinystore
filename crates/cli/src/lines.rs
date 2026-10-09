//! How the server's log lines read: short and quietly coloured in a person's
//! terminal, whole and plain in a file or a pipe, one line an event.
//!
//! ```text
//! terminal  14:31:09 info  serving dir=/srv/data
//! file      2026-10-09T14:31:09.123Z info  serving dir=/srv/data
//! ```

use std::fmt::{self, Write as _};
use std::io::{self, IsTerminal};

use jiff::{Timestamp, Zoned};
use nu_ansi_term::{Color, Style};
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// Whether a terminal's lines are coloured, by the conventions every tool
/// keeps, and whether there is a terminal at all.
///
/// | `NO_COLOR` | `FORCE_COLOR` | `TERM=dumb` | a terminal | colours |
/// |------------|---------------|-------------|------------|---------|
/// | set        | any           | any         | any        | no      |
/// | unset      | set           | any         | any        | yes     |
/// | unset      | unset         | yes         | any        | no      |
/// | unset      | unset         | no          | yes        | yes     |
#[derive(Clone, Copy, Debug)]
pub(crate) struct Terminal {
    no_color: bool,
    force_color: bool,
    dumb: bool,
    /// Whether stderr is a terminal a person reads.
    pub(crate) terminal: bool,
}

impl Terminal {
    pub(crate) fn of_stderr() -> Terminal {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0");
        Terminal {
            no_color: set("NO_COLOR"),
            force_color: set("FORCE_COLOR") || set("CLICOLOR_FORCE"),
            dumb: std::env::var_os("TERM").is_some_and(|term| term == "dumb"),
            terminal: io::stderr().is_terminal(),
        }
    }

    pub(crate) fn colours(self) -> bool {
        !self.no_color && (self.force_color || (!self.dumb && self.terminal)) && ansi_shown()
    }
}

/// A Windows console shows ANSI colours once a program asks it to; elsewhere
/// a terminal does.
fn ansi_shown() -> bool {
    #[cfg(windows)]
    {
        nu_ansi_term::enable_ansi_support().is_ok()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// The least an event must be to be logged: `LOG_LEVEL`, info when unset.
pub(crate) fn level() -> LevelFilter {
    level_named(std::env::var("LOG_LEVEL").ok().as_deref())
}

/// A level by its name, info for none or one unknown: tracing's own parse
/// reads an empty name as error, which would hide every line but errors.
fn level_named(name: Option<&str>) -> LevelFilter {
    match name.map(str::trim) {
        Some(name) if !name.is_empty() => name.parse().unwrap_or(LevelFilter::INFO),
        _ => LevelFilter::INFO,
    }
}

/// A log line's format: a person's terminal's, or a file's.
pub(crate) struct Lines {
    pub(crate) colours: bool,
    /// Local time to the second, as a person reads a clock; a file's lines
    /// carry UTC to the millisecond.
    pub(crate) person: bool,
}

impl<S, N> FormatEvent<S, N> for Lines
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, _: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let level = *event.metadata().level();
        let time = if self.person { Zoned::now().strftime("%H:%M:%S").to_string() } else { utc(Timestamp::now()) };
        let mut line = String::new();
        let _ = write!(
            line,
            "{} {} {}",
            paint(self.colours, dim(), time),
            paint(self.colours, shade(level), name(level)),
            fields.message
        );
        for (key, value) in fields.named {
            let _ = write!(line, " {}{value}", paint(self.colours, dim(), format!("{key}=")));
        }
        writeln!(writer, "{line}")
    }
}

/// A file's time: UTC to the millisecond, every line as wide.
fn utc(at: Timestamp) -> String {
    format!("{}.{:03}Z", at.strftime("%Y-%m-%dT%H:%M:%S"), at.subsec_millisecond())
}

/// A level's word, padded so that messages line up.
fn name(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn ",
        Level::INFO => "info ",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

/// Colour where it means something: a warning and an error stand out, the
/// rest stays quiet.
fn shade(level: Level) -> Style {
    match level {
        Level::ERROR => Color::Red.bold(),
        Level::WARN => Color::Yellow.normal(),
        Level::INFO => Color::Cyan.normal(),
        Level::DEBUG | Level::TRACE => Color::Purple.dimmed(),
    }
}

fn dim() -> Style {
    Style::new().dimmed()
}

pub(crate) fn paint(colours: bool, style: Style, text: impl fmt::Display) -> String {
    if colours { style.paint(text.to_string()).to_string() } else { text.to_string() }
}

/// An event's message and its fields, each value as logfmt writes one:
/// quoted when it holds a space, a quote or an `=`.
#[derive(Default)]
struct Fields {
    message: String,
    named: Vec<(&'static str, String)>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        let spoken =
            if value.contains([' ', '"', '=']) || value.is_empty() { format!("{value:?}") } else { value.to_owned() };
        self.keep(field, spoken);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.keep(field, format!("{value:?}"));
    }
}

impl Fields {
    fn keep(&mut self, field: &Field, value: String) {
        match field.name() {
            "message" => self.message = value,
            name => self.named.push((name, value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn a_terminal_shows_colours_unless_someone_said_otherwise() {
        let terminal = |no_color, force_color, dumb, terminal| Terminal { no_color, force_color, dumb, terminal };
        assert!(terminal(false, false, false, true).colours());
        assert!(!terminal(false, false, false, false).colours(), "a pipe or a file");
        assert!(!terminal(true, true, false, true).colours(), "NO_COLOR wins");
        assert!(terminal(false, true, true, false).colours(), "FORCE_COLOR, through a pipe too");
        assert!(!terminal(false, false, true, true).colours(), "TERM=dumb");
    }

    #[test]
    fn a_files_line_is_whole_plain_and_quoted_only_where_it_must_be() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&written);
        let subscriber = tracing_subscriber::fmt()
            .event_format(Lines { colours: false, person: false })
            .with_writer(move || Sink(Arc::clone(&sink)))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(dir = r"C:\data", why = "a client said stop", "leaving");
        });
        let line = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        let (time, rest) = line.split_once(' ').unwrap();
        assert!(time.len() == 24 && time.ends_with('Z'), "{time}");
        assert_eq!(rest, "info  leaving dir=C:\\data why=\"a client said stop\"\n");
    }

    #[test]
    fn an_unset_or_unknown_level_logs_info() {
        assert_eq!(level_named(None), LevelFilter::INFO);
        assert_eq!(level_named(Some("")), LevelFilter::INFO, "not tracing's error");
        assert_eq!(level_named(Some("loud")), LevelFilter::INFO);
        assert_eq!(level_named(Some("DEBUG")), LevelFilter::DEBUG);
        assert_eq!(level_named(Some(" warn ")), LevelFilter::WARN);
    }

    #[test]
    fn a_files_time_is_utc_to_the_millisecond() {
        let at = Timestamp::from_millisecond(1_791_547_200_007).unwrap();
        assert_eq!(utc(at), "2026-10-09T12:00:00.007Z");
    }

    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
