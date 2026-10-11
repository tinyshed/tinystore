//! A statement's rows by their columns, for a caller that reads numbers by
//! the hundred thousand: a chart's points, a report's measures.
//!
//! A column of INTEGERs alone or of REALs alone is packed, eight bytes a row,
//! and any other keeps its values as SQLite gave them. The rows are read
//! straight into their columns: a number never becomes a row's value first,
//! which is four times its eight bytes. SQLite types a value and not a column,
//! so a column is told by what it holds:
//!
//! | The column holds            | It is                                     |
//! |-----------------------------|-------------------------------------------|
//! | INTEGERs, NULLs among them  | `Integers`                                |
//! | REALs, NULLs among them     | `Reals`                                   |
//! | INTEGERs and REALs together | `Values`: neither is read as the other    |
//! | a text or a blob            | `Values`                                  |
//! | NULLs alone, or no row      | what its declaration says: `cpu real` is  |
//! |                             | `Reals`, and an expression, which         |
//! |                             | declares nothing, `Values`                |

use rusqlite::Statement;

use super::rows::{Answer, Held, step};
use super::values::Value;
use crate::Result;

/// What a column holds: said by its values, or by its declaration when it
/// holds none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Holds {
    Integers,
    Reals,
    /// Text, blobs, values of several types, or nothing known.
    Any,
}

impl Holds {
    /// What a column declared as `declaration` holds, by the affinity SQLite
    /// reads from it (<https://sqlite.org/datatype3.html>, 3.1): `INT`
    /// anywhere in it is integers, a text or a blob anything, and what is
    /// left, `real` and `numeric` among it, reals. A strict table's `any`
    /// holds anything, and so does an expression, which declares nothing.
    pub(crate) fn declared(declaration: Option<&str>) -> Holds {
        let Some(declaration) = declaration else {
            return Holds::Any;
        };
        let says = |word: &str| {
            declaration.as_bytes().windows(word.len()).any(|part| part.eq_ignore_ascii_case(word.as_bytes()))
        };
        if says("int") {
            return Holds::Integers;
        }
        let anything = declaration.is_empty() || declaration.eq_ignore_ascii_case("any");
        if anything || ["char", "clob", "text", "blob"].into_iter().any(says) { Holds::Any } else { Holds::Reals }
    }
}

/// A statement's rows by their columns, in the statement's order, each
/// `rows` long, and the store's memory they hold.
#[derive(Debug)]
pub(crate) struct Columns {
    pub(crate) rows: usize,
    pub(crate) columns: Vec<(String, Column)>,
    held: Held,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Column {
    /// A NULL among them is 0, and marked.
    Integers {
        values: Vec<i64>,
        nulls: Nulls,
    },
    /// A NULL among them is a NaN, which SQLite keeps none of, and marked.
    Reals {
        values: Vec<f64>,
        nulls: Nulls,
    },
    Values(Vec<Value>),
}

/// The rows of a packed column that hold NULL: empty when none does, else a
/// mark a row.
pub(crate) type Nulls = Vec<bool>;

impl Columns {
    /// The store's memory the columns hold, for whoever keeps them after the
    /// call: a download until its last part is sent.
    pub(crate) fn held(&mut self) -> Held {
        std::mem::take(&mut self.held)
    }
}

impl Answer for Columns {
    fn read(
        statement: &mut Statement<'_>,
        values: &[Value],
        bounds: (usize, usize),
        mut held: Held,
    ) -> Result<Columns> {
        let mut taking: Vec<Taking> = (0..statement.column_count()).map(|_| Taking::Nulls(0)).collect();
        let stepped = step(statement, values, bounds, &mut held, |column, value| taking[column].push(value))?;
        let columns = taking.into_iter().zip(stepped.declared).map(|(taking, declared)| taking.end(declared));
        Ok(Columns { rows: stepped.rows, columns: stepped.columns.into_iter().zip(columns).collect(), held })
    }

    fn len(&self) -> usize {
        self.rows
    }

    fn width(&self) -> usize {
        self.columns.len()
    }
}

/// A column as its values come, a row after another: packed for as long as
/// they are of one type.
enum Taking {
    /// So many NULLs, and no value yet to tell the column by.
    Nulls(usize),
    Integers {
        values: Vec<i64>,
        nulls: Nulls,
    },
    Reals {
        values: Vec<f64>,
        nulls: Nulls,
    },
    Values(Vec<Value>),
}

impl Taking {
    fn push(&mut self, value: Value) {
        match (&mut *self, value) {
            (Taking::Nulls(count), Value::Null) => *count += 1,
            (Taking::Integers { values, .. }, Value::Integer(integer)) => values.push(integer),
            (Taking::Reals { values, .. }, Value::Real(real)) => values.push(real),
            (Taking::Integers { values, nulls }, Value::Null) => {
                mark(nulls, values.len());
                values.push(0);
            }
            (Taking::Reals { values, nulls }, Value::Null) => {
                mark(nulls, values.len());
                values.push(f64::NAN);
            }
            (Taking::Values(values), value) => values.push(value),
            // the first value after NULLs alone says what the column is
            (Taking::Nulls(count), value) => {
                *self = Taking::after_nulls(*count, &value);
                self.push(value);
            }
            // a value of another type: the column keeps its values as they came
            (_, value) => {
                let mut values = std::mem::replace(self, Taking::Nulls(0)).into_values();
                values.push(value);
                *self = Taking::Values(values);
            }
        }
    }

    /// A column of `count` NULLs that `value` follows.
    fn after_nulls(count: usize, value: &Value) -> Taking {
        match value {
            Value::Integer(_) => Taking::Integers { values: vec![0; count], nulls: vec![true; count] },
            Value::Real(_) => Taking::Reals { values: vec![f64::NAN; count], nulls: vec![true; count] },
            _ => Taking::Values(vec![Value::Null; count]),
        }
    }

    /// The column's values as SQLite gave them, a row after another.
    fn into_values(self) -> Vec<Value> {
        let null = |nulls: &Nulls, row: usize| nulls.get(row).copied().unwrap_or(false);
        let value = |(row, value): (usize, Value), nulls: &Nulls| if null(nulls, row) { Value::Null } else { value };
        match self {
            Taking::Nulls(count) => vec![Value::Null; count],
            Taking::Integers { values, nulls } => {
                values.into_iter().map(Value::Integer).enumerate().map(|at| value(at, &nulls)).collect()
            }
            Taking::Reals { values, nulls } => {
                values.into_iter().map(Value::Real).enumerate().map(|at| value(at, &nulls)).collect()
            }
            Taking::Values(values) => values,
        }
    }

    /// The column its rows made: a packed column's marks reach its last row,
    /// and one of NULLs alone is what it was `declared` to hold.
    fn end(self, declared: Holds) -> Column {
        let marked = |mut nulls: Nulls, rows: usize| {
            if !nulls.is_empty() {
                nulls.resize(rows, false);
            }
            nulls
        };
        match (self, declared) {
            (Taking::Nulls(rows), Holds::Integers) => {
                Column::Integers { values: vec![0; rows], nulls: vec![true; rows] }
            }
            (Taking::Nulls(rows), Holds::Reals) => {
                Column::Reals { values: vec![f64::NAN; rows], nulls: vec![true; rows] }
            }
            (Taking::Nulls(rows), Holds::Any) => Column::Values(vec![Value::Null; rows]),
            (Taking::Integers { values, nulls }, _) => {
                let nulls = marked(nulls, values.len());
                Column::Integers { values, nulls }
            }
            (Taking::Reals { values, nulls }, _) => {
                let nulls = marked(nulls, values.len());
                Column::Reals { values, nulls }
            }
            (Taking::Values(values), _) => Column::Values(values),
        }
    }
}

/// Marks `row` as holding NULL, and the rows before it since the last as not.
fn mark(nulls: &mut Nulls, row: usize) {
    nulls.resize(row, false);
    nulls.push(true);
}

#[cfg(test)]
#[path = "columns_tests.rs"]
mod tests;
