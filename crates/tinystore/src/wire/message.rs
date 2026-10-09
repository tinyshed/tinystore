//! A message is a map from field numbers to values. A request is read whole or
//! refused: a field this server does not know is `unimplemented`, since an
//! answer to a request read in part answers another question.

use super::msgpack::{self, Value};
use crate::{Error, ErrorKind};

/// A request's fields by their numbers.
#[derive(Debug)]
pub(crate) struct Fields {
    pairs: Vec<(u64, Value)>,
}

impl Fields {
    /// Decodes a request's body, refusing a field outside `known`.
    pub(crate) fn decode(body: &[u8], known: &[u64]) -> Result<Fields, Failure> {
        let value = msgpack::decode(body).map_err(|refused| Failure::invalid(refused.0))?;
        let Value::Map(pairs) = value else {
            return Err(Failure::invalid("a request that is not a map"));
        };
        let mut fields = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            let Value::Uint(number) = key else {
                return Err(Failure::invalid("a request's field named rather than numbered"));
            };
            if !known.contains(&number) {
                return Err(Failure::unimplemented(format!("field {number}")));
            }
            fields.push((number, value));
        }
        Ok(Fields { pairs: fields })
    }

    pub(crate) fn value(&self, number: u64) -> Option<&Value> {
        self.pairs.iter().find(|(key, _)| *key == number).map(|(_, value)| value)
    }

    pub(crate) fn uint(&self, number: u64, name: &str) -> Result<Option<u64>, Failure> {
        self.typed(number, name, "an unsigned integer", Value::as_uint)
    }

    pub(crate) fn int(&self, number: u64, name: &str) -> Result<Option<i64>, Failure> {
        self.typed(number, name, "an integer", Value::as_int)
    }

    pub(crate) fn str(&self, number: u64, name: &str) -> Result<Option<&str>, Failure> {
        self.typed(number, name, "a str", Value::as_str)
    }

    pub(crate) fn bin(&self, number: u64, name: &str) -> Result<Option<&[u8]>, Failure> {
        self.typed(number, name, "a bin", Value::as_bin)
    }

    pub(crate) fn bool(&self, number: u64, name: &str) -> Result<bool, Failure> {
        Ok(self.typed(number, name, "a bool", Value::as_bool)?.unwrap_or(false))
    }

    pub(crate) fn array(&self, number: u64, name: &str) -> Result<&[Value], Failure> {
        Ok(self.typed(number, name, "an array", Value::as_array)?.unwrap_or(&[]))
    }

    fn typed<'a, T>(
        &'a self,
        number: u64,
        name: &str,
        kind: &str,
        read: impl Fn(&'a Value) -> Option<T>,
    ) -> Result<Option<T>, Failure> {
        match self.value(number) {
            None => Ok(None),
            Some(value) => read(value).map(Some).ok_or_else(|| Failure::invalid(format!("{name} is not {kind}"))),
        }
    }
}

/// An answer's fields; a field at its zero value is left out, as the profile
/// asks of a sender.
#[derive(Debug, Default)]
pub(crate) struct Answer {
    pairs: Vec<(Value, Value)>,
}

impl Answer {
    pub(crate) fn put(mut self, number: u64, value: Value) -> Self {
        let zero = matches!(value, Value::Nil | Value::Bool(false));
        if !zero {
            self.pairs.push((Value::Uint(number), value));
        }
        self
    }

    pub(crate) fn encode(self) -> Vec<u8> {
        msgpack::encode(&Value::Map(self.pairs))
    }
}

/// An error as the wire carries it: `{1: code, 2: message, 3: what}`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Failure {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) what: Vec<(String, Value)>,
}

impl Failure {
    pub(crate) fn invalid(message: impl Into<String>) -> Failure {
        Failure { code: "invalid", message: message.into(), what: Vec::new() }
    }

    /// A method or a field this server does not have, naming it and the
    /// server's version, so the client knows which server to upgrade.
    pub(crate) fn unimplemented(missing: impl Into<String>) -> Failure {
        let missing = missing.into();
        let message = format!("{missing}: not in tinystore {}", env!("CARGO_PKG_VERSION"));
        Failure { code: "unimplemented", message, what: vec![("field".to_owned(), Value::Str(missing))] }
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut answer =
            Answer::default().put(1, Value::Str(self.code.to_owned())).put(2, Value::Str(self.message.clone()));
        if !self.what.is_empty() {
            let names = self.what.iter().map(|(name, value)| (Value::Str(name.clone()), value.clone())).collect();
            answer = answer.put(3, Value::Map(names));
        }
        answer.encode()
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Failure {
        Failure { code: code_of(error.kind()), message: error.to_string(), what: Vec::new() }
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
