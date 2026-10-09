//! A value as SQLite keeps it, and how a program's value becomes one.
//!
//! | The program's value                       | Kept as                         |
//! |-------------------------------------------|---------------------------------|
//! | `bool`                                    | `INTEGER`, `0` or `1`           |
//! | integers within `i64`                     | `INTEGER`                       |
//! | `f32`, `f64` but NaN                      | `REAL`                          |
//! | `str`, `char`, a unit enum variant        | `TEXT`                          |
//! | bytes, a `Vec<u8>` that holds some        | `BLOB`                          |
//! | `SystemTime`                              | `INTEGER`, unix milliseconds    |
//! | `None`, `()`                              | `NULL`                          |
//! | anything else: lists, maps, structs       | `TEXT`, its JSON                |

use std::fmt;

use rusqlite::ToSql;
use rusqlite::types::{ToSqlOutput, ValueRef};
use serde::Serialize;
use serde::ser::{self, Impossible};

/// One value as SQLite keeps it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub(crate) fn of(value: ValueRef<'_>) -> Value {
        match value {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => Value::Integer(n),
            ValueRef::Real(x) => Value::Real(x),
            ValueRef::Text(text) => Value::Text(String::from_utf8_lossy(text).into_owned()),
            ValueRef::Blob(bytes) => Value::Blob(bytes.to_vec()),
        }
    }

    /// What it holds of memory, for the bounds on what a call reads.
    pub(crate) fn size(&self) -> usize {
        match self {
            Value::Null | Value::Integer(_) | Value::Real(_) => 8,
            Value::Text(text) => text.len(),
            Value::Blob(bytes) => bytes.len(),
        }
    }
}

impl ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(match self {
            Value::Null => ValueRef::Null,
            Value::Integer(n) => ValueRef::Integer(*n),
            Value::Real(x) => ValueRef::Real(*x),
            Value::Text(text) => ValueRef::Text(text.as_bytes()),
            Value::Blob(bytes) => ValueRef::Blob(bytes),
        }))
    }
}

/// The value SQLite keeps for `value`, or why it keeps none: a NaN, which a
/// `REAL` column would keep as `NULL`, or an integer past `i64`.
pub(crate) fn to_value<T: Serialize + ?Sized>(value: &T) -> Result<Value, String> {
    match value.serialize(Scalar) {
        Ok(value) => Ok(value),
        Err(Refusal::Compound) => match value.serialize(Bytes) {
            Ok(bytes) if !bytes.is_empty() => Ok(Value::Blob(bytes)),
            _ => serde_json::to_string(value).map(Value::Text).map_err(|error| format!("its JSON: {error}")),
        },
        Err(Refusal::Refused(why)) => Err(why),
    }
}

/// Why a value is not one SQLite keeps as it is: it is a list, a map or a
/// struct, which goes as JSON or bytes, or it cannot go at all.
#[derive(Debug)]
enum Refusal {
    Compound,
    Refused(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Compound => f.write_str("a list, a map or a struct"),
            Refusal::Refused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Refusal {}

impl ser::Error for Refusal {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Refusal::Refused(message.to_string())
    }
}

/// A value that is one of SQLite's own: a number, text, bytes or nothing.
struct Scalar;

fn integer<N: TryInto<i64> + fmt::Display + Copy>(n: N) -> Result<Value, Refusal> {
    n.try_into()
        .map(Value::Integer)
        .map_err(|_| Refusal::Refused(format!("{n}, past the 64-bit integers SQLite keeps")))
}

fn real(x: f64) -> Result<Value, Refusal> {
    if x.is_nan() {
        return Err(Refusal::Refused("NaN, which SQLite keeps as NULL".to_owned()));
    }
    Ok(Value::Real(x))
}

impl ser::Serializer for Scalar {
    type Ok = Value;
    type Error = Refusal;
    type SerializeSeq = Impossible<Value, Refusal>;
    type SerializeTuple = Impossible<Value, Refusal>;
    type SerializeTupleStruct = Impossible<Value, Refusal>;
    type SerializeTupleVariant = Impossible<Value, Refusal>;
    type SerializeMap = Impossible<Value, Refusal>;
    type SerializeStruct = Time;
    type SerializeStructVariant = Impossible<Value, Refusal>;

    fn serialize_bool(self, v: bool) -> Result<Value, Refusal> {
        Ok(Value::Integer(i64::from(v)))
    }

    fn serialize_i8(self, v: i8) -> Result<Value, Refusal> {
        Ok(Value::Integer(i64::from(v)))
    }

    fn serialize_i16(self, v: i16) -> Result<Value, Refusal> {
        Ok(Value::Integer(i64::from(v)))
    }

    fn serialize_i32(self, v: i32) -> Result<Value, Refusal> {
        Ok(Value::Integer(i64::from(v)))
    }

    fn serialize_i64(self, v: i64) -> Result<Value, Refusal> {
        Ok(Value::Integer(v))
    }

    fn serialize_i128(self, v: i128) -> Result<Value, Refusal> {
        integer(v)
    }

    fn serialize_u8(self, v: u8) -> Result<Value, Refusal> {
        Ok(Value::Integer(i64::from(v)))
    }

    fn serialize_u16(self, v: u16) -> Result<Value, Refusal> {
        Ok(Value::Integer(i64::from(v)))
    }

    fn serialize_u32(self, v: u32) -> Result<Value, Refusal> {
        Ok(Value::Integer(i64::from(v)))
    }

    fn serialize_u64(self, v: u64) -> Result<Value, Refusal> {
        integer(v)
    }

    fn serialize_u128(self, v: u128) -> Result<Value, Refusal> {
        integer(v)
    }

    fn serialize_f32(self, v: f32) -> Result<Value, Refusal> {
        real(f64::from(v))
    }

    fn serialize_f64(self, v: f64) -> Result<Value, Refusal> {
        real(v)
    }

    fn serialize_char(self, v: char) -> Result<Value, Refusal> {
        Ok(Value::Text(v.to_string()))
    }

    fn serialize_str(self, v: &str) -> Result<Value, Refusal> {
        Ok(Value::Text(v.to_owned()))
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<Value, Refusal> {
        Ok(Value::Blob(v.to_vec()))
    }

    fn serialize_none(self) -> Result<Value, Refusal> {
        Ok(Value::Null)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Value, Refusal> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Value, Refusal> {
        Ok(Value::Null)
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<Value, Refusal> {
        Ok(Value::Null)
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, variant: &'static str) -> Result<Value, Refusal> {
        Ok(Value::Text(variant.to_owned()))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> Result<Value, Refusal> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<Value, Refusal> {
        Err(Refusal::Compound)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Refusal> {
        Err(Refusal::Compound)
    }

    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Refusal> {
        Err(Refusal::Compound)
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeTupleStruct, Refusal> {
        Err(Refusal::Compound)
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Refusal> {
        Err(Refusal::Compound)
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Refusal> {
        Err(Refusal::Compound)
    }

    /// `SystemTime` is the one struct kept as a number: serde writes it as its
    /// seconds and nanoseconds since the epoch, and SQLite keeps milliseconds.
    fn serialize_struct(self, name: &'static str, _: usize) -> Result<Time, Refusal> {
        if name == "SystemTime" {
            return Ok(Time::default());
        }
        Err(Refusal::Compound)
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Refusal> {
        Err(Refusal::Compound)
    }
}

/// A `SystemTime` as serde writes it, made unix milliseconds.
#[derive(Default)]
struct Time {
    secs: u64,
    nanos: u64,
}

impl ser::SerializeStruct for Time {
    type Ok = Value;
    type Error = Refusal;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Refusal> {
        let n = value.serialize(Unsigned)?;
        match key {
            "secs_since_epoch" => self.secs = n,
            _ => self.nanos = n,
        }
        Ok(())
    }

    fn end(self) -> Result<Value, Refusal> {
        let millis = u128::from(self.secs) * 1000 + u128::from(self.nanos) / 1_000_000;
        integer(millis)
    }
}

/// Serializer methods that refuse what a probe does not take: each probe below
/// takes one shape of value and leaves the rest to the next.
macro_rules! refuse {
    ($($method:ident)*) => { $(refuse!(@ $method);)* };
    (@ bool) => { fn serialize_bool(self, _: bool) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ i8) => { fn serialize_i8(self, _: i8) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ i16) => { fn serialize_i16(self, _: i16) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ i32) => { fn serialize_i32(self, _: i32) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ i64) => { fn serialize_i64(self, _: i64) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ u8) => { fn serialize_u8(self, _: u8) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ u16) => { fn serialize_u16(self, _: u16) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ u32) => { fn serialize_u32(self, _: u32) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ u64) => { fn serialize_u64(self, _: u64) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ f32) => { fn serialize_f32(self, _: f32) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ f64) => { fn serialize_f64(self, _: f64) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ char) => { fn serialize_char(self, _: char) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ str) => { fn serialize_str(self, _: &str) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ bytes) => { fn serialize_bytes(self, _: &[u8]) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ none) => { fn serialize_none(self) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ some) => {
        fn serialize_some<T: Serialize + ?Sized>(self, _: &T) -> Result<Self::Ok, Refusal> {
            Err(Refusal::Compound)
        }
    };
    (@ unit) => { fn serialize_unit(self) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) } };
    (@ unit_struct) => {
        fn serialize_unit_struct(self, _: &'static str) -> Result<Self::Ok, Refusal> { Err(Refusal::Compound) }
    };
    (@ unit_variant) => {
        fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<Self::Ok, Refusal> {
            Err(Refusal::Compound)
        }
    };
    (@ newtype_struct) => {
        fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, _: &T) -> Result<Self::Ok, Refusal> {
            Err(Refusal::Compound)
        }
    };
    (@ newtype_variant) => {
        fn serialize_newtype_variant<T: Serialize + ?Sized>(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: &T,
        ) -> Result<Self::Ok, Refusal> {
            Err(Refusal::Compound)
        }
    };
    (@ seq) => {
        fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Refusal> { Err(Refusal::Compound) }
    };
    (@ tuple) => {
        fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Refusal> { Err(Refusal::Compound) }
    };
    (@ tuple_struct) => {
        fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeTupleStruct, Refusal> {
            Err(Refusal::Compound)
        }
    };
    (@ tuple_variant) => {
        fn serialize_tuple_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeTupleVariant, Refusal> {
            Err(Refusal::Compound)
        }
    };
    (@ map) => {
        fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Refusal> { Err(Refusal::Compound) }
    };
    (@ struct) => {
        fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeStruct, Refusal> {
            Err(Refusal::Compound)
        }
    };
    (@ struct_variant) => {
        fn serialize_struct_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeStructVariant, Refusal> {
            Err(Refusal::Compound)
        }
    };
}

/// The unsigned number a `SystemTime` writes for each of its fields.
struct Unsigned;

impl ser::Serializer for Unsigned {
    type Ok = u64;
    type Error = Refusal;
    type SerializeSeq = Impossible<u64, Refusal>;
    type SerializeTuple = Impossible<u64, Refusal>;
    type SerializeTupleStruct = Impossible<u64, Refusal>;
    type SerializeTupleVariant = Impossible<u64, Refusal>;
    type SerializeMap = Impossible<u64, Refusal>;
    type SerializeStruct = Impossible<u64, Refusal>;
    type SerializeStructVariant = Impossible<u64, Refusal>;

    fn serialize_u32(self, v: u32) -> Result<u64, Refusal> {
        Ok(u64::from(v))
    }

    fn serialize_u64(self, v: u64) -> Result<u64, Refusal> {
        Ok(v)
    }

    refuse! {
        bool i8 i16 i32 i64 u8 u16 f32 f64 char str bytes none some unit unit_struct unit_variant
        newtype_struct newtype_variant seq tuple tuple_struct tuple_variant map struct struct_variant
    }
}

/// A value that is bytes: written as bytes, or a list of `u8`, which in Rust
/// is a `Vec<u8>` and nothing else.
struct Bytes;

impl ser::Serializer for Bytes {
    type Ok = Vec<u8>;
    type Error = Refusal;
    type SerializeSeq = ByteList;
    type SerializeTuple = Impossible<Vec<u8>, Refusal>;
    type SerializeTupleStruct = Impossible<Vec<u8>, Refusal>;
    type SerializeTupleVariant = Impossible<Vec<u8>, Refusal>;
    type SerializeMap = Impossible<Vec<u8>, Refusal>;
    type SerializeStruct = Impossible<Vec<u8>, Refusal>;
    type SerializeStructVariant = Impossible<Vec<u8>, Refusal>;

    fn serialize_bytes(self, v: &[u8]) -> Result<Vec<u8>, Refusal> {
        Ok(v.to_vec())
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<ByteList, Refusal> {
        Ok(ByteList(Vec::with_capacity(len.unwrap_or(0))))
    }

    refuse! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str none some unit unit_struct unit_variant
        newtype_struct newtype_variant tuple tuple_struct tuple_variant map struct struct_variant
    }
}

struct ByteList(Vec<u8>);

impl ser::SerializeSeq for ByteList {
    type Ok = Vec<u8>;
    type Error = Refusal;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Refusal> {
        self.0.push(value.serialize(Byte)?);
        Ok(())
    }

    fn end(self) -> Result<Vec<u8>, Refusal> {
        Ok(self.0)
    }
}

/// An element of a byte list: a `u8`, as serde writes nothing else with it.
struct Byte;

impl ser::Serializer for Byte {
    type Ok = u8;
    type Error = Refusal;
    type SerializeSeq = Impossible<u8, Refusal>;
    type SerializeTuple = Impossible<u8, Refusal>;
    type SerializeTupleStruct = Impossible<u8, Refusal>;
    type SerializeTupleVariant = Impossible<u8, Refusal>;
    type SerializeMap = Impossible<u8, Refusal>;
    type SerializeStruct = Impossible<u8, Refusal>;
    type SerializeStructVariant = Impossible<u8, Refusal>;

    fn serialize_u8(self, v: u8) -> Result<u8, Refusal> {
        Ok(v)
    }

    refuse! {
        bool i8 i16 i32 i64 u16 u32 u64 f32 f64 char str bytes none some unit unit_struct unit_variant
        newtype_struct newtype_variant seq tuple tuple_struct tuple_variant map struct struct_variant
    }
}

/// A row as an insert writes it: each field of a struct, or each entry of a
/// map, its column's name and its value, in their order.
pub(crate) fn row_of<T: Serialize + ?Sized>(row: &T) -> Result<Vec<(String, Value)>, String> {
    row.serialize(RowWriter).map_err(|refusal| refusal.to_string())
}

/// What writes a row: a struct or a map, nothing else.
struct RowWriter;

/// A row's fields as they come.
#[derive(Default)]
struct Fields {
    columns: Vec<(String, Value)>,
    key: Option<String>,
}

fn not_a_row() -> Refusal {
    Refusal::Refused("a row is a struct or a map of its columns".to_owned())
}

impl ser::Serializer for RowWriter {
    type Ok = Vec<(String, Value)>;
    type Error = Refusal;
    type SerializeSeq = Impossible<Self::Ok, Refusal>;
    type SerializeTuple = Impossible<Self::Ok, Refusal>;
    type SerializeTupleStruct = Impossible<Self::Ok, Refusal>;
    type SerializeTupleVariant = Impossible<Self::Ok, Refusal>;
    type SerializeMap = Fields;
    type SerializeStruct = Fields;
    type SerializeStructVariant = Impossible<Self::Ok, Refusal>;

    fn serialize_map(self, _: Option<usize>) -> Result<Fields, Refusal> {
        Ok(Fields::default())
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Fields, Refusal> {
        Ok(Fields::default())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Self::Ok, Refusal> {
        value.serialize(self)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> Result<Self::Ok, Refusal> {
        value.serialize(self)
    }

    fn serialize_bool(self, _: bool) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_i8(self, _: i8) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_i16(self, _: i16) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_i32(self, _: i32) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_i64(self, _: i64) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_u8(self, _: u8) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_u16(self, _: u16) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_u32(self, _: u32) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_u64(self, _: u64) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_f32(self, _: f32) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_f64(self, _: f64) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_char(self, _: char) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_str(self, _: &str) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_bytes(self, _: &[u8]) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_none(self) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_unit(self) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<Self::Ok, Refusal> {
        Err(not_a_row())
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Refusal> {
        Err(not_a_row())
    }

    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Refusal> {
        Err(not_a_row())
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeTupleStruct, Refusal> {
        Err(not_a_row())
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Refusal> {
        Err(not_a_row())
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Refusal> {
        Err(not_a_row())
    }
}

impl Fields {
    fn push<T: Serialize + ?Sized>(&mut self, name: String, value: &T) -> Result<(), Refusal> {
        let value = to_value(value).map_err(|why| Refusal::Refused(format!("{name}: {why}")))?;
        self.columns.push((name, value));
        Ok(())
    }
}

impl ser::SerializeStruct for Fields {
    type Ok = Vec<(String, Value)>;
    type Error = Refusal;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Refusal> {
        self.push(key.to_owned(), value)
    }

    fn end(self) -> Result<Self::Ok, Refusal> {
        Ok(self.columns)
    }
}

impl ser::SerializeMap for Fields {
    type Ok = Vec<(String, Value)>;
    type Error = Refusal;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Refusal> {
        match key.serialize(Scalar)? {
            Value::Text(name) => {
                self.key = Some(name);
                Ok(())
            }
            _ => Err(Refusal::Refused("a row's columns are named by text".to_owned())),
        }
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Refusal> {
        let name = self.key.take().unwrap_or_default();
        self.push(name, value)
    }

    fn end(self) -> Result<Self::Ok, Refusal> {
        Ok(self.columns)
    }
}
