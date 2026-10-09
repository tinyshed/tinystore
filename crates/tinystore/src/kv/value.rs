//! A value as its row keeps it, and the one way every type gets there.
//!
//! A type is kept by what serde says it is, so a bucket's type is given once,
//! where the bucket opens, and nothing wraps it:
//!
//! | Type                                   | Row          | Example                    |
//! |----------------------------------------|--------------|----------------------------|
//! | `()`, `None`                           | nothing      | a set's member             |
//! | `bool`, integers that fit an `i64`     | an integer   | `true → 1`, `-7 → -7`      |
//! | `u64`                                  | 8 bytes, big-endian | `2^63 → 80 00 … 00` |
//! | `f64`, `f32`                           | their bits, big-endian | `-0.0 → 80 00 … 00` |
//! | `String`, `char`, serde bytes          | the bytes    | `"hé" → 68 c3 a9`          |
//! | structs, maps, sequences, enums        | JSON         | `{"user":42}`              |
//!
//! A float keeps its bits, `-0`, infinities and a NaN's payload included. A
//! newtype keeps what it wraps. `Vec<u8>` is a sequence to serde and becomes
//! JSON; [`Bytes`] keeps bytes as they are.

use std::fmt;

use serde::de::{self, DeserializeOwned, Visitor};
use serde::ser::{self, Impossible};
use serde::{Deserialize, Serialize};

/// A value as its row keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Raw {
    None,
    Int(i64),
    Bytes(Vec<u8>),
}

impl Raw {
    pub(crate) fn len(&self) -> usize {
        match self {
            Raw::None => 0,
            Raw::Int(_) => 8,
            Raw::Bytes(bytes) => bytes.len(),
        }
    }
}

/// A type a bucket can keep: anything serde reads and writes.
pub trait Value: Serialize + DeserializeOwned + Send + 'static {}

impl<T: Serialize + DeserializeOwned + Send + 'static> Value for T {}

/// Bytes kept as they are rather than as a JSON list of numbers.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Bytes(pub Vec<u8>);

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bytes({} bytes)", self.0.len())
    }
}

impl Serialize for Bytes {
    fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_byte_buf(BytesVisitor)
    }
}

struct BytesVisitor;

impl Visitor<'_> for BytesVisitor {
    type Value = Bytes;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bytes")
    }

    fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Bytes, E> {
        Ok(Bytes(bytes.to_vec()))
    }

    fn visit_byte_buf<E: de::Error>(self, bytes: Vec<u8>) -> Result<Bytes, E> {
        Ok(Bytes(bytes))
    }
}

/// Why a value could not become a row, or a row a value.
#[derive(Debug)]
pub(crate) struct Mismatch(pub(crate) String);

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

/// The row a value is kept as.
pub(crate) fn encode<V: Serialize>(value: &V) -> Result<Raw, Mismatch> {
    match value.serialize(Probe) {
        Ok(raw) => Ok(raw),
        Err(Probed::Compound) => serde_json::to_vec(value).map(Raw::Bytes).map_err(|error| Mismatch(error.to_string())),
        Err(Probed::Failed(mismatch)) => Err(mismatch),
    }
}

/// The value a row keeps.
pub(crate) fn decode<V: DeserializeOwned>(raw: &Raw) -> Result<V, Mismatch> {
    V::deserialize(Row(raw))
}

/// A serializer that keeps a top-level primitive as a row and gives up on
/// anything compound, which then becomes JSON.
struct Probe;

/// How a probe gave up: on a compound value, or on a value serde refused.
#[derive(Debug)]
enum Probed {
    Compound,
    Failed(Mismatch),
}

impl fmt::Display for Probed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Probed::Compound => f.write_str("a compound value"),
            Probed::Failed(mismatch) => mismatch.fmt(f),
        }
    }
}

impl std::error::Error for Probed {}

impl ser::Error for Probed {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Probed::Failed(Mismatch(message.to_string()))
    }
}

macro_rules! compound {
    ($($method:ident($($arg:ty),*) -> $kind:ty;)*) => {
        $(fn $method(self, $(_: $arg),*) -> Result<$kind, Probed> {
            Err(Probed::Compound)
        })*
    };
}

impl ser::Serializer for Probe {
    type Ok = Raw;
    type Error = Probed;
    type SerializeSeq = Impossible<Raw, Probed>;
    type SerializeTuple = Impossible<Raw, Probed>;
    type SerializeTupleStruct = Impossible<Raw, Probed>;
    type SerializeTupleVariant = Impossible<Raw, Probed>;
    type SerializeMap = Impossible<Raw, Probed>;
    type SerializeStruct = Impossible<Raw, Probed>;
    type SerializeStructVariant = Impossible<Raw, Probed>;

    fn serialize_bool(self, value: bool) -> Result<Raw, Probed> {
        Ok(Raw::Int(i64::from(value)))
    }

    fn serialize_i8(self, value: i8) -> Result<Raw, Probed> {
        Ok(Raw::Int(value.into()))
    }

    fn serialize_i16(self, value: i16) -> Result<Raw, Probed> {
        Ok(Raw::Int(value.into()))
    }

    fn serialize_i32(self, value: i32) -> Result<Raw, Probed> {
        Ok(Raw::Int(value.into()))
    }

    fn serialize_i64(self, value: i64) -> Result<Raw, Probed> {
        Ok(Raw::Int(value))
    }

    fn serialize_u8(self, value: u8) -> Result<Raw, Probed> {
        Ok(Raw::Int(value.into()))
    }

    fn serialize_u16(self, value: u16) -> Result<Raw, Probed> {
        Ok(Raw::Int(value.into()))
    }

    fn serialize_u32(self, value: u32) -> Result<Raw, Probed> {
        Ok(Raw::Int(value.into()))
    }

    fn serialize_u64(self, value: u64) -> Result<Raw, Probed> {
        Ok(Raw::Bytes(value.to_be_bytes().to_vec()))
    }

    fn serialize_f32(self, value: f32) -> Result<Raw, Probed> {
        Ok(Raw::Bytes(value.to_bits().to_be_bytes().to_vec()))
    }

    fn serialize_f64(self, value: f64) -> Result<Raw, Probed> {
        Ok(Raw::Bytes(value.to_bits().to_be_bytes().to_vec()))
    }

    fn serialize_char(self, value: char) -> Result<Raw, Probed> {
        Ok(Raw::Bytes(value.to_string().into_bytes()))
    }

    fn serialize_str(self, value: &str) -> Result<Raw, Probed> {
        Ok(Raw::Bytes(value.as_bytes().to_vec()))
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<Raw, Probed> {
        Ok(Raw::Bytes(value.to_vec()))
    }

    fn serialize_none(self) -> Result<Raw, Probed> {
        Ok(Raw::None)
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Raw, Probed> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Raw, Probed> {
        Ok(Raw::None)
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<Raw, Probed> {
        Ok(Raw::None)
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, value: &T) -> Result<Raw, Probed> {
        value.serialize(self)
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<Raw, Probed> {
        Err(Probed::Compound)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<Raw, Probed> {
        Err(Probed::Compound)
    }

    compound! {
        serialize_seq(Option<usize>) -> Self::SerializeSeq;
        serialize_tuple(usize) -> Self::SerializeTuple;
        serialize_tuple_struct(&'static str, usize) -> Self::SerializeTupleStruct;
        serialize_tuple_variant(&'static str, u32, &'static str, usize) -> Self::SerializeTupleVariant;
        serialize_map(Option<usize>) -> Self::SerializeMap;
        serialize_struct(&'static str, usize) -> Self::SerializeStruct;
        serialize_struct_variant(&'static str, u32, &'static str, usize) -> Self::SerializeStructVariant;
    }
}

/// A row read back as whatever the value's type asks for. It borrows the row,
/// so text and bytes are handed to the value's type without a copy here.
struct Row<'de>(&'de Raw);

impl<'de> Row<'de> {
    fn bytes(&self, wanted: &str) -> Result<&'de [u8], Mismatch> {
        match self.0 {
            Raw::Bytes(bytes) => Ok(bytes),
            other => Err(Mismatch(format!("{wanted} from a row holding {}", shown(other)))),
        }
    }

    fn int(&self, wanted: &str) -> Result<i64, Mismatch> {
        match self.0 {
            Raw::Int(int) => Ok(*int),
            other => Err(Mismatch(format!("{wanted} from a row holding {}", shown(other)))),
        }
    }

    fn fixed<const N: usize>(&self, wanted: &str) -> Result<[u8; N], Mismatch> {
        let bytes = self.bytes(wanted)?;
        bytes.try_into().map_err(|_| Mismatch(format!("{wanted} from {} bytes rather than {N}", bytes.len())))
    }

    fn text(&self) -> Result<&'de str, Mismatch> {
        std::str::from_utf8(self.bytes("text")?).map_err(|_| Mismatch("text that is not UTF-8".to_owned()))
    }

    /// Reads the row as JSON with `read`, refusing anything after the value.
    fn json<T>(
        &self,
        read: impl FnOnce(&mut serde_json::Deserializer<serde_json::de::SliceRead<'de>>) -> serde_json::Result<T>,
    ) -> Result<T, Mismatch> {
        let mut json = serde_json::Deserializer::from_slice(self.bytes("JSON")?);
        let value = read(&mut json).map_err(|error| Mismatch(error.to_string()))?;
        json.end().map_err(|error| Mismatch(error.to_string()))?;
        Ok(value)
    }
}

fn shown(raw: &Raw) -> &'static str {
    match raw {
        Raw::None => "nothing",
        Raw::Int(_) => "an integer",
        Raw::Bytes(_) => "bytes",
    }
}

macro_rules! from_int {
    ($($method:ident,)*) => {
        $(fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
            visitor.visit_i64(self.int("an integer")?)
        })*
    };
}

macro_rules! from_json {
    ($($method:ident($($arg:ident: $ty:ty),*);)*) => {
        $(fn $method<V: Visitor<'de>>(self, $($arg: $ty,)* visitor: V) -> Result<V::Value, Mismatch> {
            self.json(|json| de::Deserializer::$method(json, $($arg,)* visitor))
        })*
    };
}

impl<'de> de::Deserializer<'de> for Row<'de> {
    type Error = Mismatch;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Raw::None => visitor.visit_unit(),
            Raw::Int(int) => visitor.visit_i64(*int),
            Raw::Bytes(_) => self.json(|json| de::Deserializer::deserialize_any(json, visitor)),
        }
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.int("a bool")? {
            0 => visitor.visit_bool(false),
            1 => visitor.visit_bool(true),
            other => Err(Mismatch(format!("a bool from the integer {other}"))),
        }
    }

    from_int! {
        deserialize_i8, deserialize_i16, deserialize_i32, deserialize_i64,
        deserialize_u8, deserialize_u16, deserialize_u32,
    }

    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_u64(u64::from_be_bytes(self.fixed("a u64")?))
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_f32(f32::from_bits(u32::from_be_bytes(self.fixed("an f32")?)))
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_f64(f64::from_bits(u64::from_be_bytes(self.fixed("an f64")?)))
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_borrowed_str(self.text()?)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_borrowed_str(self.text()?)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_borrowed_str(self.text()?)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_borrowed_bytes(self.bytes("bytes")?)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_borrowed_bytes(self.bytes("bytes")?)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Raw::None => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Mismatch> {
        match self.0 {
            Raw::None => visitor.visit_unit(),
            other => Err(Mismatch(format!("nothing from a row holding {}", shown(other)))),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, Mismatch> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, Mismatch> {
        visitor.visit_newtype_struct(self)
    }

    from_json! {
        deserialize_seq();
        deserialize_tuple(len: usize);
        deserialize_tuple_struct(name: &'static str, len: usize);
        deserialize_map();
        deserialize_struct(name: &'static str, fields: &'static [&'static str]);
        deserialize_enum(name: &'static str, variants: &'static [&'static str]);
        deserialize_identifier();
        deserialize_ignored_any();
    }
}

#[cfg(test)]
#[path = "value_tests.rs"]
mod tests;
