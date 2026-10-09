//! Rows as a statement gives them, and how they become the program's values.
//!
//! A statement's rows are read inside a snapshot or a savepoint, as SQLite's
//! own values, and decoded after it ends, so that decoding never holds either
//! open: a reader's snapshot is the oldest the write-ahead log keeps, and a
//! savepoint holds the writer.

use std::fmt;
use std::sync::Arc;

use rusqlite::{Statement, params_from_iter};
use serde::de::value::{SeqDeserializer, StrDeserializer};
use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, Visitor};

use super::values::Value;
use crate::{Error, Result};

/// Values a call reads at most, in bytes, before it is `limit`: past them a
/// program reads in pages, or a row at a time.
pub(crate) const MOST_READ: usize = 64 << 20;

/// The rows a statement gave, each `columns.len()` values long.
pub(crate) struct Rows {
    columns: Arc<[String]>,
    values: Vec<Value>,
}

impl Rows {
    /// Steps `statement` to its end, or until it gave `most` rows, keeping
    /// what each row holds; past `bytes` of values it is `limit`.
    pub(crate) fn read(statement: &mut Statement<'_>, values: &[Value], most: usize, bytes: usize) -> Result<Rows> {
        let columns: Arc<[String]> = statement.column_names().into_iter().map(str::to_owned).collect();
        let width = columns.len();
        let mut rows = statement.query(params_from_iter(values)).map_err(|error| failure("its rows", error))?;
        let (mut kept, mut held) = (Vec::new(), 0usize);
        let mut count = 0;
        while count < most {
            let Some(row) = rows.next().map_err(|error| failure("its rows", error))? else {
                break;
            };
            for index in 0..width {
                let value = Value::of(row.get_ref(index).map_err(|error| failure("its rows", error))?);
                held += value.size();
                kept.push(value);
            }
            if held > bytes {
                return Err(Error::limit(format!(
                    "its rows pass {} MiB: read them a page at a time, or one at a time",
                    bytes >> 20
                )));
            }
            count += 1;
        }
        Ok(Rows { columns, values: kept })
    }

    pub(crate) fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The values, a row after another, each `width()` long.
    pub(crate) fn into_values(self) -> Vec<Value> {
        self.values
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len().checked_div(self.columns.len()).unwrap_or(0)
    }

    pub(crate) fn width(&self) -> usize {
        self.columns.len()
    }

    /// Each row as a `T`: a struct by its columns' names, a tuple by their
    /// order, or a single value when the row has one column.
    pub(crate) fn decode<T: DeserializeOwned>(self) -> Result<Vec<T>> {
        let width = self.columns.len();
        if width == 0 {
            return Ok(Vec::new());
        }
        let mut decoded = Vec::with_capacity(self.len());
        let mut values = self.values.into_iter();
        loop {
            let row: Vec<Value> = values.by_ref().take(width).collect();
            if row.is_empty() {
                return Ok(decoded);
            }
            let row = RowDecoder { columns: &self.columns, values: row };
            decoded.push(T::deserialize(row).map_err(|failure| Error::invalid(failure.0))?);
        }
    }

    /// The one value of a row of one column.
    pub(crate) fn scalar<T: DeserializeOwned>(mut self) -> Result<T> {
        let value = self.values.pop().unwrap_or(Value::Null);
        let column = self.columns.first().cloned().unwrap_or_default();
        T::deserialize(ValueDecoder(value)).map_err(|failure| Error::invalid(format!("column {column}: {}", failure.0)))
    }
}

/// A statement's failure as the error a caller acts on: a unique or primary
/// key already held is `conflict`, and any other constraint is `invalid`.
pub(crate) fn failure(what: &str, error: rusqlite::Error) -> Error {
    use rusqlite::ffi;
    if let rusqlite::Error::SqliteFailure(code, _) = &error {
        let held = [ffi::SQLITE_CONSTRAINT_UNIQUE, ffi::SQLITE_CONSTRAINT_PRIMARYKEY, ffi::SQLITE_CONSTRAINT_ROWID];
        if code.code == rusqlite::ErrorCode::ConstraintViolation {
            let kind =
                if held.contains(&code.extended_code) { crate::ErrorKind::Conflict } else { crate::ErrorKind::Invalid };
            return Error::new(kind, what).with_source(error);
        }
    }
    crate::sqlite::sql_error(what, error)
}

/// Why a value does not read as the program's: said of its column.
#[derive(Debug)]
pub(crate) struct Mismatch(String);

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Mismatch {}

impl de::Error for Mismatch {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Mismatch(message.to_string())
    }
}

/// A row: a map of its columns for a struct, a sequence for a tuple, its one
/// value for anything else.
struct RowDecoder<'a> {
    columns: &'a [String],
    values: Vec<Value>,
}

impl<'de> de::Deserializer<'de> for RowDecoder<'_> {
    type Error = Mismatch;

    fn deserialize_any<V: Visitor<'de>>(mut self, visitor: V) -> Result<V::Value, Mismatch> {
        if self.values.len() == 1 {
            let column = &self.columns[0];
            let value = self.values.pop().unwrap_or(Value::Null);
            return ValueDecoder(value)
                .deserialize_any(visitor)
                .map_err(|failure| Mismatch(format!("column {column}: {failure}")));
        }
        self.deserialize_map(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_map(Columns { columns: self.columns, values: self.values.into_iter(), at: 0 })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Mismatch> {
        self.deserialize_map(visitor)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_seq(Columns { columns: self.columns, values: self.values.into_iter(), at: 0 })
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Mismatch> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: usize,
        visitor: V,
    ) -> Result<V::Value, Mismatch> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        self.one_column()?.deserialize_option(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Mismatch> {
        self.one_column()?.deserialize_enum(name, variants, visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf unit unit_struct
        identifier ignored_any
    }
}

impl RowDecoder<'_> {
    fn one_column(mut self) -> Result<ValueDecoder, Mismatch> {
        if self.values.len() != 1 {
            return Err(Mismatch(format!("a row of {} columns does not read as one value", self.values.len())));
        }
        Ok(ValueDecoder(self.values.pop().unwrap_or(Value::Null)))
    }
}

/// A row's columns as a map or in order, each value said of its column when
/// it fails.
struct Columns<'a> {
    columns: &'a [String],
    values: std::vec::IntoIter<Value>,
    at: usize,
}

impl<'de> de::SeqAccess<'de> for Columns<'_> {
    type Error = Mismatch;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, Mismatch> {
        if self.at == self.columns.len() {
            return Ok(None);
        }
        de::MapAccess::next_value_seed(self, seed).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.columns.len() - self.at)
    }
}

impl<'de> de::MapAccess<'de> for Columns<'_> {
    type Error = Mismatch;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Mismatch> {
        let Some(column) = self.columns.get(self.at) else {
            return Ok(None);
        };
        let key: StrDeserializer<'_, Mismatch> = column.as_str().into_deserializer();
        seed.deserialize(key).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Mismatch> {
        let column = &self.columns[self.at];
        self.at += 1;
        let value = self.values.next().unwrap_or(Value::Null);
        seed.deserialize(ValueDecoder(value)).map_err(|failure| Mismatch(format!("column {column}: {failure}")))
    }
}

/// One value as SQLite keeps it, read as the program's type asks.
pub(crate) struct ValueDecoder(pub(crate) Value);

impl ValueDecoder {
    fn refuse<T>(self, wanted: &str) -> Result<T, Mismatch> {
        let shown = match &self.0 {
            Value::Text(text) if text.chars().count() > 40 => {
                format!("TEXT \"{}…\"", text.chars().take(40).collect::<String>())
            }
            Value::Text(text) => format!("TEXT {text:?}"),
            Value::Integer(n) => format!("INTEGER {n}"),
            Value::Real(x) => format!("REAL {x}"),
            Value::Blob(bytes) => format!("a BLOB of {} bytes", bytes.len()),
            Value::Null => "NULL".to_owned(),
        };
        Err(Mismatch(format!("{shown} does not read as {wanted}")))
    }

    fn integer(self, wanted: &str) -> Result<i64, Mismatch> {
        match self.0 {
            Value::Integer(n) => Ok(n),
            _ => self.refuse(wanted),
        }
    }

    /// A value of JSON's own shapes, read from the text that holds it.
    fn json(self, wanted: &str) -> Result<serde_json::Value, Mismatch> {
        match &self.0 {
            Value::Text(text) => serde_json::from_str(text)
                .map_err(|error| Mismatch(format!("its text is not JSON for {wanted}: {error}"))),
            _ => self.refuse(wanted),
        }
    }
}

macro_rules! integers {
    ($($method:ident $visit:ident $ty:ty)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
            let n = self.integer(stringify!($ty))?;
            let n = <$ty>::try_from(n).map_err(|_| Mismatch(format!("INTEGER {n} is past {}", stringify!($ty))))?;
            visitor.$visit(n)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for ValueDecoder {
    type Error = Mismatch;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Value::Null => visitor.visit_unit(),
            Value::Integer(n) => visitor.visit_i64(n),
            Value::Real(x) => visitor.visit_f64(x),
            Value::Text(text) => visitor.visit_string(text),
            Value::Blob(bytes) => visitor.visit_byte_buf(bytes),
        }
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Value::Integer(0) => visitor.visit_bool(false),
            Value::Integer(1) => visitor.visit_bool(true),
            _ => self.refuse("a bool, which SQLite keeps as 0 or 1"),
        }
    }

    integers! {
        deserialize_i8 visit_i8 i8
        deserialize_i16 visit_i16 i16
        deserialize_i32 visit_i32 i32
        deserialize_i64 visit_i64 i64
        deserialize_u8 visit_u8 u8
        deserialize_u16 visit_u16 u16
        deserialize_u32 visit_u32 u32
        deserialize_u64 visit_u64 u64
    }

    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_i128(i128::from(self.integer("i128")?))
    }

    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        let n = self.integer("u128")?;
        let n = u128::try_from(n).map_err(|_| Mismatch(format!("INTEGER {n} is past u128")))?;
        visitor.visit_u128(n)
    }

    #[expect(clippy::cast_precision_loss, reason = "an INTEGER read as a float is a float, as SQLite reads one")]
    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Value::Real(x) => visitor.visit_f64(x),
            Value::Integer(n) => visitor.visit_f64(n as f64),
            _ => self.refuse("a float"),
        }
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        self.deserialize_f64(visitor)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Value::Text(text) => visitor.visit_string(text),
            _ => self.refuse("text"),
        }
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        self.deserialize_str(visitor)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        self.deserialize_str(visitor)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Value::Blob(bytes) => visitor.visit_byte_buf(bytes),
            Value::Text(text) => visitor.visit_byte_buf(text.into_bytes()),
            _ => self.refuse("bytes"),
        }
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Value::Null => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Value::Null => visitor.visit_unit(),
            _ => self.refuse("nothing"),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, Mismatch> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_newtype_struct(self)
    }

    /// A list: a BLOB's bytes for a `Vec<u8>`, JSON's for anything else.
    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        if let Value::Blob(bytes) = self.0 {
            return visitor.visit_seq(SeqDeserializer::new(bytes.into_iter()));
        }
        de::Deserializer::deserialize_seq(self.json("a list")?, visitor).map_err(json_mismatch)
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Mismatch> {
        de::Deserializer::deserialize_tuple(self.json("a tuple")?, len, visitor).map_err(json_mismatch)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Mismatch> {
        de::Deserializer::deserialize_tuple_struct(self.json(name)?, name, len, visitor).map_err(json_mismatch)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        de::Deserializer::deserialize_map(self.json("a map")?, visitor).map_err(json_mismatch)
    }

    /// A struct is JSON, but `SystemTime`, which is unix milliseconds: serde
    /// reads it as its seconds and nanoseconds since the epoch.
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Mismatch> {
        if name == "SystemTime" {
            let millis = self.integer("a time, which SQLite keeps as unix milliseconds")?;
            let millis =
                u64::try_from(millis).map_err(|_| Mismatch(format!("INTEGER {millis} is a time before 1970")))?;
            let parts = [millis / 1000, (millis % 1000) * 1_000_000];
            return visitor.visit_seq(SeqDeserializer::new(parts.into_iter()));
        }
        de::Deserializer::deserialize_struct(self.json(name)?, name, fields, visitor).map_err(json_mismatch)
    }

    /// An enum kept as the name of its variant, or as JSON when it holds more.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Mismatch> {
        match self.0 {
            Value::Text(ref text) if !text.starts_with('{') => {
                let variant: StrDeserializer<'_, Mismatch> = text.as_str().into_deserializer();
                variant.deserialize_enum(name, variants, visitor)
            }
            _ => de::Deserializer::deserialize_enum(self.json(name)?, name, variants, visitor).map_err(json_mismatch),
        }
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_unit()
    }

    fn is_human_readable(&self) -> bool {
        true
    }
}

fn json_mismatch(error: serde_json::Error) -> Mismatch {
    Mismatch(format!("its JSON: {error}"))
}

impl<'de> IntoDeserializer<'de, Mismatch> for ValueDecoder {
    type Deserializer = ValueDecoder;

    fn into_deserializer(self) -> ValueDecoder {
        self
    }
}
