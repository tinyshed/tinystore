//! A frame: a twelve-byte header and a body, everything little-endian.
//!
//! ```text
//! 0             4      5       6          8             12
//! │ body length │ kind │ flags │ method   │ stream      │ body …
//!   u32           u8     u8      u16        u32
//! ```

use super::msgpack::Refused;

pub(crate) const HEADER: usize = 12;

/// END: the sender's last frame on its stream. ERROR, only beside END: the
/// body is an error.
pub(crate) const END: u8 = 1;
pub(crate) const ERROR: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Hello = 1,
    Welcome = 2,
    Request = 3,
    Response = 4,
    Data = 5,
    Cancel = 6,
    Credit = 7,
    Ping = 8,
    Pong = 9,
    GoAway = 10,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) kind: Kind,
    pub(crate) flags: u8,
    pub(crate) method: u16,
    pub(crate) stream: u32,
    pub(crate) body: Vec<u8>,
}

impl Frame {
    pub(crate) fn new(kind: Kind, stream: u32, body: Vec<u8>) -> Frame {
        Frame { kind, flags: 0, method: 0, stream, body }
    }

    pub(crate) fn ending(mut self) -> Frame {
        self.flags |= END;
        self
    }

    pub(crate) fn failing(mut self) -> Frame {
        self.flags |= END | ERROR;
        self
    }

    pub(crate) fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.body.len() as u32).to_le_bytes());
        out.push(self.kind as u8);
        out.push(self.flags);
        out.extend_from_slice(&self.method.to_le_bytes());
        out.extend_from_slice(&self.stream.to_le_bytes());
        out.extend_from_slice(&self.body);
    }
}

/// Frames read from a byte stream that arrives in pieces of any size.
#[derive(Debug, Default)]
pub(crate) struct Reader {
    buffer: Vec<u8>,
}

impl Reader {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole frame, `None` until its bytes have all arrived. A frame
    /// that breaks a rule is refused before a byte of its body is kept, and the
    /// connection ends.
    pub(crate) fn next(&mut self, max_body: usize) -> Result<Option<Frame>, Refused> {
        if self.buffer.len() < HEADER {
            return Ok(None);
        }
        let header: [u8; HEADER] = self.buffer[..HEADER].try_into().expect("a header is twelve bytes");
        let header = check(&header, max_body)?;
        let end = HEADER + header.length;
        if self.buffer.len() < end {
            return Ok(None);
        }
        let body = self.buffer[HEADER..end].to_vec();
        self.buffer.drain(..end);
        let Header { kind, flags, method, stream, .. } = header;
        Ok(Some(Frame { kind, flags, method, stream, body }))
    }
}

/// A frame's header, checked, before its body has arrived.
struct Header {
    kind: Kind,
    flags: u8,
    method: u16,
    stream: u32,
    length: usize,
}

/// Reads a header and checks it against the rules of its kind.
fn check(header: &[u8; HEADER], max_body: usize) -> Result<Header, Refused> {
    let length = u32::from_le_bytes(header[0..4].try_into().expect("four bytes")) as usize;
    let flags = header[5];
    let method = u16::from_le_bytes(header[6..8].try_into().expect("two bytes"));
    let stream = u32::from_le_bytes(header[8..12].try_into().expect("four bytes"));
    let kind = kind_of(header[4])?;
    if length > max_body {
        return Err(Refused(format!("a body of {length} bytes past the agreed {max_body}")));
    }
    check_flags(kind, flags)?;
    check_method(kind, method)?;
    check_stream(kind, stream)?;
    check_length(kind, length)?;
    Ok(Header { kind, flags, method, stream, length })
}

fn kind_of(byte: u8) -> Result<Kind, Refused> {
    Ok(match byte {
        1 => Kind::Hello,
        2 => Kind::Welcome,
        3 => Kind::Request,
        4 => Kind::Response,
        5 => Kind::Data,
        6 => Kind::Cancel,
        7 => Kind::Credit,
        8 => Kind::Ping,
        9 => Kind::Pong,
        10 => Kind::GoAway,
        other => return Err(Refused(format!("a kind nobody defined, {other}"))),
    })
}

fn check_flags(kind: Kind, flags: u8) -> Result<(), Refused> {
    let allowed = match kind {
        Kind::Request => END,
        Kind::Response | Kind::Data => END | ERROR,
        _ => 0,
    };
    if flags & !allowed != 0 {
        return Err(Refused(format!("a flag a {kind:?} does not define")));
    }
    if flags & ERROR != 0 && flags & END == 0 {
        return Err(Refused("ERROR without END".to_owned()));
    }
    Ok(())
}

fn check_method(kind: Kind, method: u16) -> Result<(), Refused> {
    match (kind, method) {
        (Kind::Request, 0) => Err(Refused("a REQUEST without a method".to_owned())),
        (Kind::Request, _) | (_, 0) => Ok(()),
        _ => Err(Refused(format!("a method on a {kind:?}"))),
    }
}

fn check_stream(kind: Kind, stream: u32) -> Result<(), Refused> {
    let on_stream = matches!(kind, Kind::Request | Kind::Response | Kind::Data | Kind::Cancel);
    let connection = matches!(kind, Kind::Hello | Kind::Welcome | Kind::Ping | Kind::Pong | Kind::GoAway);
    if on_stream && stream == 0 {
        return Err(Refused(format!("a {kind:?} on stream 0")));
    }
    if connection && stream != 0 {
        return Err(Refused(format!("a {kind:?} on a stream")));
    }
    Ok(())
}

fn check_length(kind: Kind, length: usize) -> Result<(), Refused> {
    let fixed = match kind {
        Kind::Credit => Some(4),
        Kind::Ping | Kind::Pong => Some(8),
        Kind::Cancel => Some(0),
        _ => None,
    };
    match fixed {
        Some(fixed) if fixed != length => Err(Refused(format!("a {kind:?} of {length} bytes, not {fixed}"))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value as Json;

    use super::*;

    fn vectors() -> Json {
        serde_json::from_str(include_str!("../../../../testdata/wire/vectors.json")).unwrap()
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
    }

    #[test]
    fn every_frame_vector_reads_back_its_header_and_body() {
        for vector in vectors()["frames"].as_array().unwrap() {
            let name = vector["name"].as_str().unwrap();
            let bytes = hex(vector["hex"].as_str().unwrap());
            let mut reader = Reader::default();
            // pieces of three bytes, as a socket may hand them over
            let mut frame = None;
            for piece in bytes.chunks(3) {
                reader.push(piece);
                frame = frame.or(reader.next(1 << 20).unwrap_or_else(|refusal| panic!("{name}: {refusal}")));
            }
            let frame = frame.unwrap_or_else(|| panic!("{name}: no frame"));
            let header = &vector["header"];
            assert_eq!(frame.kind as u64, header["kind"].as_u64().unwrap(), "{name}");
            assert_eq!(u64::from(frame.flags), header["flags"].as_u64().unwrap(), "{name}");
            assert_eq!(u64::from(frame.method), header["method"].as_u64().unwrap(), "{name}");
            assert_eq!(u64::from(frame.stream), header["stream"].as_u64().unwrap(), "{name}");
            let mut encoded = Vec::new();
            frame.encode_into(&mut encoded);
            assert_eq!(encoded, bytes, "{name}: encodes back to its bytes");
        }
    }

    #[test]
    fn every_refused_frame_is_refused_from_its_header() {
        for vector in vectors()["refused frames"].as_array().unwrap() {
            let name = vector["name"].as_str().unwrap();
            let max_body = vector.get("max body").and_then(Json::as_u64).unwrap_or(1 << 20) as usize;
            let mut reader = Reader::default();
            reader.push(&hex(vector["hex"].as_str().unwrap()));
            assert!(reader.next(max_body).is_err(), "{name}: accepted");
        }
    }
}
