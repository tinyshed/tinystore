//! A statement: its text, and the values its placeholders take.

use serde::Serialize;

use super::values::{Value, to_value};
use crate::{Error, Result};

/// A statement's text and the values of its `?`s, which are always values and
/// never SQL, so a value cannot inject any. [`sql!`](crate::sql!) makes one:
///
/// ```no_run
/// # fn main() -> tinystore::Result<()> {
/// # let db = tinystore::Store::open("data", Default::default())?.database("app").open()?;
/// # let id = "0192f2a4";
/// let title: Option<String> = db.one(tinystore::sql!("select title from notes where id = ?", id))?;
/// # Ok(())
/// # }
/// ```
///
/// Text alone is a statement without values: `db.scalar("select count(*) from notes")`.
#[derive(Clone, Debug)]
pub struct Sql {
    text: String,
    values: Vec<Value>,
    /// Why a value could not be one, told when the statement runs.
    refused: Option<String>,
    /// Whether it stands beside others in an and or an or without
    /// parentheses: one column's comparison.
    bare: bool,
}

impl Sql {
    pub fn new(text: impl Into<String>) -> Sql {
        Sql { text: text.into(), values: Vec::new(), refused: None, bare: false }
    }

    /// A piece the builder made, its values made already.
    pub(crate) fn piece(text: String, values: Vec<Value>, bare: bool) -> Sql {
        Sql { text, values, refused: None, bare }
    }

    /// A piece that could not be one, told when its statement runs.
    pub(crate) fn refused(why: String) -> Sql {
        Sql { text: String::new(), values: Vec::new(), refused: Some(why), bare: true }
    }

    /// The value of the next `?`. A value SQLite would change, a NaN or an
    /// integer past `i64`, makes the statement `invalid` when it runs.
    #[must_use]
    pub fn bind<T: Serialize + ?Sized>(mut self, value: &T) -> Sql {
        match to_value(value) {
            Ok(value) => self.values.push(value),
            Err(why) => {
                self.refused.get_or_insert(format!("value {}: {why}", self.values.len() + 1));
                self.values.push(Value::Null);
            }
        }
        self
    }

    /// A statement whose values arrived as SQLite keeps them, from the wire.
    pub(crate) fn with_values(text: String, values: Vec<Value>) -> Sql {
        Sql { text, values, refused: None, bare: false }
    }

    pub(crate) fn is_bare(&self) -> bool {
        self.bare
    }

    pub(crate) fn why_refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub(crate) fn into_parts(self) -> (String, Vec<Value>, Option<String>) {
        (self.text, self.values, self.refused)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn values(&self) -> &[Value] {
        &self.values
    }

    /// What the statement adds to a shared commit.
    pub(crate) fn weight(&self) -> usize {
        self.text.len() + self.values.iter().map(Value::size).sum::<usize>()
    }

    /// The statement as an error names it: its first words.
    pub(crate) fn describe(&self) -> String {
        let words: String = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let shown: String = words.chars().take(60).collect();
        if shown.len() < words.len() { format!("statement \"{shown}…\"") } else { format!("statement \"{shown}\"") }
    }

    /// Fails when a value could not be one, or when the statement's `?`s are
    /// not as many as its values: SQLite would read a missing one as `NULL`.
    pub(crate) fn check(&self, placeholders: usize) -> Result<()> {
        if let Some(why) = &self.refused {
            return Err(Error::invalid(why.clone()));
        }
        if placeholders != self.values.len() {
            return Err(Error::invalid(format!(
                "{placeholders} placeholders and {} values: each `?` takes one value",
                self.values.len()
            )));
        }
        Ok(())
    }
}

impl From<&str> for Sql {
    fn from(text: &str) -> Sql {
        Sql::new(text)
    }
}

impl From<String> for Sql {
    fn from(text: String) -> Sql {
        Sql::new(text)
    }
}

impl From<&String> for Sql {
    fn from(text: &String) -> Sql {
        Sql::new(text.as_str())
    }
}

/// A statement and the values of its `?`s, in order:
/// `sql!("select * from notes where author_id = ? and done = ?", user_id, false)`.
#[macro_export]
macro_rules! sql {
    ($text:expr $(, $value:expr)* $(,)?) => {
        $crate::sql::Sql::new($text)$(.bind(&$value))*
    };
}
