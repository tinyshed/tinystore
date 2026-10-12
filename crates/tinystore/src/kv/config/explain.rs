//! Where each setting's value came from, and what was read and not used.

use std::fmt;

/// Every setting of a config with its value, the layer that gave it and the
/// values it went over, then what was read and not used, each with why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Explained {
    pub settings: Vec<Line>,
    /// An optional file that was absent, a variable with the prefix that
    /// names no setting, a kept change left out.
    pub notes: Vec<String>,
}

/// One setting's value and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
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
