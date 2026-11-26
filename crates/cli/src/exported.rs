//! Where an export writes: stdout, or the file `-o` names, which appears
//! whole once written, so that a collector reading it never finds half a
//! text.
//!
//! ```text
//! tinystore metrics ./data export                       stdout, buffered
//! tinystore metrics ./data export -o shop.prom          shop.prom.part, renamed shop.prom at its end
//! ```

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::asked::{Done, Refused, failed, unwritten};

/// Runs `export` on stdout, or on the file `to` names; a reader of stdout
/// gone ends it quietly, as every command ends.
pub(crate) fn to(to: Option<&str>, export: impl FnOnce(&mut dyn Write) -> Done) -> Done {
    match to {
        None => to_stdout(export),
        Some(path) => to_file(Path::new(path), export),
    }
}

fn to_stdout(export: impl FnOnce(&mut dyn Write) -> Done) -> Done {
    let mut out = Watched { inner: BufWriter::new(io::stdout().lock()), gone: None };
    let done = export(&mut out).and_then(|()| out.flush().map_err(unwritten));
    match (done, out.gone) {
        (Err(_), Some(error)) => Err(unwritten(error)),
        (done, _) => done,
    }
}

/// Writes beside the file and renames it once whole; a failure leaves the
/// file as it was and removes what it wrote.
fn to_file(path: &Path, export: impl FnOnce(&mut dyn Write) -> Done) -> Done {
    let part = part_of(path);
    let file = File::create(&part).map_err(|error| failed(format!("{}: {error}", part.display())))?;
    let mut out = BufWriter::new(file);
    let done = export(&mut out).and_then(|()| {
        let file = out.into_inner().map_err(|error| failed(format!("{}: {}", part.display(), error.error())))?;
        file.sync_all().map_err(|error| failed(format!("{}: {error}", part.display())))?;
        drop(file);
        renamed(&part, path).map_err(|error| failed(format!("{}: {error}", path.display())))
    });
    if done.is_err() {
        let _ = fs::remove_file(&part);
    }
    done
}

/// Renames `part` over `path`, again for up to a second while another
/// handle holds `path` on Windows: a collector reading the file the export
/// replaces opens it without sharing its deletion, and the rename over it is
/// refused until it closes.
fn renamed(part: &Path, path: &Path) -> io::Result<()> {
    const TRIES: u32 = 11;
    for attempt in 0.. {
        match fs::rename(part, path) {
            Err(error) if held_elsewhere(&error) && attempt + 1 < TRIES => {
                std::thread::sleep(std::time::Duration::from_millis(1 << attempt.min(7)));
            }
            done => return done,
        }
    }
    unreachable!("the last try returns")
}

#[cfg(windows)]
fn held_elsewhere(error: &io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
}

#[cfg(not(windows))]
fn held_elsewhere(_: &io::Error) -> bool {
    false
}

/// `shop.prom.part` beside `shop.prom`: a textfile collector reads only
/// what ends `.prom`.
fn part_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// Stdout, keeping the error that ended it, so that a reader gone is told
/// from a failure of the store whatever the export wraps it in.
struct Watched<W> {
    inner: W,
    gone: Option<io::Error>,
}

impl<W: Write> Write for Watched<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf).inspect_err(|error| self.keep(error))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().inspect_err(|error| self.keep(error))
    }
}

impl<W> Watched<W> {
    fn keep(&mut self, error: &io::Error) {
        if error.kind() == io::ErrorKind::BrokenPipe {
            self.gone = Some(io::Error::from(io::ErrorKind::BrokenPipe));
        }
    }
}

/// A write of an export that failed, a reader gone among them, which
/// [`to`] tells apart once the export has ended.
pub(crate) fn unwritable(error: io::Error) -> Refused {
    failed(format!("the export: {error}"))
}

#[cfg(all(test, windows))]
mod tests {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_rename_over_a_file_a_reader_holds_waits_for_it_to_close() {
        const FILE_SHARE_READ: u32 = 1;
        let dir = tempfile::tempdir().unwrap();
        let (part, path) = (dir.path().join("shop.prom.part"), dir.path().join("shop.prom"));
        fs::write(&path, "old").unwrap();
        fs::write(&part, "new").unwrap();
        // a reader that shares no deletion, as a collector's may open it
        let held = fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path).unwrap();
        assert!(fs::rename(&part, &path).is_err(), "Windows refuses the rename while it is held");
        let reader = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
        });
        renamed(&part, &path).unwrap();
        reader.join().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }
}
