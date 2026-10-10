//! What a statement the store was given, rather than wrote, may not do.
//!
//! A connection is shared: the writer by every write of a commit, a reader by
//! whoever reads next. A statement that changed its connection would change
//! it for them, so SQLite's authorizer, which sees every statement as it is
//! compiled, refuses a given one that does:
//!
//! | The statement                              | What it would do                                 |
//! |--------------------------------------------|--------------------------------------------------|
//! | `attach`, `detach`                         | open another file, which stays on the connection |
//! | `begin`, `commit`, `rollback`, a savepoint | end the transaction the writes beside it share   |
//! | a `pragma` that sets                       | change a setting every connection opens with     |
//!
//! A pragma that only reads runs: one without a value, and those of [`READS`].
//!
//! The authorizer sees a statement once, when it is compiled, and a compiled
//! statement is kept by its text. The adapter's own `commit`, once kept, would
//! then run for a given statement of the same text, unseen. So the adapter
//! writes its own with [`own!`], and a given statement that starts as they do
//! is refused before it is looked up.

use std::cell::Cell;

use rusqlite::Connection;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

use crate::{Error, Result};

/// A statement of the adapter's own that a given one may not be: it begins or
/// ends a transaction, or sets a pragma.
macro_rules! own {
    ($sql:literal) => {
        concat!("/* tinystore */ ", $sql)
    };
}
pub(super) use own;

/// What every statement written with [`own!`] starts with.
pub(super) const OWN: &str = own!("");

/// The pragmas that only read whatever their argument is, a table's name or
/// how many rows to answer.
const READS: [&str; 10] = [
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "foreign_key_check",
    "integrity_check",
    "quick_check",
];

const ATTACHES: &str = "a statement attaches no file: its connection is shared, and the file would stay on it";
const ENDS: &str = "a statement neither begins nor ends a transaction: the one it runs in is shared with other writes";
const SETS: &str = "a statement sets no pragma: its connection is shared, and the setting would stay on it";
const UNNAMED: &str = "a statement does what SQLite gives no name to, as an attach of a file that is not a literal";
const AS_OWN: &str = "a statement starts with no `/* tinystore */`, which the store's own statements do";

thread_local! {
    /// Whether this thread is running a given statement.
    static GIVEN: Cell<bool> = const { Cell::new(false) };
    /// Why the authorizer refused the given statement this thread runs.
    static REFUSED: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Has `connection` refuse what a given statement may not do.
pub(super) fn guard(connection: &Connection) -> rusqlite::Result<()> {
    connection.authorizer(Some(authorize))
}

/// Runs `work`, which compiles and runs `text`, a statement the store was
/// given: `Invalid`, saying what it may not do, when the authorizer refused it.
pub(crate) fn given<T>(text: &str, work: impl FnOnce() -> Result<T>) -> Result<T> {
    // rusqlite keeps a statement by its text trimmed
    if text.trim_start().starts_with(OWN) {
        return Err(Error::invalid(AS_OWN));
    }
    let _running = Running::enter();
    let done = work();
    match REFUSED.take() {
        Some(why) if done.is_err() => Err(Error::invalid(why)),
        _ => done,
    }
}

fn authorize(context: AuthContext<'_>) -> Authorization {
    if !GIVEN.get() {
        return Authorization::Allow;
    }
    let Some(why) = refused(&context.action) else {
        return Authorization::Allow;
    };
    REFUSED.set(Some(why));
    Authorization::Deny
}

fn refused(action: &AuthAction<'_>) -> Option<&'static str> {
    match action {
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => Some(ATTACHES),
        AuthAction::Transaction { .. } | AuthAction::Savepoint { .. } => Some(ENDS),
        AuthAction::Pragma { pragma_name, pragma_value } => {
            let reads = pragma_value.is_none() || READS.iter().any(|read| pragma_name.eq_ignore_ascii_case(read));
            (!reads).then_some(SETS)
        }
        // SQLite names no file for an attach whose file is not a literal,
        // and rusqlite reads an action without its name as unknown.
        AuthAction::Unknown { .. } => Some(UNNAMED),
        _ => None,
    }
}

/// This thread running a given statement, until it is dropped.
struct Running {
    before: bool,
}

impl Running {
    fn enter() -> Running {
        REFUSED.set(None);
        Running { before: GIVEN.replace(true) }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        GIVEN.set(self.before);
    }
}
