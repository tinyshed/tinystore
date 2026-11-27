//! The time a table's delete and a mirror's gone write into a row: the
//! store's clock, the one a test's clock moves, in the unit the program keeps
//! its other times in. A schema whose times are seconds, SQLite's own
//! `unixepoch()`, marks in seconds; one that keeps milliseconds, as a mark did
//! before it had a unit, in milliseconds.

use std::time::SystemTime;

use crate::clock::unix_millis;

/// A column that marks a row with when it was marked, and the unit it keeps
/// that time in: `Mark::seconds("deleted_at")`. A column's name alone is a
/// mark in milliseconds, `"deleted_at".into()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub(crate) column: String,
    pub(crate) unit: Unit,
}

/// What a mark's number counts since 1970.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Unit {
    #[default]
    Millis,
    Seconds,
}

impl Mark {
    /// A column that keeps the time in unix milliseconds.
    pub fn millis(column: impl Into<String>) -> Mark {
        Mark { column: column.into(), unit: Unit::Millis }
    }

    /// A column that keeps the time in unix seconds, as `unixepoch()` does.
    pub fn seconds(column: impl Into<String>) -> Mark {
        Mark { column: column.into(), unit: Unit::Seconds }
    }

    pub fn column(&self) -> &str {
        &self.column
    }
}

impl From<&str> for Mark {
    fn from(column: &str) -> Mark {
        Mark::millis(column)
    }
}

impl From<String> for Mark {
    fn from(column: String) -> Mark {
        Mark::millis(column)
    }
}

impl From<&String> for Mark {
    fn from(column: &String) -> Mark {
        Mark::millis(column.as_str())
    }
}

impl Unit {
    /// `time` as a mark of this unit keeps it; a second is a whole one, the
    /// one the time is in, as `unixepoch()` gives it.
    ///
    /// ```text
    /// 1_795_790_506_171 ms → 1_795_790_506_171 as millis, 1_795_790_506 as seconds
    /// −1_500 ms            → −2 as seconds
    /// ```
    pub(crate) fn of(self, time: SystemTime) -> i64 {
        let millis = unix_millis(time);
        match self {
            Unit::Millis => millis,
            Unit::Seconds => millis.div_euclid(1000),
        }
    }

    /// The unit the wire names, `s` or `ms`; none for another word.
    #[cfg(feature = "wire")]
    pub(crate) fn named(name: &str) -> Option<Unit> {
        match name {
            "ms" => Some(Unit::Millis),
            "s" => Some(Unit::Seconds),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn a_second_is_the_whole_one_the_time_is_in() {
        let at = UNIX_EPOCH + Duration::from_millis(1_795_790_506_171);
        assert_eq!(Unit::Millis.of(at), 1_795_790_506_171);
        assert_eq!(Unit::Seconds.of(at), 1_795_790_506);
        assert_eq!(Unit::Seconds.of(UNIX_EPOCH - Duration::from_millis(1_500)), -2);
    }

    #[test]
    fn a_column_alone_marks_in_milliseconds() {
        assert_eq!(Mark::from("deleted_at"), Mark::millis("deleted_at"));
        assert_eq!(Mark::seconds("gone_at").unit, Unit::Seconds);
    }
}
