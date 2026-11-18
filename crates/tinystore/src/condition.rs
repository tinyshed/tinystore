//! The words a `where` of metrics and of records shares: a value that must be
//! none of some, and a text that starts with another. Each engine turns them
//! into its own condition, a label's text in metrics, a JSON value in records.

/// A value equal to none of these, or absent: `Not("dev")`, `Not(["dev",
/// "test"])`.
#[derive(Clone, Copy, Debug)]
pub struct Not<T>(pub T);

/// A text that starts with this one: `Prefix("api-")`.
#[derive(Clone, Copy, Debug)]
pub struct Prefix<T>(pub T);
