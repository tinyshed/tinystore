//! A message is a map from field numbers to values. A request is read whole or
//! refused: a field this server does not know is `unimplemented`, since an
//! answer to a request read in part answers another question. A refusal is a
//! `Failure`, the message a stream that failed ends with.

use super::msgpack::{self, Value};
use super::protocol::Failure;
use crate::{Error, ErrorKind};

/// A request's fields by their numbers.
#[derive(Debug)]
pub(crate) struct Fields {
    pairs: Vec<(u64, Value)>,
}

impl Fields {
    /// Decodes a body of the message `name`, refusing a field outside `known`.
    pub(crate) fn decode(body: &[u8], name: &str, known: &[u64]) -> Result<Fields, Failure> {
        let value = msgpack::decode(body).map_err(|refused| Failure::invalid(refused.0))?;
        Fields::from_value(&value, name, known)
    }

    /// A message's fields from its map, a nested message's too.
    pub(crate) fn from_value(value: &Value, name: &str, known: &[u64]) -> Result<Fields, Failure> {
        let Value::Map(pairs) = value else {
            return Err(Failure::invalid(format!("a {name} that is not a map")));
        };
        let mut fields = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            let Value::Uint(number) = key else {
                return Err(Failure::invalid(format!("a {name} with a field named rather than numbered")));
            };
            if !known.contains(number) {
                return Err(Failure::unimplemented(format!("field {number} of {name}")));
            }
            fields.push((*number, value.clone()));
        }
        Ok(Fields { pairs: fields })
    }

    /// A field read by `read`, or `None` when the message leaves it out.
    pub(crate) fn get<T>(
        &self,
        number: u64,
        name: &str,
        read: impl Fn(&Value, &str) -> Result<T, Failure>,
    ) -> Result<Option<T>, Failure> {
        let value = self.pairs.iter().find(|(key, _)| *key == number).map(|(_, value)| value);
        value.map(|value| read(value, name)).transpose()
    }
}

impl Failure {
    pub(crate) fn invalid(message: impl Into<String>) -> Failure {
        Failure { code: "invalid".to_owned(), message: message.into(), what: None }
    }

    /// A method or a field this server does not have, naming it and the
    /// server's version, so the client knows which server to upgrade.
    pub(crate) fn unimplemented(missing: impl Into<String>) -> Failure {
        let missing = missing.into();
        let message = format!("{missing}: not in tinystore {}", env!("CARGO_PKG_VERSION"));
        Failure { code: "unimplemented".to_owned(), message, what: None }.naming("field", missing)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Failure {
        Failure { code: "internal".to_owned(), message: message.into(), what: None }
    }

    pub(crate) fn cancelled(message: impl Into<String>) -> Failure {
        Failure { code: "cancelled".to_owned(), message: message.into(), what: None }
    }

    /// The same failure with one more name of the item it is about.
    pub(crate) fn naming(mut self, name: &str, value: impl Into<String>) -> Failure {
        self.what.get_or_insert_default().insert(name.to_owned(), value.into());
        self
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Failure {
        Failure { code: code_of(error.kind()).to_owned(), message: error.to_string(), what: None }
    }
}

/// An error kind's code, the same in every SDK's error classes.
fn code_of(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Invalid => "invalid",
        ErrorKind::NotFound => "not_found",
        ErrorKind::Conflict => "conflict",
        ErrorKind::TooOld => "too_old",
        ErrorKind::TooNew => "too_new",
        ErrorKind::Limit => "limit",
        ErrorKind::Closed => "closed",
        ErrorKind::Corrupt => "corrupt",
        ErrorKind::InUse => "in_use",
        ErrorKind::Suspended => "suspended",
        ErrorKind::Unavailable => "unavailable",
        ErrorKind::Cancelled => "cancelled",
        ErrorKind::OutcomeUnknown => "outcome_unknown",
        _ => "internal",
    }
}
