// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.

use std::collections::BTreeMap;

use super::codec::{self, Map, Message, Row};
#[cfg(feature = "sql")]
use super::codec::Cell;
use super::msgpack::Reader;

/// Opens a set of files by name, made the first time.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    /// The term of every file written without its own. Milliseconds.
    pub(crate) ttl: Option<u64>,
    /// The largest file a write may leave.
    pub(crate) max_file_size: Option<u64>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsOpen {
    const NAME: &'static str = "blobs.Open";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.ttl = Some(codec::uint(r, "ttl")?),
                3 => message.max_file_size = Some(codec::uint(r, "maxFileSize")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if let Some(ttl) = &self.ttl {
            map.field(out, 2);
            codec::write_uint(out, ttl);
        }
        if let Some(max_file_size) = &self.max_file_size {
            map.field(out, 3);
            codec::write_uint(out, max_file_size);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.ttl.map_or(0, |_| 10)
            + self.max_file_size.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.ttl.is_none()
            && self.max_file_size.is_none()
    }
}

/// Where a call stands: the files, a folder's segments, a path within it.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsAt {
    pub(crate) handle: u64,
    pub(crate) folder: Vec<String>,
    pub(crate) path: String,
}

#[cfg(feature = "blobs")]
impl Message for BlobsAt {
    const NAME: &'static str = "blobs.At";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.folder = codec::list(r, "folder", codec::str)?,
                3 => message.path = codec::str(r, "path")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.folder.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.folder, codec::write_str);
        }
        if !self.path.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.path);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.folder, |item| 5 + item.len())
            + 1 + 5 + self.path.len()
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.folder.is_empty()
            && self.path.is_empty()
    }
}

/// What a file carries: what serving it needs.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsInfo {
    /// Within the folder the call stood in.
    pub(crate) path: String,
    pub(crate) size: u64,
    /// The SHA-256 of its bytes in hex, quoted.
    pub(crate) etag: String,
    pub(crate) content_type: String,
    /// Unix milliseconds.
    pub(crate) last_modified: i64,
    /// Unix milliseconds.
    pub(crate) expires: Option<i64>,
    pub(crate) meta: Option<BTreeMap<String, String>>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsInfo {
    const NAME: &'static str = "blobs.Info";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.path = codec::str(r, "path")?,
                2 => message.size = codec::uint(r, "size")?,
                3 => message.etag = codec::str(r, "etag")?,
                4 => message.content_type = codec::str(r, "contentType")?,
                5 => message.last_modified = codec::int(r, "lastModified")?,
                6 => message.expires = Some(codec::int(r, "expires")?),
                7 => message.meta = Some(codec::names(r, "meta", codec::str)?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.path.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.path);
        }
        if self.size != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.size);
        }
        if !self.etag.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.etag);
        }
        if !self.content_type.is_empty() {
            map.field(out, 4);
            codec::write_str(out, &self.content_type);
        }
        if self.last_modified != 0 {
            map.field(out, 5);
            codec::write_int(out, &self.last_modified);
        }
        if let Some(expires) = &self.expires {
            map.field(out, 6);
            codec::write_int(out, expires);
        }
        if let Some(meta) = &self.meta {
            map.field(out, 7);
            codec::write_names(out, meta, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.path.len()
            + 10
            + 1 + 5 + self.etag.len()
            + 1 + 5 + self.content_type.len()
            + 10
            + self.expires.map_or(0, |_| 10)
            + self.meta.as_ref().map_or(0, |meta| 1 + codec::names_size(meta, |item| 5 + item.len()))
    }

    fn is_zero(&self) -> bool {
        self.path.is_empty()
            && self.size == 0
            && self.etag.is_empty()
            && self.content_type.is_empty()
            && self.last_modified == 0
            && self.expires.is_none()
            && self.meta.is_none()
    }
}

/// A write of a whole file: a put, or with create a write only where no file
/// is. Its bytes are in the message, or, for an upload, the DATA that follow.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsWrite {
    pub(crate) handle: u64,
    pub(crate) folder: Vec<String>,
    pub(crate) path: String,
    pub(crate) content_type: Option<String>,
    pub(crate) meta: Option<BTreeMap<String, String>>,
    /// The file's own term, from its write. Milliseconds.
    pub(crate) ttl: Option<u64>,
    /// The ETag of the one file it may replace.
    pub(crate) if_match: Option<String>,
    /// The body's length: one longer or shorter is refused.
    pub(crate) size: Option<u64>,
    /// Writes only where no file is.
    pub(crate) create: bool,
    /// A put's whole body; nothing for an upload.
    pub(crate) bytes: Vec<u8>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsWrite {
    const NAME: &'static str = "blobs.Write";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.folder = codec::list(r, "folder", codec::str)?,
                3 => message.path = codec::str(r, "path")?,
                4 => message.content_type = Some(codec::str(r, "contentType")?),
                5 => message.meta = Some(codec::names(r, "meta", codec::str)?),
                6 => message.ttl = Some(codec::uint(r, "ttl")?),
                7 => message.if_match = Some(codec::str(r, "ifMatch")?),
                8 => message.size = Some(codec::uint(r, "size")?),
                9 => message.create = codec::bool(r, "create")?,
                10 => message.bytes = codec::bin(r, "bytes")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.folder.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.folder, codec::write_str);
        }
        if !self.path.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.path);
        }
        if let Some(content_type) = &self.content_type {
            map.field(out, 4);
            codec::write_str(out, content_type);
        }
        if let Some(meta) = &self.meta {
            map.field(out, 5);
            codec::write_names(out, meta, codec::write_str);
        }
        if let Some(ttl) = &self.ttl {
            map.field(out, 6);
            codec::write_uint(out, ttl);
        }
        if let Some(if_match) = &self.if_match {
            map.field(out, 7);
            codec::write_str(out, if_match);
        }
        if let Some(size) = &self.size {
            map.field(out, 8);
            codec::write_uint(out, size);
        }
        if self.create {
            map.field(out, 9);
            codec::write_bool(out, &self.create);
        }
        if !self.bytes.is_empty() {
            map.field(out, 10);
            codec::write_bin(out, &self.bytes);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.folder, |item| 5 + item.len())
            + 1 + 5 + self.path.len()
            + self.content_type.as_ref().map_or(0, |content_type| 1 + 5 + content_type.len())
            + self.meta.as_ref().map_or(0, |meta| 1 + codec::names_size(meta, |item| 5 + item.len()))
            + self.ttl.map_or(0, |_| 10)
            + self.if_match.as_ref().map_or(0, |if_match| 1 + 5 + if_match.len())
            + self.size.map_or(0, |_| 10)
            + 2
            + 1 + 5 + self.bytes.len()
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.folder.is_empty()
            && self.path.is_empty()
            && self.content_type.is_none()
            && self.meta.is_none()
            && self.ttl.is_none()
            && self.if_match.is_none()
            && self.size.is_none()
            && !self.create
            && self.bytes.is_empty()
    }
}

/// What a write wrote: none when a create found a file there.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsWritten {
    pub(crate) info: Option<BlobsInfo>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsWritten {
    const NAME: &'static str = "blobs.Written";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.info = Some(BlobsInfo::read(r)?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(info) = &self.info {
            map.field(out, 1);
            codec::write_message(out, info);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.info.as_ref().map_or(0, |info| 1 + info.size())
    }

    fn is_zero(&self) -> bool {
        self.info.is_none()
    }
}

/// A piece of a file's bytes: an upload's DATA from the client, a read's from
/// the server.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsPiece {
    pub(crate) bytes: Vec<u8>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsPiece {
    const NAME: &'static str = "blobs.Piece";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.bytes = codec::bin(r, "bytes")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.bytes.is_empty() {
            map.field(out, 1);
            codec::write_bin(out, &self.bytes);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.bytes.len()
    }

    fn is_zero(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// A get's answer: what the file carries, none when there is no file, and all
/// its bytes when they fit one message; a larger file's are left to blobs.read,
/// so that a read of a range sends no byte before it.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsGot {
    pub(crate) info: Option<BlobsInfo>,
    /// The whole file, or nothing when it is larger.
    pub(crate) bytes: Vec<u8>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsGot {
    const NAME: &'static str = "blobs.Got";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.info = Some(BlobsInfo::read(r)?),
                2 => message.bytes = codec::bin(r, "bytes")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(info) = &self.info {
            map.field(out, 1);
            codec::write_message(out, info);
        }
        if !self.bytes.is_empty() {
            map.field(out, 2);
            codec::write_bin(out, &self.bytes);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.info.as_ref().map_or(0, |info| 1 + info.size())
            + 1 + 5 + self.bytes.len()
    }

    fn is_zero(&self) -> bool {
        self.info.is_none()
            && self.bytes.is_empty()
    }
}

/// A read of a file's bytes, as S3's GET with a range and If-Match: the whole
/// file, checked, its last piece sent once its bytes matched their SHA-256 and a
/// changed byte ending the stream corrupt instead; or a range, not checked.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsRead {
    pub(crate) handle: u64,
    pub(crate) folder: Vec<String>,
    pub(crate) path: String,
    /// The ETag a get gave: another file there, or none, is a conflict.
    pub(crate) if_match: Option<String>,
    /// From the first byte when absent.
    pub(crate) offset: Option<u64>,
    /// To the last byte when absent.
    pub(crate) length: Option<u64>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsRead {
    const NAME: &'static str = "blobs.Read";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.folder = codec::list(r, "folder", codec::str)?,
                3 => message.path = codec::str(r, "path")?,
                4 => message.if_match = Some(codec::str(r, "ifMatch")?),
                5 => message.offset = Some(codec::uint(r, "offset")?),
                6 => message.length = Some(codec::uint(r, "length")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.folder.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.folder, codec::write_str);
        }
        if !self.path.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.path);
        }
        if let Some(if_match) = &self.if_match {
            map.field(out, 4);
            codec::write_str(out, if_match);
        }
        if let Some(offset) = &self.offset {
            map.field(out, 5);
            codec::write_uint(out, offset);
        }
        if let Some(length) = &self.length {
            map.field(out, 6);
            codec::write_uint(out, length);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.folder, |item| 5 + item.len())
            + 1 + 5 + self.path.len()
            + self.if_match.as_ref().map_or(0, |if_match| 1 + 5 + if_match.len())
            + self.offset.map_or(0, |_| 10)
            + self.length.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.folder.is_empty()
            && self.path.is_empty()
            && self.if_match.is_none()
            && self.offset.is_none()
            && self.length.is_none()
    }
}

#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsHead {
    pub(crate) info: Option<BlobsInfo>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsHead {
    const NAME: &'static str = "blobs.Head";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.info = Some(BlobsInfo::read(r)?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(info) = &self.info {
            map.field(out, 1);
            codec::write_message(out, info);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.info.as_ref().map_or(0, |info| 1 + info.size())
    }

    fn is_zero(&self) -> bool {
        self.info.is_none()
    }
}

#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsDelete {
    pub(crate) handle: u64,
    pub(crate) folder: Vec<String>,
    pub(crate) path: String,
    pub(crate) if_match: Option<String>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsDelete {
    const NAME: &'static str = "blobs.Delete";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.folder = codec::list(r, "folder", codec::str)?,
                3 => message.path = codec::str(r, "path")?,
                4 => message.if_match = Some(codec::str(r, "ifMatch")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.folder.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.folder, codec::write_str);
        }
        if !self.path.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.path);
        }
        if let Some(if_match) = &self.if_match {
            map.field(out, 4);
            codec::write_str(out, if_match);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.folder, |item| 5 + item.len())
            + 1 + 5 + self.path.len()
            + self.if_match.as_ref().map_or(0, |if_match| 1 + 5 + if_match.len())
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.folder.is_empty()
            && self.path.is_empty()
            && self.if_match.is_none()
    }
}

/// A copy or a rename: from a path to another of the same folder. A file at
/// to is a conflict, unless ifMatch names the version to replace.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsMove {
    pub(crate) handle: u64,
    pub(crate) folder: Vec<String>,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) if_match: Option<String>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsMove {
    const NAME: &'static str = "blobs.Move";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.folder = codec::list(r, "folder", codec::str)?,
                3 => message.from = codec::str(r, "from")?,
                4 => message.to = codec::str(r, "to")?,
                5 => message.if_match = Some(codec::str(r, "ifMatch")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.folder.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.folder, codec::write_str);
        }
        if !self.from.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.from);
        }
        if !self.to.is_empty() {
            map.field(out, 4);
            codec::write_str(out, &self.to);
        }
        if let Some(if_match) = &self.if_match {
            map.field(out, 5);
            codec::write_str(out, if_match);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.folder, |item| 5 + item.len())
            + 1 + 5 + self.from.len()
            + 1 + 5 + self.to.len()
            + self.if_match.as_ref().map_or(0, |if_match| 1 + 5 + if_match.len())
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.folder.is_empty()
            && self.from.is_empty()
            && self.to.is_empty()
            && self.if_match.is_none()
    }
}

#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsExpire {
    pub(crate) handle: u64,
    pub(crate) folder: Vec<String>,
    pub(crate) path: String,
    /// Milliseconds.
    pub(crate) after: u64,
}

#[cfg(feature = "blobs")]
impl Message for BlobsExpire {
    const NAME: &'static str = "blobs.Expire";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.folder = codec::list(r, "folder", codec::str)?,
                3 => message.path = codec::str(r, "path")?,
                4 => message.after = codec::uint(r, "after")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.folder.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.folder, codec::write_str);
        }
        if !self.path.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.path);
        }
        if self.after != 0 {
            map.field(out, 4);
            codec::write_uint(out, &self.after);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.folder, |item| 5 + item.len())
            + 1 + 5 + self.path.len()
            + 10
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.folder.is_empty()
            && self.path.is_empty()
            && self.after == 0
    }
}

#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsFound {
    pub(crate) found: bool,
}

#[cfg(feature = "blobs")]
impl Message for BlobsFound {
    const NAME: &'static str = "blobs.Found";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.found = codec::bool(r, "found")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.found {
            map.field(out, 1);
            codec::write_bool(out, &self.found);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
    }

    fn is_zero(&self) -> bool {
        !self.found
    }
}

/// A page of a folder's files, in the byte order of their paths.
#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsList {
    pub(crate) handle: u64,
    pub(crate) folder: Vec<String>,
    /// Text within the folder.
    pub(crate) prefix: String,
    /// A page's next.
    pub(crate) after: Option<String>,
    /// 1000 at most, and when absent.
    pub(crate) limit: Option<u64>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsList {
    const NAME: &'static str = "blobs.List";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.folder = codec::list(r, "folder", codec::str)?,
                3 => message.prefix = codec::str(r, "prefix")?,
                4 => message.after = Some(codec::str(r, "after")?),
                5 => message.limit = Some(codec::uint(r, "limit")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.folder.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.folder, codec::write_str);
        }
        if !self.prefix.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.prefix);
        }
        if let Some(after) = &self.after {
            map.field(out, 4);
            codec::write_str(out, after);
        }
        if let Some(limit) = &self.limit {
            map.field(out, 5);
            codec::write_uint(out, limit);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.folder, |item| 5 + item.len())
            + 1 + 5 + self.prefix.len()
            + self.after.as_ref().map_or(0, |after| 1 + 5 + after.len())
            + self.limit.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.folder.is_empty()
            && self.prefix.is_empty()
            && self.after.is_none()
            && self.limit.is_none()
    }
}

#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsPage {
    pub(crate) files: Vec<BlobsInfo>,
    pub(crate) next: Option<String>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsPage {
    const NAME: &'static str = "blobs.Page";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.files = codec::list(r, "files", codec::message::<BlobsInfo>)?,
                2 => message.next = Some(codec::str(r, "next")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.files.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.files, codec::write_message);
        }
        if let Some(next) = &self.next {
            map.field(out, 2);
            codec::write_str(out, next);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.files, codec::message_size)
            + self.next.as_ref().map_or(0, |next| 1 + 5 + next.len())
    }

    fn is_zero(&self) -> bool {
        self.files.is_empty()
            && self.next.is_none()
    }
}

#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsFolder {
    pub(crate) handle: u64,
    pub(crate) folder: Vec<String>,
}

#[cfg(feature = "blobs")]
impl Message for BlobsFolder {
    const NAME: &'static str = "blobs.Folder";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.folder = codec::list(r, "folder", codec::str)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.folder.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.folder, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.folder, |item| 5 + item.len())
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.folder.is_empty()
    }
}

#[cfg(feature = "blobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BlobsUsage {
    pub(crate) count: u64,
    pub(crate) size: u64,
}

#[cfg(feature = "blobs")]
impl Message for BlobsUsage {
    const NAME: &'static str = "blobs.Usage";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.count = codec::uint(r, "count")?,
                2 => message.size = codec::uint(r, "size")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.count != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.count);
        }
        if self.size != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.size);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
    }

    fn is_zero(&self) -> bool {
        self.count == 0
            && self.size == 0
    }
}

/// What a client says first.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Hello {
    /// The newest the client speaks.
    pub(crate) protocol: u64,
    /// A name and version, for logs.
    pub(crate) client: String,
    /// Required on TCP.
    pub(crate) token: Option<String>,
    /// The largest body the client takes; the server's when absent.
    pub(crate) max_body: Option<u64>,
    /// The DATA the server may send a stream before the client grants more; 2 MiB when absent.
    pub(crate) stream_credit: Option<u64>,
    /// 16 random bytes, when the client found the server through SERVE.
    pub(crate) challenge: Option<Vec<u8>>,
}

impl Message for Hello {
    const NAME: &'static str = "Hello";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.protocol = codec::uint(r, "protocol")?,
                2 => message.client = codec::str(r, "client")?,
                3 => message.token = Some(codec::str(r, "token")?),
                4 => message.max_body = Some(codec::uint(r, "maxBody")?),
                5 => message.stream_credit = Some(codec::uint(r, "streamCredit")?),
                6 => message.challenge = Some(codec::bin(r, "challenge")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.protocol != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.protocol);
        }
        if !self.client.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.client);
        }
        if let Some(token) = &self.token {
            map.field(out, 3);
            codec::write_str(out, token);
        }
        if let Some(max_body) = &self.max_body {
            map.field(out, 4);
            codec::write_uint(out, max_body);
        }
        if let Some(stream_credit) = &self.stream_credit {
            map.field(out, 5);
            codec::write_uint(out, stream_credit);
        }
        if let Some(challenge) = &self.challenge {
            map.field(out, 6);
            codec::write_bin(out, challenge);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.client.len()
            + self.token.as_ref().map_or(0, |token| 1 + 5 + token.len())
            + self.max_body.map_or(0, |_| 10)
            + self.stream_credit.map_or(0, |_| 10)
            + self.challenge.as_ref().map_or(0, |challenge| 1 + 5 + challenge.len())
    }

    fn is_zero(&self) -> bool {
        self.protocol == 0
            && self.client.is_empty()
            && self.token.is_none()
            && self.max_body.is_none()
            && self.stream_credit.is_none()
            && self.challenge.is_none()
    }
}

/// What a server answers HELLO with: what the connection agrees.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Welcome {
    /// The one this connection speaks.
    pub(crate) protocol: u64,
    /// Its version.
    pub(crate) server: String,
    /// 16 random bytes a start.
    pub(crate) instance: Vec<u8>,
    /// Admin or data.
    pub(crate) capability: String,
    /// The largest body either side sends.
    pub(crate) max_body: u64,
    /// The streams a client may have open at once.
    pub(crate) in_flight: u64,
    /// The REQUEST and DATA bytes a client may send before credit comes back.
    pub(crate) connection_credit: u64,
    /// The DATA bytes a client may send a stream before credit comes back.
    pub(crate) stream_credit: u64,
    /// What this server serves.
    pub(crate) engines: Vec<String>,
    /// The store's clock. Unix milliseconds.
    pub(crate) now: i64,
    /// The HMAC-SHA256 of HELLO's challenge, keyed with SERVE's secret.
    pub(crate) proof: Option<Vec<u8>>,
    /// Full or os, when the store was opened with one: how far its files' commits go.
    pub(crate) durability: Option<String>,
}

impl Message for Welcome {
    const NAME: &'static str = "Welcome";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.protocol = codec::uint(r, "protocol")?,
                2 => message.server = codec::str(r, "server")?,
                3 => message.instance = codec::bin(r, "instance")?,
                4 => message.capability = codec::str(r, "capability")?,
                5 => message.max_body = codec::uint(r, "maxBody")?,
                6 => message.in_flight = codec::uint(r, "inFlight")?,
                7 => message.connection_credit = codec::uint(r, "connectionCredit")?,
                8 => message.stream_credit = codec::uint(r, "streamCredit")?,
                9 => message.engines = codec::list(r, "engines", codec::str)?,
                10 => message.now = codec::int(r, "now")?,
                11 => message.proof = Some(codec::bin(r, "proof")?),
                12 => message.durability = Some(codec::str(r, "durability")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.protocol != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.protocol);
        }
        if !self.server.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.server);
        }
        if !self.instance.is_empty() {
            map.field(out, 3);
            codec::write_bin(out, &self.instance);
        }
        if !self.capability.is_empty() {
            map.field(out, 4);
            codec::write_str(out, &self.capability);
        }
        if self.max_body != 0 {
            map.field(out, 5);
            codec::write_uint(out, &self.max_body);
        }
        if self.in_flight != 0 {
            map.field(out, 6);
            codec::write_uint(out, &self.in_flight);
        }
        if self.connection_credit != 0 {
            map.field(out, 7);
            codec::write_uint(out, &self.connection_credit);
        }
        if self.stream_credit != 0 {
            map.field(out, 8);
            codec::write_uint(out, &self.stream_credit);
        }
        if !self.engines.is_empty() {
            map.field(out, 9);
            codec::write_list(out, &self.engines, codec::write_str);
        }
        if self.now != 0 {
            map.field(out, 10);
            codec::write_int(out, &self.now);
        }
        if let Some(proof) = &self.proof {
            map.field(out, 11);
            codec::write_bin(out, proof);
        }
        if let Some(durability) = &self.durability {
            map.field(out, 12);
            codec::write_str(out, durability);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.server.len()
            + 1 + 5 + self.instance.len()
            + 1 + 5 + self.capability.len()
            + 10
            + 10
            + 10
            + 10
            + 1 + codec::list_size(&self.engines, |item| 5 + item.len())
            + 10
            + self.proof.as_ref().map_or(0, |proof| 1 + 5 + proof.len())
            + self.durability.as_ref().map_or(0, |durability| 1 + 5 + durability.len())
    }

    fn is_zero(&self) -> bool {
        self.protocol == 0
            && self.server.is_empty()
            && self.instance.is_empty()
            && self.capability.is_empty()
            && self.max_body == 0
            && self.in_flight == 0
            && self.connection_credit == 0
            && self.stream_credit == 0
            && self.engines.is_empty()
            && self.now == 0
            && self.proof.is_none()
            && self.durability.is_none()
    }
}

/// How a host opens the store in its own process: the pipe's open takes it,
/// and a server reads the same from its command line.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StoreOptions {
    /// Full or os: how far a commit of the store's files goes before it returns; each engine's own when absent.
    pub(crate) durability: Option<String>,
}

impl Message for StoreOptions {
    const NAME: &'static str = "store.Options";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.durability = Some(codec::str(r, "durability")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(durability) = &self.durability {
            map.field(out, 1);
            codec::write_str(out, durability);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.durability.as_ref().map_or(0, |durability| 1 + 5 + durability.len())
    }

    fn is_zero(&self) -> bool {
        self.durability.is_none()
    }
}

/// The last frame of a connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GoAway {
    pub(crate) code: String,
    pub(crate) message: String,
}

impl Message for GoAway {
    const NAME: &'static str = "GoAway";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.code = codec::str(r, "code")?,
                2 => message.message = codec::str(r, "message")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.code.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.code);
        }
        if !self.message.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.code.len()
            + 1 + 5 + self.message.len()
    }

    fn is_zero(&self) -> bool {
        self.code.is_empty()
            && self.message.is_empty()
    }
}

/// A stream's last frame when it failed, with ERROR set: its kind's code, what
/// failed, and the item it names. A message refused is one too.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Failure {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) what: Option<BTreeMap<String, String>>,
}

impl Message for Failure {
    const NAME: &'static str = "Failure";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.code = codec::str(r, "code")?,
                2 => message.message = codec::str(r, "message")?,
                3 => message.what = Some(codec::names(r, "what", codec::str)?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.code.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.code);
        }
        if !self.message.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.message);
        }
        if let Some(what) = &self.what {
            map.field(out, 3);
            codec::write_names(out, what, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.code.len()
            + 1 + 5 + self.message.len()
            + self.what.as_ref().map_or(0, |what| 1 + codec::names_size(what, |item| 5 + item.len()))
    }

    fn is_zero(&self) -> bool {
        self.code.is_empty()
            && self.message.is_empty()
            && self.what.is_none()
    }
}

/// What a call that opens something answers: its handle, which later calls name.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Handle {
    pub(crate) handle: u64,
}

impl Message for Handle {
    const NAME: &'static str = "Handle";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
    }
}

/// A message with nothing to say.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Empty {
}

impl Message for Empty {
    const NAME: &'static str = "Empty";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        match codec::fields(r, Self::NAME)? {
            0 => Ok(Self {}),
            _ => Err(codec::unknown(codec::field(r, Self::NAME, &mut 0)?, Self::NAME)),
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        Map::open(out).close(out);
    }

    fn size(&self) -> usize {
        3
    }

    fn is_zero(&self) -> bool {
        true
    }
}

/// How many jobs of a queue run at once: in all, across every worker of the
/// store, and in each group.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsConcurrency {
    pub(crate) total: Option<u64>,
    pub(crate) group: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsConcurrency {
    const NAME: &'static str = "jobs.Concurrency";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.total = Some(codec::uint(r, "total")?),
                2 => message.group = Some(codec::uint(r, "group")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(total) = &self.total {
            map.field(out, 1);
            codec::write_uint(out, total);
        }
        if let Some(group) = &self.group {
            map.field(out, 2);
            codec::write_uint(out, group);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.total.map_or(0, |_| 10)
            + self.group.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.total.is_none()
            && self.group.is_none()
    }
}

/// The wait before a retry: initial, doubling each time up to max.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsBackoff {
    /// Milliseconds.
    pub(crate) initial: u64,
    /// Milliseconds.
    pub(crate) max: u64,
}

#[cfg(feature = "jobs")]
impl Message for JobsBackoff {
    const NAME: &'static str = "jobs.Backoff";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.initial = codec::uint(r, "initial")?,
                2 => message.max = codec::uint(r, "max")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.initial != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.initial);
        }
        if self.max != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.max);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
    }

    fn is_zero(&self) -> bool {
        self.initial == 0
            && self.max == 0
    }
}

/// Jobs started in any span of per.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsRate {
    pub(crate) count: u64,
    /// Milliseconds.
    pub(crate) per: u64,
}

#[cfg(feature = "jobs")]
impl Message for JobsRate {
    const NAME: &'static str = "jobs.Rate";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.count = codec::uint(r, "count")?,
                2 => message.per = codec::uint(r, "per")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.count != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.count);
        }
        if self.per != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.per);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
    }

    fn is_zero(&self) -> bool {
        self.count == 0
            && self.per == 0
    }
}

/// Opens a queue; an option left out takes its default.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsQueueOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    /// 10 when absent, the first run counted.
    pub(crate) attempts: Option<u64>,
    /// 1 s doubling to 1 h when absent.
    pub(crate) backoff: Option<JobsBackoff>,
    /// How long one run may take: a minute when absent. Milliseconds.
    pub(crate) timeout: Option<u64>,
    /// One at a time in each worker when absent.
    pub(crate) concurrency: Option<JobsConcurrency>,
    pub(crate) rate: Option<JobsRate>,
    /// How long a done job's id stays taken. Milliseconds.
    pub(crate) dedupe: Option<u64>,
    /// How long a failed job stays: a week when absent. Milliseconds.
    pub(crate) keep: Option<u64>,
    /// Ten million when absent.
    pub(crate) max_waiting: Option<u64>,
    /// A database's handle: the queue lives in its file; jobs.db when absent.
    pub(crate) database: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsQueueOpen {
    const NAME: &'static str = "jobs.QueueOpen";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.attempts = Some(codec::uint(r, "attempts")?),
                3 => message.backoff = Some(JobsBackoff::read(r)?),
                4 => message.timeout = Some(codec::uint(r, "timeout")?),
                5 => message.concurrency = Some(JobsConcurrency::read(r)?),
                6 => message.rate = Some(JobsRate::read(r)?),
                7 => message.dedupe = Some(codec::uint(r, "dedupe")?),
                8 => message.keep = Some(codec::uint(r, "keep")?),
                9 => message.max_waiting = Some(codec::uint(r, "maxWaiting")?),
                10 => message.database = Some(codec::uint(r, "database")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if let Some(attempts) = &self.attempts {
            map.field(out, 2);
            codec::write_uint(out, attempts);
        }
        if let Some(backoff) = &self.backoff {
            map.field(out, 3);
            codec::write_message(out, backoff);
        }
        if let Some(timeout) = &self.timeout {
            map.field(out, 4);
            codec::write_uint(out, timeout);
        }
        if let Some(concurrency) = &self.concurrency {
            map.field(out, 5);
            codec::write_message(out, concurrency);
        }
        if let Some(rate) = &self.rate {
            map.field(out, 6);
            codec::write_message(out, rate);
        }
        if let Some(dedupe) = &self.dedupe {
            map.field(out, 7);
            codec::write_uint(out, dedupe);
        }
        if let Some(keep) = &self.keep {
            map.field(out, 8);
            codec::write_uint(out, keep);
        }
        if let Some(max_waiting) = &self.max_waiting {
            map.field(out, 9);
            codec::write_uint(out, max_waiting);
        }
        if let Some(database) = &self.database {
            map.field(out, 10);
            codec::write_uint(out, database);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.attempts.map_or(0, |_| 10)
            + self.backoff.as_ref().map_or(0, |backoff| 1 + backoff.size())
            + self.timeout.map_or(0, |_| 10)
            + self.concurrency.as_ref().map_or(0, |concurrency| 1 + concurrency.size())
            + self.rate.as_ref().map_or(0, |rate| 1 + rate.size())
            + self.dedupe.map_or(0, |_| 10)
            + self.keep.map_or(0, |_| 10)
            + self.max_waiting.map_or(0, |_| 10)
            + self.database.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.attempts.is_none()
            && self.backoff.is_none()
            && self.timeout.is_none()
            && self.concurrency.is_none()
            && self.rate.is_none()
            && self.dedupe.is_none()
            && self.keep.is_none()
            && self.max_waiting.is_none()
            && self.database.is_none()
    }
}

/// Opens a schedule: one repeating job the code owns, under its name, whose
/// repeat replaces the one kept.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsScheduleOpen {
    pub(crate) name: String,
    /// Milliseconds.
    pub(crate) every: Option<u64>,
    /// Five fields, with timeZone.
    pub(crate) cron: Option<String>,
    /// An IANA name, UTC among them.
    pub(crate) time_zone: Option<String>,
    pub(crate) attempts: Option<u64>,
    pub(crate) backoff: Option<JobsBackoff>,
    /// Milliseconds.
    pub(crate) timeout: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsScheduleOpen {
    const NAME: &'static str = "jobs.ScheduleOpen";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.every = Some(codec::uint(r, "every")?),
                3 => message.cron = Some(codec::str(r, "cron")?),
                4 => message.time_zone = Some(codec::str(r, "timeZone")?),
                5 => message.attempts = Some(codec::uint(r, "attempts")?),
                6 => message.backoff = Some(JobsBackoff::read(r)?),
                7 => message.timeout = Some(codec::uint(r, "timeout")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if let Some(every) = &self.every {
            map.field(out, 2);
            codec::write_uint(out, every);
        }
        if let Some(cron) = &self.cron {
            map.field(out, 3);
            codec::write_str(out, cron);
        }
        if let Some(time_zone) = &self.time_zone {
            map.field(out, 4);
            codec::write_str(out, time_zone);
        }
        if let Some(attempts) = &self.attempts {
            map.field(out, 5);
            codec::write_uint(out, attempts);
        }
        if let Some(backoff) = &self.backoff {
            map.field(out, 6);
            codec::write_message(out, backoff);
        }
        if let Some(timeout) = &self.timeout {
            map.field(out, 7);
            codec::write_uint(out, timeout);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.every.map_or(0, |_| 10)
            + self.cron.as_ref().map_or(0, |cron| 1 + 5 + cron.len())
            + self.time_zone.as_ref().map_or(0, |time_zone| 1 + 5 + time_zone.len())
            + self.attempts.map_or(0, |_| 10)
            + self.backoff.as_ref().map_or(0, |backoff| 1 + backoff.size())
            + self.timeout.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.every.is_none()
            && self.cron.is_none()
            && self.time_zone.is_none()
            && self.attempts.is_none()
            && self.backoff.is_none()
            && self.timeout.is_none()
    }
}

/// A call on one job: an add, a set or an update.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsCall {
    pub(crate) handle: u64,
    /// 1 to 1024 bytes; a set and an update need one.
    pub(crate) id: Option<String>,
    pub(crate) value: String,
    /// Unix milliseconds.
    pub(crate) at: Option<i64>,
    /// From now; not beside at. Milliseconds.
    pub(crate) delay: Option<u64>,
    pub(crate) group: Option<String>,
    /// A repeat, which needs an id. Milliseconds.
    pub(crate) every: Option<u64>,
    pub(crate) cron: Option<String>,
    pub(crate) time_zone: Option<String>,
}

#[cfg(feature = "jobs")]
impl Message for JobsCall {
    const NAME: &'static str = "jobs.Call";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.id = Some(codec::str(r, "id")?),
                3 => message.value = codec::str(r, "value")?,
                4 => message.at = Some(codec::int(r, "at")?),
                5 => message.delay = Some(codec::uint(r, "delay")?),
                6 => message.group = Some(codec::str(r, "group")?),
                7 => message.every = Some(codec::uint(r, "every")?),
                8 => message.cron = Some(codec::str(r, "cron")?),
                9 => message.time_zone = Some(codec::str(r, "timeZone")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if let Some(id) = &self.id {
            map.field(out, 2);
            codec::write_str(out, id);
        }
        if !self.value.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.value);
        }
        if let Some(at) = &self.at {
            map.field(out, 4);
            codec::write_int(out, at);
        }
        if let Some(delay) = &self.delay {
            map.field(out, 5);
            codec::write_uint(out, delay);
        }
        if let Some(group) = &self.group {
            map.field(out, 6);
            codec::write_str(out, group);
        }
        if let Some(every) = &self.every {
            map.field(out, 7);
            codec::write_uint(out, every);
        }
        if let Some(cron) = &self.cron {
            map.field(out, 8);
            codec::write_str(out, cron);
        }
        if let Some(time_zone) = &self.time_zone {
            map.field(out, 9);
            codec::write_str(out, time_zone);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + self.id.as_ref().map_or(0, |id| 1 + 5 + id.len())
            + 1 + 5 + self.value.len()
            + self.at.map_or(0, |_| 10)
            + self.delay.map_or(0, |_| 10)
            + self.group.as_ref().map_or(0, |group| 1 + 5 + group.len())
            + self.every.map_or(0, |_| 10)
            + self.cron.as_ref().map_or(0, |cron| 1 + 5 + cron.len())
            + self.time_zone.as_ref().map_or(0, |time_zone| 1 + 5 + time_zone.len())
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.id.is_none()
            && self.value.is_empty()
            && self.at.is_none()
            && self.delay.is_none()
            && self.group.is_none()
            && self.every.is_none()
            && self.cron.is_none()
            && self.time_zone.is_none()
    }
}

/// The job under an id.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsId {
    pub(crate) handle: u64,
    pub(crate) id: String,
}

#[cfg(feature = "jobs")]
impl Message for JobsId {
    const NAME: &'static str = "jobs.Id";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.id = codec::str(r, "id")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.id.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.id);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.id.len()
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.id.is_empty()
    }
}

/// Whether a call did what it asked: an add added, an update changed, a cancel
/// found a job.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsChanged {
    pub(crate) changed: bool,
}

#[cfg(feature = "jobs")]
impl Message for JobsChanged {
    const NAME: &'static str = "jobs.Changed";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.changed = codec::bool(r, "changed")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.changed {
            map.field(out, 1);
            codec::write_bool(out, &self.changed);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
    }

    fn is_zero(&self) -> bool {
        !self.changed
    }
}

/// A job as its queue holds it.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsJob {
    pub(crate) found: bool,
    pub(crate) id: Option<String>,
    pub(crate) value: Option<String>,
    /// Scheduled, waiting, running, done, failed or cancelled.
    pub(crate) state: String,
    /// When it runs next; for one done or failed, when its last run was for. Unix milliseconds.
    pub(crate) at: Option<i64>,
    pub(crate) attempt: u64,
    /// The jobs that run before a waiting one, up to 10,000.
    pub(crate) ahead: u64,
    pub(crate) progress: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) group: Option<String>,
    /// As the job keeps it: @every 30s +6178ms, 10 3 * * * Europe/Berlin.
    pub(crate) repeat: Option<String>,
    /// The last run a handler finished. Unix milliseconds.
    pub(crate) started_at: Option<i64>,
    /// Unix milliseconds.
    pub(crate) ended_at: Option<i64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsJob {
    const NAME: &'static str = "jobs.Job";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.found = codec::bool(r, "found")?,
                2 => message.id = Some(codec::str(r, "id")?),
                3 => message.value = Some(codec::str(r, "value")?),
                4 => message.state = codec::str(r, "state")?,
                5 => message.at = Some(codec::int(r, "at")?),
                6 => message.attempt = codec::uint(r, "attempt")?,
                7 => message.ahead = codec::uint(r, "ahead")?,
                8 => message.progress = Some(codec::str(r, "progress")?),
                9 => message.error = Some(codec::str(r, "error")?),
                10 => message.group = Some(codec::str(r, "group")?),
                11 => message.repeat = Some(codec::str(r, "repeat")?),
                12 => message.started_at = Some(codec::int(r, "startedAt")?),
                13 => message.ended_at = Some(codec::int(r, "endedAt")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.found {
            map.field(out, 1);
            codec::write_bool(out, &self.found);
        }
        if let Some(id) = &self.id {
            map.field(out, 2);
            codec::write_str(out, id);
        }
        if let Some(value) = &self.value {
            map.field(out, 3);
            codec::write_str(out, value);
        }
        if !self.state.is_empty() {
            map.field(out, 4);
            codec::write_str(out, &self.state);
        }
        if let Some(at) = &self.at {
            map.field(out, 5);
            codec::write_int(out, at);
        }
        if self.attempt != 0 {
            map.field(out, 6);
            codec::write_uint(out, &self.attempt);
        }
        if self.ahead != 0 {
            map.field(out, 7);
            codec::write_uint(out, &self.ahead);
        }
        if let Some(progress) = &self.progress {
            map.field(out, 8);
            codec::write_str(out, progress);
        }
        if let Some(error) = &self.error {
            map.field(out, 9);
            codec::write_str(out, error);
        }
        if let Some(group) = &self.group {
            map.field(out, 10);
            codec::write_str(out, group);
        }
        if let Some(repeat) = &self.repeat {
            map.field(out, 11);
            codec::write_str(out, repeat);
        }
        if let Some(started_at) = &self.started_at {
            map.field(out, 12);
            codec::write_int(out, started_at);
        }
        if let Some(ended_at) = &self.ended_at {
            map.field(out, 13);
            codec::write_int(out, ended_at);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
            + self.id.as_ref().map_or(0, |id| 1 + 5 + id.len())
            + self.value.as_ref().map_or(0, |value| 1 + 5 + value.len())
            + 1 + 5 + self.state.len()
            + self.at.map_or(0, |_| 10)
            + 10
            + 10
            + self.progress.as_ref().map_or(0, |progress| 1 + 5 + progress.len())
            + self.error.as_ref().map_or(0, |error| 1 + 5 + error.len())
            + self.group.as_ref().map_or(0, |group| 1 + 5 + group.len())
            + self.repeat.as_ref().map_or(0, |repeat| 1 + 5 + repeat.len())
            + self.started_at.map_or(0, |_| 10)
            + self.ended_at.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        !self.found
            && self.id.is_none()
            && self.value.is_none()
            && self.state.is_empty()
            && self.at.is_none()
            && self.attempt == 0
            && self.ahead == 0
            && self.progress.is_none()
            && self.error.is_none()
            && self.group.is_none()
            && self.repeat.is_none()
            && self.started_at.is_none()
            && self.ended_at.is_none()
    }
}

/// A page of a queue's jobs: those whose ids start with a prefix, in the byte
/// order of their ids, or with no prefix and the failed state, the last failed
/// first.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsList {
    pub(crate) handle: u64,
    pub(crate) prefix: Option<String>,
    /// Scheduled, waiting, running or failed.
    pub(crate) state: Option<String>,
    /// The page before's next.
    pub(crate) after: Option<String>,
    /// 100 when absent, 1000 at most.
    pub(crate) limit: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsList {
    const NAME: &'static str = "jobs.List";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.prefix = Some(codec::str(r, "prefix")?),
                3 => message.state = Some(codec::str(r, "state")?),
                4 => message.after = Some(codec::str(r, "after")?),
                5 => message.limit = Some(codec::uint(r, "limit")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if let Some(prefix) = &self.prefix {
            map.field(out, 2);
            codec::write_str(out, prefix);
        }
        if let Some(state) = &self.state {
            map.field(out, 3);
            codec::write_str(out, state);
        }
        if let Some(after) = &self.after {
            map.field(out, 4);
            codec::write_str(out, after);
        }
        if let Some(limit) = &self.limit {
            map.field(out, 5);
            codec::write_uint(out, limit);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + self.prefix.as_ref().map_or(0, |prefix| 1 + 5 + prefix.len())
            + self.state.as_ref().map_or(0, |state| 1 + 5 + state.len())
            + self.after.as_ref().map_or(0, |after| 1 + 5 + after.len())
            + self.limit.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.prefix.is_none()
            && self.state.is_none()
            && self.after.is_none()
            && self.limit.is_none()
    }
}

#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsPage {
    pub(crate) jobs: Vec<JobsJob>,
    pub(crate) next: Option<String>,
}

#[cfg(feature = "jobs")]
impl Message for JobsPage {
    const NAME: &'static str = "jobs.Page";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.jobs = codec::list(r, "jobs", codec::message::<JobsJob>)?,
                2 => message.next = Some(codec::str(r, "next")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.jobs.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.jobs, codec::write_message);
        }
        if let Some(next) = &self.next {
            map.field(out, 2);
            codec::write_str(out, next);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.jobs, codec::message_size)
            + self.next.as_ref().map_or(0, |next| 1 + 5 + next.len())
    }

    fn is_zero(&self) -> bool {
        self.jobs.is_empty()
            && self.next.is_none()
    }
}

/// Starts the queue's worker for the client: the server claims its jobs as they
/// fall due and hands each over as a held job, no more at once than the
/// client's handlers, and the client answers each. A stop, or the server's
/// GOAWAY, hands no job more, and the server ends the stream with DATA·END once
/// the jobs the client holds are answered and written. The client's DATA·END
/// ends the worker at once, the attempt of a job it still holds failing.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsWork {
    pub(crate) handle: u64,
    /// The handlers the client runs at once, 1024 at most: the queue's total, or one.
    pub(crate) concurrency: Option<u64>,
    /// RunDue: the server ends the stream once no job is due and none is held.
    pub(crate) until_idle: bool,
}

#[cfg(feature = "jobs")]
impl Message for JobsWork {
    const NAME: &'static str = "jobs.Work";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.concurrency = Some(codec::uint(r, "concurrency")?),
                3 => message.until_idle = codec::bool(r, "untilIdle")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if let Some(concurrency) = &self.concurrency {
            map.field(out, 2);
            codec::write_uint(out, concurrency);
        }
        if self.until_idle {
            map.field(out, 3);
            codec::write_bool(out, &self.until_idle);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + self.concurrency.map_or(0, |_| 10)
            + 2
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.concurrency.is_none()
            && !self.until_idle
    }
}

/// A job the server hands the client's worker. Its run's number names it to the
/// answer, a step and a keep; a cancel sends it again, cancelled, so that its
/// handler stops.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsHeld {
    pub(crate) run: u64,
    pub(crate) id: Option<String>,
    pub(crate) value: Option<String>,
    /// When the run was due. Unix milliseconds.
    pub(crate) at: i64,
    /// The first being 1.
    pub(crate) attempt: u64,
    pub(crate) group: Option<String>,
    pub(crate) cancelled: bool,
}

#[cfg(feature = "jobs")]
impl Message for JobsHeld {
    const NAME: &'static str = "jobs.Held";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.run = codec::uint(r, "run")?,
                2 => message.id = Some(codec::str(r, "id")?),
                3 => message.value = Some(codec::str(r, "value")?),
                4 => message.at = codec::int(r, "at")?,
                5 => message.attempt = codec::uint(r, "attempt")?,
                6 => message.group = Some(codec::str(r, "group")?),
                7 => message.cancelled = codec::bool(r, "cancelled")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.run != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.run);
        }
        if let Some(id) = &self.id {
            map.field(out, 2);
            codec::write_str(out, id);
        }
        if let Some(value) = &self.value {
            map.field(out, 3);
            codec::write_str(out, value);
        }
        if self.at != 0 {
            map.field(out, 4);
            codec::write_int(out, &self.at);
        }
        if self.attempt != 0 {
            map.field(out, 5);
            codec::write_uint(out, &self.attempt);
        }
        if let Some(group) = &self.group {
            map.field(out, 6);
            codec::write_str(out, group);
        }
        if self.cancelled {
            map.field(out, 7);
            codec::write_bool(out, &self.cancelled);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + self.id.as_ref().map_or(0, |id| 1 + 5 + id.len())
            + self.value.as_ref().map_or(0, |value| 1 + 5 + value.len())
            + 10
            + 10
            + self.group.as_ref().map_or(0, |group| 1 + 5 + group.len())
            + 2
    }

    fn is_zero(&self) -> bool {
        self.run == 0
            && self.id.is_none()
            && self.value.is_none()
            && self.at == 0
            && self.attempt == 0
            && self.group.is_none()
            && !self.cancelled
    }
}

/// The client's answer for a held job: how its run ended, or how far it got,
/// which settles nothing; or a stop, which names no run. An answer for a job a
/// cancel took settles nothing.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsAnswer {
    pub(crate) run: u64,
    /// Done, retry, snooze, fail, back, progress or stop.
    pub(crate) how: String,
    /// When a retry or a snooze runs again; a retry without one waits its backoff. Unix milliseconds.
    pub(crate) at: Option<i64>,
    /// Why a retry or a failure.
    pub(crate) error: Option<String>,
    /// 4 KiB at most.
    pub(crate) progress: Option<String>,
    /// From now on the server's clock; not beside at. Milliseconds.
    pub(crate) delay: Option<u64>,
}

#[cfg(feature = "jobs")]
impl Message for JobsAnswer {
    const NAME: &'static str = "jobs.Answer";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.run = codec::uint(r, "run")?,
                2 => message.how = codec::str(r, "how")?,
                3 => message.at = Some(codec::int(r, "at")?),
                4 => message.error = Some(codec::str(r, "error")?),
                5 => message.progress = Some(codec::str(r, "progress")?),
                6 => message.delay = Some(codec::uint(r, "delay")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.run != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.run);
        }
        if !self.how.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.how);
        }
        if let Some(at) = &self.at {
            map.field(out, 3);
            codec::write_int(out, at);
        }
        if let Some(error) = &self.error {
            map.field(out, 4);
            codec::write_str(out, error);
        }
        if let Some(progress) = &self.progress {
            map.field(out, 5);
            codec::write_str(out, progress);
        }
        if let Some(delay) = &self.delay {
            map.field(out, 6);
            codec::write_uint(out, delay);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.how.len()
            + self.at.map_or(0, |_| 10)
            + self.error.as_ref().map_or(0, |error| 1 + 5 + error.len())
            + self.progress.as_ref().map_or(0, |progress| 1 + 5 + progress.len())
            + self.delay.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.run == 0
            && self.how.is_empty()
            && self.at.is_none()
            && self.error.is_none()
            && self.progress.is_none()
            && self.delay.is_none()
    }
}

/// A step of a held job's run: jobs.step asks for its kept answer, jobs.keep
/// keeps one.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsStep {
    pub(crate) run: u64,
    /// 1 to 256 bytes.
    pub(crate) name: String,
    /// Jobs.keep's: 1 MiB at most.
    pub(crate) answer: Option<String>,
}

#[cfg(feature = "jobs")]
impl Message for JobsStep {
    const NAME: &'static str = "jobs.Step";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.run = codec::uint(r, "run")?,
                2 => message.name = codec::str(r, "name")?,
                3 => message.answer = Some(codec::str(r, "answer")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.run != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.run);
        }
        if !self.name.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.name);
        }
        if let Some(answer) = &self.answer {
            map.field(out, 3);
            codec::write_str(out, answer);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.name.len()
            + self.answer.as_ref().map_or(0, |answer| 1 + 5 + answer.len())
    }

    fn is_zero(&self) -> bool {
        self.run == 0
            && self.name.is_empty()
            && self.answer.is_none()
    }
}

#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsKept {
    pub(crate) found: bool,
    pub(crate) answer: Option<String>,
}

#[cfg(feature = "jobs")]
impl Message for JobsKept {
    const NAME: &'static str = "jobs.Kept";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.found = codec::bool(r, "found")?,
                2 => message.answer = Some(codec::str(r, "answer")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.found {
            map.field(out, 1);
            codec::write_bool(out, &self.found);
        }
        if let Some(answer) = &self.answer {
            map.field(out, 2);
            codec::write_str(out, answer);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
            + self.answer.as_ref().map_or(0, |answer| 1 + 5 + answer.len())
    }

    fn is_zero(&self) -> bool {
        !self.found
            && self.answer.is_none()
    }
}

/// One write of a transaction: an add, a set or an update with its call, or a
/// cancel with its id.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsOp {
    /// Jobs.add, jobs.set, jobs.update or jobs.cancel.
    pub(crate) method: u64,
    pub(crate) call: Option<JobsCall>,
    pub(crate) id: Option<JobsId>,
}

#[cfg(feature = "jobs")]
impl Message for JobsOp {
    const NAME: &'static str = "jobs.Op";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.method = codec::uint(r, "method")?,
                2 => message.call = Some(JobsCall::read(r)?),
                3 => message.id = Some(JobsId::read(r)?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.method != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.method);
        }
        if let Some(call) = &self.call {
            map.field(out, 2);
            codec::write_message(out, call);
        }
        if let Some(id) = &self.id {
            map.field(out, 3);
            codec::write_message(out, id);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + self.call.as_ref().map_or(0, |call| 1 + call.size())
            + self.id.as_ref().map_or(0, |id| 1 + id.size())
    }

    fn is_zero(&self) -> bool {
        self.method == 0
            && self.call.is_none()
            && self.id.is_none()
    }
}

/// A transaction of jobs.db across the wire: its writes, all applied or none.
/// One that fails names its place in what, as write.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsTx {
    pub(crate) writes: Vec<JobsOp>,
}

#[cfg(feature = "jobs")]
impl Message for JobsTx {
    const NAME: &'static str = "jobs.Tx";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.writes = codec::list(r, "writes", codec::message::<JobsOp>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.writes.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.writes, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.writes, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.writes.is_empty()
    }
}

/// What each write of a transaction did, in their order: whether an add added,
/// an update changed or a cancel found a job; a set changes always.
#[cfg(feature = "jobs")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobsTxResults {
    pub(crate) outcomes: Vec<JobsChanged>,
}

#[cfg(feature = "jobs")]
impl Message for JobsTxResults {
    const NAME: &'static str = "jobs.TxResults";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.outcomes = codec::list(r, "outcomes", codec::message::<JobsChanged>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.outcomes.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.outcomes, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.outcomes, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.outcomes.is_empty()
    }
}

/// Opens a bucket of values by key. Its keys expire ttl after they are written,
/// or idle after they were last read or written; not both.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvBucketOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    /// Milliseconds.
    pub(crate) ttl: Option<u64>,
    /// Milliseconds.
    pub(crate) idle: Option<u64>,
    /// A database's handle: the bucket lives in its file; kv.db when absent.
    pub(crate) database: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvBucketOpen {
    const NAME: &'static str = "kv.BucketOpen";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.ttl = Some(codec::uint(r, "ttl")?),
                3 => message.idle = Some(codec::uint(r, "idle")?),
                4 => message.database = Some(codec::uint(r, "database")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if let Some(ttl) = &self.ttl {
            map.field(out, 2);
            codec::write_uint(out, ttl);
        }
        if let Some(idle) = &self.idle {
            map.field(out, 3);
            codec::write_uint(out, idle);
        }
        if let Some(database) = &self.database {
            map.field(out, 4);
            codec::write_uint(out, database);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.ttl.map_or(0, |_| 10)
            + self.idle.map_or(0, |_| 10)
            + self.database.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.ttl.is_none()
            && self.idle.is_none()
            && self.database.is_none()
    }
}

/// A call on one key of a handle's branch.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvCall {
    pub(crate) handle: u64,
    /// The branch's owners, outermost first; the bucket's root when empty.
    pub(crate) under: Vec<String>,
    pub(crate) key: String,
    /// What a set or a create writes.
    pub(crate) value: Option<Row>,
    /// The expiry a write gives, from now. Milliseconds.
    pub(crate) ttl: Option<u64>,
    /// The expiry a write gives, as a time. Unix milliseconds.
    pub(crate) expires_at: Option<i64>,
    /// The version the key must still be at.
    pub(crate) if_version: Option<Vec<u8>>,
    /// What a counter adds; the requests or uses a limit is asked for, 1 when absent.
    pub(crate) n: Option<i64>,
}

#[cfg(feature = "kv")]
impl Message for KvCall {
    const NAME: &'static str = "kv.Call";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.under = codec::list(r, "under", codec::key)?,
                3 => message.key = codec::key(r, "key")?,
                4 => message.value = Some(codec::row(r, "value")?),
                5 => message.ttl = Some(codec::uint(r, "ttl")?),
                6 => message.expires_at = Some(codec::int(r, "expiresAt")?),
                7 => message.if_version = Some(codec::bin(r, "ifVersion")?),
                8 => message.n = Some(codec::int(r, "n")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.under.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.under, codec::write_str);
        }
        if !self.key.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.key);
        }
        if let Some(value) = &self.value {
            map.field(out, 4);
            codec::write_row(out, value);
        }
        if let Some(ttl) = &self.ttl {
            map.field(out, 5);
            codec::write_uint(out, ttl);
        }
        if let Some(expires_at) = &self.expires_at {
            map.field(out, 6);
            codec::write_int(out, expires_at);
        }
        if let Some(if_version) = &self.if_version {
            map.field(out, 7);
            codec::write_bin(out, if_version);
        }
        if let Some(n) = &self.n {
            map.field(out, 8);
            codec::write_int(out, n);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.under, |item| 5 + item.len())
            + 1 + 5 + self.key.len()
            + self.value.as_ref().map_or(0, |value| 1 + codec::row_size(value))
            + self.ttl.map_or(0, |_| 10)
            + self.expires_at.map_or(0, |_| 10)
            + self.if_version.as_ref().map_or(0, |if_version| 1 + 5 + if_version.len())
            + self.n.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.under.is_empty()
            && self.key.is_empty()
            && self.value.is_none()
            && self.ttl.is_none()
            && self.expires_at.is_none()
            && self.if_version.is_none()
            && self.n.is_none()
    }
}

/// A key's value with what a conditional write needs.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvEntry {
    pub(crate) found: bool,
    pub(crate) value: Option<Row>,
    /// Compared only for equality.
    pub(crate) version: Option<Vec<u8>>,
    /// Absent for a key that never expires. Unix milliseconds.
    pub(crate) expires_at: Option<i64>,
    /// A page's entry alone.
    pub(crate) key: Option<String>,
}

#[cfg(feature = "kv")]
impl Message for KvEntry {
    const NAME: &'static str = "kv.Entry";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.found = codec::bool(r, "found")?,
                2 => message.value = Some(codec::row(r, "value")?),
                3 => message.version = Some(codec::bin(r, "version")?),
                4 => message.expires_at = Some(codec::int(r, "expiresAt")?),
                5 => message.key = Some(codec::key(r, "key")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.found {
            map.field(out, 1);
            codec::write_bool(out, &self.found);
        }
        if let Some(value) = &self.value {
            map.field(out, 2);
            codec::write_row(out, value);
        }
        if let Some(version) = &self.version {
            map.field(out, 3);
            codec::write_bin(out, version);
        }
        if let Some(expires_at) = &self.expires_at {
            map.field(out, 4);
            codec::write_int(out, expires_at);
        }
        if let Some(key) = &self.key {
            map.field(out, 5);
            codec::write_str(out, key);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
            + self.value.as_ref().map_or(0, |value| 1 + codec::row_size(value))
            + self.version.as_ref().map_or(0, |version| 1 + 5 + version.len())
            + self.expires_at.map_or(0, |_| 10)
            + self.key.as_ref().map_or(0, |key| 1 + 5 + key.len())
    }

    fn is_zero(&self) -> bool {
        !self.found
            && self.value.is_none()
            && self.version.is_none()
            && self.expires_at.is_none()
            && self.key.is_none()
    }
}

/// What a write left: whether it wrote, and the version and expiry the key has
/// now, the live key's own when a create found one.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvWritten {
    pub(crate) written: bool,
    pub(crate) version: Vec<u8>,
    /// Unix milliseconds.
    pub(crate) expires_at: Option<i64>,
}

#[cfg(feature = "kv")]
impl Message for KvWritten {
    const NAME: &'static str = "kv.Written";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.written = codec::bool(r, "written")?,
                2 => message.version = codec::bin(r, "version")?,
                3 => message.expires_at = Some(codec::int(r, "expiresAt")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.written {
            map.field(out, 1);
            codec::write_bool(out, &self.written);
        }
        if !self.version.is_empty() {
            map.field(out, 2);
            codec::write_bin(out, &self.version);
        }
        if let Some(expires_at) = &self.expires_at {
            map.field(out, 3);
            codec::write_int(out, expires_at);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
            + 1 + 5 + self.version.len()
            + self.expires_at.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        !self.written
            && self.version.is_empty()
            && self.expires_at.is_none()
    }
}

#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvFound {
    pub(crate) found: bool,
}

#[cfg(feature = "kv")]
impl Message for KvFound {
    const NAME: &'static str = "kv.Found";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.found = codec::bool(r, "found")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.found {
            map.field(out, 1);
            codec::write_bool(out, &self.found);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
    }

    fn is_zero(&self) -> bool {
        !self.found
    }
}

/// A branch of a handle, for a clear.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvBranch {
    pub(crate) handle: u64,
    pub(crate) under: Vec<String>,
}

#[cfg(feature = "kv")]
impl Message for KvBranch {
    const NAME: &'static str = "kv.Branch";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.under = codec::list(r, "under", codec::key)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.under.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.under, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.under, |item| 5 + item.len())
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.under.is_empty()
    }
}

/// A page of a branch's own keys, in the byte order of their text.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvList {
    pub(crate) handle: u64,
    pub(crate) under: Vec<String>,
    /// The key the page starts after.
    pub(crate) after: Option<String>,
    /// 100 when absent, 1000 at most.
    pub(crate) limit: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvList {
    const NAME: &'static str = "kv.List";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.under = codec::list(r, "under", codec::key)?,
                3 => message.after = Some(codec::key(r, "after")?),
                4 => message.limit = Some(codec::uint(r, "limit")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.under.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.under, codec::write_str);
        }
        if let Some(after) = &self.after {
            map.field(out, 3);
            codec::write_str(out, after);
        }
        if let Some(limit) = &self.limit {
            map.field(out, 4);
            codec::write_uint(out, limit);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.under, |item| 5 + item.len())
            + self.after.as_ref().map_or(0, |after| 1 + 5 + after.len())
            + self.limit.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.under.is_empty()
            && self.after.is_none()
            && self.limit.is_none()
    }
}

/// A page of entries, as many as the limit asks and the body holds, and where
/// the next page starts, when there is one.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvPage {
    pub(crate) entries: Vec<KvEntry>,
    pub(crate) next: Option<String>,
}

#[cfg(feature = "kv")]
impl Message for KvPage {
    const NAME: &'static str = "kv.Page";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.entries = codec::list(r, "entries", codec::message::<KvEntry>)?,
                2 => message.next = Some(codec::key(r, "next")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.entries.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.entries, codec::write_message);
        }
        if let Some(next) = &self.next {
            map.field(out, 2);
            codec::write_str(out, next);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.entries, codec::message_size)
            + self.next.as_ref().map_or(0, |next| 1 + 5 + next.len())
    }

    fn is_zero(&self) -> bool {
        self.entries.is_empty()
            && self.next.is_none()
    }
}

/// Opens counters: numbers by key that only add up.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvCountersOpen {
    pub(crate) name: String,
    /// A counter lasts this long from its first add. Milliseconds.
    pub(crate) ttl: Option<u64>,
    /// Kept in memory and written every span; each add written when absent. Milliseconds.
    pub(crate) flush_every: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvCountersOpen {
    const NAME: &'static str = "kv.CountersOpen";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.ttl = Some(codec::uint(r, "ttl")?),
                3 => message.flush_every = Some(codec::uint(r, "flushEvery")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if let Some(ttl) = &self.ttl {
            map.field(out, 2);
            codec::write_uint(out, ttl);
        }
        if let Some(flush_every) = &self.flush_every {
            map.field(out, 3);
            codec::write_uint(out, flush_every);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.ttl.map_or(0, |_| 10)
            + self.flush_every.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.ttl.is_none()
            && self.flush_every.is_none()
    }
}

#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvCount {
    pub(crate) value: i64,
}

#[cfg(feature = "kv")]
impl Message for KvCount {
    const NAME: &'static str = "kv.Count";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.value = codec::int(r, "value")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.value != 0 {
            map.field(out, 1);
            codec::write_int(out, &self.value);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
    }

    fn is_zero(&self) -> bool {
        self.value == 0
    }
}

/// Opens a rate limit: rate requests a key every per, burst at once.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvRateLimitOpen {
    pub(crate) name: String,
    pub(crate) rate: u64,
    /// Milliseconds.
    pub(crate) per: u64,
    /// The rate when absent.
    pub(crate) burst: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvRateLimitOpen {
    const NAME: &'static str = "kv.RateLimitOpen";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.rate = codec::uint(r, "rate")?,
                3 => message.per = codec::uint(r, "per")?,
                4 => message.burst = Some(codec::uint(r, "burst")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if self.rate != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.rate);
        }
        if self.per != 0 {
            map.field(out, 3);
            codec::write_uint(out, &self.per);
        }
        if let Some(burst) = &self.burst {
            map.field(out, 4);
            codec::write_uint(out, burst);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 10
            + 10
            + self.burst.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.rate == 0
            && self.per == 0
            && self.burst.is_none()
    }
}

/// One window of a quota: a key may use up to limit every per from its first use.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvWindow {
    /// [a-z][a-z0-9_]{0,31}.
    pub(crate) name: String,
    pub(crate) limit: u64,
    /// Milliseconds.
    pub(crate) per: u64,
}

#[cfg(feature = "kv")]
impl Message for KvWindow {
    const NAME: &'static str = "kv.Window";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.limit = codec::uint(r, "limit")?,
                3 => message.per = codec::uint(r, "per")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if self.limit != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.limit);
        }
        if self.per != 0 {
            map.field(out, 3);
            codec::write_uint(out, &self.per);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 10
            + 10
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.limit == 0
            && self.per == 0
    }
}

#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvQuotaOpen {
    pub(crate) name: String,
    /// One to eight.
    pub(crate) windows: Vec<KvWindow>,
}

#[cfg(feature = "kv")]
impl Message for KvQuotaOpen {
    const NAME: &'static str = "kv.QuotaOpen";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.windows = codec::list(r, "windows", codec::message::<KvWindow>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if !self.windows.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.windows, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + codec::list_size(&self.windows, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.windows.is_empty()
    }
}

/// One window of a quota as a key stands in it.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvWindowUse {
    pub(crate) name: String,
    pub(crate) used: u64,
    pub(crate) limit: u64,
    pub(crate) left: u64,
    /// Absent for a window not started. Unix milliseconds.
    pub(crate) resets_at: Option<i64>,
}

#[cfg(feature = "kv")]
impl Message for KvWindowUse {
    const NAME: &'static str = "kv.WindowUse";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.used = codec::uint(r, "used")?,
                3 => message.limit = codec::uint(r, "limit")?,
                4 => message.left = codec::uint(r, "left")?,
                5 => message.resets_at = Some(codec::int(r, "resetsAt")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if self.used != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.used);
        }
        if self.limit != 0 {
            map.field(out, 3);
            codec::write_uint(out, &self.limit);
        }
        if self.left != 0 {
            map.field(out, 4);
            codec::write_uint(out, &self.left);
        }
        if let Some(resets_at) = &self.resets_at {
            map.field(out, 5);
            codec::write_int(out, resets_at);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 10
            + 10
            + 10
            + self.resets_at.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.used == 0
            && self.limit == 0
            && self.left == 0
            && self.resets_at.is_none()
    }
}

/// What a rate limit or a quota answers a request.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvAllowance {
    pub(crate) ok: bool,
    /// How many more would pass now.
    pub(crate) left: u64,
    /// When the request would pass; absent when it did. Unix milliseconds.
    pub(crate) retry_at: Option<i64>,
    /// A quota's, in the order it names them.
    pub(crate) windows: Vec<KvWindowUse>,
}

#[cfg(feature = "kv")]
impl Message for KvAllowance {
    const NAME: &'static str = "kv.Allowance";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.ok = codec::bool(r, "ok")?,
                2 => message.left = codec::uint(r, "left")?,
                3 => message.retry_at = Some(codec::int(r, "retryAt")?),
                4 => message.windows = codec::list(r, "windows", codec::message::<KvWindowUse>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.ok {
            map.field(out, 1);
            codec::write_bool(out, &self.ok);
        }
        if self.left != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.left);
        }
        if let Some(retry_at) = &self.retry_at {
            map.field(out, 3);
            codec::write_int(out, retry_at);
        }
        if !self.windows.is_empty() {
            map.field(out, 4);
            codec::write_list(out, &self.windows, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
            + 10
            + self.retry_at.map_or(0, |_| 10)
            + 1 + codec::list_size(&self.windows, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        !self.ok
            && self.left == 0
            && self.retry_at.is_none()
            && self.windows.is_empty()
    }
}

/// Opens once's answers: a function run once a key, its answer kept.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvOnceOpen {
    pub(crate) name: String,
    /// A day when absent. Milliseconds.
    pub(crate) keep: Option<u64>,
}

#[cfg(feature = "kv")]
impl Message for KvOnceOpen {
    const NAME: &'static str = "kv.OnceOpen";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.keep = Some(codec::uint(r, "keep")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if let Some(keep) = &self.keep {
            map.field(out, 2);
            codec::write_uint(out, keep);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.keep.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.keep.is_none()
    }
}

/// An answer of once: in the server's RESPONSE, found when one was kept, and
/// otherwise the run is the client's; in the client's last DATA, found with the
/// answer to keep, or not found when the function failed.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvAnswer {
    pub(crate) found: bool,
    pub(crate) value: Option<Row>,
}

#[cfg(feature = "kv")]
impl Message for KvAnswer {
    const NAME: &'static str = "kv.Answer";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.found = codec::bool(r, "found")?,
                2 => message.value = Some(codec::row(r, "value")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.found {
            map.field(out, 1);
            codec::write_bool(out, &self.found);
        }
        if let Some(value) = &self.value {
            map.field(out, 2);
            codec::write_row(out, value);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
            + self.value.as_ref().map_or(0, |value| 1 + codec::row_size(value))
    }

    fn is_zero(&self) -> bool {
        !self.found
            && self.value.is_none()
    }
}

/// A read a transaction made, which its commit checks: the key still at the
/// version it was found at, or still absent.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvCheck {
    pub(crate) handle: u64,
    pub(crate) under: Vec<String>,
    pub(crate) key: String,
    /// Absent: the read found nothing.
    pub(crate) version: Option<Vec<u8>>,
}

#[cfg(feature = "kv")]
impl Message for KvCheck {
    const NAME: &'static str = "kv.Check";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.under = codec::list(r, "under", codec::key)?,
                3 => message.key = codec::key(r, "key")?,
                4 => message.version = Some(codec::bin(r, "version")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.under.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.under, codec::write_str);
        }
        if !self.key.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.key);
        }
        if let Some(version) = &self.version {
            map.field(out, 4);
            codec::write_bin(out, version);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.under, |item| 5 + item.len())
            + 1 + 5 + self.key.len()
            + self.version.as_ref().map_or(0, |version| 1 + 5 + version.len())
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.under.is_empty()
            && self.key.is_empty()
            && self.version.is_none()
    }
}

/// One write of a transaction: the method it is, and its call.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvOp {
    /// Kv.set, kv.create, kv.take, kv.delete, kv.expire, kv.clear or kv.counters.add.
    pub(crate) method: u64,
    pub(crate) call: KvCall,
}

#[cfg(feature = "kv")]
impl Message for KvOp {
    const NAME: &'static str = "kv.Op";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.method = codec::uint(r, "method")?,
                2 => message.call = KvCall::read(r)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.method != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.method);
        }
        if !self.call.is_zero() {
            map.field(out, 2);
            codec::write_message(out, &self.call);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + self.call.size()
    }

    fn is_zero(&self) -> bool {
        self.method == 0
            && self.call.is_zero()
    }
}

/// A transaction across the wire: its reads' checks, then its writes, all
/// applied or none. A failed check or write names its place in what.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvTx {
    pub(crate) checks: Vec<KvCheck>,
    pub(crate) writes: Vec<KvOp>,
}

#[cfg(feature = "kv")]
impl Message for KvTx {
    const NAME: &'static str = "kv.Tx";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.checks = codec::list(r, "checks", codec::message::<KvCheck>)?,
                2 => message.writes = codec::list(r, "writes", codec::message::<KvOp>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.checks.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.checks, codec::write_message);
        }
        if !self.writes.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.writes, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.checks, codec::message_size)
            + 1 + codec::list_size(&self.writes, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.checks.is_empty()
            && self.writes.is_empty()
    }
}

/// What one write of a transaction did.
#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvOutcome {
    /// A take, a delete or an expire found a live key.
    pub(crate) found: bool,
    /// A set or a create wrote.
    pub(crate) written: bool,
    /// What a take took.
    pub(crate) value: Option<Row>,
    pub(crate) version: Option<Vec<u8>>,
    /// Unix milliseconds.
    pub(crate) expires_at: Option<i64>,
    /// A counter's value after its add.
    pub(crate) count: Option<i64>,
}

#[cfg(feature = "kv")]
impl Message for KvOutcome {
    const NAME: &'static str = "kv.Outcome";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.found = codec::bool(r, "found")?,
                2 => message.written = codec::bool(r, "written")?,
                3 => message.value = Some(codec::row(r, "value")?),
                4 => message.version = Some(codec::bin(r, "version")?),
                5 => message.expires_at = Some(codec::int(r, "expiresAt")?),
                6 => message.count = Some(codec::int(r, "count")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.found {
            map.field(out, 1);
            codec::write_bool(out, &self.found);
        }
        if self.written {
            map.field(out, 2);
            codec::write_bool(out, &self.written);
        }
        if let Some(value) = &self.value {
            map.field(out, 3);
            codec::write_row(out, value);
        }
        if let Some(version) = &self.version {
            map.field(out, 4);
            codec::write_bin(out, version);
        }
        if let Some(expires_at) = &self.expires_at {
            map.field(out, 5);
            codec::write_int(out, expires_at);
        }
        if let Some(count) = &self.count {
            map.field(out, 6);
            codec::write_int(out, count);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
            + 2
            + self.value.as_ref().map_or(0, |value| 1 + codec::row_size(value))
            + self.version.as_ref().map_or(0, |version| 1 + 5 + version.len())
            + self.expires_at.map_or(0, |_| 10)
            + self.count.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        !self.found
            && !self.written
            && self.value.is_none()
            && self.version.is_none()
            && self.expires_at.is_none()
            && self.count.is_none()
    }
}

#[cfg(feature = "kv")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct KvTxResults {
    pub(crate) outcomes: Vec<KvOutcome>,
}

#[cfg(feature = "kv")]
impl Message for KvTxResults {
    const NAME: &'static str = "kv.TxResults";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.outcomes = codec::list(r, "outcomes", codec::message::<KvOutcome>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.outcomes.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.outcomes, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.outcomes, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.outcomes.is_empty()
    }
}

/// A private server's clock: a time to set it to, a span to move it forward by,
/// or neither to read it. The answer is the time it reads once moved.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ServerClock {
    /// Unix milliseconds.
    pub(crate) at: Option<i64>,
    /// Milliseconds.
    pub(crate) advance: Option<u64>,
}

impl Message for ServerClock {
    const NAME: &'static str = "server.Clock";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.at = Some(codec::int(r, "at")?),
                2 => message.advance = Some(codec::uint(r, "advance")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(at) = &self.at {
            map.field(out, 1);
            codec::write_int(out, at);
        }
        if let Some(advance) = &self.advance {
            map.field(out, 2);
            codec::write_uint(out, advance);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.at.map_or(0, |_| 10)
            + self.advance.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.at.is_none()
            && self.advance.is_none()
    }
}

/// A migration as its file has it: the name that numbers it, and its SQL.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlMigration {
    /// 0001_notes.sql.
    pub(crate) name: String,
    pub(crate) sql: String,
}

#[cfg(feature = "sql")]
impl Message for SqlMigration {
    const NAME: &'static str = "sql.Migration";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.sql = codec::str(r, "sql")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if !self.sql.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.sql);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + 5 + self.sql.len()
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.sql.is_empty()
    }
}

/// Opens a database. Migrations, when given, are applied and checked; without
/// them the file opens as it is.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    pub(crate) migrations: Option<Vec<SqlMigration>>,
    /// Full or os: how far its commits go before they return; the store's own when absent.
    pub(crate) durability: Option<String>,
}

#[cfg(feature = "sql")]
impl Message for SqlOpen {
    const NAME: &'static str = "sql.Open";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.migrations = Some(codec::list(r, "migrations", codec::message::<SqlMigration>)?),
                3 => message.durability = Some(codec::str(r, "durability")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.name.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.name);
        }
        if let Some(migrations) = &self.migrations {
            map.field(out, 2);
            codec::write_list(out, migrations, codec::write_message);
        }
        if let Some(durability) = &self.durability {
            map.field(out, 3);
            codec::write_str(out, durability);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.migrations.as_ref().map_or(0, |migrations| 1 + codec::list_size(migrations, codec::message_size))
            + self.durability.as_ref().map_or(0, |durability| 1 + 5 + durability.len())
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.migrations.is_none()
            && self.durability.is_none()
    }
}

/// A statement's text and the values of its ?s, in order.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlText {
    pub(crate) text: String,
    pub(crate) values: Vec<Cell>,
}

#[cfg(feature = "sql")]
impl Message for SqlText {
    const NAME: &'static str = "sql.Text";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.text = codec::str(r, "text")?,
                2 => message.values = codec::list(r, "values", codec::cell)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.text.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.text);
        }
        if !self.values.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.values, codec::write_cell);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.text.len()
            + 1 + codec::list_size(&self.values, codec::cell_size)
    }

    fn is_zero(&self) -> bool {
        self.text.is_empty()
            && self.values.is_empty()
    }
}

/// A write on a database.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlStatement {
    pub(crate) handle: u64,
    pub(crate) text: String,
    pub(crate) values: Vec<Cell>,
}

#[cfg(feature = "sql")]
impl Message for SqlStatement {
    const NAME: &'static str = "sql.Statement";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.text = codec::str(r, "text")?,
                3 => message.values = codec::list(r, "values", codec::cell)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.text.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.text);
        }
        if !self.values.is_empty() {
            map.field(out, 3);
            codec::write_list(out, &self.values, codec::write_cell);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.text.len()
            + 1 + codec::list_size(&self.values, codec::cell_size)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.text.is_empty()
            && self.values.is_empty()
    }
}

/// What a statement gives back: its rows, one row or none, or one value. A
/// write with returning runs on the writer, its rows given once it is durable.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlQuery {
    pub(crate) handle: u64,
    pub(crate) text: String,
    pub(crate) values: Vec<Cell>,
    /// All, one or scalar.
    pub(crate) want: String,
}

#[cfg(feature = "sql")]
impl Message for SqlQuery {
    const NAME: &'static str = "sql.Query";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.text = codec::str(r, "text")?,
                3 => message.values = codec::list(r, "values", codec::cell)?,
                4 => message.want = codec::str(r, "want")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.text.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.text);
        }
        if !self.values.is_empty() {
            map.field(out, 3);
            codec::write_list(out, &self.values, codec::write_cell);
        }
        if !self.want.is_empty() {
            map.field(out, 4);
            codec::write_str(out, &self.want);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.text.len()
            + 1 + codec::list_size(&self.values, codec::cell_size)
            + 1 + 5 + self.want.len()
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.text.is_empty()
            && self.values.is_empty()
            && self.want.is_empty()
    }
}

/// Rows a statement gave, a part of them a message: the first part names the
/// columns, and every row holds a value a column, in their order.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlRows {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Cell>>,
}

#[cfg(feature = "sql")]
impl Message for SqlRows {
    const NAME: &'static str = "sql.Rows";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.columns = codec::list(r, "columns", codec::str)?,
                2 => message.rows = codec::list(r, "rows", |r, name| codec::list(r, name, codec::cell))?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.columns.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.columns, codec::write_str);
        }
        if !self.rows.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.rows, |out, items| codec::write_list(out, items, codec::write_cell));
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.columns, |item| 5 + item.len())
            + 1 + codec::list_size(&self.rows, |item| codec::list_size(item, codec::cell_size))
    }

    fn is_zero(&self) -> bool {
        self.columns.is_empty()
            && self.rows.is_empty()
    }
}

/// What a write changed: the rows, and SQLite's rowid of the row it inserted,
/// 0 when it inserted none.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlDone {
    pub(crate) changes: u64,
    pub(crate) last_insert_rowid: i64,
}

#[cfg(feature = "sql")]
impl Message for SqlDone {
    const NAME: &'static str = "sql.Done";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.changes = codec::uint(r, "changes")?,
                2 => message.last_insert_rowid = codec::int(r, "lastInsertRowid")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.changes != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.changes);
        }
        if self.last_insert_rowid != 0 {
            map.field(out, 2);
            codec::write_int(out, &self.last_insert_rowid);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
    }

    fn is_zero(&self) -> bool {
        self.changes == 0
            && self.last_insert_rowid == 0
    }
}

/// Statements known before they run, written as one in a shared commit.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlBatch {
    pub(crate) handle: u64,
    pub(crate) statements: Vec<SqlText>,
}

#[cfg(feature = "sql")]
impl Message for SqlBatch {
    const NAME: &'static str = "sql.Batch";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.statements = codec::list(r, "statements", codec::message::<SqlText>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        if !self.statements.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.statements, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.statements, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.statements.is_empty()
    }
}

#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlBatched {
    pub(crate) done: Vec<SqlDone>,
}

#[cfg(feature = "sql")]
impl Message for SqlBatched {
    const NAME: &'static str = "sql.Batched";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.done = codec::list(r, "done", codec::message::<SqlDone>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.done.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.done, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.done, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.done.is_empty()
    }
}

/// Opens a transaction: it holds the database's writer until the client's last
/// DATA, five seconds at most.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlTxOpen {
    pub(crate) handle: u64,
}

#[cfg(feature = "sql")]
impl Message for SqlTxOpen {
    const NAME: &'static str = "sql.TxOpen";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.handle != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.handle);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
    }
}

/// A transaction's call, or its end: the last DATA commits, or not. A call is
/// a statement, or a call of kv or jobs on a bucket or a queue kept in the
/// database's file: a method of theirs with its request, kv.set or jobs.add,
/// which runs in the transaction.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlTxCall {
    pub(crate) text: String,
    pub(crate) values: Vec<Cell>,
    /// All, one, scalar or exec; call for a method; nothing in the last.
    pub(crate) want: String,
    /// In the last: true commits, false rolls back.
    pub(crate) commit: bool,
    /// With want call: the method, its request in body.
    pub(crate) method: Option<u64>,
    pub(crate) body: Option<Vec<u8>>,
}

#[cfg(feature = "sql")]
impl Message for SqlTxCall {
    const NAME: &'static str = "sql.TxCall";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.text = codec::str(r, "text")?,
                2 => message.values = codec::list(r, "values", codec::cell)?,
                3 => message.want = codec::str(r, "want")?,
                4 => message.commit = codec::bool(r, "commit")?,
                5 => message.method = Some(codec::uint(r, "method")?),
                6 => message.body = Some(codec::bin(r, "body")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.text.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.text);
        }
        if !self.values.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.values, codec::write_cell);
        }
        if !self.want.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.want);
        }
        if self.commit {
            map.field(out, 4);
            codec::write_bool(out, &self.commit);
        }
        if let Some(method) = &self.method {
            map.field(out, 5);
            codec::write_uint(out, method);
        }
        if let Some(body) = &self.body {
            map.field(out, 6);
            codec::write_bin(out, body);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.text.len()
            + 1 + codec::list_size(&self.values, codec::cell_size)
            + 1 + 5 + self.want.len()
            + 2
            + self.method.map_or(0, |_| 10)
            + self.body.as_ref().map_or(0, |body| 1 + 5 + body.len())
    }

    fn is_zero(&self) -> bool {
        self.text.is_empty()
            && self.values.is_empty()
            && self.want.is_empty()
            && !self.commit
            && self.method.is_none()
            && self.body.is_none()
    }
}

/// A call's answer: its rows, what it changed, a method's answer, or why it
/// failed, which leaves the transaction as it was before the call.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlTxAnswer {
    pub(crate) rows: Option<SqlRows>,
    pub(crate) done: Option<SqlDone>,
    pub(crate) failure: Option<Failure>,
    /// What a method answered, as its own answer.
    pub(crate) body: Option<Vec<u8>>,
}

#[cfg(feature = "sql")]
impl Message for SqlTxAnswer {
    const NAME: &'static str = "sql.TxAnswer";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.rows = Some(SqlRows::read(r)?),
                2 => message.done = Some(SqlDone::read(r)?),
                3 => message.failure = Some(Failure::read(r)?),
                4 => message.body = Some(codec::bin(r, "body")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(rows) = &self.rows {
            map.field(out, 1);
            codec::write_message(out, rows);
        }
        if let Some(done) = &self.done {
            map.field(out, 2);
            codec::write_message(out, done);
        }
        if let Some(failure) = &self.failure {
            map.field(out, 3);
            codec::write_message(out, failure);
        }
        if let Some(body) = &self.body {
            map.field(out, 4);
            codec::write_bin(out, body);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.rows.as_ref().map_or(0, |rows| 1 + rows.size())
            + self.done.as_ref().map_or(0, |done| 1 + done.size())
            + self.failure.as_ref().map_or(0, |failure| 1 + failure.size())
            + self.body.as_ref().map_or(0, |body| 1 + 5 + body.len())
    }

    fn is_zero(&self) -> bool {
        self.rows.is_none()
            && self.done.is_none()
            && self.failure.is_none()
            && self.body.is_none()
    }
}

/// Every method's number, by its name in the schema.
pub(crate) mod method {
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_OPEN: u16 = 0x0401;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_PUT: u16 = 0x0402;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_UPLOAD: u16 = 0x0403;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_GET: u16 = 0x0404;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_HEAD: u16 = 0x0405;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_DELETE: u16 = 0x0406;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_COPY: u16 = 0x0407;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_RENAME: u16 = 0x0408;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_EXPIRE: u16 = 0x0409;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_LIST: u16 = 0x040a;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_USAGE: u16 = 0x040b;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_CLEAR: u16 = 0x040c;
    #[cfg(feature = "blobs")]
    pub(crate) const BLOBS_READ: u16 = 0x040d;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_QUEUE_OPEN: u16 = 0x0201;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_SCHEDULE_OPEN: u16 = 0x0202;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_ADD: u16 = 0x0203;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_SET: u16 = 0x0204;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_UPDATE: u16 = 0x0205;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_CANCEL: u16 = 0x0206;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_GET: u16 = 0x0207;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_LIST: u16 = 0x0208;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_WORK: u16 = 0x0209;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_STEP: u16 = 0x020a;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_KEEP: u16 = 0x020b;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_WATCH: u16 = 0x020c;
    #[cfg(feature = "jobs")]
    pub(crate) const JOBS_TX: u16 = 0x020d;
    #[cfg(feature = "kv")]
    pub(crate) const KV_BUCKET_OPEN: u16 = 0x0101;
    #[cfg(feature = "kv")]
    pub(crate) const KV_GET: u16 = 0x0102;
    #[cfg(feature = "kv")]
    pub(crate) const KV_HAS: u16 = 0x0103;
    #[cfg(feature = "kv")]
    pub(crate) const KV_SET: u16 = 0x0104;
    #[cfg(feature = "kv")]
    pub(crate) const KV_CREATE: u16 = 0x0105;
    #[cfg(feature = "kv")]
    pub(crate) const KV_TAKE: u16 = 0x0106;
    #[cfg(feature = "kv")]
    pub(crate) const KV_DELETE: u16 = 0x0107;
    #[cfg(feature = "kv")]
    pub(crate) const KV_EXPIRE: u16 = 0x0108;
    #[cfg(feature = "kv")]
    pub(crate) const KV_CLEAR: u16 = 0x0109;
    #[cfg(feature = "kv")]
    pub(crate) const KV_LIST: u16 = 0x010a;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_OPEN: u16 = 0x0110;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_ADD: u16 = 0x0111;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_GET: u16 = 0x0112;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_DELETE: u16 = 0x0113;
    #[cfg(feature = "kv")]
    pub(crate) const KV_COUNTERS_CLEAR: u16 = 0x0114;
    #[cfg(feature = "kv")]
    pub(crate) const KV_RATE_LIMIT_OPEN: u16 = 0x0120;
    #[cfg(feature = "kv")]
    pub(crate) const KV_QUOTA_OPEN: u16 = 0x0121;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ALLOW: u16 = 0x0122;
    #[cfg(feature = "kv")]
    pub(crate) const KV_PEEK: u16 = 0x0123;
    #[cfg(feature = "kv")]
    pub(crate) const KV_RESET: u16 = 0x0124;
    #[cfg(feature = "kv")]
    pub(crate) const KV_REFUND: u16 = 0x0125;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ONCE_OPEN: u16 = 0x0130;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ONCE_RUN: u16 = 0x0131;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ONCE_GET: u16 = 0x0132;
    #[cfg(feature = "kv")]
    pub(crate) const KV_ONCE_DELETE: u16 = 0x0133;
    #[cfg(feature = "kv")]
    pub(crate) const KV_TX: u16 = 0x0140;
    pub(crate) const SERVER_STOP: u16 = 0x0001;
    pub(crate) const SERVER_CLOCK: u16 = 0x0002;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_OPEN: u16 = 0x0301;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_QUERY: u16 = 0x0302;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_EXEC: u16 = 0x0303;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_BATCH: u16 = 0x0304;
    #[cfg(feature = "sql")]
    pub(crate) const SQL_TX: u16 = 0x0305;
}

/// Every method, its name and number, for the test that the server answers each.
#[cfg(test)]
pub(crate) const METHODS: &[(&str, u16)] = &[
    #[cfg(feature = "blobs")]
    ("blobs.open", 0x0401),
    #[cfg(feature = "blobs")]
    ("blobs.put", 0x0402),
    #[cfg(feature = "blobs")]
    ("blobs.upload", 0x0403),
    #[cfg(feature = "blobs")]
    ("blobs.get", 0x0404),
    #[cfg(feature = "blobs")]
    ("blobs.head", 0x0405),
    #[cfg(feature = "blobs")]
    ("blobs.delete", 0x0406),
    #[cfg(feature = "blobs")]
    ("blobs.copy", 0x0407),
    #[cfg(feature = "blobs")]
    ("blobs.rename", 0x0408),
    #[cfg(feature = "blobs")]
    ("blobs.expire", 0x0409),
    #[cfg(feature = "blobs")]
    ("blobs.list", 0x040a),
    #[cfg(feature = "blobs")]
    ("blobs.usage", 0x040b),
    #[cfg(feature = "blobs")]
    ("blobs.clear", 0x040c),
    #[cfg(feature = "blobs")]
    ("blobs.read", 0x040d),
    #[cfg(feature = "jobs")]
    ("jobs.queue.open", 0x0201),
    #[cfg(feature = "jobs")]
    ("jobs.schedule.open", 0x0202),
    #[cfg(feature = "jobs")]
    ("jobs.add", 0x0203),
    #[cfg(feature = "jobs")]
    ("jobs.set", 0x0204),
    #[cfg(feature = "jobs")]
    ("jobs.update", 0x0205),
    #[cfg(feature = "jobs")]
    ("jobs.cancel", 0x0206),
    #[cfg(feature = "jobs")]
    ("jobs.get", 0x0207),
    #[cfg(feature = "jobs")]
    ("jobs.list", 0x0208),
    #[cfg(feature = "jobs")]
    ("jobs.work", 0x0209),
    #[cfg(feature = "jobs")]
    ("jobs.step", 0x020a),
    #[cfg(feature = "jobs")]
    ("jobs.keep", 0x020b),
    #[cfg(feature = "jobs")]
    ("jobs.watch", 0x020c),
    #[cfg(feature = "jobs")]
    ("jobs.tx", 0x020d),
    #[cfg(feature = "kv")]
    ("kv.bucket.open", 0x0101),
    #[cfg(feature = "kv")]
    ("kv.get", 0x0102),
    #[cfg(feature = "kv")]
    ("kv.has", 0x0103),
    #[cfg(feature = "kv")]
    ("kv.set", 0x0104),
    #[cfg(feature = "kv")]
    ("kv.create", 0x0105),
    #[cfg(feature = "kv")]
    ("kv.take", 0x0106),
    #[cfg(feature = "kv")]
    ("kv.delete", 0x0107),
    #[cfg(feature = "kv")]
    ("kv.expire", 0x0108),
    #[cfg(feature = "kv")]
    ("kv.clear", 0x0109),
    #[cfg(feature = "kv")]
    ("kv.list", 0x010a),
    #[cfg(feature = "kv")]
    ("kv.counters.open", 0x0110),
    #[cfg(feature = "kv")]
    ("kv.counters.add", 0x0111),
    #[cfg(feature = "kv")]
    ("kv.counters.get", 0x0112),
    #[cfg(feature = "kv")]
    ("kv.counters.delete", 0x0113),
    #[cfg(feature = "kv")]
    ("kv.counters.clear", 0x0114),
    #[cfg(feature = "kv")]
    ("kv.rateLimit.open", 0x0120),
    #[cfg(feature = "kv")]
    ("kv.quota.open", 0x0121),
    #[cfg(feature = "kv")]
    ("kv.allow", 0x0122),
    #[cfg(feature = "kv")]
    ("kv.peek", 0x0123),
    #[cfg(feature = "kv")]
    ("kv.reset", 0x0124),
    #[cfg(feature = "kv")]
    ("kv.refund", 0x0125),
    #[cfg(feature = "kv")]
    ("kv.once.open", 0x0130),
    #[cfg(feature = "kv")]
    ("kv.once.run", 0x0131),
    #[cfg(feature = "kv")]
    ("kv.once.get", 0x0132),
    #[cfg(feature = "kv")]
    ("kv.once.delete", 0x0133),
    #[cfg(feature = "kv")]
    ("kv.tx", 0x0140),
    ("server.stop", 0x0001),
    ("server.clock", 0x0002),
    #[cfg(feature = "sql")]
    ("sql.open", 0x0301),
    #[cfg(feature = "sql")]
    ("sql.query", 0x0302),
    #[cfg(feature = "sql")]
    ("sql.exec", 0x0303),
    #[cfg(feature = "sql")]
    ("sql.batch", 0x0304),
    #[cfg(feature = "sql")]
    ("sql.tx", 0x0305),
];

/// A body of the message `name` read and written again, and the size the message
/// says it takes, for the test that the vectors' bytes are what this codec writes
/// within that size; `None` for a name it lacks.
#[cfg(test)]
pub(crate) fn rewrite(name: &str, body: &[u8]) -> Option<Result<(Vec<u8>, usize), Failure>> {
    let rewritten = match name {
        #[cfg(feature = "blobs")]
        "blobs.Open" => BlobsOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.At" => BlobsAt::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Info" => BlobsInfo::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Write" => BlobsWrite::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Written" => BlobsWritten::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Piece" => BlobsPiece::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Got" => BlobsGot::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Read" => BlobsRead::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Head" => BlobsHead::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Delete" => BlobsDelete::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Move" => BlobsMove::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Expire" => BlobsExpire::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Found" => BlobsFound::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.List" => BlobsList::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Page" => BlobsPage::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Folder" => BlobsFolder::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "blobs")]
        "blobs.Usage" => BlobsUsage::decode(body).map(|message| (message.encode(), message.size())),
        "Hello" => Hello::decode(body).map(|message| (message.encode(), message.size())),
        "Welcome" => Welcome::decode(body).map(|message| (message.encode(), message.size())),
        "store.Options" => StoreOptions::decode(body).map(|message| (message.encode(), message.size())),
        "GoAway" => GoAway::decode(body).map(|message| (message.encode(), message.size())),
        "Failure" => Failure::decode(body).map(|message| (message.encode(), message.size())),
        "Handle" => Handle::decode(body).map(|message| (message.encode(), message.size())),
        "Empty" => Empty::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Concurrency" => JobsConcurrency::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Backoff" => JobsBackoff::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Rate" => JobsRate::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.QueueOpen" => JobsQueueOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.ScheduleOpen" => JobsScheduleOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Call" => JobsCall::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Id" => JobsId::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Changed" => JobsChanged::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Job" => JobsJob::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.List" => JobsList::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Page" => JobsPage::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Work" => JobsWork::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Held" => JobsHeld::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Answer" => JobsAnswer::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Step" => JobsStep::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Kept" => JobsKept::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Op" => JobsOp::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.Tx" => JobsTx::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "jobs")]
        "jobs.TxResults" => JobsTxResults::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.BucketOpen" => KvBucketOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Call" => KvCall::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Entry" => KvEntry::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Written" => KvWritten::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Found" => KvFound::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Branch" => KvBranch::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.List" => KvList::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Page" => KvPage::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.CountersOpen" => KvCountersOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Count" => KvCount::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.RateLimitOpen" => KvRateLimitOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Window" => KvWindow::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.QuotaOpen" => KvQuotaOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.WindowUse" => KvWindowUse::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Allowance" => KvAllowance::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.OnceOpen" => KvOnceOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Answer" => KvAnswer::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Check" => KvCheck::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Op" => KvOp::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Tx" => KvTx::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.Outcome" => KvOutcome::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "kv")]
        "kv.TxResults" => KvTxResults::decode(body).map(|message| (message.encode(), message.size())),
        "server.Clock" => ServerClock::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Migration" => SqlMigration::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Open" => SqlOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Text" => SqlText::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Statement" => SqlStatement::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Query" => SqlQuery::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Rows" => SqlRows::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Done" => SqlDone::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Batch" => SqlBatch::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Batched" => SqlBatched::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.TxOpen" => SqlTxOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.TxCall" => SqlTxCall::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.TxAnswer" => SqlTxAnswer::decode(body).map(|message| (message.encode(), message.size())),
        _ => return None,
    };
    Some(rewritten)
}
