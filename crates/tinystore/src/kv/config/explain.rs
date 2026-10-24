//! Where each setting's value came from, and what was read and not used.

use std::fmt;
use std::time::SystemTime;

use crate::clock::unix_millis;

/// Every setting of a config with its value, the layer that gave it and the
/// values it went over, then what was read and not used, each with why.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Explained {
    pub settings: Vec<Line>,
    /// An optional file that was absent, a variable with the prefix that
    /// names no setting, a kept change left out.
    pub notes: Vec<String>,
}

/// One setting's value and where it came from, as text and as data.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Line {
    /// The setting's path: `limits.rps`.
    pub path: String,
    /// Its value as JSON, a span spelled as `30s`, a secret as `***`.
    pub value: String,
    /// The layer that gave it: `APP_LIMITS_RPS`, `app.yaml`, `the
    /// declaration`, `a change, version 7, 2026-10-13 14:03:05 UTC`.
    pub from: String,
    /// The values it went over, newest first: `8000 from app.yaml`.
    pub over: Vec<String>,
    /// Every value the setting was given, newest first: its own, then the
    /// ones it went over. `from` is the first one's `from` as text, and each
    /// of `over` is one of the rest as `{value} from {from}`.
    pub given: Vec<Given>,
}

/// A value a setting was given, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Given {
    /// As JSON, a span spelled as `30s`, a secret as `***`.
    pub value: String,
    pub from: Origin,
}

/// Where a value came from. Its text is what a line's `from` says.
///
/// ```text
/// Declaration                                          the declaration
/// Layer { place: 2 }                                   layer 2
/// File { path: "app.yaml" }                            app.yaml
/// Env { variable: "APP_PORT" }                         APP_PORT
/// Env { variable: "APP_PORT", dotenv: ".env" }         APP_PORT in .env
/// Env { variable: "DB_URL_FILE", file: "/run/db" }     DB_URL_FILE, /run/db
/// Change { version: 7, at }                            a change, version 7, 2026-10-13 14:03:05 UTC
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Origin {
    /// The default the declaration gives.
    Declaration,
    /// An object in an SDK's list of layers, by its place in the list, the
    /// declaration 1.
    #[non_exhaustive]
    Layer { place: usize },
    /// A file, by its path as the program wrote it.
    #[non_exhaustive]
    File { path: String },
    /// A variable of the environment. `dotenv` is the `.env` file that set
    /// it, and `file` the file a `NAME_FILE` variable named: at most one of
    /// the two, and neither for the process's own variable.
    #[non_exhaustive]
    Env { variable: String, dotenv: Option<String>, file: Option<String> },
    /// A change made while the program ran: the config's version it made,
    /// and when.
    #[non_exhaustive]
    Change { version: i64, at: SystemTime },
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Declaration => f.write_str("the declaration"),
            Origin::Layer { place } => write!(f, "layer {place}"),
            Origin::File { path } => f.write_str(path),
            Origin::Env { variable, dotenv: Some(dotenv), .. } => write!(f, "{variable} in {dotenv}"),
            Origin::Env { variable, file: Some(file), .. } => write!(f, "{variable}, {file}"),
            Origin::Env { variable, .. } => f.write_str(variable),
            Origin::Change { version, at } => write!(f, "a change, version {version}, {}", utc(*at)),
        }
    }
}

/// A time in UTC to the second: `2026-10-13 14:03:05 UTC`.
fn utc(at: SystemTime) -> String {
    let millis = unix_millis(at);
    let (days, seconds) = (millis.div_euclid(86_400_000), millis.rem_euclid(86_400_000) / 1000);
    let (year, month, day) = crate::clock::civil_from_days(days);
    let (hour, minute, second) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// The settings as a table, the notes after it:
///
/// ```text
/// port        9000  PORT                                     over 8000 from app.yaml, 8080 from the declaration
/// limits.rps  200   a change, version 7, 2026-10-13 14:03:05 UTC  over 100 from the declaration
/// ```
impl fmt::Display for Explained {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = |column: fn(&Line) -> &str| self.settings.iter().map(|line| column(line).len()).max().unwrap_or(0);
        let (path, value, from) = (width(|line| &line.path), width(|line| &line.value), width(|line| &line.from));
        for line in &self.settings {
            let over = if line.over.is_empty() { String::new() } else { format!("  over {}", line.over.join(", ")) };
            let row = format!("{:path$}  {:value$}  {:from$}{over}", line.path, line.value, line.from);
            writeln!(f, "{}", row.trim_end())?;
        }
        for note in &self.notes {
            writeln!(f, "{note}")?;
        }
        Ok(())
    }
}
