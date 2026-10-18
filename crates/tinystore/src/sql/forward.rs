//! A write on a copy of a database, passed to the file's home: its
//! statements as their text and values, run there on the same database.
//!
//! ```text
//! name         the database's
//! u8 call      0 one statement, 1 a batch of them
//! statements   each its text, then its values: u8 0 null, 1 integer,
//!              2 real, 3 text, 4 blob, and the value
//! ```

use super::database::Done;
use super::statement::Sql;
use super::values::Value;
use crate::replication::{In, Out};
use crate::{Error, ErrorKind, Result, Store};

/// The file a database is kept in, as a node names it.
pub(crate) fn file_of(name: &str) -> String {
    format!("sql/{name}.db")
}

/// Passes statements to the home of the copy the database `name` is, as
/// one statement or as a batch, and gives what each did.
pub(crate) fn pass_on(store: &Store, name: &str, statements: &[Sql], batch: bool) -> Result<Vec<Done>> {
    if let Some(why) = statements.iter().find_map(Sql::why_refused) {
        return Err(Error::invalid(why.to_owned()));
    }
    let mut out = Out(vec![crate::replication::SQL]);
    out.text(name);
    out.byte(u8::from(batch));
    out.count(statements.len());
    for statement in statements {
        out.text(statement.text());
        out.count(statement.values().len());
        for value in statement.values() {
            value_out(&mut out, value);
        }
    }
    let answer = crate::replication::pass_on(store, &file_of(name), out.0)?;
    let mut at = In { bytes: &answer, at: 0 };
    (0..at.count()?).map(|_| Ok(Done { changes: at.long()?, last_insert_rowid: at.long()? as i64 })).collect()
}

/// Runs at the home statements another node passed on, and gives the
/// answer's bytes and the file they wrote.
pub(crate) fn run(store: &Store, bytes: &[u8]) -> Result<(Vec<u8>, String)> {
    let mut at = In { bytes, at: 0 };
    let name = at.text()?;
    let batch = at.byte()? == 1;
    let mut statements = Vec::new();
    for _ in 0..at.count()? {
        let text = at.text()?;
        let values = (0..at.count()?).map(|_| value_in(&mut at)).collect::<Result<Vec<_>>>()?;
        statements.push(Sql::with_values(text, values));
    }
    if !at.done() || (!batch && statements.len() != 1) {
        return Err(corrupt("its statements"));
    }
    let db = store.database(&name).open()?;
    let done = if batch { db.batch(statements)? } else { vec![db.exec(statements.remove(0))?] };
    let mut out = Out(Vec::new());
    out.list(&done, |out, one| {
        out.long(one.changes).long(one.last_insert_rowid as u64);
    });
    Ok((out.0, file_of(&name)))
}

fn value_out(out: &mut Out, value: &Value) {
    match value {
        Value::Null => out.byte(0),
        Value::Integer(n) => out.byte(1).long(*n as u64),
        Value::Real(x) => out.byte(2).long(x.to_bits()),
        Value::Text(text) => out.byte(3).text(text),
        Value::Blob(bytes) => out.byte(4).bytes(bytes),
    };
}

fn value_in(at: &mut In<'_>) -> Result<Value> {
    Ok(match at.byte()? {
        0 => Value::Null,
        1 => Value::Integer(at.long()? as i64),
        2 => Value::Real(f64::from_bits(at.long()?)),
        3 => Value::Text(at.text()?),
        4 => Value::Blob(at.bytes()?.to_vec()),
        _ => return Err(corrupt("a value")),
    })
}

fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a sql write passed on: {what}"))
}
