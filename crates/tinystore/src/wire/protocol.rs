// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.

use std::collections::BTreeMap;

use super::codec::{self, Map, Message, Row};
#[cfg(any(feature = "search", feature = "sql"))]
use super::codec::Cell;
use super::msgpack::Reader;

/// What a backup takes beside every engine's files: the files of the
/// program's it includes, by their paths inside the store's directory, and
/// whether it includes the store's encryption key.
#[cfg(feature = "backup")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BackupWrite {
    pub(crate) include: Vec<String>,
    pub(crate) include_encryption_key: bool,
}

#[cfg(feature = "backup")]
impl Message for BackupWrite {
    const NAME: &'static str = "backup.Write";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.include = codec::list(r, "include", codec::str)?,
                2 => message.include_encryption_key = codec::bool(r, "includeEncryptionKey")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.include.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.include, codec::write_str);
        }
        if self.include_encryption_key {
            map.field(out, 2);
            codec::write_bool(out, &self.include_encryption_key);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.include, |item| 5 + item.len())
            + 2
    }

    fn is_zero(&self) -> bool {
        self.include.is_empty()
            && !self.include_encryption_key
    }
}

/// A piece of the zip, in order.
#[cfg(feature = "backup")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BackupPiece {
    pub(crate) bytes: Vec<u8>,
}

#[cfg(feature = "backup")]
impl Message for BackupPiece {
    const NAME: &'static str = "backup.Piece";

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

/// What the backup holds, once its zip has gone whole.
#[cfg(feature = "backup")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BackupWritten {
    pub(crate) files: u64,
    pub(crate) bytes: u64,
    /// The store seals values with a key the backup does not hold.
    pub(crate) key_left_out: bool,
}

#[cfg(feature = "backup")]
impl Message for BackupWritten {
    const NAME: &'static str = "backup.Written";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.files = codec::uint(r, "files")?,
                2 => message.bytes = codec::uint(r, "bytes")?,
                3 => message.key_left_out = codec::bool(r, "keyLeftOut")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.files != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.files);
        }
        if self.bytes != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.bytes);
        }
        if self.key_left_out {
            map.field(out, 3);
            codec::write_bool(out, &self.key_left_out);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
            + 2
    }

    fn is_zero(&self) -> bool {
        self.files == 0
            && self.bytes == 0
            && !self.key_left_out
    }
}

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

/// A message published on a channel's branch, as docs/guides/channels/README.md has
/// it: every subscription listening there now receives it.
#[cfg(feature = "channels")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChannelsPublish {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    /// The branch's owners, 1 KiB together at most.
    pub(crate) under: Vec<String>,
    /// The message, 1 MiB at most.
    pub(crate) value: String,
}

#[cfg(feature = "channels")]
impl Message for ChannelsPublish {
    const NAME: &'static str = "channels.Publish";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.under = codec::list(r, "under", codec::str)?,
                3 => message.value = codec::str(r, "value")?,
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
        if !self.under.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.under, codec::write_str);
        }
        if !self.value.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.value);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + codec::list_size(&self.under, |item| 5 + item.len())
            + 1 + 5 + self.value.len()
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.under.is_empty()
            && self.value.is_empty()
    }
}

/// A subscription of a branch, or of it and every branch below it.
#[cfg(feature = "channels")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChannelsSubscribe {
    pub(crate) name: String,
    pub(crate) under: Vec<String>,
    pub(crate) recursive: bool,
}

#[cfg(feature = "channels")]
impl Message for ChannelsSubscribe {
    const NAME: &'static str = "channels.Subscribe";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.under = codec::list(r, "under", codec::str)?,
                3 => message.recursive = codec::bool(r, "recursive")?,
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
        if !self.under.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.under, codec::write_str);
        }
        if self.recursive {
            map.field(out, 3);
            codec::write_bool(out, &self.recursive);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + codec::list_size(&self.under, |item| 5 + item.len())
            + 2
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.under.is_empty()
            && !self.recursive
    }
}

/// What a subscription receives at once: how many it missed since its last
/// batch, then messages in the order they were released.
#[cfg(feature = "channels")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChannelsBatch {
    /// Lost because it fell behind its branch's log.
    pub(crate) missed: u64,
    pub(crate) messages: Vec<ChannelsMessage>,
}

#[cfg(feature = "channels")]
impl Message for ChannelsBatch {
    const NAME: &'static str = "channels.Batch";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.missed = codec::uint(r, "missed")?,
                2 => message.messages = codec::list(r, "messages", codec::message::<ChannelsMessage>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.missed != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.missed);
        }
        if !self.messages.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.messages, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.messages, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.missed == 0
            && self.messages.is_empty()
    }
}

#[cfg(feature = "channels")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChannelsMessage {
    /// The branch it was published under.
    pub(crate) under: Vec<String>,
    pub(crate) value: String,
}

#[cfg(feature = "channels")]
impl Message for ChannelsMessage {
    const NAME: &'static str = "channels.Message";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.under = codec::list(r, "under", codec::str)?,
                2 => message.value = codec::str(r, "value")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.under.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.under, codec::write_str);
        }
        if !self.value.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.value);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.under, |item| 5 + item.len())
            + 1 + 5 + self.value.len()
    }

    fn is_zero(&self) -> bool {
        self.under.is_empty()
            && self.value.is_empty()
    }
}

/// A config a program opens: its declaration and its layers as data, as
/// docs/guides/config/README.md has them, with what the program read itself, a file's
/// text and its variables, since those belong to it and not to the server.
#[cfg(feature = "config")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfigOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    /// Each setting's kind, rules, default and marks.
    pub(crate) declaration: String,
    /// Objects, files and the environment, in the order written.
    pub(crate) layers: String,
    /// A kept change that no longer fits refuses the open.
    pub(crate) strict: bool,
}

#[cfg(feature = "config")]
impl Message for ConfigOpen {
    const NAME: &'static str = "config.Open";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.declaration = codec::str(r, "declaration")?,
                3 => message.layers = codec::str(r, "layers")?,
                4 => message.strict = codec::bool(r, "strict")?,
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
        if !self.declaration.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.declaration);
        }
        if !self.layers.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.layers);
        }
        if self.strict {
            map.field(out, 4);
            codec::write_bool(out, &self.strict);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + 5 + self.declaration.len()
            + 1 + 5 + self.layers.len()
            + 2
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.declaration.is_empty()
            && self.layers.is_empty()
            && !self.strict
    }
}

/// A config's value at a version of its changes, and where each setting's
/// value came from.
#[cfg(feature = "config")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfigValue {
    pub(crate) handle: u64,
    pub(crate) version: i64,
    /// The settings, each span its milliseconds, each secret its text.
    pub(crate) value: String,
    pub(crate) lines: Vec<ConfigLine>,
    /// What was read and not used, each with why.
    pub(crate) notes: Vec<String>,
}

#[cfg(feature = "config")]
impl Message for ConfigValue {
    const NAME: &'static str = "config.Value";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.version = codec::int(r, "version")?,
                3 => message.value = codec::str(r, "value")?,
                4 => message.lines = codec::list(r, "lines", codec::message::<ConfigLine>)?,
                5 => message.notes = codec::list(r, "notes", codec::str)?,
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
        if self.version != 0 {
            map.field(out, 2);
            codec::write_int(out, &self.version);
        }
        if !self.value.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.value);
        }
        if !self.lines.is_empty() {
            map.field(out, 4);
            codec::write_list(out, &self.lines, codec::write_message);
        }
        if !self.notes.is_empty() {
            map.field(out, 5);
            codec::write_list(out, &self.notes, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
            + 1 + 5 + self.value.len()
            + 1 + codec::list_size(&self.lines, codec::message_size)
            + 1 + codec::list_size(&self.notes, |item| 5 + item.len())
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.version == 0
            && self.value.is_empty()
            && self.lines.is_empty()
            && self.notes.is_empty()
    }
}

/// A setting's line of explain.
#[cfg(feature = "config")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfigLine {
    pub(crate) path: String,
    /// As JSON, a span spelled, a secret as ***.
    pub(crate) value: String,
    pub(crate) from: String,
    /// The values it went over, newest first.
    pub(crate) over: Vec<String>,
}

#[cfg(feature = "config")]
impl Message for ConfigLine {
    const NAME: &'static str = "config.Line";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.path = codec::str(r, "path")?,
                2 => message.value = codec::str(r, "value")?,
                3 => message.from = codec::str(r, "from")?,
                4 => message.over = codec::list(r, "over", codec::str)?,
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
        if !self.value.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.value);
        }
        if !self.from.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.from);
        }
        if !self.over.is_empty() {
            map.field(out, 4);
            codec::write_list(out, &self.over, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.path.len()
            + 1 + 5 + self.value.len()
            + 1 + 5 + self.from.len()
            + 1 + codec::list_size(&self.over, |item| 5 + item.len())
    }

    fn is_zero(&self) -> bool {
        self.path.is_empty()
            && self.value.is_empty()
            && self.from.is_empty()
            && self.over.is_empty()
    }
}

/// A change of live settings, made on the version the program's value had.
#[cfg(feature = "config")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfigUpdate {
    pub(crate) handle: u64,
    /// A change made since is a conflict.
    pub(crate) version: i64,
    /// The settings it changes, in the declaration's shape.
    pub(crate) changes: String,
}

#[cfg(feature = "config")]
impl Message for ConfigUpdate {
    const NAME: &'static str = "config.Update";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.version = codec::int(r, "version")?,
                3 => message.changes = codec::str(r, "changes")?,
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
        if self.version != 0 {
            map.field(out, 2);
            codec::write_int(out, &self.version);
        }
        if !self.changes.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.changes);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
            + 1 + 5 + self.changes.len()
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.version == 0
            && self.changes.is_empty()
    }
}

/// Drops the changes kept of a setting or of a group, or of every setting.
#[cfg(feature = "config")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfigReset {
    pub(crate) handle: u64,
    /// Empty for every change.
    pub(crate) path: String,
}

#[cfg(feature = "config")]
impl Message for ConfigReset {
    const NAME: &'static str = "config.Reset";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.path = codec::str(r, "path")?,
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
        if !self.path.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.path);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.path.len()
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.path.is_empty()
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
    /// Admin, data or read.
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
    /// The file of the store's encryption key, which the store only reads; encryption.key of its directory, made when needed, when absent.
    pub(crate) encryption_key_file: Option<String>,
    /// Makes the store a node, which keeps a log of each file it writes; from or serve make it one too.
    pub(crate) node: bool,
    /// Where the node takes the files it copies, host:port, tried in order.
    pub(crate) from: Vec<String>,
    /// Where other nodes take files from this one, host:port.
    pub(crate) serve: Option<String>,
    /// The files it copies, as kv.db or sql/app.db; every file but its own records and metrics when empty.
    pub(crate) files: Vec<String>,
    /// What it says to the node it takes from, and asks of the nodes that take from it.
    pub(crate) token: Option<String>,
    /// A write of a file it writes answers once a node copying it holds the commit.
    pub(crate) synchronous: bool,
    /// It is the home of the catalog, which names each file's home.
    pub(crate) catalog: bool,
    /// Where other nodes reach it, host:port; the address serve listens on when absent.
    pub(crate) advertise: Option<String>,
    /// The nodes, by address or id, a synchronous commit waits for one of; any when empty.
    pub(crate) synchronous_with: Vec<String>,
    /// Plain TCP on links beyond this machine, which anyone on the way reads.
    pub(crate) insecure: bool,
    /// Measures the store into its own metrics.db, under names that start with tinystore_.
    pub(crate) self_metrics: bool,
    /// With selfMetrics, leaves out the machine it runs on: its process's memory and processor, the system's.
    pub(crate) without_system: bool,
    /// The metrics series the store holds at most; 100,000 when absent.
    pub(crate) max_series: Option<u64>,
}

impl Message for StoreOptions {
    const NAME: &'static str = "store.Options";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.durability = Some(codec::str(r, "durability")?),
                2 => message.encryption_key_file = Some(codec::str(r, "encryptionKeyFile")?),
                3 => message.node = codec::bool(r, "node")?,
                4 => message.from = codec::list(r, "from", codec::str)?,
                5 => message.serve = Some(codec::str(r, "serve")?),
                6 => message.files = codec::list(r, "files", codec::str)?,
                7 => message.token = Some(codec::str(r, "token")?),
                8 => message.synchronous = codec::bool(r, "synchronous")?,
                9 => message.catalog = codec::bool(r, "catalog")?,
                10 => message.advertise = Some(codec::str(r, "advertise")?),
                11 => message.synchronous_with = codec::list(r, "synchronousWith", codec::str)?,
                12 => message.insecure = codec::bool(r, "insecure")?,
                13 => message.self_metrics = codec::bool(r, "selfMetrics")?,
                14 => message.without_system = codec::bool(r, "withoutSystem")?,
                15 => message.max_series = Some(codec::uint(r, "maxSeries")?),
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
        if let Some(encryption_key_file) = &self.encryption_key_file {
            map.field(out, 2);
            codec::write_str(out, encryption_key_file);
        }
        if self.node {
            map.field(out, 3);
            codec::write_bool(out, &self.node);
        }
        if !self.from.is_empty() {
            map.field(out, 4);
            codec::write_list(out, &self.from, codec::write_str);
        }
        if let Some(serve) = &self.serve {
            map.field(out, 5);
            codec::write_str(out, serve);
        }
        if !self.files.is_empty() {
            map.field(out, 6);
            codec::write_list(out, &self.files, codec::write_str);
        }
        if let Some(token) = &self.token {
            map.field(out, 7);
            codec::write_str(out, token);
        }
        if self.synchronous {
            map.field(out, 8);
            codec::write_bool(out, &self.synchronous);
        }
        if self.catalog {
            map.field(out, 9);
            codec::write_bool(out, &self.catalog);
        }
        if let Some(advertise) = &self.advertise {
            map.field(out, 10);
            codec::write_str(out, advertise);
        }
        if !self.synchronous_with.is_empty() {
            map.field(out, 11);
            codec::write_list(out, &self.synchronous_with, codec::write_str);
        }
        if self.insecure {
            map.field(out, 12);
            codec::write_bool(out, &self.insecure);
        }
        if self.self_metrics {
            map.field(out, 13);
            codec::write_bool(out, &self.self_metrics);
        }
        if self.without_system {
            map.field(out, 14);
            codec::write_bool(out, &self.without_system);
        }
        if let Some(max_series) = &self.max_series {
            map.field(out, 15);
            codec::write_uint(out, max_series);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.durability.as_ref().map_or(0, |durability| 1 + 5 + durability.len())
            + self.encryption_key_file.as_ref().map_or(0, |encryption_key_file| 1 + 5 + encryption_key_file.len())
            + 2
            + 1 + codec::list_size(&self.from, |item| 5 + item.len())
            + self.serve.as_ref().map_or(0, |serve| 1 + 5 + serve.len())
            + 1 + codec::list_size(&self.files, |item| 5 + item.len())
            + self.token.as_ref().map_or(0, |token| 1 + 5 + token.len())
            + 2
            + 2
            + self.advertise.as_ref().map_or(0, |advertise| 1 + 5 + advertise.len())
            + 1 + codec::list_size(&self.synchronous_with, |item| 5 + item.len())
            + 2
            + 2
            + 2
            + self.max_series.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.durability.is_none()
            && self.encryption_key_file.is_none()
            && !self.node
            && self.from.is_empty()
            && self.serve.is_none()
            && self.files.is_empty()
            && self.token.is_none()
            && !self.synchronous
            && !self.catalog
            && self.advertise.is_none()
            && self.synchronous_with.is_empty()
            && !self.insecure
            && !self.self_metrics
            && !self.without_system
            && self.max_series.is_none()
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
    /// Its values are sealed with the server's encryption key; a read connection opens none.
    pub(crate) encrypted: bool,
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
                5 => message.encrypted = codec::bool(r, "encrypted")?,
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
        if self.encrypted {
            map.field(out, 5);
            codec::write_bool(out, &self.encrypted);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.ttl.map_or(0, |_| 10)
            + self.idle.map_or(0, |_| 10)
            + self.database.map_or(0, |_| 10)
            + 2
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.ttl.is_none()
            && self.idle.is_none()
            && self.database.is_none()
            && !self.encrypted
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

/// A key of a lock or a semaphore that a holder through a connection asks
/// for, as docs/guides/locks/README.md has it: the lease lasts `ttl` from its grant and
/// from each renewal, so that a holder whose program froze loses it.
#[cfg(feature = "locks")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LocksHold {
    /// Lock or semaphore.
    pub(crate) kind: String,
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    /// 1 to 1024 bytes.
    pub(crate) key: String,
    /// How many may hold the key at once: 1 for a lock, 1 to 1000.
    pub(crate) limit: u64,
    /// 1 s to 1 hour. Milliseconds.
    pub(crate) ttl: u64,
    /// From the grant, however the lease is renewed. Milliseconds.
    pub(crate) max_hold: Option<u64>,
    /// 0 takes the key only if it is free. Milliseconds.
    pub(crate) wait: u64,
    /// 16 random bytes: asked again, the lease given answers.
    pub(crate) request: Vec<u8>,
}

#[cfg(feature = "locks")]
impl Message for LocksHold {
    const NAME: &'static str = "locks.Hold";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.kind = codec::str(r, "kind")?,
                2 => message.name = codec::str(r, "name")?,
                3 => message.key = codec::str(r, "key")?,
                4 => message.limit = codec::uint(r, "limit")?,
                5 => message.ttl = codec::uint(r, "ttl")?,
                6 => message.max_hold = Some(codec::uint(r, "maxHold")?),
                7 => message.wait = codec::uint(r, "wait")?,
                8 => message.request = codec::bin(r, "request")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.kind.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.kind);
        }
        if !self.name.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.name);
        }
        if !self.key.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.key);
        }
        if self.limit != 0 {
            map.field(out, 4);
            codec::write_uint(out, &self.limit);
        }
        if self.ttl != 0 {
            map.field(out, 5);
            codec::write_uint(out, &self.ttl);
        }
        if let Some(max_hold) = &self.max_hold {
            map.field(out, 6);
            codec::write_uint(out, max_hold);
        }
        if self.wait != 0 {
            map.field(out, 7);
            codec::write_uint(out, &self.wait);
        }
        if !self.request.is_empty() {
            map.field(out, 8);
            codec::write_bin(out, &self.request);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.kind.len()
            + 1 + 5 + self.name.len()
            + 1 + 5 + self.key.len()
            + 10
            + 10
            + self.max_hold.map_or(0, |_| 10)
            + 10
            + 1 + 5 + self.request.len()
    }

    fn is_zero(&self) -> bool {
        self.kind.is_empty()
            && self.name.is_empty()
            && self.key.is_empty()
            && self.limit == 0
            && self.ttl == 0
            && self.max_hold.is_none()
            && self.wait == 0
            && self.request.is_empty()
    }
}

/// A lease, or the key held by others when the hold would not wait.
#[cfg(feature = "locks")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LocksHeld {
    pub(crate) acquired: bool,
    /// 16 random bytes only its holder learns.
    pub(crate) lease: Vec<u8>,
    /// A decimal integer of up to 128 bits; empty for a semaphore.
    pub(crate) fence: String,
    /// How long until maxHold revokes the lease, from now. Milliseconds.
    pub(crate) revoke_in: Option<u64>,
}

#[cfg(feature = "locks")]
impl Message for LocksHeld {
    const NAME: &'static str = "locks.Held";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.acquired = codec::bool(r, "acquired")?,
                2 => message.lease = codec::bin(r, "lease")?,
                3 => message.fence = codec::str(r, "fence")?,
                4 => message.revoke_in = Some(codec::uint(r, "revokeIn")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.acquired {
            map.field(out, 1);
            codec::write_bool(out, &self.acquired);
        }
        if !self.lease.is_empty() {
            map.field(out, 2);
            codec::write_bin(out, &self.lease);
        }
        if !self.fence.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.fence);
        }
        if let Some(revoke_in) = &self.revoke_in {
            map.field(out, 4);
            codec::write_uint(out, revoke_in);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
            + 1 + 5 + self.lease.len()
            + 1 + 5 + self.fence.len()
            + self.revoke_in.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        !self.acquired
            && self.lease.is_empty()
            && self.fence.is_empty()
            && self.revoke_in.is_none()
    }
}

#[cfg(feature = "locks")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LocksRenew {
    pub(crate) lease: Vec<u8>,
    /// How long the lease lasts from now. Milliseconds.
    pub(crate) ttl: u64,
}

#[cfg(feature = "locks")]
impl Message for LocksRenew {
    const NAME: &'static str = "locks.Renew";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.lease = codec::bin(r, "lease")?,
                2 => message.ttl = codec::uint(r, "ttl")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.lease.is_empty() {
            map.field(out, 1);
            codec::write_bin(out, &self.lease);
        }
        if self.ttl != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.ttl);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.lease.len()
            + 10
    }

    fn is_zero(&self) -> bool {
        self.lease.is_empty()
            && self.ttl == 0
    }
}

#[cfg(feature = "locks")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LocksLease {
    pub(crate) lease: Vec<u8>,
}

#[cfg(feature = "locks")]
impl Message for LocksLease {
    const NAME: &'static str = "locks.Lease";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.lease = codec::bin(r, "lease")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.lease.is_empty() {
            map.field(out, 1);
            codec::write_bin(out, &self.lease);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.lease.len()
    }

    fn is_zero(&self) -> bool {
        self.lease.is_empty()
    }
}

/// What a label's value must be: one of values, none of them with not, the
/// label absent too, or a value that starts with prefix.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsCondition {
    pub(crate) values: Vec<String>,
    pub(crate) not: bool,
    pub(crate) prefix: Option<String>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsCondition {
    const NAME: &'static str = "metrics.Condition";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.values = codec::list(r, "values", codec::str)?,
                2 => message.not = codec::bool(r, "not")?,
                3 => message.prefix = Some(codec::str(r, "prefix")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.values.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.values, codec::write_str);
        }
        if self.not {
            map.field(out, 2);
            codec::write_bool(out, &self.not);
        }
        if let Some(prefix) = &self.prefix {
            map.field(out, 3);
            codec::write_str(out, prefix);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.values, |item| 5 + item.len())
            + 2
            + self.prefix.as_ref().map_or(0, |prefix| 1 + 5 + prefix.len())
    }

    fn is_zero(&self) -> bool {
        self.values.is_empty()
            && !self.not
            && self.prefix.is_none()
    }
}

/// The series a call finds, and its range: since before now, or from and to.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsQuery {
    pub(crate) name: Option<String>,
    pub(crate) r#where: BTreeMap<String, MetricsCondition>,
    /// Milliseconds.
    pub(crate) since: Option<u64>,
    /// Unix milliseconds.
    pub(crate) from: Option<i64>,
    /// Excluded; now, a sample taken now in the range, unless given. Unix milliseconds.
    pub(crate) to: Option<i64>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsQuery {
    const NAME: &'static str = "metrics.Query";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = Some(codec::str(r, "name")?),
                2 => message.r#where = codec::names(r, "where", codec::message::<MetricsCondition>)?,
                3 => message.since = Some(codec::uint(r, "since")?),
                4 => message.from = Some(codec::int(r, "from")?),
                5 => message.to = Some(codec::int(r, "to")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(name) = &self.name {
            map.field(out, 1);
            codec::write_str(out, name);
        }
        if !self.r#where.is_empty() {
            map.field(out, 2);
            codec::write_names(out, &self.r#where, codec::write_message);
        }
        if let Some(since) = &self.since {
            map.field(out, 3);
            codec::write_uint(out, since);
        }
        if let Some(from) = &self.from {
            map.field(out, 4);
            codec::write_int(out, from);
        }
        if let Some(to) = &self.to {
            map.field(out, 5);
            codec::write_int(out, to);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.name.as_ref().map_or(0, |name| 1 + 5 + name.len())
            + 1 + codec::names_size(&self.r#where, codec::message_size)
            + self.since.map_or(0, |_| 10)
            + self.from.map_or(0, |_| 10)
            + self.to.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_none()
            && self.r#where.is_empty()
            && self.since.is_none()
            && self.from.is_none()
            && self.to.is_none()
    }
}

/// A series and its samples, or a part of them: a series longer than a body
/// goes on in the next part.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsSeries {
    pub(crate) name: String,
    /// Gauge, counter or histogram.
    pub(crate) kind: String,
    pub(crate) labels: BTreeMap<String, String>,
    /// An int64 a sample.
    pub(crate) times: Vec<u8>,
    /// A float64 a sample.
    pub(crate) values: Vec<u8>,
    /// The next part goes on with this series.
    pub(crate) more: bool,
}

#[cfg(feature = "metrics")]
impl Message for MetricsSeries {
    const NAME: &'static str = "metrics.Series";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.kind = codec::str(r, "kind")?,
                3 => message.labels = codec::names(r, "labels", codec::str)?,
                4 => message.times = codec::bin(r, "times")?,
                5 => message.values = codec::bin(r, "values")?,
                6 => message.more = codec::bool(r, "more")?,
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
        if !self.kind.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.kind);
        }
        if !self.labels.is_empty() {
            map.field(out, 3);
            codec::write_names(out, &self.labels, codec::write_str);
        }
        if !self.times.is_empty() {
            map.field(out, 4);
            codec::write_bin(out, &self.times);
        }
        if !self.values.is_empty() {
            map.field(out, 5);
            codec::write_bin(out, &self.values);
        }
        if self.more {
            map.field(out, 6);
            codec::write_bool(out, &self.more);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + 5 + self.kind.len()
            + 1 + codec::names_size(&self.labels, |item| 5 + item.len())
            + 1 + 5 + self.times.len()
            + 1 + 5 + self.values.len()
            + 2
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.kind.is_empty()
            && self.labels.is_empty()
            && self.times.is_empty()
            && self.values.is_empty()
            && !self.more
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsRead {
    pub(crate) series: Vec<MetricsSeries>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsRead {
    const NAME: &'static str = "metrics.Read";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.series = codec::list(r, "series", codec::message::<MetricsSeries>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.series.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.series, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.series, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.series.is_empty()
    }
}

/// A series' newest sample in the range.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsNewest {
    pub(crate) name: String,
    pub(crate) labels: BTreeMap<String, String>,
    /// Unix milliseconds.
    pub(crate) at: i64,
    pub(crate) value: f64,
}

#[cfg(feature = "metrics")]
impl Message for MetricsNewest {
    const NAME: &'static str = "metrics.Newest";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.labels = codec::names(r, "labels", codec::str)?,
                3 => message.at = codec::int(r, "at")?,
                4 => message.value = codec::float(r, "value")?,
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
        if !self.labels.is_empty() {
            map.field(out, 2);
            codec::write_names(out, &self.labels, codec::write_str);
        }
        if self.at != 0 {
            map.field(out, 3);
            codec::write_int(out, &self.at);
        }
        if self.value.to_bits() != 0 {
            map.field(out, 4);
            codec::write_float(out, &self.value);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + codec::names_size(&self.labels, |item| 5 + item.len())
            + 10
            + 10
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.labels.is_empty()
            && self.at == 0
            && self.value.to_bits() == 0
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsLatest {
    pub(crate) series: Vec<MetricsNewest>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsLatest {
    const NAME: &'static str = "metrics.Latest";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.series = codec::list(r, "series", codec::message::<MetricsNewest>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.series.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.series, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.series, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.series.is_empty()
    }
}

/// An aggregate: read's query, its buckets, its op and its groups. Without
/// by and without, every series of a name is one group; without: [] keeps
/// every series apart.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsAggregateAsked {
    pub(crate) query: MetricsQuery,
    /// Milliseconds.
    pub(crate) every: Option<u64>,
    /// Count, sum, avg, min, max, increase, rate, delta, first, last, or p and a rank; none for a read's plan.
    pub(crate) op: Option<String>,
    pub(crate) by: Option<Vec<String>>,
    pub(crate) without: Option<Vec<String>>,
    /// A timer's percentile also gives a point between its bounds.
    pub(crate) estimate: bool,
}

#[cfg(feature = "metrics")]
impl Message for MetricsAggregateAsked {
    const NAME: &'static str = "metrics.AggregateAsked";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.query = MetricsQuery::read(r)?,
                2 => message.every = Some(codec::uint(r, "every")?),
                3 => message.op = Some(codec::str(r, "op")?),
                4 => message.by = Some(codec::list(r, "by", codec::str)?),
                5 => message.without = Some(codec::list(r, "without", codec::str)?),
                6 => message.estimate = codec::bool(r, "estimate")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.query.is_zero() {
            map.field(out, 1);
            codec::write_message(out, &self.query);
        }
        if let Some(every) = &self.every {
            map.field(out, 2);
            codec::write_uint(out, every);
        }
        if let Some(op) = &self.op {
            map.field(out, 3);
            codec::write_str(out, op);
        }
        if let Some(by) = &self.by {
            map.field(out, 4);
            codec::write_list(out, by, codec::write_str);
        }
        if let Some(without) = &self.without {
            map.field(out, 5);
            codec::write_list(out, without, codec::write_str);
        }
        if self.estimate {
            map.field(out, 6);
            codec::write_bool(out, &self.estimate);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + self.query.size()
            + self.every.map_or(0, |_| 10)
            + self.op.as_ref().map_or(0, |op| 1 + 5 + op.len())
            + self.by.as_ref().map_or(0, |by| 1 + codec::list_size(by, |item| 5 + item.len()))
            + self.without.as_ref().map_or(0, |without| 1 + codec::list_size(without, |item| 5 + item.len()))
            + 2
    }

    fn is_zero(&self) -> bool {
        self.query.is_zero()
            && self.every.is_none()
            && self.op.is_none()
            && self.by.is_none()
            && self.without.is_none()
            && !self.estimate
    }
}

/// A bucket of an aggregate: its value, or a timer's percentile's bounds.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsBucket {
    /// Where it begins. Unix milliseconds.
    pub(crate) at: i64,
    /// None for a timer's percentile.
    pub(crate) value: Option<f64>,
    pub(crate) lower: Option<f64>,
    /// Infinity past the last bound.
    pub(crate) upper: Option<f64>,
    pub(crate) estimate: Option<f64>,
    pub(crate) samples: u64,
    pub(crate) resets: u64,
    /// Its value comes from less than its span.
    pub(crate) partial: bool,
    /// Its first step began before the bucket before it.
    pub(crate) gap: bool,
}

#[cfg(feature = "metrics")]
impl Message for MetricsBucket {
    const NAME: &'static str = "metrics.Bucket";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.at = codec::int(r, "at")?,
                2 => message.value = Some(codec::float(r, "value")?),
                3 => message.lower = Some(codec::float(r, "lower")?),
                4 => message.upper = Some(codec::float(r, "upper")?),
                5 => message.estimate = Some(codec::float(r, "estimate")?),
                6 => message.samples = codec::uint(r, "samples")?,
                7 => message.resets = codec::uint(r, "resets")?,
                8 => message.partial = codec::bool(r, "partial")?,
                9 => message.gap = codec::bool(r, "gap")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.at != 0 {
            map.field(out, 1);
            codec::write_int(out, &self.at);
        }
        if let Some(value) = &self.value {
            map.field(out, 2);
            codec::write_float(out, value);
        }
        if let Some(lower) = &self.lower {
            map.field(out, 3);
            codec::write_float(out, lower);
        }
        if let Some(upper) = &self.upper {
            map.field(out, 4);
            codec::write_float(out, upper);
        }
        if let Some(estimate) = &self.estimate {
            map.field(out, 5);
            codec::write_float(out, estimate);
        }
        if self.samples != 0 {
            map.field(out, 6);
            codec::write_uint(out, &self.samples);
        }
        if self.resets != 0 {
            map.field(out, 7);
            codec::write_uint(out, &self.resets);
        }
        if self.partial {
            map.field(out, 8);
            codec::write_bool(out, &self.partial);
        }
        if self.gap {
            map.field(out, 9);
            codec::write_bool(out, &self.gap);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + self.value.map_or(0, |_| 10)
            + self.lower.map_or(0, |_| 10)
            + self.upper.map_or(0, |_| 10)
            + self.estimate.map_or(0, |_| 10)
            + 10
            + 10
            + 2
            + 2
    }

    fn is_zero(&self) -> bool {
        self.at == 0
            && self.value.is_none()
            && self.lower.is_none()
            && self.upper.is_none()
            && self.estimate.is_none()
            && self.samples == 0
            && self.resets == 0
            && !self.partial
            && !self.gap
    }
}

/// A group's buckets, or a part of them: a group longer than a body goes on
/// in the next part.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsGroup {
    pub(crate) name: String,
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) buckets: Vec<MetricsBucket>,
    pub(crate) more: bool,
}

#[cfg(feature = "metrics")]
impl Message for MetricsGroup {
    const NAME: &'static str = "metrics.Group";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.labels = codec::names(r, "labels", codec::str)?,
                3 => message.buckets = codec::list(r, "buckets", codec::message::<MetricsBucket>)?,
                4 => message.more = codec::bool(r, "more")?,
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
        if !self.labels.is_empty() {
            map.field(out, 2);
            codec::write_names(out, &self.labels, codec::write_str);
        }
        if !self.buckets.is_empty() {
            map.field(out, 3);
            codec::write_list(out, &self.buckets, codec::write_message);
        }
        if self.more {
            map.field(out, 4);
            codec::write_bool(out, &self.more);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + codec::names_size(&self.labels, |item| 5 + item.len())
            + 1 + codec::list_size(&self.buckets, codec::message_size)
            + 2
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.labels.is_empty()
            && self.buckets.is_empty()
            && !self.more
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsAggregates {
    pub(crate) groups: Vec<MetricsGroup>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsAggregates {
    const NAME: &'static str = "metrics.Aggregates";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.groups = codec::list(r, "groups", codec::message::<MetricsGroup>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.groups.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.groups, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.groups, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.groups.is_empty()
    }
}

/// What a call would spend of each bound, and the bound it would stop at.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsPlan {
    pub(crate) series: u64,
    pub(crate) bytes: u64,
    pub(crate) decoded: u64,
    pub(crate) exceeds: Option<MetricsExceeds>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsPlan {
    const NAME: &'static str = "metrics.Plan";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.series = codec::uint(r, "series")?,
                2 => message.bytes = codec::uint(r, "bytes")?,
                3 => message.decoded = codec::uint(r, "decoded")?,
                4 => message.exceeds = Some(MetricsExceeds::read(r)?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.series != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.series);
        }
        if self.bytes != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.bytes);
        }
        if self.decoded != 0 {
            map.field(out, 3);
            codec::write_uint(out, &self.decoded);
        }
        if let Some(exceeds) = &self.exceeds {
            map.field(out, 4);
            codec::write_message(out, exceeds);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
            + 10
            + self.exceeds.as_ref().map_or(0, |exceeds| 1 + exceeds.size())
    }

    fn is_zero(&self) -> bool {
        self.series == 0
            && self.bytes == 0
            && self.decoded == 0
            && self.exceeds.is_none()
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsExceeds {
    /// Series, bytes read or samples decoded.
    pub(crate) limit: String,
    pub(crate) wanted: u64,
    pub(crate) bound: u64,
}

#[cfg(feature = "metrics")]
impl Message for MetricsExceeds {
    const NAME: &'static str = "metrics.Exceeds";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.limit = codec::str(r, "limit")?,
                2 => message.wanted = codec::uint(r, "wanted")?,
                3 => message.bound = codec::uint(r, "bound")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.limit.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.limit);
        }
        if self.wanted != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.wanted);
        }
        if self.bound != 0 {
            map.field(out, 3);
            codec::write_uint(out, &self.bound);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.limit.len()
            + 10
            + 10
    }

    fn is_zero(&self) -> bool {
        self.limit.is_empty()
            && self.wanted == 0
            && self.bound == 0
    }
}

/// A series to write, its samples as two columns.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsNewSeries {
    pub(crate) name: String,
    /// Gauge, counter or histogram.
    pub(crate) kind: String,
    pub(crate) labels: BTreeMap<String, String>,
    /// An int64 a sample.
    pub(crate) times: Vec<u8>,
    /// A float64 a sample.
    pub(crate) values: Vec<u8>,
    /// How long the name's samples stay: its own, kept in the file. Milliseconds.
    pub(crate) keep: Option<u64>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsNewSeries {
    const NAME: &'static str = "metrics.NewSeries";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.kind = codec::str(r, "kind")?,
                3 => message.labels = codec::names(r, "labels", codec::str)?,
                4 => message.times = codec::bin(r, "times")?,
                5 => message.values = codec::bin(r, "values")?,
                6 => message.keep = Some(codec::uint(r, "keep")?),
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
        if !self.kind.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.kind);
        }
        if !self.labels.is_empty() {
            map.field(out, 3);
            codec::write_names(out, &self.labels, codec::write_str);
        }
        if !self.times.is_empty() {
            map.field(out, 4);
            codec::write_bin(out, &self.times);
        }
        if !self.values.is_empty() {
            map.field(out, 5);
            codec::write_bin(out, &self.values);
        }
        if let Some(keep) = &self.keep {
            map.field(out, 6);
            codec::write_uint(out, keep);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + 5 + self.kind.len()
            + 1 + codec::names_size(&self.labels, |item| 5 + item.len())
            + 1 + 5 + self.times.len()
            + 1 + 5 + self.values.len()
            + self.keep.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.kind.is_empty()
            && self.labels.is_empty()
            && self.times.is_empty()
            && self.values.is_empty()
            && self.keep.is_none()
    }
}

/// Series to write, every one or none.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsIngest {
    pub(crate) series: Vec<MetricsNewSeries>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsIngest {
    const NAME: &'static str = "metrics.Ingest";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.series = codec::list(r, "series", codec::message::<MetricsNewSeries>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.series.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.series, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.series, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.series.is_empty()
    }
}

/// A page of names, of a name's labels or of a label's values.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsPageAsked {
    /// The name whose labels, or whose label's values, it lists.
    pub(crate) name: Option<String>,
    /// The label whose values it lists.
    pub(crate) label: Option<String>,
    pub(crate) prefix: Option<String>,
    /// A page's next.
    pub(crate) after: Option<String>,
    /// 1000 unless given, 10,000 at most.
    pub(crate) limit: Option<u64>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsPageAsked {
    const NAME: &'static str = "metrics.PageAsked";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = Some(codec::str(r, "name")?),
                2 => message.label = Some(codec::str(r, "label")?),
                3 => message.prefix = Some(codec::str(r, "prefix")?),
                4 => message.after = Some(codec::str(r, "after")?),
                5 => message.limit = Some(codec::uint(r, "limit")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(name) = &self.name {
            map.field(out, 1);
            codec::write_str(out, name);
        }
        if let Some(label) = &self.label {
            map.field(out, 2);
            codec::write_str(out, label);
        }
        if let Some(prefix) = &self.prefix {
            map.field(out, 3);
            codec::write_str(out, prefix);
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
            + self.name.as_ref().map_or(0, |name| 1 + 5 + name.len())
            + self.label.as_ref().map_or(0, |label| 1 + 5 + label.len())
            + self.prefix.as_ref().map_or(0, |prefix| 1 + 5 + prefix.len())
            + self.after.as_ref().map_or(0, |after| 1 + 5 + after.len())
            + self.limit.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_none()
            && self.label.is_none()
            && self.prefix.is_none()
            && self.after.is_none()
            && self.limit.is_none()
    }
}

/// A name the store holds samples of.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsName {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) series_count: u64,
    pub(crate) unit: String,
    pub(crate) help: String,
    /// Milliseconds.
    pub(crate) keep: u64,
    /// Its oldest sample, or where its keep begins while it holds older. Unix milliseconds.
    pub(crate) retained_from: i64,
    /// Unix milliseconds.
    pub(crate) newest: i64,
}

#[cfg(feature = "metrics")]
impl Message for MetricsName {
    const NAME: &'static str = "metrics.Name";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.kind = codec::str(r, "kind")?,
                3 => message.series_count = codec::uint(r, "seriesCount")?,
                4 => message.unit = codec::str(r, "unit")?,
                5 => message.help = codec::str(r, "help")?,
                6 => message.keep = codec::uint(r, "keep")?,
                7 => message.retained_from = codec::int(r, "retainedFrom")?,
                8 => message.newest = codec::int(r, "newest")?,
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
        if !self.kind.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.kind);
        }
        if self.series_count != 0 {
            map.field(out, 3);
            codec::write_uint(out, &self.series_count);
        }
        if !self.unit.is_empty() {
            map.field(out, 4);
            codec::write_str(out, &self.unit);
        }
        if !self.help.is_empty() {
            map.field(out, 5);
            codec::write_str(out, &self.help);
        }
        if self.keep != 0 {
            map.field(out, 6);
            codec::write_uint(out, &self.keep);
        }
        if self.retained_from != 0 {
            map.field(out, 7);
            codec::write_int(out, &self.retained_from);
        }
        if self.newest != 0 {
            map.field(out, 8);
            codec::write_int(out, &self.newest);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + 5 + self.kind.len()
            + 10
            + 1 + 5 + self.unit.len()
            + 1 + 5 + self.help.len()
            + 10
            + 10
            + 10
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.kind.is_empty()
            && self.series_count == 0
            && self.unit.is_empty()
            && self.help.is_empty()
            && self.keep == 0
            && self.retained_from == 0
            && self.newest == 0
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsNamePage {
    pub(crate) names: Vec<MetricsName>,
    /// The page after it; the last page has none.
    pub(crate) next: Option<String>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsNamePage {
    const NAME: &'static str = "metrics.NamePage";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.names = codec::list(r, "names", codec::message::<MetricsName>)?,
                2 => message.next = Some(codec::str(r, "next")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.names.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.names, codec::write_message);
        }
        if let Some(next) = &self.next {
            map.field(out, 2);
            codec::write_str(out, next);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.names, codec::message_size)
            + self.next.as_ref().map_or(0, |next| 1 + 5 + next.len())
    }

    fn is_zero(&self) -> bool {
        self.names.is_empty()
            && self.next.is_none()
    }
}

/// A page of a name's labels, or of a label's values.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsTextPage {
    pub(crate) texts: Vec<String>,
    pub(crate) next: Option<String>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsTextPage {
    const NAME: &'static str = "metrics.TextPage";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.texts = codec::list(r, "texts", codec::str)?,
                2 => message.next = Some(codec::str(r, "next")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.texts.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.texts, codec::write_str);
        }
        if let Some(next) = &self.next {
            map.field(out, 2);
            codec::write_str(out, next);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.texts, |item| 5 + item.len())
            + self.next.as_ref().map_or(0, |next| 1 + 5 + next.len())
    }

    fn is_zero(&self) -> bool {
        self.texts.is_empty()
            && self.next.is_none()
    }
}

/// A name's unit and help; empty ones remove them.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsDescription {
    pub(crate) name: String,
    pub(crate) unit: String,
    pub(crate) help: String,
}

#[cfg(feature = "metrics")]
impl Message for MetricsDescription {
    const NAME: &'static str = "metrics.Description";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.unit = codec::str(r, "unit")?,
                3 => message.help = codec::str(r, "help")?,
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
        if !self.unit.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.unit);
        }
        if !self.help.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.help);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + 5 + self.unit.len()
            + 1 + 5 + self.help.len()
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.unit.is_empty()
            && self.help.is_empty()
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsNameAsked {
    pub(crate) name: String,
}

#[cfg(feature = "metrics")]
impl Message for MetricsNameAsked {
    const NAME: &'static str = "metrics.NameAsked";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
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
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
    }
}

/// One series, by exactly its name and labels.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsSeriesNamed {
    pub(crate) name: String,
    pub(crate) labels: BTreeMap<String, String>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsSeriesNamed {
    const NAME: &'static str = "metrics.SeriesNamed";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.labels = codec::names(r, "labels", codec::str)?,
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
        if !self.labels.is_empty() {
            map.field(out, 2);
            codec::write_names(out, &self.labels, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + codec::names_size(&self.labels, |item| 5 + item.len())
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.labels.is_empty()
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsDeleted {
    pub(crate) found: bool,
}

#[cfg(feature = "metrics")]
impl Message for MetricsDeleted {
    const NAME: &'static str = "metrics.Deleted";

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

/// A series whose bytes no longer read, and what broke.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsDamage {
    pub(crate) name: String,
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) error: String,
}

#[cfg(feature = "metrics")]
impl Message for MetricsDamage {
    const NAME: &'static str = "metrics.Damage";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.labels = codec::names(r, "labels", codec::str)?,
                3 => message.error = codec::str(r, "error")?,
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
        if !self.labels.is_empty() {
            map.field(out, 2);
            codec::write_names(out, &self.labels, codec::write_str);
        }
        if !self.error.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.error);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + codec::names_size(&self.labels, |item| 5 + item.len())
            + 1 + 5 + self.error.len()
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.labels.is_empty()
            && self.error.is_empty()
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsDamages {
    pub(crate) damages: Vec<MetricsDamage>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsDamages {
    const NAME: &'static str = "metrics.Damages";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.damages = codec::list(r, "damages", codec::message::<MetricsDamage>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.damages.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.damages, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.damages, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.damages.is_empty()
    }
}

/// An SDK's instruments' stream: its session, sixteen random bytes drawn when
/// it opened the store, and the last batch the store answered it, 0 for none.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsSession {
    pub(crate) id: Vec<u8>,
    pub(crate) after: u64,
}

#[cfg(feature = "metrics")]
impl Message for MetricsSession {
    const NAME: &'static str = "metrics.Session";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.id = codec::bin(r, "id")?,
                2 => message.after = codec::uint(r, "after")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.id.is_empty() {
            map.field(out, 1);
            codec::write_bin(out, &self.id);
        }
        if self.after != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.after);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.id.len()
            + 10
    }

    fn is_zero(&self) -> bool {
        self.id.is_empty()
            && self.after == 0
    }
}

/// An instrument a batch counts into, by a number the stream gives it.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsInstrument {
    pub(crate) number: u64,
    pub(crate) name: String,
    /// Counter, gauge, timer or histogram.
    pub(crate) form: String,
    pub(crate) unit: Option<String>,
    pub(crate) help: Option<String>,
    /// Milliseconds.
    pub(crate) keep: Option<u64>,
    /// A timer's or a histogram's upper bounds, ascending.
    pub(crate) bounds: Option<Vec<f64>>,
    /// A timer's spread on the store's grid.
    pub(crate) exponential: bool,
}

#[cfg(feature = "metrics")]
impl Message for MetricsInstrument {
    const NAME: &'static str = "metrics.Instrument";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.number = codec::uint(r, "number")?,
                2 => message.name = codec::str(r, "name")?,
                3 => message.form = codec::str(r, "form")?,
                4 => message.unit = Some(codec::str(r, "unit")?),
                5 => message.help = Some(codec::str(r, "help")?),
                6 => message.keep = Some(codec::uint(r, "keep")?),
                7 => message.bounds = Some(codec::list(r, "bounds", codec::float)?),
                8 => message.exponential = codec::bool(r, "exponential")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.number != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.number);
        }
        if !self.name.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.name);
        }
        if !self.form.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.form);
        }
        if let Some(unit) = &self.unit {
            map.field(out, 4);
            codec::write_str(out, unit);
        }
        if let Some(help) = &self.help {
            map.field(out, 5);
            codec::write_str(out, help);
        }
        if let Some(keep) = &self.keep {
            map.field(out, 6);
            codec::write_uint(out, keep);
        }
        if let Some(bounds) = &self.bounds {
            map.field(out, 7);
            codec::write_list(out, bounds, codec::write_float);
        }
        if self.exponential {
            map.field(out, 8);
            codec::write_bool(out, &self.exponential);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.name.len()
            + 1 + 5 + self.form.len()
            + self.unit.as_ref().map_or(0, |unit| 1 + 5 + unit.len())
            + self.help.as_ref().map_or(0, |help| 1 + 5 + help.len())
            + self.keep.map_or(0, |_| 10)
            + self.bounds.as_ref().map_or(0, |bounds| 1 + codec::list_size(bounds, |_| 9))
            + 2
    }

    fn is_zero(&self) -> bool {
        self.number == 0
            && self.name.is_empty()
            && self.form.is_empty()
            && self.unit.is_none()
            && self.help.is_none()
            && self.keep.is_none()
            && self.bounds.is_none()
            && !self.exponential
    }
}

/// A set of labels of an instrument, by a number the stream gives it, or its
/// series of __overflow, where a client counts past the sets it holds.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsSet {
    pub(crate) number: u64,
    pub(crate) instrument: u64,
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) overflow: bool,
}

#[cfg(feature = "metrics")]
impl Message for MetricsSet {
    const NAME: &'static str = "metrics.Set";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.number = codec::uint(r, "number")?,
                2 => message.instrument = codec::uint(r, "instrument")?,
                3 => message.labels = codec::names(r, "labels", codec::str)?,
                4 => message.overflow = codec::bool(r, "overflow")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.number != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.number);
        }
        if self.instrument != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.instrument);
        }
        if !self.labels.is_empty() {
            map.field(out, 3);
            codec::write_names(out, &self.labels, codec::write_str);
        }
        if self.overflow {
            map.field(out, 4);
            codec::write_bool(out, &self.overflow);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
            + 1 + codec::names_size(&self.labels, |item| 5 + item.len())
            + 2
    }

    fn is_zero(&self) -> bool {
        self.number == 0
            && self.instrument == 0
            && self.labels.is_empty()
            && !self.overflow
    }
}

/// What a set of labels changed since the batch before: a counter's sum to
/// add, a gauge's value, or what a timer or a histogram measured.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsChange {
    pub(crate) set: u64,
    pub(crate) add: Option<f64>,
    pub(crate) value: Option<f64>,
    pub(crate) count: u64,
    pub(crate) sum: f64,
    pub(crate) longest: f64,
    /// Each bound's own count, +Inf last.
    pub(crate) buckets: Vec<u64>,
    /// Each interval of the grid's count, by its label: 3:13, zero.
    pub(crate) bins: BTreeMap<String, u64>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsChange {
    const NAME: &'static str = "metrics.Change";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.set = codec::uint(r, "set")?,
                2 => message.add = Some(codec::float(r, "add")?),
                3 => message.value = Some(codec::float(r, "value")?),
                4 => message.count = codec::uint(r, "count")?,
                5 => message.sum = codec::float(r, "sum")?,
                6 => message.longest = codec::float(r, "longest")?,
                7 => message.buckets = codec::list(r, "buckets", codec::uint)?,
                8 => message.bins = codec::names(r, "bins", codec::uint)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.set != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.set);
        }
        if let Some(add) = &self.add {
            map.field(out, 2);
            codec::write_float(out, add);
        }
        if let Some(value) = &self.value {
            map.field(out, 3);
            codec::write_float(out, value);
        }
        if self.count != 0 {
            map.field(out, 4);
            codec::write_uint(out, &self.count);
        }
        if self.sum.to_bits() != 0 {
            map.field(out, 5);
            codec::write_float(out, &self.sum);
        }
        if self.longest.to_bits() != 0 {
            map.field(out, 6);
            codec::write_float(out, &self.longest);
        }
        if !self.buckets.is_empty() {
            map.field(out, 7);
            codec::write_list(out, &self.buckets, codec::write_uint);
        }
        if !self.bins.is_empty() {
            map.field(out, 8);
            codec::write_names(out, &self.bins, codec::write_uint);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + self.add.map_or(0, |_| 10)
            + self.value.map_or(0, |_| 10)
            + 10
            + 10
            + 10
            + 1 + codec::list_size(&self.buckets, |_| 9)
            + 1 + codec::names_size(&self.bins, |_| 9)
    }

    fn is_zero(&self) -> bool {
        self.set == 0
            && self.add.is_none()
            && self.value.is_none()
            && self.count == 0
            && self.sum.to_bits() == 0
            && self.longest.to_bits() == 0
            && self.buckets.is_empty()
            && self.bins.is_empty()
    }
}

/// A batch of an SDK's changes, numbered from 1 in its session, with the
/// instruments and the sets of labels it names that the stream has not
/// declared yet.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsBatch {
    pub(crate) number: u64,
    pub(crate) instruments: Vec<MetricsInstrument>,
    pub(crate) sets: Vec<MetricsSet>,
    pub(crate) changes: Vec<MetricsChange>,
    /// The store writes its totals before it answers.
    pub(crate) flush: bool,
}

#[cfg(feature = "metrics")]
impl Message for MetricsBatch {
    const NAME: &'static str = "metrics.Batch";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.number = codec::uint(r, "number")?,
                2 => message.instruments = codec::list(r, "instruments", codec::message::<MetricsInstrument>)?,
                3 => message.sets = codec::list(r, "sets", codec::message::<MetricsSet>)?,
                4 => message.changes = codec::list(r, "changes", codec::message::<MetricsChange>)?,
                5 => message.flush = codec::bool(r, "flush")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.number != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.number);
        }
        if !self.instruments.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.instruments, codec::write_message);
        }
        if !self.sets.is_empty() {
            map.field(out, 3);
            codec::write_list(out, &self.sets, codec::write_message);
        }
        if !self.changes.is_empty() {
            map.field(out, 4);
            codec::write_list(out, &self.changes, codec::write_message);
        }
        if self.flush {
            map.field(out, 5);
            codec::write_bool(out, &self.flush);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.instruments, codec::message_size)
            + 1 + codec::list_size(&self.sets, codec::message_size)
            + 1 + codec::list_size(&self.changes, codec::message_size)
            + 2
    }

    fn is_zero(&self) -> bool {
        self.number == 0
            && self.instruments.is_empty()
            && self.sets.is_empty()
            && self.changes.is_empty()
            && !self.flush
    }
}

/// An instrument the store refused, by the stream's number for it.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsRefused {
    pub(crate) instrument: u64,
    pub(crate) error: String,
}

#[cfg(feature = "metrics")]
impl Message for MetricsRefused {
    const NAME: &'static str = "metrics.Refused";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.instrument = codec::uint(r, "instrument")?,
                2 => message.error = codec::str(r, "error")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.instrument != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.instrument);
        }
        if !self.error.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.error);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.error.len()
    }

    fn is_zero(&self) -> bool {
        self.instrument == 0
            && self.error.is_empty()
    }
}

/// The store's answer: the last batch of the session it applied, and whether
/// it knew the session; the stream's RESPONSE says it first.
#[cfg(feature = "metrics")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsAck {
    pub(crate) applied: u64,
    pub(crate) known: bool,
    pub(crate) refused: Vec<MetricsRefused>,
}

#[cfg(feature = "metrics")]
impl Message for MetricsAck {
    const NAME: &'static str = "metrics.Ack";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.applied = codec::uint(r, "applied")?,
                2 => message.known = codec::bool(r, "known")?,
                3 => message.refused = codec::list(r, "refused", codec::message::<MetricsRefused>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.applied != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.applied);
        }
        if self.known {
            map.field(out, 2);
            codec::write_bool(out, &self.known);
        }
        if !self.refused.is_empty() {
            map.field(out, 3);
            codec::write_list(out, &self.refused, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 2
            + 1 + codec::list_size(&self.refused, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.applied == 0
            && !self.known
            && self.refused.is_empty()
    }
}

/// Opens a stream of events by name, made with its first event.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) stream: String,
    /// How long it keeps its events: its own, kept in the file. Milliseconds.
    pub(crate) keep: Option<u64>,
}

#[cfg(feature = "records")]
impl Message for RecordsOpen {
    const NAME: &'static str = "records.Open";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.stream = codec::str(r, "stream")?,
                2 => message.keep = Some(codec::uint(r, "keep")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.stream.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.stream);
        }
        if let Some(keep) = &self.keep {
            map.field(out, 2);
            codec::write_uint(out, keep);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.stream.len()
            + self.keep.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.stream.is_empty()
            && self.keep.is_none()
    }
}

/// An event to write.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsNewEvent {
    pub(crate) name: String,
    /// The store's clock's now unless given. Unix nanoseconds.
    pub(crate) at: Option<i64>,
    /// -4 debug, 0 info, 4 warn, 8 error, between them as slog counts.
    pub(crate) level: Option<i64>,
    pub(crate) message: Option<String>,
    /// An object.
    pub(crate) fields: Option<String>,
    /// An object.
    pub(crate) context: Option<String>,
    /// 16 bytes.
    pub(crate) trace_id: Option<Vec<u8>>,
    /// 8 bytes.
    pub(crate) span_id: Option<Vec<u8>>,
}

#[cfg(feature = "records")]
impl Message for RecordsNewEvent {
    const NAME: &'static str = "records.NewEvent";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.at = Some(codec::int(r, "at")?),
                3 => message.level = Some(codec::int(r, "level")?),
                4 => message.message = Some(codec::str(r, "message")?),
                5 => message.fields = Some(codec::str(r, "fields")?),
                6 => message.context = Some(codec::str(r, "context")?),
                7 => message.trace_id = Some(codec::bin(r, "traceId")?),
                8 => message.span_id = Some(codec::bin(r, "spanId")?),
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
        if let Some(at) = &self.at {
            map.field(out, 2);
            codec::write_int(out, at);
        }
        if let Some(level) = &self.level {
            map.field(out, 3);
            codec::write_int(out, level);
        }
        if let Some(message) = &self.message {
            map.field(out, 4);
            codec::write_str(out, message);
        }
        if let Some(fields) = &self.fields {
            map.field(out, 5);
            codec::write_str(out, fields);
        }
        if let Some(context) = &self.context {
            map.field(out, 6);
            codec::write_str(out, context);
        }
        if let Some(trace_id) = &self.trace_id {
            map.field(out, 7);
            codec::write_bin(out, trace_id);
        }
        if let Some(span_id) = &self.span_id {
            map.field(out, 8);
            codec::write_bin(out, span_id);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.at.map_or(0, |_| 10)
            + self.level.map_or(0, |_| 10)
            + self.message.as_ref().map_or(0, |message| 1 + 5 + message.len())
            + self.fields.as_ref().map_or(0, |fields| 1 + 5 + fields.len())
            + self.context.as_ref().map_or(0, |context| 1 + 5 + context.len())
            + self.trace_id.as_ref().map_or(0, |trace_id| 1 + 5 + trace_id.len())
            + self.span_id.as_ref().map_or(0, |span_id| 1 + 5 + span_id.len())
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.at.is_none()
            && self.level.is_none()
            && self.message.is_none()
            && self.fields.is_none()
            && self.context.is_none()
            && self.trace_id.is_none()
            && self.span_id.is_none()
    }
}

/// Events to write to a stream, all of them or none.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsAppend {
    pub(crate) handle: u64,
    pub(crate) events: Vec<RecordsNewEvent>,
}

#[cfg(feature = "records")]
impl Message for RecordsAppend {
    const NAME: &'static str = "records.Append";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.events = codec::list(r, "events", codec::message::<RecordsNewEvent>)?,
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
        if !self.events.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.events, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.events, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.events.is_empty()
    }
}

/// An event as a read gives it.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsEvent {
    /// Unix nanoseconds.
    pub(crate) at: i64,
    pub(crate) stream: String,
    pub(crate) name: String,
    pub(crate) level: Option<i64>,
    pub(crate) message: Option<String>,
    /// An object; none when it has no field.
    pub(crate) fields: Option<String>,
    pub(crate) context: Option<String>,
    pub(crate) trace_id: Option<Vec<u8>>,
    pub(crate) span_id: Option<Vec<u8>>,
}

#[cfg(feature = "records")]
impl Message for RecordsEvent {
    const NAME: &'static str = "records.Event";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.at = codec::int(r, "at")?,
                2 => message.stream = codec::str(r, "stream")?,
                3 => message.name = codec::str(r, "name")?,
                4 => message.level = Some(codec::int(r, "level")?),
                5 => message.message = Some(codec::str(r, "message")?),
                6 => message.fields = Some(codec::str(r, "fields")?),
                7 => message.context = Some(codec::str(r, "context")?),
                8 => message.trace_id = Some(codec::bin(r, "traceId")?),
                9 => message.span_id = Some(codec::bin(r, "spanId")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.at != 0 {
            map.field(out, 1);
            codec::write_int(out, &self.at);
        }
        if !self.stream.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.stream);
        }
        if !self.name.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.name);
        }
        if let Some(level) = &self.level {
            map.field(out, 4);
            codec::write_int(out, level);
        }
        if let Some(message) = &self.message {
            map.field(out, 5);
            codec::write_str(out, message);
        }
        if let Some(fields) = &self.fields {
            map.field(out, 6);
            codec::write_str(out, fields);
        }
        if let Some(context) = &self.context {
            map.field(out, 7);
            codec::write_str(out, context);
        }
        if let Some(trace_id) = &self.trace_id {
            map.field(out, 8);
            codec::write_bin(out, trace_id);
        }
        if let Some(span_id) = &self.span_id {
            map.field(out, 9);
            codec::write_bin(out, span_id);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.stream.len()
            + 1 + 5 + self.name.len()
            + self.level.map_or(0, |_| 10)
            + self.message.as_ref().map_or(0, |message| 1 + 5 + message.len())
            + self.fields.as_ref().map_or(0, |fields| 1 + 5 + fields.len())
            + self.context.as_ref().map_or(0, |context| 1 + 5 + context.len())
            + self.trace_id.as_ref().map_or(0, |trace_id| 1 + 5 + trace_id.len())
            + self.span_id.as_ref().map_or(0, |span_id| 1 + 5 + span_id.len())
    }

    fn is_zero(&self) -> bool {
        self.at == 0
            && self.stream.is_empty()
            && self.name.is_empty()
            && self.level.is_none()
            && self.message.is_none()
            && self.fields.is_none()
            && self.context.is_none()
            && self.trace_id.is_none()
            && self.span_id.is_none()
    }
}

/// What a read finds: each condition given holds.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsQuery {
    /// The stream opened as it; every stream without one.
    pub(crate) handle: Option<u64>,
    /// The span before now; or from and to. Milliseconds.
    pub(crate) since: Option<u64>,
    /// Unix nanoseconds.
    pub(crate) from: Option<i64>,
    /// Unix nanoseconds.
    pub(crate) to: Option<i64>,
    pub(crate) streams: Vec<String>,
    pub(crate) names: Vec<String>,
    pub(crate) min_level: Option<i64>,
    pub(crate) trace_id: Option<Vec<u8>>,
    /// Each key with this value, in fields or in context.
    pub(crate) r#where: BTreeMap<String, String>,
    pub(crate) search: Option<String>,
    pub(crate) newest: bool,
    /// A page's events, top's values, fieldNames' keys.
    pub(crate) limit: Option<u64>,
    /// A page's next.
    pub(crate) after: Option<String>,
}

#[cfg(feature = "records")]
impl Message for RecordsQuery {
    const NAME: &'static str = "records.Query";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = Some(codec::uint(r, "handle")?),
                2 => message.since = Some(codec::uint(r, "since")?),
                3 => message.from = Some(codec::int(r, "from")?),
                4 => message.to = Some(codec::int(r, "to")?),
                5 => message.streams = codec::list(r, "streams", codec::str)?,
                6 => message.names = codec::list(r, "names", codec::str)?,
                7 => message.min_level = Some(codec::int(r, "minLevel")?),
                8 => message.trace_id = Some(codec::bin(r, "traceId")?),
                9 => message.r#where = codec::names(r, "where", codec::str)?,
                10 => message.search = Some(codec::str(r, "search")?),
                11 => message.newest = codec::bool(r, "newest")?,
                12 => message.limit = Some(codec::uint(r, "limit")?),
                13 => message.after = Some(codec::str(r, "after")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(handle) = &self.handle {
            map.field(out, 1);
            codec::write_uint(out, handle);
        }
        if let Some(since) = &self.since {
            map.field(out, 2);
            codec::write_uint(out, since);
        }
        if let Some(from) = &self.from {
            map.field(out, 3);
            codec::write_int(out, from);
        }
        if let Some(to) = &self.to {
            map.field(out, 4);
            codec::write_int(out, to);
        }
        if !self.streams.is_empty() {
            map.field(out, 5);
            codec::write_list(out, &self.streams, codec::write_str);
        }
        if !self.names.is_empty() {
            map.field(out, 6);
            codec::write_list(out, &self.names, codec::write_str);
        }
        if let Some(min_level) = &self.min_level {
            map.field(out, 7);
            codec::write_int(out, min_level);
        }
        if let Some(trace_id) = &self.trace_id {
            map.field(out, 8);
            codec::write_bin(out, trace_id);
        }
        if !self.r#where.is_empty() {
            map.field(out, 9);
            codec::write_names(out, &self.r#where, codec::write_str);
        }
        if let Some(search) = &self.search {
            map.field(out, 10);
            codec::write_str(out, search);
        }
        if self.newest {
            map.field(out, 11);
            codec::write_bool(out, &self.newest);
        }
        if let Some(limit) = &self.limit {
            map.field(out, 12);
            codec::write_uint(out, limit);
        }
        if let Some(after) = &self.after {
            map.field(out, 13);
            codec::write_str(out, after);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.handle.map_or(0, |_| 10)
            + self.since.map_or(0, |_| 10)
            + self.from.map_or(0, |_| 10)
            + self.to.map_or(0, |_| 10)
            + 1 + codec::list_size(&self.streams, |item| 5 + item.len())
            + 1 + codec::list_size(&self.names, |item| 5 + item.len())
            + self.min_level.map_or(0, |_| 10)
            + self.trace_id.as_ref().map_or(0, |trace_id| 1 + 5 + trace_id.len())
            + 1 + codec::names_size(&self.r#where, |item| 5 + item.len())
            + self.search.as_ref().map_or(0, |search| 1 + 5 + search.len())
            + 2
            + self.limit.map_or(0, |_| 10)
            + self.after.as_ref().map_or(0, |after| 1 + 5 + after.len())
    }

    fn is_zero(&self) -> bool {
        self.handle.is_none()
            && self.since.is_none()
            && self.from.is_none()
            && self.to.is_none()
            && self.streams.is_empty()
            && self.names.is_empty()
            && self.min_level.is_none()
            && self.trace_id.is_none()
            && self.r#where.is_empty()
            && self.search.is_none()
            && !self.newest
            && self.limit.is_none()
            && self.after.is_none()
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsPage {
    pub(crate) events: Vec<RecordsEvent>,
    /// The page after it; the last page has none.
    pub(crate) next: Option<String>,
}

#[cfg(feature = "records")]
impl Message for RecordsPage {
    const NAME: &'static str = "records.Page";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.events = codec::list(r, "events", codec::message::<RecordsEvent>)?,
                2 => message.next = Some(codec::str(r, "next")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.events.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.events, codec::write_message);
        }
        if let Some(next) = &self.next {
            map.field(out, 2);
            codec::write_str(out, next);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.events, codec::message_size)
            + self.next.as_ref().map_or(0, |next| 1 + 5 + next.len())
    }

    fn is_zero(&self) -> bool {
        self.events.is_empty()
            && self.next.is_none()
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsCount {
    pub(crate) count: u64,
}

#[cfg(feature = "records")]
impl Message for RecordsCount {
    const NAME: &'static str = "records.Count";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.count = codec::uint(r, "count")?,
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
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
    }

    fn is_zero(&self) -> bool {
        self.count == 0
    }
}

/// A key's most frequent values among what a query finds.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsTopAsked {
    pub(crate) query: RecordsQuery,
    /// A field's or a context's, or stream, name or level.
    pub(crate) key: String,
}

#[cfg(feature = "records")]
impl Message for RecordsTopAsked {
    const NAME: &'static str = "records.TopAsked";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.query = RecordsQuery::read(r)?,
                2 => message.key = codec::str(r, "key")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.query.is_zero() {
            map.field(out, 1);
            codec::write_message(out, &self.query);
        }
        if !self.key.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.key);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + self.query.size()
            + 1 + 5 + self.key.len()
    }

    fn is_zero(&self) -> bool {
        self.query.is_zero()
            && self.key.is_empty()
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsTop {
    pub(crate) value: String,
    pub(crate) count: u64,
}

#[cfg(feature = "records")]
impl Message for RecordsTop {
    const NAME: &'static str = "records.Top";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.value = codec::str(r, "value")?,
                2 => message.count = codec::uint(r, "count")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.value.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.value);
        }
        if self.count != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.count);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.value.len()
            + 10
    }

    fn is_zero(&self) -> bool {
        self.value.is_empty()
            && self.count == 0
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsTops {
    pub(crate) values: Vec<RecordsTop>,
}

#[cfg(feature = "records")]
impl Message for RecordsTops {
    const NAME: &'static str = "records.Tops";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.values = codec::list(r, "values", codec::message::<RecordsTop>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.values.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.values, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.values, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.values.is_empty()
    }
}

/// A row for each span of every and each value of the keys of by.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsAggregateAsked {
    pub(crate) query: RecordsQuery,
    /// Milliseconds.
    pub(crate) every: Option<u64>,
    pub(crate) by: Vec<String>,
    pub(crate) stats: Vec<String>,
    pub(crate) distinct: Vec<String>,
}

#[cfg(feature = "records")]
impl Message for RecordsAggregateAsked {
    const NAME: &'static str = "records.AggregateAsked";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.query = RecordsQuery::read(r)?,
                2 => message.every = Some(codec::uint(r, "every")?),
                3 => message.by = codec::list(r, "by", codec::str)?,
                4 => message.stats = codec::list(r, "stats", codec::str)?,
                5 => message.distinct = codec::list(r, "distinct", codec::str)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.query.is_zero() {
            map.field(out, 1);
            codec::write_message(out, &self.query);
        }
        if let Some(every) = &self.every {
            map.field(out, 2);
            codec::write_uint(out, every);
        }
        if !self.by.is_empty() {
            map.field(out, 3);
            codec::write_list(out, &self.by, codec::write_str);
        }
        if !self.stats.is_empty() {
            map.field(out, 4);
            codec::write_list(out, &self.stats, codec::write_str);
        }
        if !self.distinct.is_empty() {
            map.field(out, 5);
            codec::write_list(out, &self.distinct, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + self.query.size()
            + self.every.map_or(0, |_| 10)
            + 1 + codec::list_size(&self.by, |item| 5 + item.len())
            + 1 + codec::list_size(&self.stats, |item| 5 + item.len())
            + 1 + codec::list_size(&self.distinct, |item| 5 + item.len())
    }

    fn is_zero(&self) -> bool {
        self.query.is_zero()
            && self.every.is_none()
            && self.by.is_empty()
            && self.stats.is_empty()
            && self.distinct.is_empty()
    }
}

/// A field's numbers in a row: a sum, a min and a max are JSON numbers, an
/// integer's exact.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsStats {
    pub(crate) samples: u64,
    pub(crate) sum: String,
    pub(crate) min: String,
    pub(crate) max: String,
    pub(crate) avg: f64,
}

#[cfg(feature = "records")]
impl Message for RecordsStats {
    const NAME: &'static str = "records.Stats";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.samples = codec::uint(r, "samples")?,
                2 => message.sum = codec::str(r, "sum")?,
                3 => message.min = codec::str(r, "min")?,
                4 => message.max = codec::str(r, "max")?,
                5 => message.avg = codec::float(r, "avg")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.samples != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.samples);
        }
        if !self.sum.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.sum);
        }
        if !self.min.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.min);
        }
        if !self.max.is_empty() {
            map.field(out, 4);
            codec::write_str(out, &self.max);
        }
        if self.avg.to_bits() != 0 {
            map.field(out, 5);
            codec::write_float(out, &self.avg);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.sum.len()
            + 1 + 5 + self.min.len()
            + 1 + 5 + self.max.len()
            + 10
    }

    fn is_zero(&self) -> bool {
        self.samples == 0
            && self.sum.is_empty()
            && self.min.is_empty()
            && self.max.is_empty()
            && self.avg.to_bits() == 0
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsRow {
    /// Where the row's span begins, counted from 1970. Unix nanoseconds.
    pub(crate) at: i64,
    pub(crate) count: u64,
    pub(crate) by: BTreeMap<String, String>,
    pub(crate) stats: BTreeMap<String, RecordsStats>,
    pub(crate) distinct: BTreeMap<String, u64>,
}

#[cfg(feature = "records")]
impl Message for RecordsRow {
    const NAME: &'static str = "records.Row";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.at = codec::int(r, "at")?,
                2 => message.count = codec::uint(r, "count")?,
                3 => message.by = codec::names(r, "by", codec::str)?,
                4 => message.stats = codec::names(r, "stats", codec::message::<RecordsStats>)?,
                5 => message.distinct = codec::names(r, "distinct", codec::uint)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.at != 0 {
            map.field(out, 1);
            codec::write_int(out, &self.at);
        }
        if self.count != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.count);
        }
        if !self.by.is_empty() {
            map.field(out, 3);
            codec::write_names(out, &self.by, codec::write_str);
        }
        if !self.stats.is_empty() {
            map.field(out, 4);
            codec::write_names(out, &self.stats, codec::write_message);
        }
        if !self.distinct.is_empty() {
            map.field(out, 5);
            codec::write_names(out, &self.distinct, codec::write_uint);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
            + 1 + codec::names_size(&self.by, |item| 5 + item.len())
            + 1 + codec::names_size(&self.stats, codec::message_size)
            + 1 + codec::names_size(&self.distinct, |_| 9)
    }

    fn is_zero(&self) -> bool {
        self.at == 0
            && self.count == 0
            && self.by.is_empty()
            && self.stats.is_empty()
            && self.distinct.is_empty()
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsRows {
    pub(crate) rows: Vec<RecordsRow>,
}

#[cfg(feature = "records")]
impl Message for RecordsRows {
    const NAME: &'static str = "records.Rows";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.rows = codec::list(r, "rows", codec::message::<RecordsRow>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.rows.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.rows, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.rows, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.rows.is_empty()
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsStream {
    pub(crate) name: String,
    /// Milliseconds.
    pub(crate) keep: u64,
    /// Its oldest event, or where its keep begins while it holds older. Unix nanoseconds.
    pub(crate) retained_from: i64,
    /// Unix nanoseconds.
    pub(crate) newest: i64,
}

#[cfg(feature = "records")]
impl Message for RecordsStream {
    const NAME: &'static str = "records.Stream";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.keep = codec::uint(r, "keep")?,
                3 => message.retained_from = codec::int(r, "retainedFrom")?,
                4 => message.newest = codec::int(r, "newest")?,
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
        if self.keep != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.keep);
        }
        if self.retained_from != 0 {
            map.field(out, 3);
            codec::write_int(out, &self.retained_from);
        }
        if self.newest != 0 {
            map.field(out, 4);
            codec::write_int(out, &self.newest);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 10
            + 10
            + 10
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.keep == 0
            && self.retained_from == 0
            && self.newest == 0
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsStreams {
    pub(crate) streams: Vec<RecordsStream>,
}

#[cfg(feature = "records")]
impl Message for RecordsStreams {
    const NAME: &'static str = "records.Streams";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.streams = codec::list(r, "streams", codec::message::<RecordsStream>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.streams.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.streams, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.streams, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.streams.is_empty()
    }
}

/// The keys the events a query finds hold, sorted.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsFieldNamesAsked {
    pub(crate) query: RecordsQuery,
    pub(crate) prefix: Option<String>,
}

#[cfg(feature = "records")]
impl Message for RecordsFieldNamesAsked {
    const NAME: &'static str = "records.FieldNamesAsked";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.query = RecordsQuery::read(r)?,
                2 => message.prefix = Some(codec::str(r, "prefix")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.query.is_zero() {
            map.field(out, 1);
            codec::write_message(out, &self.query);
        }
        if let Some(prefix) = &self.prefix {
            map.field(out, 2);
            codec::write_str(out, prefix);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + self.query.size()
            + self.prefix.as_ref().map_or(0, |prefix| 1 + 5 + prefix.len())
    }

    fn is_zero(&self) -> bool {
        self.query.is_zero()
            && self.prefix.is_none()
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsNames {
    pub(crate) names: Vec<String>,
}

#[cfg(feature = "records")]
impl Message for RecordsNames {
    const NAME: &'static str = "records.Names";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.names = codec::list(r, "names", codec::str)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.names.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.names, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.names, |item| 5 + item.len())
    }

    fn is_zero(&self) -> bool {
        self.names.is_empty()
    }
}

/// Every packed event once, a batch a call, from a cursor the program keeps.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsPollAsked {
    /// The oldest event kept without one.
    pub(crate) cursor: Option<String>,
    /// 1000 unless given.
    pub(crate) limit: Option<u64>,
}

#[cfg(feature = "records")]
impl Message for RecordsPollAsked {
    const NAME: &'static str = "records.PollAsked";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.cursor = Some(codec::str(r, "cursor")?),
                2 => message.limit = Some(codec::uint(r, "limit")?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if let Some(cursor) = &self.cursor {
            map.field(out, 1);
            codec::write_str(out, cursor);
        }
        if let Some(limit) = &self.limit {
            map.field(out, 2);
            codec::write_uint(out, limit);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + self.cursor.as_ref().map_or(0, |cursor| 1 + 5 + cursor.len())
            + self.limit.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.cursor.is_none()
            && self.limit.is_none()
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsBatch {
    pub(crate) events: Vec<RecordsEvent>,
    pub(crate) cursor: String,
    /// The events keep removed before the cursor reached them.
    pub(crate) expired: u64,
}

#[cfg(feature = "records")]
impl Message for RecordsBatch {
    const NAME: &'static str = "records.Batch";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.events = codec::list(r, "events", codec::message::<RecordsEvent>)?,
                2 => message.cursor = codec::str(r, "cursor")?,
                3 => message.expired = codec::uint(r, "expired")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.events.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.events, codec::write_message);
        }
        if !self.cursor.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.cursor);
        }
        if self.expired != 0 {
            map.field(out, 3);
            codec::write_uint(out, &self.expired);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.events, codec::message_size)
            + 1 + 5 + self.cursor.len()
            + 10
    }

    fn is_zero(&self) -> bool {
        self.events.is_empty()
            && self.cursor.is_empty()
            && self.expired == 0
    }
}

/// A tail's batch: the events appended since the last that its query finds,
/// and how many left the store's hold before the tail read them.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsTailBatch {
    pub(crate) events: Vec<RecordsEvent>,
    pub(crate) missed: u64,
}

#[cfg(feature = "records")]
impl Message for RecordsTailBatch {
    const NAME: &'static str = "records.TailBatch";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.events = codec::list(r, "events", codec::message::<RecordsEvent>)?,
                2 => message.missed = codec::uint(r, "missed")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.events.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.events, codec::write_message);
        }
        if self.missed != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.missed);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.events, codec::message_size)
            + 10
    }

    fn is_zero(&self) -> bool {
        self.events.is_empty()
            && self.missed == 0
    }
}

/// A piece of another program's output, cut anywhere.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsPiece {
    pub(crate) bytes: Vec<u8>,
}

#[cfg(feature = "records")]
impl Message for RecordsPiece {
    const NAME: &'static str = "records.Piece";

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

/// What a writer of lines dropped: lines past its buffer, longer than an
/// event holds, or refused by the file.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsDropped {
    pub(crate) dropped: u64,
}

#[cfg(feature = "records")]
impl Message for RecordsDropped {
    const NAME: &'static str = "records.Dropped";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.dropped = codec::uint(r, "dropped")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.dropped != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.dropped);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
    }

    fn is_zero(&self) -> bool {
        self.dropped == 0
    }
}

/// A head row or a segment that no longer reads, as damaged lists it and drop
/// takes it: a head row holds events not packed yet, a segment packed ones.
#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsDamage {
    /// Head row or segment.
    pub(crate) part: String,
    pub(crate) id: u64,
    pub(crate) stream: String,
    /// Its events' oldest time. Unix nanoseconds.
    pub(crate) first: i64,
    /// Unix nanoseconds.
    pub(crate) last: i64,
    /// What broke.
    pub(crate) error: String,
}

#[cfg(feature = "records")]
impl Message for RecordsDamage {
    const NAME: &'static str = "records.Damage";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.part = codec::str(r, "part")?,
                2 => message.id = codec::uint(r, "id")?,
                3 => message.stream = codec::str(r, "stream")?,
                4 => message.first = codec::int(r, "first")?,
                5 => message.last = codec::int(r, "last")?,
                6 => message.error = codec::str(r, "error")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.part.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.part);
        }
        if self.id != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.id);
        }
        if !self.stream.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.stream);
        }
        if self.first != 0 {
            map.field(out, 4);
            codec::write_int(out, &self.first);
        }
        if self.last != 0 {
            map.field(out, 5);
            codec::write_int(out, &self.last);
        }
        if !self.error.is_empty() {
            map.field(out, 6);
            codec::write_str(out, &self.error);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.part.len()
            + 10
            + 1 + 5 + self.stream.len()
            + 10
            + 10
            + 1 + 5 + self.error.len()
    }

    fn is_zero(&self) -> bool {
        self.part.is_empty()
            && self.id == 0
            && self.stream.is_empty()
            && self.first == 0
            && self.last == 0
            && self.error.is_empty()
    }
}

#[cfg(feature = "records")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RecordsDamages {
    pub(crate) damages: Vec<RecordsDamage>,
}

#[cfg(feature = "records")]
impl Message for RecordsDamages {
    const NAME: &'static str = "records.Damages";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.damages = codec::list(r, "damages", codec::message::<RecordsDamage>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.damages.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.damages, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.damages, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.damages.is_empty()
    }
}

/// Opens an index, made the first time; one that holds other settings is
/// changed to these, as docs/guides/search/rebuilds.md has it. A read
/// connection opens an index only as it is. With a database, the index
/// follows a table of it: its fields and filters are columns.
#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchOpen {
    /// [a-z0-9][a-z0-9_-]{0,63}.
    pub(crate) name: String,
    /// In order; none for a document that is its own text.
    pub(crate) fields: Vec<SearchField>,
    /// The fields a search may filter by.
    pub(crate) filters: Vec<String>,
    /// On when absent.
    pub(crate) typo_tolerance: Option<bool>,
    /// The vectors' dimensions; none for an index without vectors.
    pub(crate) dimensions: Option<u64>,
    /// Above 0 and below 1; exact when absent.
    pub(crate) recall: Option<f64>,
    /// 0.5 when absent.
    pub(crate) vector_weight: Option<f64>,
    pub(crate) rerank: bool,
    /// A change that needs reading again waits for reindex.
    pub(crate) manual: bool,
    /// Reads the index again beside itself.
    pub(crate) reindex: bool,
    /// A database's handle: the index follows a table in its file.
    pub(crate) database: Option<u64>,
    /// The table a database's index follows.
    pub(crate) table: Option<String>,
    /// The table's column of unique values; its primary key when absent.
    pub(crate) key: Option<String>,
    /// The column of each row's vector, a blob of little-endian floats.
    pub(crate) vector_column: Option<String>,
}

#[cfg(feature = "search")]
impl Message for SearchOpen {
    const NAME: &'static str = "search.Open";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.fields = codec::list(r, "fields", codec::message::<SearchField>)?,
                3 => message.filters = codec::list(r, "filters", codec::str)?,
                4 => message.typo_tolerance = Some(codec::bool(r, "typoTolerance")?),
                5 => message.dimensions = Some(codec::uint(r, "dimensions")?),
                6 => message.recall = Some(codec::float(r, "recall")?),
                7 => message.vector_weight = Some(codec::float(r, "vectorWeight")?),
                8 => message.rerank = codec::bool(r, "rerank")?,
                9 => message.manual = codec::bool(r, "manual")?,
                10 => message.reindex = codec::bool(r, "reindex")?,
                11 => message.database = Some(codec::uint(r, "database")?),
                12 => message.table = Some(codec::str(r, "table")?),
                13 => message.key = Some(codec::str(r, "key")?),
                14 => message.vector_column = Some(codec::str(r, "vectorColumn")?),
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
        if !self.fields.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.fields, codec::write_message);
        }
        if !self.filters.is_empty() {
            map.field(out, 3);
            codec::write_list(out, &self.filters, codec::write_str);
        }
        if let Some(typo_tolerance) = &self.typo_tolerance {
            map.field(out, 4);
            codec::write_bool(out, typo_tolerance);
        }
        if let Some(dimensions) = &self.dimensions {
            map.field(out, 5);
            codec::write_uint(out, dimensions);
        }
        if let Some(recall) = &self.recall {
            map.field(out, 6);
            codec::write_float(out, recall);
        }
        if let Some(vector_weight) = &self.vector_weight {
            map.field(out, 7);
            codec::write_float(out, vector_weight);
        }
        if self.rerank {
            map.field(out, 8);
            codec::write_bool(out, &self.rerank);
        }
        if self.manual {
            map.field(out, 9);
            codec::write_bool(out, &self.manual);
        }
        if self.reindex {
            map.field(out, 10);
            codec::write_bool(out, &self.reindex);
        }
        if let Some(database) = &self.database {
            map.field(out, 11);
            codec::write_uint(out, database);
        }
        if let Some(table) = &self.table {
            map.field(out, 12);
            codec::write_str(out, table);
        }
        if let Some(key) = &self.key {
            map.field(out, 13);
            codec::write_str(out, key);
        }
        if let Some(vector_column) = &self.vector_column {
            map.field(out, 14);
            codec::write_str(out, vector_column);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 1 + codec::list_size(&self.fields, codec::message_size)
            + 1 + codec::list_size(&self.filters, |item| 5 + item.len())
            + self.typo_tolerance.map_or(0, |_| 2)
            + self.dimensions.map_or(0, |_| 10)
            + self.recall.map_or(0, |_| 10)
            + self.vector_weight.map_or(0, |_| 10)
            + 2
            + 2
            + 2
            + self.database.map_or(0, |_| 10)
            + self.table.as_ref().map_or(0, |table| 1 + 5 + table.len())
            + self.key.as_ref().map_or(0, |key| 1 + 5 + key.len())
            + self.vector_column.as_ref().map_or(0, |vector_column| 1 + 5 + vector_column.len())
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.fields.is_empty()
            && self.filters.is_empty()
            && self.typo_tolerance.is_none()
            && self.dimensions.is_none()
            && self.recall.is_none()
            && self.vector_weight.is_none()
            && !self.rerank
            && !self.manual
            && !self.reindex
            && self.database.is_none()
            && self.table.is_none()
            && self.key.is_none()
            && self.vector_column.is_none()
    }
}

/// A field a search reads, by its path into the document, and what a word
/// found there weighs.
#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchField {
    pub(crate) path: String,
    /// Above 0, at most 1000.
    pub(crate) weight: f64,
}

#[cfg(feature = "search")]
impl Message for SearchField {
    const NAME: &'static str = "search.Field";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.path = codec::str(r, "path")?,
                2 => message.weight = codec::float(r, "weight")?,
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
        if self.weight.to_bits() != 0 {
            map.field(out, 2);
            codec::write_float(out, &self.weight);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.path.len()
            + 10
    }

    fn is_zero(&self) -> bool {
        self.path.is_empty()
            && self.weight.to_bits() == 0
    }
}

/// A document set by its id, replacing it whole.
#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchSet {
    pub(crate) handle: u64,
    /// 1 to 1024 bytes.
    pub(crate) id: String,
    /// 1 MiB at most.
    pub(crate) value: String,
    /// Four bytes a dimension, little-endian floats.
    pub(crate) vector: Option<Vec<u8>>,
    /// The document has no vector, on an index with vectors.
    pub(crate) no_vector: bool,
}

#[cfg(feature = "search")]
impl Message for SearchSet {
    const NAME: &'static str = "search.Set";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.id = codec::str(r, "id")?,
                3 => message.value = codec::str(r, "value")?,
                4 => message.vector = Some(codec::bin(r, "vector")?),
                5 => message.no_vector = codec::bool(r, "noVector")?,
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
        if !self.value.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.value);
        }
        if let Some(vector) = &self.vector {
            map.field(out, 4);
            codec::write_bin(out, vector);
        }
        if self.no_vector {
            map.field(out, 5);
            codec::write_bool(out, &self.no_vector);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.id.len()
            + 1 + 5 + self.value.len()
            + self.vector.as_ref().map_or(0, |vector| 1 + 5 + vector.len())
            + 2
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.id.is_empty()
            && self.value.is_empty()
            && self.vector.is_none()
            && !self.no_vector
    }
}

/// Documents set in one commit, all of them or none: 1,000 and 16 MiB at most.
#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchSetMany {
    pub(crate) handle: u64,
    pub(crate) documents: Vec<SearchDocument>,
}

#[cfg(feature = "search")]
impl Message for SearchSetMany {
    const NAME: &'static str = "search.SetMany";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.documents = codec::list(r, "documents", codec::message::<SearchDocument>)?,
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
        if !self.documents.is_empty() {
            map.field(out, 2);
            codec::write_list(out, &self.documents, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + codec::list_size(&self.documents, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.documents.is_empty()
    }
}

#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchDocument {
    pub(crate) id: String,
    pub(crate) value: String,
    pub(crate) vector: Option<Vec<u8>>,
    pub(crate) no_vector: bool,
}

#[cfg(feature = "search")]
impl Message for SearchDocument {
    const NAME: &'static str = "search.Document";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.id = codec::str(r, "id")?,
                2 => message.value = codec::str(r, "value")?,
                3 => message.vector = Some(codec::bin(r, "vector")?),
                4 => message.no_vector = codec::bool(r, "noVector")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.id.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.id);
        }
        if !self.value.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.value);
        }
        if let Some(vector) = &self.vector {
            map.field(out, 3);
            codec::write_bin(out, vector);
        }
        if self.no_vector {
            map.field(out, 4);
            codec::write_bool(out, &self.no_vector);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.id.len()
            + 1 + 5 + self.value.len()
            + self.vector.as_ref().map_or(0, |vector| 1 + 5 + vector.len())
            + 2
    }

    fn is_zero(&self) -> bool {
        self.id.is_empty()
            && self.value.is_empty()
            && self.vector.is_none()
            && !self.no_vector
    }
}

#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchKey {
    pub(crate) handle: u64,
    pub(crate) id: String,
}

#[cfg(feature = "search")]
impl Message for SearchKey {
    const NAME: &'static str = "search.Key";

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

#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchDeleted {
    pub(crate) deleted: bool,
}

#[cfg(feature = "search")]
impl Message for SearchDeleted {
    const NAME: &'static str = "search.Deleted";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.deleted = codec::bool(r, "deleted")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.deleted {
            map.field(out, 1);
            codec::write_bool(out, &self.deleted);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
    }

    fn is_zero(&self) -> bool {
        !self.deleted
    }
}

#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchGot {
    pub(crate) found: bool,
    pub(crate) value: Option<String>,
}

#[cfg(feature = "search")]
impl Message for SearchGot {
    const NAME: &'static str = "search.Got";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.found = codec::bool(r, "found")?,
                2 => message.value = Some(codec::str(r, "value")?),
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
            codec::write_str(out, value);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
            + self.value.as_ref().map_or(0, |value| 1 + 5 + value.len())
    }

    fn is_zero(&self) -> bool {
        !self.found
            && self.value.is_none()
    }
}

/// A search: by `text`'s words, `vector`, or both, kept to what `where` passes.
#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchQuery {
    pub(crate) handle: u64,
    pub(crate) text: String,
    pub(crate) vector: Option<Vec<u8>>,
    /// This search's, in place of the index's.
    pub(crate) vector_weight: Option<f64>,
    pub(crate) rerank: bool,
    /// The conditions, as docs/wire.md's "search's where" writes them.
    pub(crate) r#where: Option<String>,
    pub(crate) offset: u64,
    /// 10 when absent; offset and limit together 1000 at most.
    pub(crate) limit: Option<u64>,
}

#[cfg(feature = "search")]
impl Message for SearchQuery {
    const NAME: &'static str = "search.Query";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.handle = codec::uint(r, "handle")?,
                2 => message.text = codec::str(r, "text")?,
                3 => message.vector = Some(codec::bin(r, "vector")?),
                4 => message.vector_weight = Some(codec::float(r, "vectorWeight")?),
                5 => message.rerank = codec::bool(r, "rerank")?,
                6 => message.r#where = Some(codec::str(r, "where")?),
                7 => message.offset = codec::uint(r, "offset")?,
                8 => message.limit = Some(codec::uint(r, "limit")?),
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
        if let Some(vector) = &self.vector {
            map.field(out, 3);
            codec::write_bin(out, vector);
        }
        if let Some(vector_weight) = &self.vector_weight {
            map.field(out, 4);
            codec::write_float(out, vector_weight);
        }
        if self.rerank {
            map.field(out, 5);
            codec::write_bool(out, &self.rerank);
        }
        if let Some(r#where) = &self.r#where {
            map.field(out, 6);
            codec::write_str(out, r#where);
        }
        if self.offset != 0 {
            map.field(out, 7);
            codec::write_uint(out, &self.offset);
        }
        if let Some(limit) = &self.limit {
            map.field(out, 8);
            codec::write_uint(out, limit);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 1 + 5 + self.text.len()
            + self.vector.as_ref().map_or(0, |vector| 1 + 5 + vector.len())
            + self.vector_weight.map_or(0, |_| 10)
            + 2
            + self.r#where.as_ref().map_or(0, |r#where| 1 + 5 + r#where.len())
            + 10
            + self.limit.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.handle == 0
            && self.text.is_empty()
            && self.vector.is_none()
            && self.vector_weight.is_none()
            && !self.rerank
            && self.r#where.is_none()
            && self.offset == 0
            && self.limit.is_none()
    }
}

/// A hit: a document and its value, or a table's row, a value a column of
/// the answer's columns, as sql.Rows holds one.
#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchHit {
    pub(crate) id: String,
    /// Null for a table's row.
    pub(crate) value: String,
    pub(crate) score: f64,
    pub(crate) row: Option<Vec<Cell>>,
}

#[cfg(feature = "search")]
impl Message for SearchHit {
    const NAME: &'static str = "search.Hit";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.id = codec::str(r, "id")?,
                2 => message.value = codec::str(r, "value")?,
                3 => message.score = codec::float(r, "score")?,
                4 => message.row = Some(codec::list(r, "row", codec::cell)?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.id.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.id);
        }
        if !self.value.is_empty() {
            map.field(out, 2);
            codec::write_str(out, &self.value);
        }
        if self.score.to_bits() != 0 {
            map.field(out, 3);
            codec::write_float(out, &self.score);
        }
        if let Some(row) = &self.row {
            map.field(out, 4);
            codec::write_list(out, row, codec::write_cell);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.id.len()
            + 1 + 5 + self.value.len()
            + 10
            + self.row.as_ref().map_or(0, |row| 1 + codec::list_size(row, codec::cell_size))
    }

    fn is_zero(&self) -> bool {
        self.id.is_empty()
            && self.value.is_empty()
            && self.score.to_bits() == 0
            && self.row.is_none()
    }
}

#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchFound {
    pub(crate) hits: Vec<SearchHit>,
    /// The text the search read in fact, when a word was read as another.
    pub(crate) corrected: Option<String>,
    /// The columns of a table's index's rows.
    pub(crate) columns: Option<Vec<String>>,
}

#[cfg(feature = "search")]
impl Message for SearchFound {
    const NAME: &'static str = "search.Found";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.hits = codec::list(r, "hits", codec::message::<SearchHit>)?,
                2 => message.corrected = Some(codec::str(r, "corrected")?),
                3 => message.columns = Some(codec::list(r, "columns", codec::str)?),
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.hits.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.hits, codec::write_message);
        }
        if let Some(corrected) = &self.corrected {
            map.field(out, 2);
            codec::write_str(out, corrected);
        }
        if let Some(columns) = &self.columns {
            map.field(out, 3);
            codec::write_list(out, columns, codec::write_str);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.hits, codec::message_size)
            + self.corrected.as_ref().map_or(0, |corrected| 1 + 5 + corrected.len())
            + self.columns.as_ref().map_or(0, |columns| 1 + codec::list_size(columns, |item| 5 + item.len()))
    }

    fn is_zero(&self) -> bool {
        self.hits.is_empty()
            && self.corrected.is_none()
            && self.columns.is_none()
    }
}

/// Where an index stands: ready, rebuilding and how far, or failed and why.
#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchStatus {
    /// Ready, rebuilding or failed.
    pub(crate) state: String,
    /// The share a rebuild has read.
    pub(crate) done: f64,
    /// Why a rebuild failed.
    pub(crate) error: String,
}

#[cfg(feature = "search")]
impl Message for SearchStatus {
    const NAME: &'static str = "search.Status";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.state = codec::str(r, "state")?,
                2 => message.done = codec::float(r, "done")?,
                3 => message.error = codec::str(r, "error")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.state.is_empty() {
            map.field(out, 1);
            codec::write_str(out, &self.state);
        }
        if self.done.to_bits() != 0 {
            map.field(out, 2);
            codec::write_float(out, &self.done);
        }
        if !self.error.is_empty() {
            map.field(out, 3);
            codec::write_str(out, &self.error);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.state.len()
            + 10
            + 1 + 5 + self.error.len()
    }

    fn is_zero(&self) -> bool {
        self.state.is_empty()
            && self.done.to_bits() == 0
            && self.error.is_empty()
    }
}

#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchName {
    pub(crate) name: String,
    /// A database's handle, for a table's index.
    pub(crate) database: Option<u64>,
}

#[cfg(feature = "search")]
impl Message for SearchName {
    const NAME: &'static str = "search.Name";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.database = Some(codec::uint(r, "database")?),
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
        if let Some(database) = &self.database {
            map.field(out, 2);
            codec::write_uint(out, database);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.database.map_or(0, |_| 10)
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.database.is_none()
    }
}

#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchDropped {
    pub(crate) dropped: bool,
}

#[cfg(feature = "search")]
impl Message for SearchDropped {
    const NAME: &'static str = "search.Dropped";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.dropped = codec::bool(r, "dropped")?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.dropped {
            map.field(out, 1);
            codec::write_bool(out, &self.dropped);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 2
    }

    fn is_zero(&self) -> bool {
        !self.dropped
    }
}

/// An index as the store's search.db holds it.
#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchIndex {
    pub(crate) name: String,
    pub(crate) documents: u64,
    pub(crate) vectors: u64,
    /// When a program last opened it. Unix milliseconds.
    pub(crate) opened: Option<i64>,
    pub(crate) state: String,
    pub(crate) done: f64,
    pub(crate) error: String,
}

#[cfg(feature = "search")]
impl Message for SearchIndex {
    const NAME: &'static str = "search.Index";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.documents = codec::uint(r, "documents")?,
                3 => message.vectors = codec::uint(r, "vectors")?,
                4 => message.opened = Some(codec::int(r, "opened")?),
                5 => message.state = codec::str(r, "state")?,
                6 => message.done = codec::float(r, "done")?,
                7 => message.error = codec::str(r, "error")?,
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
        if self.documents != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.documents);
        }
        if self.vectors != 0 {
            map.field(out, 3);
            codec::write_uint(out, &self.vectors);
        }
        if let Some(opened) = &self.opened {
            map.field(out, 4);
            codec::write_int(out, opened);
        }
        if !self.state.is_empty() {
            map.field(out, 5);
            codec::write_str(out, &self.state);
        }
        if self.done.to_bits() != 0 {
            map.field(out, 6);
            codec::write_float(out, &self.done);
        }
        if !self.error.is_empty() {
            map.field(out, 7);
            codec::write_str(out, &self.error);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + 10
            + 10
            + self.opened.map_or(0, |_| 10)
            + 1 + 5 + self.state.len()
            + 10
            + 1 + 5 + self.error.len()
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.documents == 0
            && self.vectors == 0
            && self.opened.is_none()
            && self.state.is_empty()
            && self.done.to_bits() == 0
            && self.error.is_empty()
    }
}

#[cfg(feature = "search")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SearchListed {
    pub(crate) indexes: Vec<SearchIndex>,
}

#[cfg(feature = "search")]
impl Message for SearchListed {
    const NAME: &'static str = "search.Listed";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.indexes = codec::list(r, "indexes", codec::message::<SearchIndex>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if !self.indexes.is_empty() {
            map.field(out, 1);
            codec::write_list(out, &self.indexes, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + codec::list_size(&self.indexes, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.indexes.is_empty()
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

/// A statement on a database: a write, or a read answered by its columns.
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

/// A column of a statement's rows, a part of it a message, as one of three. A
/// column of INTEGERs alone goes as integers and one of REALs alone as reals,
/// 8 bytes a row, little end first; where a row holds NULL its bit of nulls is
/// set, and its value is 0 among integers and a NaN among reals, which SQLite
/// keeps none of. Any other column goes as values: text, blobs, and INTEGERs
/// beside REALs, neither read as the other.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlColumn {
    pub(crate) name: String,
    /// An int64 a row.
    pub(crate) integers: Option<Vec<u8>>,
    /// A float64 a row.
    pub(crate) reals: Option<Vec<u8>>,
    /// A bit a row, a byte's lowest first, set where the row holds NULL; absent when none does.
    pub(crate) nulls: Option<Vec<u8>>,
    pub(crate) values: Option<Vec<Cell>>,
}

#[cfg(feature = "sql")]
impl Message for SqlColumn {
    const NAME: &'static str = "sql.Column";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.name = codec::str(r, "name")?,
                2 => message.integers = Some(codec::bin(r, "integers")?),
                3 => message.reals = Some(codec::bin(r, "reals")?),
                4 => message.nulls = Some(codec::bin(r, "nulls")?),
                5 => message.values = Some(codec::list(r, "values", codec::cell)?),
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
        if let Some(integers) = &self.integers {
            map.field(out, 2);
            codec::write_bin(out, integers);
        }
        if let Some(reals) = &self.reals {
            map.field(out, 3);
            codec::write_bin(out, reals);
        }
        if let Some(nulls) = &self.nulls {
            map.field(out, 4);
            codec::write_bin(out, nulls);
        }
        if let Some(values) = &self.values {
            map.field(out, 5);
            codec::write_list(out, values, codec::write_cell);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 1 + 5 + self.name.len()
            + self.integers.as_ref().map_or(0, |integers| 1 + 5 + integers.len())
            + self.reals.as_ref().map_or(0, |reals| 1 + 5 + reals.len())
            + self.nulls.as_ref().map_or(0, |nulls| 1 + 5 + nulls.len())
            + self.values.as_ref().map_or(0, |values| 1 + codec::list_size(values, codec::cell_size))
    }

    fn is_zero(&self) -> bool {
        self.name.is_empty()
            && self.integers.is_none()
            && self.reals.is_none()
            && self.nulls.is_none()
            && self.values.is_none()
    }
}

/// Rows a statement gave, by their columns, a part of them a message: every
/// part holds every column, in the statement's order and as the same one of
/// three, for the same rows.
#[cfg(feature = "sql")]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SqlColumns {
    /// The rows of this part.
    pub(crate) rows: u64,
    /// The rows of every part.
    pub(crate) total: u64,
    pub(crate) columns: Vec<SqlColumn>,
}

#[cfg(feature = "sql")]
impl Message for SqlColumns {
    const NAME: &'static str = "sql.Columns";

    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {
        let mut message = Self::default();
        let mut seen = 0;
        for _ in 0..codec::fields(r, Self::NAME)? {
            match codec::field(r, Self::NAME, &mut seen)? {
                1 => message.rows = codec::uint(r, "rows")?,
                2 => message.total = codec::uint(r, "total")?,
                3 => message.columns = codec::list(r, "columns", codec::message::<SqlColumn>)?,
                number => return Err(codec::unknown(number, Self::NAME)),
            }
        }
        Ok(message)
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut map = Map::open(out);
        if self.rows != 0 {
            map.field(out, 1);
            codec::write_uint(out, &self.rows);
        }
        if self.total != 0 {
            map.field(out, 2);
            codec::write_uint(out, &self.total);
        }
        if !self.columns.is_empty() {
            map.field(out, 3);
            codec::write_list(out, &self.columns, codec::write_message);
        }
        map.close(out);
    }

    fn size(&self) -> usize {
        3
            + 10
            + 10
            + 1 + codec::list_size(&self.columns, codec::message_size)
    }

    fn is_zero(&self) -> bool {
        self.rows == 0
            && self.total == 0
            && self.columns.is_empty()
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
    #[cfg(feature = "backup")]
    pub(crate) const BACKUP_WRITE: u16 = 0x0801;
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
    /// Answers once the store has the message, not once anyone handled it; in a
    /// database's transaction, it is handed over once the transaction commits.
    #[cfg(feature = "channels")]
    pub(crate) const CHANNELS_PUBLISH: u16 = 0x0a01;
    /// Answers once the subscription is registered, then sends batches within the
    /// client's credit until it cancels: a client that grants none falls behind.
    #[cfg(feature = "channels")]
    pub(crate) const CHANNELS_SUBSCRIBE: u16 = 0x0a02;
    #[cfg(feature = "config")]
    pub(crate) const CONFIG_OPEN: u16 = 0x0701;
    /// The config's value now, then each time it changes, until the client
    /// cancels: one stream a config a process, whose value every handle and
    /// watcher of it there takes.
    #[cfg(feature = "config")]
    pub(crate) const CONFIG_FOLLOW: u16 = 0x0702;
    #[cfg(feature = "config")]
    pub(crate) const CONFIG_UPDATE: u16 = 0x0703;
    #[cfg(feature = "config")]
    pub(crate) const CONFIG_RESET: u16 = 0x0704;
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
    /// Answers once the key is held, or held past the wait, `in_use`; a CANCEL
    /// takes the ask out of its line.
    #[cfg(feature = "locks")]
    pub(crate) const LOCKS_HOLD: u16 = 0x0901;
    /// A lease that ran out or was revoked is `lost`.
    #[cfg(feature = "locks")]
    pub(crate) const LOCKS_RENEW: u16 = 0x0902;
    #[cfg(feature = "locks")]
    pub(crate) const LOCKS_RELEASE: u16 = 0x0903;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_READ: u16 = 0x0601;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_LATEST: u16 = 0x0602;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_AGGREGATE: u16 = 0x0603;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_EXPLAIN: u16 = 0x0604;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_NAMES: u16 = 0x0605;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_LABEL_NAMES: u16 = 0x0606;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_LABEL_VALUES: u16 = 0x0607;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_DESCRIPTION: u16 = 0x0608;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_DAMAGED: u16 = 0x0609;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_INGEST: u16 = 0x060a;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_DESCRIBE: u16 = 0x060b;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_DELETE: u16 = 0x060c;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_FLUSH: u16 = 0x060d;
    #[cfg(feature = "metrics")]
    pub(crate) const METRICS_INSTRUMENTS: u16 = 0x060e;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_OPEN: u16 = 0x0501;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_APPEND: u16 = 0x0502;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_LIST: u16 = 0x0503;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_COUNT: u16 = 0x0504;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_TOP: u16 = 0x0505;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_AGGREGATE: u16 = 0x0506;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_STREAMS: u16 = 0x0507;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_FIELD_NAMES: u16 = 0x0508;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_POLL: u16 = 0x0509;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_TAIL: u16 = 0x050a;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_LINES: u16 = 0x050b;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_DAMAGED: u16 = 0x050c;
    #[cfg(feature = "records")]
    pub(crate) const RECORDS_DROP: u16 = 0x050d;
    #[cfg(feature = "search")]
    pub(crate) const SEARCH_OPEN: u16 = 0x0b01;
    #[cfg(feature = "search")]
    pub(crate) const SEARCH_SET: u16 = 0x0b02;
    #[cfg(feature = "search")]
    pub(crate) const SEARCH_SET_MANY: u16 = 0x0b03;
    /// Answers whether the index held the document.
    #[cfg(feature = "search")]
    pub(crate) const SEARCH_DELETE: u16 = 0x0b04;
    #[cfg(feature = "search")]
    pub(crate) const SEARCH_GET: u16 = 0x0b05;
    #[cfg(feature = "search")]
    pub(crate) const SEARCH_SEARCH: u16 = 0x0b06;
    #[cfg(feature = "search")]
    pub(crate) const SEARCH_STATUS: u16 = 0x0b07;
    /// Drops an index and every document it holds, in the background.
    #[cfg(feature = "search")]
    pub(crate) const SEARCH_DROP_INDEX: u16 = 0x0b08;
    #[cfg(feature = "search")]
    pub(crate) const SEARCH_LIST: u16 = 0x0b09;
    /// Stops the server as its closing does: the streams running finish, every
    /// connection is told GOAWAY, and a sidecar gives back SERVE and its directory.
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
    #[cfg(feature = "sql")]
    pub(crate) const SQL_COLUMNS: u16 = 0x0306;

    /// Who may call a method: the word its schema line starts with.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Access {
        Read,
        Write,
        Admin,
    }

    /// Who may call a method; none for a number the schema does not have.
    pub(crate) fn access(method: u16) -> Option<Access> {
        match method {
            #[cfg(feature = "backup")]
            BACKUP_WRITE => Some(Access::Admin),
            #[cfg(feature = "blobs")]
            BLOBS_OPEN => Some(Access::Read),
            #[cfg(feature = "blobs")]
            BLOBS_PUT => Some(Access::Write),
            #[cfg(feature = "blobs")]
            BLOBS_UPLOAD => Some(Access::Write),
            #[cfg(feature = "blobs")]
            BLOBS_GET => Some(Access::Read),
            #[cfg(feature = "blobs")]
            BLOBS_HEAD => Some(Access::Read),
            #[cfg(feature = "blobs")]
            BLOBS_DELETE => Some(Access::Write),
            #[cfg(feature = "blobs")]
            BLOBS_COPY => Some(Access::Write),
            #[cfg(feature = "blobs")]
            BLOBS_RENAME => Some(Access::Write),
            #[cfg(feature = "blobs")]
            BLOBS_EXPIRE => Some(Access::Write),
            #[cfg(feature = "blobs")]
            BLOBS_LIST => Some(Access::Read),
            #[cfg(feature = "blobs")]
            BLOBS_USAGE => Some(Access::Read),
            #[cfg(feature = "blobs")]
            BLOBS_CLEAR => Some(Access::Write),
            #[cfg(feature = "blobs")]
            BLOBS_READ => Some(Access::Read),
            #[cfg(feature = "channels")]
            CHANNELS_PUBLISH => Some(Access::Write),
            #[cfg(feature = "channels")]
            CHANNELS_SUBSCRIBE => Some(Access::Read),
            #[cfg(feature = "config")]
            CONFIG_OPEN => Some(Access::Read),
            #[cfg(feature = "config")]
            CONFIG_FOLLOW => Some(Access::Read),
            #[cfg(feature = "config")]
            CONFIG_UPDATE => Some(Access::Write),
            #[cfg(feature = "config")]
            CONFIG_RESET => Some(Access::Write),
            #[cfg(feature = "jobs")]
            JOBS_QUEUE_OPEN => Some(Access::Read),
            #[cfg(feature = "jobs")]
            JOBS_SCHEDULE_OPEN => Some(Access::Write),
            #[cfg(feature = "jobs")]
            JOBS_ADD => Some(Access::Write),
            #[cfg(feature = "jobs")]
            JOBS_SET => Some(Access::Write),
            #[cfg(feature = "jobs")]
            JOBS_UPDATE => Some(Access::Write),
            #[cfg(feature = "jobs")]
            JOBS_CANCEL => Some(Access::Write),
            #[cfg(feature = "jobs")]
            JOBS_GET => Some(Access::Read),
            #[cfg(feature = "jobs")]
            JOBS_LIST => Some(Access::Read),
            #[cfg(feature = "jobs")]
            JOBS_WORK => Some(Access::Write),
            #[cfg(feature = "jobs")]
            JOBS_STEP => Some(Access::Write),
            #[cfg(feature = "jobs")]
            JOBS_KEEP => Some(Access::Write),
            #[cfg(feature = "jobs")]
            JOBS_WATCH => Some(Access::Read),
            #[cfg(feature = "jobs")]
            JOBS_TX => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_BUCKET_OPEN => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_GET => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_HAS => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_SET => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_CREATE => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_TAKE => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_DELETE => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_EXPIRE => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_CLEAR => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_LIST => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_COUNTERS_OPEN => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_COUNTERS_ADD => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_COUNTERS_GET => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_COUNTERS_DELETE => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_COUNTERS_CLEAR => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_RATE_LIMIT_OPEN => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_QUOTA_OPEN => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_ALLOW => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_PEEK => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_RESET => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_REFUND => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_ONCE_OPEN => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_ONCE_RUN => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_ONCE_GET => Some(Access::Read),
            #[cfg(feature = "kv")]
            KV_ONCE_DELETE => Some(Access::Write),
            #[cfg(feature = "kv")]
            KV_TX => Some(Access::Write),
            #[cfg(feature = "locks")]
            LOCKS_HOLD => Some(Access::Write),
            #[cfg(feature = "locks")]
            LOCKS_RENEW => Some(Access::Write),
            #[cfg(feature = "locks")]
            LOCKS_RELEASE => Some(Access::Write),
            #[cfg(feature = "metrics")]
            METRICS_READ => Some(Access::Read),
            #[cfg(feature = "metrics")]
            METRICS_LATEST => Some(Access::Read),
            #[cfg(feature = "metrics")]
            METRICS_AGGREGATE => Some(Access::Read),
            #[cfg(feature = "metrics")]
            METRICS_EXPLAIN => Some(Access::Read),
            #[cfg(feature = "metrics")]
            METRICS_NAMES => Some(Access::Read),
            #[cfg(feature = "metrics")]
            METRICS_LABEL_NAMES => Some(Access::Read),
            #[cfg(feature = "metrics")]
            METRICS_LABEL_VALUES => Some(Access::Read),
            #[cfg(feature = "metrics")]
            METRICS_DESCRIPTION => Some(Access::Read),
            #[cfg(feature = "metrics")]
            METRICS_DAMAGED => Some(Access::Read),
            #[cfg(feature = "metrics")]
            METRICS_INGEST => Some(Access::Write),
            #[cfg(feature = "metrics")]
            METRICS_DESCRIBE => Some(Access::Write),
            #[cfg(feature = "metrics")]
            METRICS_DELETE => Some(Access::Write),
            #[cfg(feature = "metrics")]
            METRICS_FLUSH => Some(Access::Write),
            #[cfg(feature = "metrics")]
            METRICS_INSTRUMENTS => Some(Access::Write),
            #[cfg(feature = "records")]
            RECORDS_OPEN => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_APPEND => Some(Access::Write),
            #[cfg(feature = "records")]
            RECORDS_LIST => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_COUNT => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_TOP => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_AGGREGATE => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_STREAMS => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_FIELD_NAMES => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_POLL => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_TAIL => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_LINES => Some(Access::Write),
            #[cfg(feature = "records")]
            RECORDS_DAMAGED => Some(Access::Read),
            #[cfg(feature = "records")]
            RECORDS_DROP => Some(Access::Write),
            #[cfg(feature = "search")]
            SEARCH_OPEN => Some(Access::Read),
            #[cfg(feature = "search")]
            SEARCH_SET => Some(Access::Write),
            #[cfg(feature = "search")]
            SEARCH_SET_MANY => Some(Access::Write),
            #[cfg(feature = "search")]
            SEARCH_DELETE => Some(Access::Write),
            #[cfg(feature = "search")]
            SEARCH_GET => Some(Access::Read),
            #[cfg(feature = "search")]
            SEARCH_SEARCH => Some(Access::Read),
            #[cfg(feature = "search")]
            SEARCH_STATUS => Some(Access::Read),
            #[cfg(feature = "search")]
            SEARCH_DROP_INDEX => Some(Access::Write),
            #[cfg(feature = "search")]
            SEARCH_LIST => Some(Access::Read),
            SERVER_STOP => Some(Access::Admin),
            SERVER_CLOCK => Some(Access::Admin),
            #[cfg(feature = "sql")]
            SQL_OPEN => Some(Access::Read),
            #[cfg(feature = "sql")]
            SQL_QUERY => Some(Access::Read),
            #[cfg(feature = "sql")]
            SQL_EXEC => Some(Access::Write),
            #[cfg(feature = "sql")]
            SQL_BATCH => Some(Access::Write),
            #[cfg(feature = "sql")]
            SQL_TX => Some(Access::Write),
            #[cfg(feature = "sql")]
            SQL_COLUMNS => Some(Access::Read),
            _ => None,
        }
    }
}

/// Every method: its name, its number and the shape of its answer, `call` for
/// one message, for the tests that the server answers each, and as its line
/// says, and for a store's own metrics, which count requests by their name.
pub(crate) const METHODS: &[(&str, u16, &str)] = &[
    #[cfg(feature = "backup")]
    ("backup.write", 0x0801, "download"),
    #[cfg(feature = "blobs")]
    ("blobs.open", 0x0401, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.put", 0x0402, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.upload", 0x0403, "exchange"),
    #[cfg(feature = "blobs")]
    ("blobs.get", 0x0404, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.head", 0x0405, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.delete", 0x0406, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.copy", 0x0407, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.rename", 0x0408, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.expire", 0x0409, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.list", 0x040a, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.usage", 0x040b, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.clear", 0x040c, "call"),
    #[cfg(feature = "blobs")]
    ("blobs.read", 0x040d, "download"),
    #[cfg(feature = "channels")]
    ("channels.publish", 0x0a01, "call"),
    #[cfg(feature = "channels")]
    ("channels.subscribe", 0x0a02, "download"),
    #[cfg(feature = "config")]
    ("config.open", 0x0701, "call"),
    #[cfg(feature = "config")]
    ("config.follow", 0x0702, "download"),
    #[cfg(feature = "config")]
    ("config.update", 0x0703, "call"),
    #[cfg(feature = "config")]
    ("config.reset", 0x0704, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.queue.open", 0x0201, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.schedule.open", 0x0202, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.add", 0x0203, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.set", 0x0204, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.update", 0x0205, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.cancel", 0x0206, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.get", 0x0207, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.list", 0x0208, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.work", 0x0209, "exchange"),
    #[cfg(feature = "jobs")]
    ("jobs.step", 0x020a, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.keep", 0x020b, "call"),
    #[cfg(feature = "jobs")]
    ("jobs.watch", 0x020c, "download"),
    #[cfg(feature = "jobs")]
    ("jobs.tx", 0x020d, "call"),
    #[cfg(feature = "kv")]
    ("kv.bucket.open", 0x0101, "call"),
    #[cfg(feature = "kv")]
    ("kv.get", 0x0102, "call"),
    #[cfg(feature = "kv")]
    ("kv.has", 0x0103, "call"),
    #[cfg(feature = "kv")]
    ("kv.set", 0x0104, "call"),
    #[cfg(feature = "kv")]
    ("kv.create", 0x0105, "call"),
    #[cfg(feature = "kv")]
    ("kv.take", 0x0106, "call"),
    #[cfg(feature = "kv")]
    ("kv.delete", 0x0107, "call"),
    #[cfg(feature = "kv")]
    ("kv.expire", 0x0108, "call"),
    #[cfg(feature = "kv")]
    ("kv.clear", 0x0109, "call"),
    #[cfg(feature = "kv")]
    ("kv.list", 0x010a, "call"),
    #[cfg(feature = "kv")]
    ("kv.counters.open", 0x0110, "call"),
    #[cfg(feature = "kv")]
    ("kv.counters.add", 0x0111, "call"),
    #[cfg(feature = "kv")]
    ("kv.counters.get", 0x0112, "call"),
    #[cfg(feature = "kv")]
    ("kv.counters.delete", 0x0113, "call"),
    #[cfg(feature = "kv")]
    ("kv.counters.clear", 0x0114, "call"),
    #[cfg(feature = "kv")]
    ("kv.rateLimit.open", 0x0120, "call"),
    #[cfg(feature = "kv")]
    ("kv.quota.open", 0x0121, "call"),
    #[cfg(feature = "kv")]
    ("kv.allow", 0x0122, "call"),
    #[cfg(feature = "kv")]
    ("kv.peek", 0x0123, "call"),
    #[cfg(feature = "kv")]
    ("kv.reset", 0x0124, "call"),
    #[cfg(feature = "kv")]
    ("kv.refund", 0x0125, "call"),
    #[cfg(feature = "kv")]
    ("kv.once.open", 0x0130, "call"),
    #[cfg(feature = "kv")]
    ("kv.once.run", 0x0131, "handover"),
    #[cfg(feature = "kv")]
    ("kv.once.get", 0x0132, "call"),
    #[cfg(feature = "kv")]
    ("kv.once.delete", 0x0133, "call"),
    #[cfg(feature = "kv")]
    ("kv.tx", 0x0140, "call"),
    #[cfg(feature = "locks")]
    ("locks.hold", 0x0901, "call"),
    #[cfg(feature = "locks")]
    ("locks.renew", 0x0902, "call"),
    #[cfg(feature = "locks")]
    ("locks.release", 0x0903, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.read", 0x0601, "download"),
    #[cfg(feature = "metrics")]
    ("metrics.latest", 0x0602, "download"),
    #[cfg(feature = "metrics")]
    ("metrics.aggregate", 0x0603, "download"),
    #[cfg(feature = "metrics")]
    ("metrics.explain", 0x0604, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.names", 0x0605, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.labelNames", 0x0606, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.labelValues", 0x0607, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.description", 0x0608, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.damaged", 0x0609, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.ingest", 0x060a, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.describe", 0x060b, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.delete", 0x060c, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.flush", 0x060d, "call"),
    #[cfg(feature = "metrics")]
    ("metrics.instruments", 0x060e, "exchange"),
    #[cfg(feature = "records")]
    ("records.open", 0x0501, "call"),
    #[cfg(feature = "records")]
    ("records.append", 0x0502, "call"),
    #[cfg(feature = "records")]
    ("records.list", 0x0503, "call"),
    #[cfg(feature = "records")]
    ("records.count", 0x0504, "call"),
    #[cfg(feature = "records")]
    ("records.top", 0x0505, "call"),
    #[cfg(feature = "records")]
    ("records.aggregate", 0x0506, "call"),
    #[cfg(feature = "records")]
    ("records.streams", 0x0507, "call"),
    #[cfg(feature = "records")]
    ("records.fieldNames", 0x0508, "call"),
    #[cfg(feature = "records")]
    ("records.poll", 0x0509, "call"),
    #[cfg(feature = "records")]
    ("records.tail", 0x050a, "download"),
    #[cfg(feature = "records")]
    ("records.lines", 0x050b, "exchange"),
    #[cfg(feature = "records")]
    ("records.damaged", 0x050c, "call"),
    #[cfg(feature = "records")]
    ("records.drop", 0x050d, "call"),
    #[cfg(feature = "search")]
    ("search.open", 0x0b01, "call"),
    #[cfg(feature = "search")]
    ("search.set", 0x0b02, "call"),
    #[cfg(feature = "search")]
    ("search.setMany", 0x0b03, "call"),
    #[cfg(feature = "search")]
    ("search.delete", 0x0b04, "call"),
    #[cfg(feature = "search")]
    ("search.get", 0x0b05, "call"),
    #[cfg(feature = "search")]
    ("search.search", 0x0b06, "call"),
    #[cfg(feature = "search")]
    ("search.status", 0x0b07, "call"),
    #[cfg(feature = "search")]
    ("search.dropIndex", 0x0b08, "call"),
    #[cfg(feature = "search")]
    ("search.list", 0x0b09, "call"),
    ("server.stop", 0x0001, "call"),
    ("server.clock", 0x0002, "call"),
    #[cfg(feature = "sql")]
    ("sql.open", 0x0301, "call"),
    #[cfg(feature = "sql")]
    ("sql.query", 0x0302, "download"),
    #[cfg(feature = "sql")]
    ("sql.exec", 0x0303, "call"),
    #[cfg(feature = "sql")]
    ("sql.batch", 0x0304, "call"),
    #[cfg(feature = "sql")]
    ("sql.tx", 0x0305, "exchange"),
    #[cfg(feature = "sql")]
    ("sql.columns", 0x0306, "download"),
];

/// A body of the message `name` read and written again, and the size the message
/// says it takes, for the test that the vectors' bytes are what this codec writes
/// within that size; `None` for a name it lacks.
#[cfg(test)]
pub(crate) fn rewrite(name: &str, body: &[u8]) -> Option<Result<(Vec<u8>, usize), Failure>> {
    let rewritten = match name {
        #[cfg(feature = "backup")]
        "backup.Write" => BackupWrite::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "backup")]
        "backup.Piece" => BackupPiece::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "backup")]
        "backup.Written" => BackupWritten::decode(body).map(|message| (message.encode(), message.size())),
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
        #[cfg(feature = "channels")]
        "channels.Publish" => ChannelsPublish::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "channels")]
        "channels.Subscribe" => ChannelsSubscribe::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "channels")]
        "channels.Batch" => ChannelsBatch::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "channels")]
        "channels.Message" => ChannelsMessage::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "config")]
        "config.Open" => ConfigOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "config")]
        "config.Value" => ConfigValue::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "config")]
        "config.Line" => ConfigLine::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "config")]
        "config.Update" => ConfigUpdate::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "config")]
        "config.Reset" => ConfigReset::decode(body).map(|message| (message.encode(), message.size())),
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
        #[cfg(feature = "locks")]
        "locks.Hold" => LocksHold::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "locks")]
        "locks.Held" => LocksHeld::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "locks")]
        "locks.Renew" => LocksRenew::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "locks")]
        "locks.Lease" => LocksLease::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Condition" => MetricsCondition::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Query" => MetricsQuery::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Series" => MetricsSeries::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Read" => MetricsRead::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Newest" => MetricsNewest::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Latest" => MetricsLatest::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.AggregateAsked" => MetricsAggregateAsked::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Bucket" => MetricsBucket::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Group" => MetricsGroup::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Aggregates" => MetricsAggregates::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Plan" => MetricsPlan::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Exceeds" => MetricsExceeds::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.NewSeries" => MetricsNewSeries::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Ingest" => MetricsIngest::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.PageAsked" => MetricsPageAsked::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Name" => MetricsName::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.NamePage" => MetricsNamePage::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.TextPage" => MetricsTextPage::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Description" => MetricsDescription::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.NameAsked" => MetricsNameAsked::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.SeriesNamed" => MetricsSeriesNamed::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Deleted" => MetricsDeleted::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Damage" => MetricsDamage::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Damages" => MetricsDamages::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Session" => MetricsSession::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Instrument" => MetricsInstrument::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Set" => MetricsSet::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Change" => MetricsChange::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Batch" => MetricsBatch::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Refused" => MetricsRefused::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "metrics")]
        "metrics.Ack" => MetricsAck::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Open" => RecordsOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.NewEvent" => RecordsNewEvent::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Append" => RecordsAppend::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Event" => RecordsEvent::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Query" => RecordsQuery::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Page" => RecordsPage::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Count" => RecordsCount::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.TopAsked" => RecordsTopAsked::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Top" => RecordsTop::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Tops" => RecordsTops::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.AggregateAsked" => RecordsAggregateAsked::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Stats" => RecordsStats::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Row" => RecordsRow::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Rows" => RecordsRows::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Stream" => RecordsStream::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Streams" => RecordsStreams::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.FieldNamesAsked" => RecordsFieldNamesAsked::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Names" => RecordsNames::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.PollAsked" => RecordsPollAsked::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Batch" => RecordsBatch::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.TailBatch" => RecordsTailBatch::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Piece" => RecordsPiece::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Dropped" => RecordsDropped::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Damage" => RecordsDamage::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "records")]
        "records.Damages" => RecordsDamages::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Open" => SearchOpen::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Field" => SearchField::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Set" => SearchSet::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.SetMany" => SearchSetMany::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Document" => SearchDocument::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Key" => SearchKey::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Deleted" => SearchDeleted::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Got" => SearchGot::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Query" => SearchQuery::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Hit" => SearchHit::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Found" => SearchFound::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Status" => SearchStatus::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Name" => SearchName::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Dropped" => SearchDropped::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Index" => SearchIndex::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "search")]
        "search.Listed" => SearchListed::decode(body).map(|message| (message.encode(), message.size())),
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
        "sql.Column" => SqlColumn::decode(body).map(|message| (message.encode(), message.size())),
        #[cfg(feature = "sql")]
        "sql.Columns" => SqlColumns::decode(body).map(|message| (message.encode(), message.size())),
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
