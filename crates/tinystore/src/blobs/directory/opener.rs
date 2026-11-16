#![allow(unsafe_code)]
//! A file of a directory opened a segment at a time from its folder,
//! following no link: a path a scan listed and someone swapped for a link
//! before the read is refused at the read (BLOB-14).
//!
//! On Unix each segment is opened with `openat` from the folder opened before
//! it, `O_NOFOLLOW` refusing a link, and on another file system's device
//! refused too: std has no `openat`, so this is a place of `unsafe`. On
//! Windows each segment is opened by its path with its reparse point
//! unfollowed and refused, and the folders above it are held without sharing
//! their deletion, so that none can be renamed away or replaced by a junction
//! between its check and the next segment's open; std's own options do that.

use std::fs::File;
use std::io;
use std::path::Path;

/// The file at `path` in the folder `root`, a path of segments between `/`:
/// none when it does not lead to a file, through a link, a file that is not
/// one, a folder on another disk or a segment gone.
#[cfg(unix)]
pub(super) fn open_beneath(root: &Path, path: &str) -> io::Result<Option<File>> {
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    // The folder's own path is the program's, its links its choice.
    let folder = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY).open(root)?;
    let device = folder.metadata()?.dev();
    let mut at: OwnedFd = folder.into();
    let mut segments = path.split('/').peekable();
    while let Some(segment) = segments.next() {
        let last = segments.peek().is_none();
        // A FIFO in a file's place would block its open: O_NONBLOCK opens it at
        // once, to be refused below, and changes nothing of a regular file's reads.
        let kind = if last { libc::O_NONBLOCK } else { libc::O_DIRECTORY };
        let opened = match open_at(&at, segment, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | kind) {
            Ok(opened) => File::from(opened),
            Err(error) if gone(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = opened.metadata()?;
        if metadata.dev() != device {
            return Ok(None);
        }
        if last {
            return Ok(metadata.is_file().then_some(opened));
        }
        if !metadata.is_dir() {
            return Ok(None);
        }
        at = opened.into();
    }
    Ok(None)
}

/// `name` opened from the folder `at` with `flags`, none of which makes a file.
#[cfg(unix)]
fn open_at(at: &std::os::fd::OwnedFd, name: &str, flags: libc::c_int) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = std::ffi::CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    loop {
        // SAFETY: `at` is an open descriptor borrowed for the call, `name` a
        // NUL-terminated string that outlives it, and without O_CREAT among
        // the flags openat reads no mode argument.
        let fd = unsafe { libc::openat(at.as_raw_fd(), name.as_ptr(), flags) };
        if fd >= 0 {
            // SAFETY: openat returned a descriptor of its own, which nothing else owns or closes.
            return Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Whether an open failed because the segment is not what the list holds:
/// gone, a link (`ELOOP`, `EMLINK` on the BSDs), or a file where a folder was.
#[cfg(unix)]
fn gone(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ELOOP | libc::EMLINK | libc::ENOTDIR))
}

#[cfg(windows)]
pub(super) fn open_beneath(root: &Path, path: &str) -> io::Result<Option<File>> {
    use std::os::windows::fs::MetadataExt;

    // The folder's own path is the program's, its junction its choice.
    let mut held = vec![open_segment(root, false, false)?];
    let mut at = root.to_path_buf();
    let mut segments = path.split('/').peekable();
    while let Some(segment) = segments.next() {
        let last = segments.peek().is_none();
        at.push(segment);
        let opened = match open_segment(&at, last, true) {
            Ok(opened) => opened,
            Err(error) if gone(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = opened.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Ok(None);
        }
        if last {
            return Ok(metadata.is_file().then_some(opened));
        }
        if !metadata.is_dir() {
            return Ok(None);
        }
        held.push(opened);
    }
    Ok(None)
}

#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// One segment opened by its path. A folder is held to read its attributes,
/// sharing its reads and writes and not its deletion, so that it stays where
/// it is while it is held; a file to read, sharing all three, as Rust opens
/// every file, so that the folder's owner can replace it while it is read.
/// `unfollowed` opens a reparse point itself rather than what it names.
#[cfg(windows)]
fn open_segment(path: &Path, file: bool, unfollowed: bool) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut options = std::fs::OpenOptions::new();
    // backup semantics opens a folder, and a file as it opens any
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if unfollowed {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    if file {
        options.read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    } else {
        options.access_mode(FILE_READ_ATTRIBUTES).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    options.custom_flags(flags).open(path)
}

/// Whether an open failed because the segment is not what the list holds:
/// gone, or a folder's path where a file is.
#[cfg(windows)]
fn gone(error: &io::Error) -> bool {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PATH_NOT_FOUND: i32 = 3;
    const ERROR_INVALID_NAME: i32 = 123;
    const ERROR_DIRECTORY: i32 = 267;
    matches!(
        error.raw_os_error(),
        Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_INVALID_NAME | ERROR_DIRECTORY)
    )
}
