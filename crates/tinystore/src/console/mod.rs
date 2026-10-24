//! The console: a logger's lines as they are printed, pretty for a person at
//! a terminal and one JSON object a line for a collector, the same bytes the
//! Go, Bun and Python loggers print. It needs no store and links no SQLite;
//! a store's logger prints the same lines and keeps them too.
//!
//! ```text
//! 11:02:11.123 WARN  api  slow request  ms=1200 password=[redacted]
//! {"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request","ms":1200,"password":"[redacted]"}
//! ```

/// The options every logger takes, on the console alone and a store's: each
/// sets what `Told` holds. Each builder has a `level` of its own, since a
/// store's logger keeps and prints apart.
macro_rules! told_options {
    () => {
        /// How lines print: pretty on a terminal and JSON otherwise without
        /// it, and `LOG_FORMAT` wins over it unless it is `Off`.
        pub fn console(mut self, format: $crate::console::Format) -> Self {
            self.told.format = Some(format);
            self
        }

        /// A pretty line's time; `LOG_TIME` wins over it.
        pub fn time(mut self, time: $crate::console::Time) -> Self {
            self.told.time = time;
            self
        }

        /// Leaves the stream out of a pretty line.
        pub fn hide_stream(mut self) -> Self {
            self.told.hide_stream = true;
            self
        }

        /// Prints to `out` instead of standard error.
        pub fn to(mut self, out: impl std::io::Write + Send + 'static) -> Self {
            self.told.to = Some(Box::new(out));
            self
        }

        /// Hides the values of fields whose keys name a secret, at any depth,
        /// where they print and where they are kept: `SECRETS` is the usual
        /// names. A message's text is left as it is.
        pub fn redact<S: AsRef<str>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
            self.told.redact.extend(names.into_iter().map(|name| name.as_ref().to_owned()));
            self
        }

        /// Leaves the password of a URL inside a value as it is.
        pub fn keep_url_passwords(mut self) -> Self {
            self.told.keep_url_passwords = true;
            self
        }

        /// Changes each field before it is hidden: given its key and its
        /// JSON, the JSON to keep, text that is no JSON kept as a string, or
        /// none to drop the field.
        pub fn replace(mut self, replace: impl Fn(&str, &str) -> Option<String> + Send + Sync + 'static) -> Self {
            self.told.replace = Some(std::sync::Arc::new(replace));
            self
        }

        /// Adds where each line was logged: `source={"function":…,"file":…,"line":42}`.
        pub fn source(mut self) -> Self {
            self.told.source = true;
            self
        }

        /// Reads `PREFIX_LOG_LEVEL`, `PREFIX_LOG_CONSOLE_LEVEL`,
        /// `PREFIX_LOG_FORMAT` and `PREFIX_LOG_TIME` in place of `LOG_LEVEL`
        /// and the rest.
        pub fn env(mut self, prefix: &str) -> Self {
            self.told.env = Some($crate::console::Environment { prefix: prefix.to_owned(), ..Default::default() });
            self
        }

        /// Reads no variable: what the code says is what the logger does.
        pub fn no_env(mut self) -> Self {
            self.told.env = None;
            self
        }
    };
}
#[allow(unused_imports, reason = "a store's logger takes them; the console alone has none")]
pub(crate) use told_options;

mod env;
#[cfg(test)]
pub(crate) mod fixture;
mod layer;
mod line;
mod redact;

pub use layer::{Layer, Replace};
pub use line::Time;
pub use redact::SECRETS;

pub(crate) use env::Environment;
#[cfg(all(feature = "records", feature = "config"))]
pub(crate) use env::{FORMATS, LEVELS, TIMES};
#[cfg(all(test, feature = "records", feature = "config"))]
pub(crate) use layer::Declared;
#[allow(unused_imports, reason = "a store's logger takes them; the console alone has none")]
pub(crate) use layer::{Console, Keep, Told, own_line};
#[allow(unused_imports, reason = "a store's logger takes them; the console alone has none")]
pub(crate) use line::{Line, object_pairs, quoted};

/// A logger of the console alone, its lines printed and kept nowhere: the
/// same calls, the same options but a store's, and the same lines as a
/// store's logger.
///
/// ```no_run
/// use tracing_subscriber::layer::SubscriberExt;
/// use tracing_subscriber::util::SubscriberInitExt;
///
/// let console = tinystore::console::logger("app").redact(tinystore::console::SECRETS).layer();
/// tracing_subscriber::registry().with(console).init();
/// tracing::info!(user = 42, "signed in");
/// ```
pub fn logger(stream: &str) -> LoggerBuilder {
    LoggerBuilder { stream: stream.to_owned(), told: Told::default() }
}

/// What a logger of the console alone is told before its layer is made.
pub struct LoggerBuilder {
    stream: String,
    told: Told,
}

impl std::fmt::Debug for LoggerBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggerBuilder").field("stream", &self.stream).finish()
    }
}

impl LoggerBuilder {
    /// The least level printed, every level without it. `LOG_LEVEL` wins
    /// over it, and `LOG_CONSOLE_LEVEL` over both, since this logger only
    /// prints.
    pub fn level(mut self, level: tracing::Level) -> Self {
        self.told.level = Some(level);
        self
    }

    told_options!();

    /// The layer that prints what `tracing` hands it.
    pub fn layer(self) -> Layer {
        Layer::new(std::sync::Arc::new(Console::new(&self.stream, self.told)), None)
    }
}

/// How a console prints a line. Without one, lines are pretty on a terminal
/// and JSON otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// `11:02:11.123 INFO  api  started  port=3000`
    Pretty,
    /// One object a line, for a collector.
    Json,
    /// Nothing printed: a store's logger still keeps each line.
    Off,
}

#[cfg(test)]
#[path = "vectors_tests.rs"]
mod tests;
