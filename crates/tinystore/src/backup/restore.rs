//! A backup put back into a directory before a store opens there: every
//! file checked against its manifest's size and SHA-256 and its zip's
//! CRC-32 as it is written, and nothing left in the directory when one does
//! not match.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use super::manifest::{Listed, MANIFEST, Manifest};
use super::{Backup, is_inside, zip};
use crate::durable::sync_directory;
use crate::store::{HALF_RESTORED, RESTORING};
use crate::text::hex;
use crate::{Error, ErrorKind, Result};

/// Restores the backup at `archive` into `dir`, which is empty or absent,
/// before a store opens there; the store's `LOCK` is held meanwhile, so that
/// none opens before the restore ends. A file that does not match what the
/// backup says of it fails the restore `Corrupt`, and leaves `dir` empty.
pub fn restore(archive: impl AsRef<Path>, dir: impl AsRef<Path>) -> Result<Backup> {
    let (archive, dir) = (archive.as_ref(), dir.as_ref());
    let within = |error: Error| error.within(format!("restore {} into {}", archive.display(), dir.display()));
    let mut zip = io::BufReader::new(fs::File::open(archive).map_err(|error| within(Error::io("open", error)))?);
    let (entries, manifest) = read(&mut zip).map_err(|why| within(Error::new(ErrorKind::Corrupt, why)))?;
    claim_empty(dir).map_err(within)?;
    let lock = crate::store::take_lock(dir).map_err(within)?;
    let restored = begun(dir)
        .and_then(|()| manifest.files.iter().try_for_each(|file| put(&mut zip, &entries, dir, file)))
        .and_then(|()| synced(dir))
        .and_then(|()| finished(dir));
    if let Err(error) = restored {
        // Emptied while the LOCK is held, so no store opens the half restored;
        // the marker goes last, so that a directory emptied in part opens no more.
        let emptied = empty(dir).and_then(|()| removed(&dir.join(RESTORING)));
        drop(lock);
        let lock_file = dir.join("LOCK");
        let error = match emptied.and_then(|()| removed(&lock_file)) {
            Ok(()) => error,
            Err(left) => error.within(left),
        };
        return Err(within(error));
    }
    drop(lock);
    Ok(manifest.summary())
}

/// The zip's files by name, and its manifest, checked.
fn read(zip: &mut (impl Read + io::Seek)) -> std::result::Result<(HashMap<String, zip::Entry>, Manifest), String> {
    let entries: HashMap<String, zip::Entry> =
        zip::entries(zip)?.into_iter().map(|entry| (entry.name.clone(), entry)).collect();
    let listed = entries.get(MANIFEST).ok_or("a zip without the manifest a backup holds")?;
    // a manifest lists files its zip holds, so no more than it holds
    let manifest = Manifest::read(zip::open(zip, listed)?, entries.len())?;
    for file in &manifest.files {
        if !is_inside(&file.name) || [MANIFEST, "LOCK", RESTORING].contains(&file.name.as_str()) {
            return Err(format!("{:?}: a path that is not a file of a store's directory", file.name));
        }
        if !entries.contains_key(&file.name) {
            return Err(format!("{}: listed by its manifest, and not in the zip", file.name));
        }
    }
    Ok((entries, manifest))
}

/// Makes `dir` when it is absent; one that holds anything is refused, and
/// one a restore did not finish in says what to do.
fn claim_empty(dir: &Path) -> Result<()> {
    match fs::read_dir(dir).map(|mut entries| entries.next().is_some()) {
        Ok(true) if dir.join(RESTORING).exists() => {
            Err(Error::invalid(format!("the directory is not empty: {HALF_RESTORED}")))
        }
        Ok(true) => Err(Error::invalid("the directory is not empty: a restore writes into an empty one")),
        Ok(false) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(dir).map_err(|error| Error::io("make the directory", error))
        }
        Err(error) => Err(Error::io("read the directory", error)),
    }
}

/// Writes one file beside its name, checks it, syncs it, and only then gives
/// it its name.
fn put(
    zip: &mut (impl Read + io::Seek),
    entries: &HashMap<String, zip::Entry>,
    dir: &Path,
    file: &Listed,
) -> Result<()> {
    let corrupt = |why: String| Error::new(ErrorKind::Corrupt, format!("{}: {why}", file.name));
    let failed = |error: io::Error| Error::io(file.name.clone(), error);
    let entry = &entries[&file.name];
    if entry.size != file.size {
        return Err(corrupt(format!("{} bytes in the zip, where its manifest says {}", entry.size, file.size)));
    }
    let target = dir.join(&file.name);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(failed)?;
    }
    let partial = target
        .with_file_name(format!("{}.partial", target.file_name().and_then(|name| name.to_str()).unwrap_or("file")));
    let mut out = owned_file(&partial).map_err(failed)?;
    let mut bytes = zip::open(zip, entry).map_err(corrupt)?;
    let mut sha = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = match bytes.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if matches!(error.kind(), io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof) => {
                return Err(corrupt(error.to_string()));
            }
            Err(error) => return Err(failed(error)),
        };
        sha.update(&buffer[..read]);
        out.write_all(&buffer[..read]).map_err(failed)?;
    }
    if hex(&sha.finalize()) != file.sha256 {
        return Err(corrupt("its SHA-256 is not its manifest's".to_owned()));
    }
    out.sync_all().map_err(failed)?;
    drop(out);
    fs::rename(&partial, &target).map_err(failed)
}

/// A new file only its owner reads and writes, as the files of a store and
/// its key are kept.
fn owned_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Syncs every directory the restore wrote names in, so that the names
/// survive a power loss as the bytes do.
fn synced(dir: &Path) -> Result<()> {
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).map_err(|error| Error::io("sync", error))? {
            let path = entry.map_err(|error| Error::io("sync", error))?.path();
            if path.is_dir() {
                folders.push(path);
            }
        }
        sync_directory(&folder).map_err(|error| Error::io(format!("sync {}", folder.display()), error))?;
    }
    Ok(())
}

/// Puts `RESTORING` in `dir` before any file of the backup's, its name
/// synced, so that a restore whose process dies leaves a directory no store
/// opens.
fn begun(dir: &Path) -> Result<()> {
    let failed = |error: io::Error| Error::io(RESTORING, error);
    owned_file(&dir.join(RESTORING)).and_then(|marker| marker.sync_all()).map_err(failed)?;
    sync_directory(dir).map_err(failed)
}

/// Removes `RESTORING` once every file and name is synced, and syncs its
/// removal: the directory opens from then on.
fn finished(dir: &Path) -> Result<()> {
    let failed = |error: io::Error| Error::io(RESTORING, error);
    fs::remove_file(dir.join(RESTORING)).map_err(failed)?;
    sync_directory(dir).map_err(failed)
}

/// Removes what a restore that failed wrote, `dir` itself left; what will
/// not go is said.
fn empty(dir: &Path) -> std::result::Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|error| format!("{} is left: {error}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|error| format!("something of {} is left: {error}", dir.display()))?.path();
        // The LOCK is held while this runs, and goes once let go; the marker
        // goes once all else has.
        if path.file_name().is_some_and(|name| name == "LOCK" || name == RESTORING) {
            continue;
        }
        let removed = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        removed.map_err(|error| format!("{} is left: {error}", path.display()))?;
    }
    Ok(())
}

fn removed(file: &Path) -> std::result::Result<(), String> {
    match fs::remove_file(file) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(format!("{} is left: {error}", file.display())),
        _ => Ok(()),
    }
}
