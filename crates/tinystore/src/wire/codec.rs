//! What the messages crates/protocol writes read their fields with and write
//! them with: a function a type of the schema, each the one place its rule
//! lives. A message is read straight from its body, a field at a time, and
//! written straight into the bytes that carry it, with no tree of values
//! between:
//!
//! ```text
//! kv.Call { handle: 7, key: "a" }   →   82 01 07 03 a1 61
//!                                        │  └─┬─┘ └──┬───┘
//!                                        │    1: 7   3: "a"
//!                                        └ a map of 2, its count written last
//! ```

use std::collections::BTreeMap;

use super::msgpack::{self, Reader, Refused, Token};
use super::protocol::Failure;

/// A message of the schema: read whole or refused, and written with every
/// field at its zero value left out.
pub(crate) trait Message: Sized + Default {
    const NAME: &'static str;

    /// Reads the message whose map comes next.
    fn read(r: &mut Reader<'_>) -> Result<Self, Failure>;

    /// Writes the message's map at the end of `out`.
    fn write(&self, out: &mut Vec<u8>);

    /// Whether every field is at its zero value and every optional one absent,
    /// which leaves the message out where a field holds it, as an empty map.
    fn is_zero(&self) -> bool;

    fn decode(body: &[u8]) -> Result<Self, Failure> {
        let mut r = Reader::new(body);
        let message = Self::read(&mut r)?;
        r.end()?;
        Ok(message)
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        self.write(&mut out);
        out
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

impl From<Refused> for Failure {
    /// Bytes that are not the profile's, refused for the rule they break.
    fn from(refused: Refused) -> Failure {
        Failure::invalid(refused.0)
    }
}

/// A message's map as it is written. Its count is known only once the
/// fields are, since a field at its zero value is left out, so the byte a
/// fixmap's count takes is kept, and filled when the map closes.
pub(crate) struct Map {
    head: usize,
    count: usize,
}

impl Map {
    pub(crate) fn open(out: &mut Vec<u8>) -> Map {
        out.push(0x80);
        Map { head: out.len() - 1, count: 0 }
    }

    /// Writes a field's number, one byte as every number of the schema is;
    /// its value comes next.
    pub(crate) fn field(&mut self, out: &mut Vec<u8>, number: u8) {
        out.push(number);
        self.count += 1;
    }

    /// Writes the count in its byte. Past fifteen fields the count is a
    /// map 16's three bytes, which the fields move up to make room for: the
    /// profile writes the shortest head, so none is kept wider in advance.
    pub(crate) fn close(self, out: &mut Vec<u8>) {
        if self.count <= 15 {
            out[self.head] = 0x80 | self.count as u8;
        } else {
            let count = u16::try_from(self.count).expect("a schema numbers its fields below 128");
            let [high, low] = count.to_be_bytes();
            out.splice(self.head..=self.head, [0xde, high, low]);
        }
    }
}

/// How many fields the message `name` has, refusing a body that is not a map.
pub(crate) fn fields(r: &mut Reader<'_>, name: &str) -> Result<usize, Failure> {
    match r.next()? {
        Token::Map(count) => Ok(count),
        _ => Err(Failure::invalid(format!("a {name} that is not a map"))),
    }
}

/// The number of the message `name`'s next field. A field named rather than
/// numbered is refused, and a number twice: `seen` keeps the numbers read,
/// which a schema keeps below 128.
pub(crate) fn field(r: &mut Reader<'_>, name: &str, seen: &mut u128) -> Result<u64, Failure> {
    match r.next()? {
        Token::Uint(number) => {
            if let Some(bit) = 1u128.checked_shl(u32::try_from(number).unwrap_or(u32::MAX)) {
                if *seen & bit != 0 {
                    return Err(Failure::invalid("a key twice"));
                }
                *seen |= bit;
            }
            Ok(number)
        }
        Token::Str(_) => Err(Failure::invalid(format!("a {name} with a field named rather than numbered"))),
        _ => Err(Failure::invalid("a key that is neither an unsigned integer nor a name")),
    }
}

/// A field the message `name` lacks, from a newer client: named with this
/// server's version, so that the client knows which server to upgrade.
pub(crate) fn unknown(number: u64, name: &str) -> Failure {
    Failure::unimplemented(format!("field {number} of {name}"))
}

pub(crate) fn bool(r: &mut Reader<'_>, name: &str) -> Result<bool, Failure> {
    match r.next()? {
        Token::Bool(flag) => Ok(flag),
        _ => Err(not(name, "a bool")),
    }
}

pub(crate) fn uint(r: &mut Reader<'_>, name: &str) -> Result<u64, Failure> {
    match r.next()? {
        Token::Uint(number) => Ok(number),
        _ => Err(not(name, "an unsigned integer")),
    }
}

pub(crate) fn int(r: &mut Reader<'_>, name: &str) -> Result<i64, Failure> {
    match r.next()? {
        Token::Uint(number) => i64::try_from(number).map_err(|_| not(name, "an integer")),
        Token::Int(number) => Ok(number),
        _ => Err(not(name, "an integer")),
    }
}

/// A float, or an integer a float holds exactly, as JavaScript writes 3.0.
#[expect(dead_code, reason = "a float field reads with it, and no message has one yet")]
pub(crate) fn float(r: &mut Reader<'_>, name: &str) -> Result<f64, Failure> {
    let float = match r.next()? {
        Token::Float(float) => Some(float),
        Token::Uint(number) => {
            Some(number as f64).filter(|float| *float as u64 == number && *float < 18_446_744_073_709_551_616.0)
        }
        Token::Int(number) => Some(number as f64).filter(|float| *float as i64 == number),
        _ => None,
    };
    float.ok_or_else(|| not(name, "a float"))
}

pub(crate) fn str(r: &mut Reader<'_>, name: &str) -> Result<String, Failure> {
    match r.next()? {
        Token::Str(text) => Ok(text.to_owned()),
        _ => Err(not(name, "a str")),
    }
}

pub(crate) fn bin(r: &mut Reader<'_>, name: &str) -> Result<Vec<u8>, Failure> {
    match r.next()? {
        Token::Bin(bytes) => Ok(bytes.to_vec()),
        _ => Err(not(name, "a bin")),
    }
}

/// A key's text: a str, a bin that is UTF-8, or an integer's decimal spelling.
pub(crate) fn key(r: &mut Reader<'_>, name: &str) -> Result<String, Failure> {
    match r.next()? {
        Token::Str(text) => Ok(text.to_owned()),
        Token::Uint(number) => Ok(number.to_string()),
        Token::Int(number) => Ok(number.to_string()),
        Token::Bin(bytes) => String::from_utf8(bytes.to_vec())
            .map_err(|_| Failure::unimplemented(format!("{name} of bytes that are not UTF-8"))),
        _ => Err(not(name, "a str, a bin or an integer")),
    }
}

pub(crate) fn row(r: &mut Reader<'_>, name: &str) -> Result<Row, Failure> {
    match r.next()? {
        Token::Nil => Ok(Row::Nil),
        Token::Uint(number) => {
            i64::try_from(number).map(Row::Int).map_err(|_| Failure::invalid(format!("{name} past an int 64")))
        }
        Token::Int(number) => Ok(Row::Int(number)),
        Token::Bin(bytes) => Ok(Row::Bin(bytes.to_vec())),
        _ => Err(not(name, "nil, an integer or a bin")),
    }
}

#[cfg(feature = "sql")]
pub(crate) fn cell(r: &mut Reader<'_>, name: &str) -> Result<Cell, Failure> {
    match r.next()? {
        Token::Nil => Ok(Cell::Nil),
        Token::Bool(flag) => Ok(Cell::Int(i64::from(flag))),
        Token::Uint(number) => {
            i64::try_from(number).map(Cell::Int).map_err(|_| Failure::invalid(format!("{name} past an int 64")))
        }
        Token::Int(number) => Ok(Cell::Int(number)),
        Token::Float(number) => Ok(Cell::Float(number)),
        Token::Str(text) => Ok(Cell::Str(text.to_owned())),
        Token::Bin(bytes) => Ok(Cell::Bin(bytes.to_vec())),
        _ => Err(not(name, "nil, a bool, a number, a str or a bin")),
    }
}

pub(crate) fn list<T>(
    r: &mut Reader<'_>,
    name: &str,
    item: impl Fn(&mut Reader<'_>, &str) -> Result<T, Failure>,
) -> Result<Vec<T>, Failure> {
    let Token::Array(count) = r.next()? else {
        return Err(not(name, "an array"));
    };
    // the reader has checked the count against the bytes left, each item a byte at least
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        items.push(item(r, name)?);
    }
    Ok(items)
}

pub(crate) fn names<T>(
    r: &mut Reader<'_>,
    name: &str,
    item: impl Fn(&mut Reader<'_>, &str) -> Result<T, Failure>,
) -> Result<BTreeMap<String, T>, Failure> {
    let Token::Map(count) = r.next()? else {
        return Err(not(name, "a map of names"));
    };
    let mut named = BTreeMap::new();
    for _ in 0..count {
        let key = match r.next()? {
            Token::Str(key) => key.to_owned(),
            Token::Uint(_) => return Err(not(name, "a map whose keys are names")),
            _ => return Err(Failure::invalid("a key that is neither an unsigned integer nor a name")),
        };
        if named.insert(key, item(r, name)?).is_some() {
            return Err(Failure::invalid("a key twice"));
        }
    }
    Ok(named)
}

pub(crate) fn message<M: Message>(r: &mut Reader<'_>, _name: &str) -> Result<M, Failure> {
    M::read(r)
}

pub(crate) fn write_bool(out: &mut Vec<u8>, flag: &bool) {
    msgpack::write_bool(out, *flag);
}

pub(crate) fn write_uint(out: &mut Vec<u8>, number: &u64) {
    msgpack::write_uint(out, *number);
}

/// An integer as the profile writes it: unsigned when it is not negative.
pub(crate) fn write_int(out: &mut Vec<u8>, number: &i64) {
    msgpack::write_int(out, *number);
}

#[expect(dead_code, reason = "a float field writes with it, and no message has one yet")]
pub(crate) fn write_float(out: &mut Vec<u8>, float: &f64) {
    msgpack::write_float(out, *float);
}

pub(crate) fn write_str(out: &mut Vec<u8>, text: &impl AsRef<str>) {
    msgpack::write_str(out, text.as_ref());
}

pub(crate) fn write_bin(out: &mut Vec<u8>, bytes: &impl AsRef<[u8]>) {
    msgpack::write_bin(out, bytes.as_ref());
}

pub(crate) fn write_row(out: &mut Vec<u8>, row: &Row) {
    match row {
        Row::Nil => msgpack::write_nil(out),
        Row::Int(number) => msgpack::write_int(out, *number),
        Row::Bin(bytes) => msgpack::write_bin(out, bytes),
    }
}

#[cfg(feature = "sql")]
pub(crate) fn write_cell(out: &mut Vec<u8>, cell: &Cell) {
    match cell {
        Cell::Nil => msgpack::write_nil(out),
        Cell::Int(number) => msgpack::write_int(out, *number),
        Cell::Float(number) => msgpack::write_float(out, *number),
        Cell::Str(text) => msgpack::write_str(out, text),
        Cell::Bin(bytes) => msgpack::write_bin(out, bytes),
    }
}

pub(crate) fn write_list<T>(out: &mut Vec<u8>, items: &[T], item: impl Fn(&mut Vec<u8>, &T)) {
    msgpack::write_array_head(out, items.len());
    for each in items {
        item(out, each);
    }
}

/// Names in the byte order of their text, which a map's canonical order is.
pub(crate) fn write_names<T>(out: &mut Vec<u8>, named: &BTreeMap<String, T>, item: impl Fn(&mut Vec<u8>, &T)) {
    msgpack::write_map_head(out, named.len());
    for (name, each) in named {
        msgpack::write_str(out, name);
        item(out, each);
    }
}

pub(crate) fn write_message<M: Message>(out: &mut Vec<u8>, message: &M) {
    message.write(out);
}

/// Whether a row writes as a zero value of the profile, which a field that
/// holds it leaves out: nil, the integer 0 or no bytes.
#[expect(dead_code, reason = "a field that is not optional holds a row with it, and every row field is optional yet")]
pub(crate) fn row_is_zero(row: &Row) -> bool {
    match row {
        Row::Nil | Row::Int(0) => true,
        Row::Int(_) => false,
        Row::Bin(bytes) => bytes.is_empty(),
    }
}

/// Whether a cell writes as a zero value of the profile: nil, the integer 0,
/// a float whose bits are all 0, no text or no bytes.
#[cfg(feature = "sql")]
#[expect(dead_code, reason = "a field that is not optional holds a cell with it, and cells are in lists yet")]
pub(crate) fn cell_is_zero(cell: &Cell) -> bool {
    match cell {
        Cell::Nil | Cell::Int(0) => true,
        Cell::Int(_) => false,
        Cell::Float(number) => number.to_bits() == 0,
        Cell::Str(text) => text.is_empty(),
        Cell::Bin(bytes) => bytes.is_empty(),
    }
}

fn not(name: &str, kind: &str) -> Failure {
    Failure::invalid(format!("{name} is not {kind}"))
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
