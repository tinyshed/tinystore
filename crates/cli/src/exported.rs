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
        fs::rename(&part, path).map_err(|error| failed(format!("{}: {error}", path.display())))
    });
    if done.is_err() {
        let _ = fs::remove_file(&part);
    }
    done
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
