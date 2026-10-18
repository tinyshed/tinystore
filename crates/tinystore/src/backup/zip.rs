//! A zip as a backup writes and reads one: each file stored as it is, its
//! CRC-32 and sizes after its bytes, so that a file is read once however
//! large, and ZIP64's fields where a size, an offset or a count passes its
//! field. Every tool that opens a zip opens it.
//!
//! ```text
//! local header · bytes · data descriptor   a file, as many as the zip holds
//! central directory                         each file's name, CRC-32, size and offset
//! ZIP64 end and its locator                 when a count, a size or an offset is past its field
//! end of the central directory
//! ```

use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::compress::checksum::Crc32;

const LOCAL: u32 = 0x0403_4b50;
const DESCRIPTOR: u32 = 0x0807_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const END64: u32 = 0x0606_4b50;
const LOCATOR64: u32 = 0x0706_4b50;
/// The extra field of ZIP64's sizes and offset.
const ZIP64: u16 = 0x0001;
/// Sizes and CRC-32 after the bytes, and the name in UTF-8.
const FLAGS: u16 = 1 << 3 | 1 << 11;
/// What reading a ZIP64 entry takes, and a stored one.
const NEEDS_ZIP64: u16 = 45;
const NEEDS: u16 = 20;
/// 1980-01-01, the first day a zip's time can say: the manifest has the
/// backup's own.
const DOS_DATE: u16 = 1 << 5 | 1;
/// A regular file only its owner reads and writes, as Unix's tools read it.
const MODE: u32 = 0o100_600 << 16;
/// Made on Unix, by ZIP 4.5.
const MADE_BY: u16 = 3 << 8 | 45;
const MAX16: u64 = 0xffff;
const MAX32: u64 = 0xffff_ffff;
/// The end of the central directory without a comment, and the longest
/// comment a zip may end with.
const END_SIZE: u64 = 22;
const MOST_COMMENT: u64 = 0xffff;

/// A file a zip holds: where it lies and what checks its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) crc: u32,
    pub(crate) size: u64,
    /// The offset of its local header.
    pub(crate) offset: u64,
}

/// A zip being written to `out`, a file at a time.
pub(crate) struct Writer<W: Write> {
    out: Counted<W>,
    entries: Vec<Entry>,
    /// The size or offset from which a field is ZIP64's: four bytes' worth,
    /// less in a test.
    wide_from: u64,
}

impl<W: Write> Writer<W> {
    pub(crate) fn new(out: W) -> Self {
        Writer { out: Counted { out, written: 0, headers: 0 }, entries: Vec::new(), wide_from: MAX32 }
    }

    /// Writes a file of `size` bytes from `bytes`, handing each piece to
    /// `seen` as it goes by: a file that gives more or fewer is refused.
    pub(crate) fn add(&mut self, name: &str, size: u64, bytes: impl Read, seen: impl FnMut(&[u8])) -> io::Result<()> {
        let offset = self.out.written;
        let wide = size >= self.wide_from || offset >= self.wide_from;
        self.local_header(name, wide)?;
        let (copied, crc) = copy(bytes, &mut self.out, seen)?;
        if copied != size {
            return Err(io::Error::other(format!("{name}: {copied} bytes where {size} were, changed as it was read")));
        }
        self.out.u32(DESCRIPTOR)?.u32(crc)?;
        if wide {
            self.out.u64(size)?.u64(size)?;
        } else {
            self.out.u32(size as u32)?.u32(size as u32)?;
        }
        self.entries.push(Entry { name: name.to_owned(), crc, size, offset });
        Ok(())
    }

    /// Writes the central directory and the end, and gives back the writer
    /// and the bytes the zip has.
    pub(crate) fn finish(mut self) -> io::Result<(W, u64)> {
        let start = self.out.written;
        for entry in std::mem::take(&mut self.entries).iter() {
            self.central_header(entry)?;
        }
        let (size, count) = (self.out.written - start, self.out.headers);
        let wide = count >= MAX16 || size >= self.wide_from || start >= self.wide_from;
        if wide {
            let end64 = self.out.written;
            self.out.u32(END64)?.u64(44)?.u16(MADE_BY)?.u16(NEEDS_ZIP64)?.u32(0)?.u32(0)?;
            self.out.u64(count)?.u64(count)?.u64(size)?.u64(start)?;
            self.out.u32(LOCATOR64)?.u32(0)?.u64(end64)?.u32(1)?;
        }
        let (entries, size, start) = if wide { (MAX16, MAX32, MAX32) } else { (count, size, start) };
        self.out.u32(END)?.u16(0)?.u16(0)?.u16(entries as u16)?.u16(entries as u16)?;
        self.out.u32(size as u32)?.u32(start as u32)?.u16(0)?;
        self.out.out.flush()?;
        Ok((self.out.out, self.out.written))
    }

    fn local_header(&mut self, name: &str, wide: bool) -> io::Result<()> {
        let (needs, extra, sizes) = if wide { (NEEDS_ZIP64, 20, MAX32 as u32) } else { (NEEDS, 0, 0) };
        self.out.u32(LOCAL)?.u16(needs)?.u16(FLAGS)?.u16(0)?.u16(0)?.u16(DOS_DATE)?;
        self.out.u32(0)?.u32(sizes)?.u32(sizes)?.u16(name_length(name)?)?.u16(extra)?.bytes(name.as_bytes())?;
        if wide {
            // the sizes come after the bytes, eight bytes each, as this says
            self.out.u16(ZIP64)?.u16(16)?.u64(0)?.u64(0)?;
        }
        Ok(())
    }

    fn central_header(&mut self, entry: &Entry) -> io::Result<()> {
        let (wide_size, wide_offset) = (entry.size >= self.wide_from, entry.offset >= self.wide_from);
        let mut extra = Vec::new();
        if wide_size {
            extra.extend_from_slice(&entry.size.to_le_bytes());
            extra.extend_from_slice(&entry.size.to_le_bytes());
        }
        if wide_offset {
            extra.extend_from_slice(&entry.offset.to_le_bytes());
        }
        let needs = if extra.is_empty() { NEEDS } else { NEEDS_ZIP64 };
        let size = if wide_size { MAX32 as u32 } else { entry.size as u32 };
        let offset = if wide_offset { MAX32 as u32 } else { entry.offset as u32 };
        let extra_length = if extra.is_empty() { 0 } else { 4 + extra.len() as u16 };
        self.out.u32(CENTRAL)?.u16(MADE_BY)?.u16(needs)?.u16(FLAGS)?.u16(0)?.u16(0)?.u16(DOS_DATE)?;
        self.out.u32(entry.crc)?.u32(size)?.u32(size)?.u16(name_length(&entry.name)?)?.u16(extra_length)?;
        self.out.u16(0)?.u16(0)?.u16(0)?.u32(MODE)?.u32(offset)?.bytes(entry.name.as_bytes())?;
        if !extra.is_empty() {
            self.out.u16(ZIP64)?.u16(extra.len() as u16)?.bytes(&extra)?;
        }
        self.out.headers += 1;
        Ok(())
    }
}

fn name_length(name: &str) -> io::Result<u16> {
    u16::try_from(name.len()).map_err(|_| io::Error::other(format!("{name}: a name past 65535 bytes")))
}

/// Copies `from` to `to` a piece at a time, each piece handed to `seen`, and
/// gives back the bytes copied and their CRC-32.
fn copy(mut from: impl Read, to: &mut impl Write, mut seen: impl FnMut(&[u8])) -> io::Result<(u64, u32)> {
    let mut buffer = vec![0; 1 << 16];
    let (mut copied, mut crc) = (0_u64, Crc32::new());
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) => return Ok((copied, crc.finish())),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let piece = &buffer[..read];
        to.write_all(piece)?;
        seen(piece);
        crc = crc.update(piece);
        copied += read as u64;
    }
}

/// A writer that counts what it wrote and the central headers among it, and
/// writes a zip's integers little end first.
struct Counted<W: Write> {
    out: W,
    written: u64,
    headers: u64,
}

impl<W: Write> Counted<W> {
    fn bytes(&mut self, bytes: &[u8]) -> io::Result<&mut Self> {
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(self)
    }

    fn u16(&mut self, value: u16) -> io::Result<&mut Self> {
        self.bytes(&value.to_le_bytes())
    }

    fn u32(&mut self, value: u32) -> io::Result<&mut Self> {
        self.bytes(&value.to_le_bytes())
    }

    fn u64(&mut self, value: u64) -> io::Result<&mut Self> {
        self.bytes(&value.to_le_bytes())
    }
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.out.write(bytes)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// The files a zip holds, read from its central directory; a zip this
/// module did not write reads too, while each file is stored as it is.
pub(crate) fn entries(zip: &mut (impl Read + Seek)) -> Result<Vec<Entry>, String> {
    let length = zip.seek(SeekFrom::End(0)).map_err(|error| error.to_string())?;
    let ((mut count, mut size, mut start), end) = end_of(zip, length)?;
    if count == MAX16 || size == MAX32 || start == MAX32 {
        (count, size, start) = end64_of(zip, end)?;
    }
    if start.checked_add(size).is_none_or(|end| end > length) {
        return Err("its central directory lies past its end".to_owned());
    }
    let mut directory = vec![0; usize::try_from(size).map_err(|_| "a central directory past memory")?];
    read_at(zip, start, &mut directory)?;
    let mut fields = Fields { bytes: &directory, at: 0 };
    (0..count).map(|_| central_entry(&mut fields)).collect()
}

/// The count of files, the central directory's size and its offset, as the
/// end of the central directory says them, and where that end begins.
fn end_of(zip: &mut (impl Read + Seek), length: u64) -> Result<((u64, u64, u64), u64), String> {
    const CUT_SHORT: &str = "no end of a central directory: not a zip, or cut short";
    if length < END_SIZE {
        return Err(CUT_SHORT.to_owned());
    }
    let tail_length = length.min(END_SIZE + MOST_COMMENT);
    let mut tail = vec![0; tail_length as usize];
    read_at(zip, length - tail_length, &mut tail)?;
    let found = (0..=tail.len() - END_SIZE as usize)
        .rev()
        .find(|&at| tail[at..at + 4] == END.to_le_bytes())
        .ok_or(CUT_SHORT)?;
    let mut fields = Fields { bytes: &tail[found..], at: 10 };
    let count = u64::from(fields.u16()?);
    let (size, start) = (u64::from(fields.u32()?), u64::from(fields.u32()?));
    Ok(((count, size, start), length - tail_length + found as u64))
}

/// The ZIP64 end, whose locator lies just before the end at `end`, whatever
/// comment follows that end.
fn end64_of(zip: &mut (impl Read + Seek), end: u64) -> Result<(u64, u64, u64), String> {
    let mut locator = [0; 20];
    let at = end.checked_sub(20).ok_or("a ZIP64 end that is not there")?;
    read_at(zip, at, &mut locator)?;
    let mut fields = Fields { bytes: &locator, at: 0 };
    if fields.u32()? != LOCATOR64 {
        return Err("a ZIP64 end without its locator".to_owned());
    }
    fields.at = 8;
    let end64 = fields.u64()?;
    let mut record = [0; 56];
    read_at(zip, end64, &mut record)?;
    let mut fields = Fields { bytes: &record, at: 0 };
    if fields.u32()? != END64 {
        return Err("a ZIP64 end that is not one".to_owned());
    }
    fields.at = 32;
    let count = fields.u64()?;
    Ok((count, fields.u64()?, fields.u64()?))
}

fn central_entry(fields: &mut Fields<'_>) -> Result<Entry, String> {
    if fields.u32()? != CENTRAL {
        return Err("a central directory whose header is not one".to_owned());
    }
    fields.at += 6;
    let method = fields.u16()?;
    fields.at += 4;
    let crc = fields.u32()?;
    let (stored, size) = (u64::from(fields.u32()?), u64::from(fields.u32()?));
    let (name_length, extra_length, comment_length) = (fields.u16()?, fields.u16()?, fields.u16()?);
    fields.at += 8;
    let offset = u64::from(fields.u32()?);
    let name = String::from_utf8(fields.take(usize::from(name_length))?.to_vec())
        .map_err(|_| "a file's name that is no UTF-8".to_owned())?;
    let extra = fields.take(usize::from(extra_length))?;
    fields.take(usize::from(comment_length))?;
    let (size, stored, offset) = widened(extra, size, stored, offset)?;
    if method != 0 || stored != size {
        return Err(format!("{name}: compressed, where a backup stores each file as it is"));
    }
    Ok(Entry { name, crc, size, offset })
}

/// The sizes and the offset ZIP64's extra field gives in place of those
/// their fields hold as all ones.
fn widened(extra: &[u8], size: u64, stored: u64, offset: u64) -> Result<(u64, u64, u64), String> {
    let mut fields = Fields { bytes: extra, at: 0 };
    while fields.at + 4 <= extra.len() {
        let (id, length) = (fields.u16()?, usize::from(fields.u16()?));
        let mut field = Fields { bytes: fields.take(length)?, at: 0 };
        if id != ZIP64 {
            continue;
        }
        let size = if size == MAX32 { field.u64()? } else { size };
        let stored = if stored == MAX32 { field.u64()? } else { stored };
        let offset = if offset == MAX32 { field.u64()? } else { offset };
        return Ok((size, stored, offset));
    }
    Ok((size, stored, offset))
}

/// A reader of a file's bytes inside the zip, which checks them against its
/// CRC-32 once they end.
pub(crate) fn open<'z, R: Read + Seek>(zip: &'z mut R, entry: &Entry) -> Result<impl Read + 'z, String> {
    let mut header = [0; 30];
    read_at(zip, entry.offset, &mut header)?;
    let mut fields = Fields { bytes: &header, at: 0 };
    if fields.u32()? != LOCAL {
        return Err(format!("{}: its local header is not one", entry.name));
    }
    fields.at = 26;
    let skip = u64::from(fields.u16()?) + u64::from(fields.u16()?);
    let start = entry.offset + 30 + skip;
    zip.seek(SeekFrom::Start(start)).map_err(|error| error.to_string())?;
    Ok(Checked { bytes: zip.take(entry.size), crc: Crc32::new(), expected: entry.crc, name: entry.name.clone() })
}

/// A file's bytes, refused at their end when their CRC-32 is not the zip's.
struct Checked<R: Read> {
    bytes: io::Take<R>,
    crc: Crc32,
    expected: u32,
    name: String,
}

impl<R: Read> Read for Checked<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.bytes.read(buffer)?;
        self.crc = self.crc.update(&buffer[..read]);
        if read == 0 && !buffer.is_empty() {
            if self.bytes.limit() > 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("{}: cut short", self.name)));
            }
            if self.crc.finish() != self.expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: its CRC-32 is not the zip's", self.name),
                ));
            }
        }
        Ok(read)
    }
}

fn read_at(zip: &mut (impl Read + Seek), at: u64, bytes: &mut [u8]) -> Result<(), String> {
    zip.seek(SeekFrom::Start(at)).map_err(|error| error.to_string())?;
    zip.read_exact(bytes).map_err(|_| "cut short".to_owned())
}

/// A zip's fields read little end first, each bounded by what is left.
struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(length).filter(|end| *end <= self.bytes.len());
        let end = end.ok_or("a field past the end of what holds it")?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap_or_default()))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap_or_default()))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap_or_default()))
    }
}

#[cfg(test)]
#[path = "zip_tests.rs"]
mod tests;
