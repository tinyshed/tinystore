use std::error::Error as StdError;
use std::fmt;

/// What went wrong, with the same meaning in every engine, on the wire and in
/// every SDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    Invalid,
    NotFound,
    Conflict,
    /// A time before the engine's window: older than its retention, or behind
    /// what was already sealed.
    TooOld,
    /// A time past now plus the store's clock skew.
    TooNew,
    /// A bound was reached; the answer is refused rather than made smaller.
    Limit,
    Closed,
    Corrupt,
    /// Another process or engine holds what was asked for.
    InUse,
    Suspended,
    /// Busy for now; the same call may succeed later.
    Unavailable,
    Cancelled,
    /// A commit failed after the write ran: it may or may not be in the file,
    /// and the caller reconciles before retrying.
    OutcomeUnknown,
    Io,
    Internal,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NotFound => "not found",
            Self::Conflict => "conflict",
            Self::TooOld => "too old",
            Self::TooNew => "too new",
            Self::Limit => "limit",
            Self::Closed => "closed",
            Self::Corrupt => "corrupt",
            Self::InUse => "in use",
            Self::Suspended => "suspended",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
            Self::OutcomeUnknown => "outcome unknown",
            Self::Io => "io",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error that names what failed: `kv bucket sessions: key "a": not found`.
pub struct Error {
    kind: ErrorKind,
    what: String,
    source: Option<Box<dyn StdError + Send + Sync>>,
    /// What a program acts on without reading `what`, by name: the constraint
    /// a write broke, the bound a limit reached. The wire carries them by the
    /// same names, which every SDK reads.
    facts: Vec<(&'static str, String)>,
}

/// A constraint a write broke, as SQLite names it: its kind always, and the
/// table and the columns, or the constraint's own name, when SQLite says them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constraint {
    pub kind: ConstraintKind,
    pub table: Option<String>,
    pub columns: Vec<String>,
    /// A check's name, or its text when it has none; the index of a unique
    /// key on an expression.
    pub name: Option<String>,
}

/// A unique or a primary key already held is `conflict`; any other is
/// `invalid`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ConstraintKind {
    Unique,
    PrimaryKey,
    ForeignKey,
    Check,
    NotNull,
}

impl ConstraintKind {
    /// The kind as the wire and every SDK say it: `primaryKey`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unique => "unique",
            Self::PrimaryKey => "primaryKey",
            Self::ForeignKey => "foreignKey",
            Self::Check => "check",
            Self::NotNull => "notNull",
        }
    }

    fn named(name: &str) -> Option<Self> {
        [Self::Unique, Self::PrimaryKey, Self::ForeignKey, Self::Check, Self::NotNull]
            .into_iter()
            .find(|kind| kind.as_str() == name)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn new(kind: ErrorKind, what: impl Into<String>) -> Self {
        Self { kind, what: what.into(), source: None, facts: Vec::new() }
    }

    pub fn invalid(what: impl Into<String>) -> Self {
        Self::new(ErrorKind::Invalid, what)
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, what)
    }

    pub fn closed(what: impl Into<String>) -> Self {
        Self::new(ErrorKind::Closed, what)
    }

    pub fn limit(what: impl Into<String>) -> Self {
        Self::new(ErrorKind::Limit, what)
    }

    pub fn corrupt(what: impl Into<String>) -> Self {
        Self::new(ErrorKind::Corrupt, what)
    }

    pub fn internal(what: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, what)
    }

    pub fn io(what: impl Into<String>, source: std::io::Error) -> Self {
        Self::new(ErrorKind::Io, what).with_source(source)
    }

    pub fn with_source(mut self, source: impl Into<Box<dyn StdError + Send + Sync>>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// The same error, said of what contains it: `kv bucket sessions` around
    /// `key "a"` reads `kv bucket sessions: key "a"`.
    pub fn within(mut self, outer: impl fmt::Display) -> Self {
        self.what = if self.what.is_empty() { outer.to_string() } else { format!("{outer}: {}", self.what) };
        self
    }

    /// A copy for each of several callers one failure answers, its cause kept
    /// as text.
    pub fn duplicate(&self) -> Self {
        let mut copy = Self::new(self.kind, self.what.clone());
        if let Some(source) = &self.source {
            copy.source = Some(source.to_string().into());
        }
        copy.facts.clone_from(&self.facts);
        copy
    }

    /// The same error with one fact more, which a program reads by its name.
    pub(crate) fn naming(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.facts.push((name, value.into()));
        self
    }

    /// A fact the error carries by name: a limit's `limit`, `wanted` and
    /// `bound`; a broken constraint's `constraint`, `table`, `columns` and
    /// `name`, which [`Error::constraint`] reads as one.
    pub fn fact(&self, name: &str) -> Option<&str> {
        self.facts.iter().find(|(named, _)| *named == name).map(|(_, value)| value.as_str())
    }

    pub(crate) fn facts(&self) -> &[(&'static str, String)] {
        &self.facts
    }

    /// The constraint a write broke, as SQLite names it:
    ///
    /// ```no_run
    /// # use tinystore::{ConstraintKind, sql};
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// # let db = store.database("app").open()?;
    /// match db.exec(sql!("insert into users (email) values (?)", "ann@example.com")) {
    ///     Err(error) if error.constraint().is_some_and(|broken| broken.columns == ["email"]) => {
    ///         println!("that email is taken")
    ///     }
    ///     done => drop(done?),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn constraint(&self) -> Option<Constraint> {
        let kind = ConstraintKind::named(self.fact("constraint")?)?;
        let columns = self.fact("columns").map(|columns| columns.split(", ").map(str::to_owned).collect());
        Some(Constraint {
            kind,
            table: self.fact("table").map(str::to_owned),
            columns: columns.unwrap_or_default(),
            name: self.fact("name").map(str::to_owned),
        })
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn what(&self) -> &str {
        &self.what
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.what.is_empty() {
            write!(f, "{}: ", self.what)?;
        }
        f.write_str(self.kind.as_str())?;
        if let Some(source) = &self.source {
            write!(f, ": {source}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref().map(|source| source as &(dyn StdError + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_reads_from_the_outside_in() {
        let error = Error::not_found(r#"key "a""#).within("kv bucket sessions");
        assert_eq!(error.to_string(), r#"kv bucket sessions: key "a": not found"#);
        assert_eq!(error.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn an_error_says_its_cause_last() {
        let cause = std::io::Error::other("permission denied");
        let error = Error::io("store /data: LOCK", cause);
        assert_eq!(error.to_string(), "store /data: LOCK: io: permission denied");
        assert!(error.source().is_some());
    }
}
