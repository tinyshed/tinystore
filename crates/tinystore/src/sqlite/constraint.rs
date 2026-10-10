//! The constraint a write broke, as SQLite says it: its extended code says
//! which kind, and its message names the table and the columns, or the
//! constraint itself.
//!
//! | Extended code | Message                                     | Facts                                  |
//! |---------------|---------------------------------------------|----------------------------------------|
//! | `UNIQUE`      | `UNIQUE constraint failed: users.email`     | unique, table users, columns email     |
//! | `UNIQUE`      | `UNIQUE constraint failed: index 'by_name'` | unique, name by_name                   |
//! | `PRIMARYKEY`  | `UNIQUE constraint failed: users.id`        | primaryKey, table users, columns id    |
//! | `NOTNULL`     | `NOT NULL constraint failed: users.email`   | notNull, table users, columns email    |
//! | `CHECK`       | `CHECK constraint failed: positive`         | check, name positive, or its text      |
//! | `FOREIGNKEY`  | `FOREIGN KEY constraint failed`             | foreignKey, which SQLite does not name |

use rusqlite::ffi;

use crate::Error;

/// Names `error` by the constraint `failure` broke, when it broke one.
pub(crate) fn named(error: Error, failure: &rusqlite::Error) -> Error {
    broken(failure).into_iter().fold(error, |error, (name, value)| error.naming(name, value))
}

/// The facts of the constraint `failure` broke: none for any other failure.
fn broken(failure: &rusqlite::Error) -> Vec<(&'static str, String)> {
    let rusqlite::Error::SqliteFailure(code, message) = failure else {
        return Vec::new();
    };
    let kind = match code.extended_code {
        ffi::SQLITE_CONSTRAINT_UNIQUE => "unique",
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY | ffi::SQLITE_CONSTRAINT_ROWID => "primaryKey",
        ffi::SQLITE_CONSTRAINT_NOTNULL => "notNull",
        ffi::SQLITE_CONSTRAINT_CHECK => "check",
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY => "foreignKey",
        _ => return Vec::new(),
    };
    let mut facts = vec![("constraint", kind.to_owned())];
    let said = message.as_deref().and_then(|message| message.split_once("constraint failed: ")).map(|(_, said)| said);
    match said {
        None => {}
        Some(said) if kind == "check" => facts.push(("name", said.to_owned())),
        Some(said) => match said.strip_prefix("index '").and_then(|index| index.strip_suffix('\'')) {
            Some(index) => facts.push(("name", index.to_owned())),
            None => facts.extend(columns(said)),
        },
    }
    facts
}

/// The table and the columns of `users.a, users.b`, each name's last dot
/// parting its table from its column.
fn columns(said: &str) -> Vec<(&'static str, String)> {
    let mut table = None;
    let mut columns = Vec::new();
    for qualified in said.split(", ") {
        match qualified.rsplit_once('.') {
            Some((of, column)) => {
                table.get_or_insert(of);
                columns.push(column);
            }
            None => columns.push(qualified),
        }
    }
    let mut facts: Vec<(&'static str, String)> = table.map(|table| ("table", table.to_owned())).into_iter().collect();
    facts.push(("columns", columns.join(", ")));
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(extended_code: i32, message: &str) -> rusqlite::Error {
        let code = rusqlite::ffi::Error { code: rusqlite::ErrorCode::ConstraintViolation, extended_code };
        rusqlite::Error::SqliteFailure(code, Some(message.to_owned()))
    }

    #[test]
    fn a_constraint_is_named_as_sqlite_says_it() {
        let said = |code, message| broken(&failure(code, message));
        let unique = said(ffi::SQLITE_CONSTRAINT_UNIQUE, "UNIQUE constraint failed: users.org, users.email");
        assert_eq!(
            unique,
            [("constraint", "unique".into()), ("table", "users".into()), ("columns", "org, email".into())]
        );
        let index = said(ffi::SQLITE_CONSTRAINT_UNIQUE, "UNIQUE constraint failed: index 'by_lower_email'");
        assert_eq!(index, [("constraint", "unique".into()), ("name", "by_lower_email".into())]);
        let check = said(ffi::SQLITE_CONSTRAINT_CHECK, "CHECK constraint failed: total > 0");
        assert_eq!(check, [("constraint", "check".into()), ("name", "total > 0".into())]);
        let foreign = said(ffi::SQLITE_CONSTRAINT_FOREIGNKEY, "FOREIGN KEY constraint failed");
        assert_eq!(foreign, [("constraint", "foreignKey".into())]);
        assert!(said(ffi::SQLITE_CONSTRAINT_TRIGGER, "the order is closed").is_empty(), "a trigger's own refusal");
    }
}
