//! What the messages crates/protocol writes read their fields with and write
//! them with: a function a type of the schema, each the one place its rule
//! lives.

use std::collections::BTreeMap;

use super::message::Fields;
use super::msgpack::{self, Value};
use super::protocol::Failure;

/// A message of the schema: decoded whole or refused, and encoded with every
/// field at its zero value left out.
pub(crate) trait Message: Sized + Default {
    const NAME: &'static str;
    const KEYS: &'static [u64];

    fn read(fields: &Fields) -> Result<Self, Failure>;

    fn write(&self, out: &mut Out);

    fn decode(body: &[u8]) -> Result<Self, Failure> {
        Self::read(&Fields::decode(body, Self::NAME, Self::KEYS)?)
    }

    fn encode(&self) -> Vec<u8> {
        msgpack::encode(&message_value(self))
    }
}

/// A kv row as it travels: nothing, an integer or bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Row {
    #[default]
    Nil,
    Int(i64),
    Bin(Vec<u8>),
}

/// A value as SQLite keeps it, as it travels: nothing, an integer, a float,
/// text or bytes. A bool from a client is the integer SQLite keeps for it.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Cell {
    #[default]
    Nil,
    Int(i64),
    Float(f64),
    Str(String),
    Bin(Vec<u8>),
}

/// A message's fields as they are written.
#[derive(Debug, Default)]
pub(crate) struct Out {
    pairs: Vec<(Value, Value)>,
}

impl Out {
    /// Writes a field, unless it is at its zero value.
    pub(crate) fn put(&mut self, number: u64, value: Value) {
        if !is_zero(&value) {
            self.pairs.push((Value::Uint(number), value));
        }
    }

    /// Writes a field that was given, at its zero value too.
    pub(crate) fn given(&mut self, number: u64, value: Option<Value>) {
        if let Some(value) = value {
            self.pairs.push((Value::Uint(number), value));
        }
    }
}

fn is_zero(value: &Value) -> bool {
    match value {
        Value::Nil | Value::Bool(false) | Value::Uint(0) => true,
        Value::Float(float) => float.to_bits() == 0,
        Value::Str(text) => text.is_empty(),
        Value::Bin(bytes) => bytes.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Map(pairs) => pairs.is_empty(),
        _ => false,
    }
}

pub(crate) fn bool(value: &Value, name: &str) -> Result<bool, Failure> {
    value.as_bool().ok_or_else(|| not(name, "a bool"))
}

pub(crate) fn uint(value: &Value, name: &str) -> Result<u64, Failure> {
    value.as_uint().ok_or_else(|| not(name, "an unsigned integer"))
}

pub(crate) fn int(value: &Value, name: &str) -> Result<i64, Failure> {
    value.as_int().ok_or_else(|| not(name, "an integer"))
}

#[expect(dead_code, reason = "a float field reads with it, and no message has one yet")]
pub(crate) fn float(value: &Value, name: &str) -> Result<f64, Failure> {
    value.as_float().ok_or_else(|| not(name, "a float"))
}

pub(crate) fn str(value: &Value, name: &str) -> Result<String, Failure> {
    value.as_str().map(str::to_owned).ok_or_else(|| not(name, "a str"))
}

pub(crate) fn bin(value: &Value, name: &str) -> Result<Vec<u8>, Failure> {
    value.as_bin().map(<[u8]>::to_vec).ok_or_else(|| not(name, "a bin"))
}

/// A key's text: a str, a bin that is UTF-8, or an integer's decimal spelling.
pub(crate) fn key(value: &Value, name: &str) -> Result<String, Failure> {
    match value {
        Value::Str(text) => Ok(text.clone()),
        Value::Uint(number) => Ok(number.to_string()),
        Value::Int(number) => Ok(number.to_string()),
        Value::Bin(bytes) => String::from_utf8(bytes.clone())
            .map_err(|_| Failure::unimplemented(format!("{name} of bytes that are not UTF-8"))),
        _ => Err(not(name, "a str, a bin or an integer")),
    }
}

pub(crate) fn row(value: &Value, name: &str) -> Result<Row, Failure> {
    match value {
        Value::Nil => Ok(Row::Nil),
        Value::Uint(number) => {
            i64::try_from(*number).map(Row::Int).map_err(|_| Failure::invalid(format!("{name} past an int 64")))
        }
        Value::Int(number) => Ok(Row::Int(*number)),
        Value::Bin(bytes) => Ok(Row::Bin(bytes.clone())),
        _ => Err(not(name, "nil, an integer or a bin")),
    }
}

#[cfg(feature = "sql")]
pub(crate) fn cell(value: &Value, name: &str) -> Result<Cell, Failure> {
    match value {
        Value::Nil => Ok(Cell::Nil),
        Value::Bool(flag) => Ok(Cell::Int(i64::from(*flag))),
        Value::Uint(number) => {
            i64::try_from(*number).map(Cell::Int).map_err(|_| Failure::invalid(format!("{name} past an int 64")))
        }
        Value::Int(number) => Ok(Cell::Int(*number)),
        Value::Float(number) => Ok(Cell::Float(*number)),
        Value::Str(text) => Ok(Cell::Str(text.clone())),
        Value::Bin(bytes) => Ok(Cell::Bin(bytes.clone())),
        _ => Err(not(name, "nil, a bool, a number, a str or a bin")),
    }
}

pub(crate) fn list<T>(
    item: impl Fn(&Value, &str) -> Result<T, Failure>,
) -> impl Fn(&Value, &str) -> Result<Vec<T>, Failure> {
    move |value, name| {
        let items = value.as_array().ok_or_else(|| not(name, "an array"))?;
        items.iter().map(|each| item(each, name)).collect()
    }
}

pub(crate) fn names<T>(
    item: impl Fn(&Value, &str) -> Result<T, Failure>,
) -> impl Fn(&Value, &str) -> Result<BTreeMap<String, T>, Failure> {
    move |value, name| {
        let Value::Map(pairs) = value else {
            return Err(not(name, "a map of names"));
        };
        let mut named = BTreeMap::new();
        for (key, each) in pairs {
            let key = key.as_str().ok_or_else(|| not(name, "a map whose keys are names"))?;
            named.insert(key.to_owned(), item(each, name)?);
        }
        Ok(named)
    }
}

pub(crate) fn message<M: Message>(value: &Value, _name: &str) -> Result<M, Failure> {
    M::read(&Fields::from_value(value, M::NAME, M::KEYS)?)
}

pub(crate) fn bool_value(value: &bool) -> Value {
    Value::Bool(*value)
}

pub(crate) fn uint_value(value: &u64) -> Value {
    Value::Uint(*value)
}

/// An integer as the profile writes it: unsigned when it is not negative.
pub(crate) fn int_value(value: &i64) -> Value {
    u64::try_from(*value).map_or(Value::Int(*value), Value::Uint)
}

#[expect(dead_code, reason = "a float field writes with it, and no message has one yet")]
pub(crate) fn float_value(value: &f64) -> Value {
    Value::Float(*value)
}

pub(crate) fn str_value(text: &impl AsRef<str>) -> Value {
    Value::Str(text.as_ref().to_owned())
}

pub(crate) fn bin_value(bytes: &impl AsRef<[u8]>) -> Value {
    Value::Bin(bytes.as_ref().to_vec())
}

pub(crate) fn row_value(row: &Row) -> Value {
    match row {
        Row::Nil => Value::Nil,
        Row::Int(number) => int_value(number),
        Row::Bin(bytes) => Value::Bin(bytes.clone()),
    }
}

#[cfg(feature = "sql")]
pub(crate) fn cell_value(cell: &Cell) -> Value {
    match cell {
        Cell::Nil => Value::Nil,
        Cell::Int(number) => int_value(number),
        Cell::Float(number) => Value::Float(*number),
        Cell::Str(text) => Value::Str(text.clone()),
        Cell::Bin(bytes) => Value::Bin(bytes.clone()),
    }
}

pub(crate) fn list_value<T>(items: &[T], item: impl Fn(&T) -> Value) -> Value {
    Value::Array(items.iter().map(item).collect())
}

pub(crate) fn names_value<T>(named: &BTreeMap<String, T>, item: impl Fn(&T) -> Value) -> Value {
    Value::Map(named.iter().map(|(name, each)| (Value::Str(name.clone()), item(each))).collect())
}

pub(crate) fn message_value<M: Message>(message: &M) -> Value {
    let mut out = Out::default();
    message.write(&mut out);
    Value::Map(out.pairs)
}

fn not(name: &str, kind: &str) -> Failure {
    Failure::invalid(format!("{name} is not {kind}"))
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
