//! The protocol's MessagePack profile, docs/wire.md's "Messages": nil, bool,
//! integers, float 64, str, bin, array and map, keys that are unsigned
//! integers or names, eight levels deep at most. A decoder takes any valid
//! encoding; the encoder writes the canonical one, so a vector compares byte
//! for byte:
//!
//! ```text
//! 300 as ce 00 00 01 2c   → decodes to 300 → encodes as cd 01 2c
//! 3 where a float belongs → decodes to 3.0 when asked for a float
//! ```

use std::fmt;

/// The deepest a value nests: an array in an array, eight times.
const MAX_DEPTH: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Nil,
    Bool(bool),
    Uint(u64),
    /// Negative integers only; a non-negative one decodes as `Uint`.
    Int(i64),
    Float(f64),
    Str(String),
    Bin(Vec<u8>),
    Array(Vec<Value>),
    /// Keys are `Uint` or `Str`, each once.
    Map(Vec<(Value, Value)>),
}

/// Why bytes are not a value of the profile; a request that carries such bytes
/// fails `invalid` naming the rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refused(pub(crate) String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn refused<T>(rule: impl Into<String>) -> Result<T, Refused> {
    Err(Refused(rule.into()))
}

/// Decodes one value that is the whole of `bytes`.
pub(crate) fn decode(bytes: &[u8]) -> Result<Value, Refused> {
    let mut reader = Reader { bytes, at: 0 };
    let value = reader.value(0)?;
    if reader.at != bytes.len() {
        return refused("bytes after the value");
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn value(&mut self, depth: usize) -> Result<Value, Refused> {
        if depth > MAX_DEPTH {
            return refused("a value nested past eight levels");
        }
        let marker = self.byte()?;
        match marker {
            0x00..=0x7f => Ok(Value::Uint(u64::from(marker))),
            0x80..=0x8f => self.map(usize::from(marker & 0x0f), depth),
            0x90..=0x9f => self.array(usize::from(marker & 0x0f), depth),
            0xa0..=0xbf => self.str(usize::from(marker & 0x1f)),
            0xc0 => Ok(Value::Nil),
            0xc2 => Ok(Value::Bool(false)),
            0xc3 => Ok(Value::Bool(true)),
            0xc4..=0xc6 => {
                let length = self.length(marker - 0xc4)?;
                Ok(Value::Bin(self.take(length)?.to_vec()))
            }
            0xcb => Ok(Value::Float(f64::from_bits(self.uint(8)?))),
            0xcc..=0xcf => Ok(Value::Uint(self.uint(1 << (marker - 0xcc))?)),
            0xd0..=0xd3 => Ok(signed(self.int(1 << (marker - 0xd0))?)),
            0xd9..=0xdb => {
                let length = self.length(marker - 0xd9)?;
                self.str(length)
            }
            0xdc | 0xdd => {
                let count = self.length(marker - 0xdc + 1)?;
                self.array(count, depth)
            }
            0xde | 0xdf => {
                let count = self.length(marker - 0xde + 1)?;
                self.map(count, depth)
            }
            0xe0..=0xff => Ok(Value::Int(i64::from(marker as i8))),
            0xca => refused("a float 32"),
            0xc7..=0xc9 | 0xd4..=0xd8 => refused("an extension"),
            0xc1 => refused("the byte MessagePack never uses"),
        }
    }

    /// A length or count in 1, 2 or 4 bytes, as `width` 0, 1 or 2 says.
    fn length(&mut self, width: u8) -> Result<usize, Refused> {
        let length = self.uint(1 << width)?;
        usize::try_from(length).or_else(|_| refused("a length past the bytes left"))
    }

    fn uint(&mut self, size: usize) -> Result<u64, Refused> {
        let bytes = self.take(size)?;
        Ok(bytes.iter().fold(0u64, |value, &byte| (value << 8) | u64::from(byte)))
    }

    fn int(&mut self, size: usize) -> Result<i64, Refused> {
        let raw = self.uint(size)?;
        let shift = 64 - 8 * size as u32;
        Ok(((raw << shift) as i64) >> shift)
    }

    fn str(&mut self, length: usize) -> Result<Value, Refused> {
        let bytes = self.take(length)?;
        match std::str::from_utf8(bytes) {
            Ok(text) => Ok(Value::Str(text.to_owned())),
            Err(_) => refused("a str that is not UTF-8"),
        }
    }

    fn array(&mut self, count: usize, depth: usize) -> Result<Value, Refused> {
        // every element takes at least a byte, so a count past the bytes left is a lie
        if count > self.left() {
            return refused("a count past the bytes left");
        }
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(self.value(depth + 1)?);
        }
        Ok(Value::Array(items))
    }

    fn map(&mut self, count: usize, depth: usize) -> Result<Value, Refused> {
        if count.saturating_mul(2) > self.left() {
            return refused("a count past the bytes left");
        }
        let mut pairs: Vec<(Value, Value)> = Vec::with_capacity(count);
        for _ in 0..count {
            let key = self.value(depth + 1)?;
            if !matches!(key, Value::Uint(_) | Value::Str(_)) {
                return refused("a key that is neither an unsigned integer nor a name");
            }
            if pairs.iter().any(|(seen, _)| *seen == key) {
                return refused("a key twice");
            }
            let value = self.value(depth + 1)?;
            pairs.push((key, value));
        }
        Ok(Value::Map(pairs))
    }

    fn byte(&mut self) -> Result<u8, Refused> {
        Ok(self.take(1)?[0])
    }

    fn take(&mut self, length: usize) -> Result<&[u8], Refused> {
        if length > self.left() {
            return refused(if self.at == 0 && length == 1 { "an empty body" } else { "a length past the bytes left" });
        }
        let taken = &self.bytes[self.at..self.at + length];
        self.at += length;
        Ok(taken)
    }

    fn left(&self) -> usize {
        self.bytes.len() - self.at
    }
}

fn signed(value: i64) -> Value {
    match u64::try_from(value) {
        Ok(unsigned) => Value::Uint(unsigned),
        Err(_) => Value::Int(value),
    }
}

/// Encodes a value canonically: the shortest form of every integer, length and
/// count, every float in eight bytes, a map's keys in ascending order.
pub(crate) fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write(&mut out, value);
    out
}

fn write(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Nil => out.push(0xc0),
        Value::Bool(flag) => out.push(if *flag { 0xc3 } else { 0xc2 }),
        Value::Uint(unsigned) => write_uint(out, *unsigned),
        Value::Int(signed) => write_int(out, *signed),
        Value::Float(float) => {
            out.push(0xcb);
            out.extend_from_slice(&float.to_bits().to_be_bytes());
        }
        Value::Str(text) => {
            write_head(out, text.len(), (0xa0, 31), [0xd9, 0xda, 0xdb]);
            out.extend_from_slice(text.as_bytes());
        }
        Value::Bin(bytes) => {
            write_head(out, bytes.len(), (0, 0), [0xc4, 0xc5, 0xc6]);
            out.extend_from_slice(bytes);
        }
        Value::Array(items) => {
            write_head(out, items.len(), (0x90, 15), [0, 0xdc, 0xdd]);
            items.iter().for_each(|item| write(out, item));
        }
        Value::Map(pairs) => write_map(out, pairs),
    }
}

fn write_map(out: &mut Vec<u8>, pairs: &[(Value, Value)]) {
    write_head(out, pairs.len(), (0x80, 15), [0, 0xde, 0xdf]);
    let mut sorted: Vec<&(Value, Value)> = pairs.iter().collect();
    sorted.sort_by(|(a, _), (b, _)| key_order(a, b));
    for (key, value) in sorted {
        write(out, key);
        write(out, value);
    }
}

fn key_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Uint(a), Value::Uint(b)) => a.cmp(b),
        (Value::Str(a), Value::Str(b)) => a.as_bytes().cmp(b.as_bytes()),
        (Value::Uint(_), _) => std::cmp::Ordering::Less,
        _ => std::cmp::Ordering::Greater,
    }
}

/// A length's head: its fix form when it fits, then 8, 16 or 32 bits; a
/// marker of 0 means the format has no such width.
fn write_head(out: &mut Vec<u8>, length: usize, fix: (u8, usize), markers: [u8; 3]) {
    let (fix_marker, fix_most) = fix;
    if fix_marker != 0 && length <= fix_most {
        out.push(fix_marker | length as u8);
    } else if markers[0] != 0 && length <= 0xff {
        out.extend_from_slice(&[markers[0], length as u8]);
    } else if length <= 0xffff {
        out.push(markers[1]);
        out.extend_from_slice(&(length as u16).to_be_bytes());
    } else {
        out.push(markers[2]);
        out.extend_from_slice(&(length as u32).to_be_bytes());
    }
}

fn write_uint(out: &mut Vec<u8>, value: u64) {
    match value {
        0..=0x7f => out.push(value as u8),
        0x80..=0xff => out.extend_from_slice(&[0xcc, value as u8]),
        0x100..=0xffff => {
            out.push(0xcd);
            out.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xce);
            out.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            out.push(0xcf);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn write_int(out: &mut Vec<u8>, value: i64) {
    if let Ok(unsigned) = u64::try_from(value) {
        return write_uint(out, unsigned);
    }
    match value {
        -32..=-1 => out.push(value as u8),
        -0x80..=-33 => out.extend_from_slice(&[0xd0, value as u8]),
        -0x8000..=-0x81 => {
            out.push(0xd1);
            out.extend_from_slice(&(value as i16).to_be_bytes());
        }
        -0x8000_0000..=-0x8001 => {
            out.push(0xd2);
            out.extend_from_slice(&(value as i32).to_be_bytes());
        }
        _ => {
            out.push(0xd3);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

impl Value {
    pub(crate) fn as_uint(&self) -> Option<u64> {
        match self {
            Value::Uint(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn as_int(&self) -> Option<i64> {
        match self {
            Value::Uint(value) => i64::try_from(*value).ok(),
            Value::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// A float, or an integer a float holds exactly, as JavaScript writes 3.0.
    #[allow(dead_code, reason = "float fields arrive with metrics; the vectors test it now")]
    pub(crate) fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(value) => Some(*value),
            Value::Uint(value) => {
                exact(*value as f64, |float| float as u64 == *value && float < 18_446_744_073_709_551_616.0)
            }
            Value::Int(value) => exact(*value as f64, |float| float as i64 == *value),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }

    pub(crate) fn as_bin(&self) -> Option<&[u8]> {
        match self {
            Value::Bin(bytes) => Some(bytes),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }
}

#[allow(dead_code, reason = "as_float's helper")]
fn exact(float: f64, round_trips: impl Fn(f64) -> bool) -> Option<f64> {
    round_trips(float).then_some(float)
}

#[cfg(test)]
#[path = "msgpack_tests.rs"]
mod tests;
