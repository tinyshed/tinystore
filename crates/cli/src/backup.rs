//! `tinystore backup` and `tinystore restore`: a store's directory as one
//! checked zip, and that zip back into an empty directory.
//!
//! ```text
//! tinystore backup ./data backup-2026-10-13.zip [--include secret.key]… [--include-encryption-key]
//! tinystore restore backup-2026-10-13.zip ./data
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use tinystore::{ErrorKind, Options, Store};

pub(crate) const BACKUP_USAGE: &str = "usage: tinystore backup <dir> <file.zip> [--include <name>]... [--include-encryption-key] [--encryption-key-file <file>]";
pub(crate) const RESTORE_USAGE: &str = "usage: tinystore restore <file.zip> <dir>";

/// What `tinystore backup` is asked.
#[derive(Debug, PartialEq)]
pub(crate) struct Asked {
    pub(crate) dir: PathBuf,
    pub(crate) zip: PathBuf,
    pub(crate) include: Vec<String>,
    pub(crate) include_encryption_key: bool,
    pub(crate) encryption_key_file: Option<PathBuf>,
}

pub(crate) fn parse(args: &[String]) -> Result<Asked, String> {
    let (mut places, mut include, mut include_key, mut key_file) = (Vec::new(), Vec::new(), false, None);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--include" => include.push(args.next().ok_or("--include takes a name")?.clone()),
            "--include-encryption-key" => include_key = true,
            "--encryption-key-file" => {
                key_file = Some(PathBuf::from(args.next().ok_or("--encryption-key-file takes a file")?));
            }
            flag if flag.starts_with("--") => return Err(format!("{flag}: no such flag")),
            place => places.push(PathBuf::from(place)),
        }
    }
    let [dir, zip] = <[PathBuf; 2]>::try_from(places).map_err(|_| "a directory and a zip".to_owned())?;
    Ok(Asked { dir, zip, include, include_encryption_key: include_key, encryption_key_file: key_file })
}

/// Backs up the store in a directory that no server or program holds.
pub(crate) fn backup(args: &[String]) -> ExitCode {
    let asked = match parse(args) {
        Ok(asked) => asked,
        Err(why) => {
            eprintln!("tinystore backup: {why}\n{BACKUP_USAGE}");
            return ExitCode::from(2);
        }
    };
    let options =
        Options { background: false, encryption_key_file: asked.encryption_key_file.clone(), ..Options::default() };
    let store = match Store::open(&asked.dir, options) {
        Ok(store) => store,
        Err(error) if error.kind() == ErrorKind::InUse => {
            eprintln!(
                "tinystore backup: {error}\na program or a server holds {}: back it up through it, as an SDK's store.backup does",
                asked.dir.display()
            );
            return ExitCode::FAILURE;
        }
        Err(error) => return failed("backup", &error),
    };
    let mut backup = store.backup();
    for name in &asked.include {
        backup = backup.include(name);
    }
    if asked.include_encryption_key {
        backup = backup.include_encryption_key();
    }
    let written = backup.write(&asked.zip).and_then(|backup| store.close().map(|()| backup));
    match written {
        Ok(backup) => {
            println!(
                "● {}  backed up to {} · {} files, {} bytes",
                asked.dir.display(),
                asked.zip.display(),
                backup.files,
                backup.bytes
            );
            if backup.key_left_out {
                println!(
                    "  the store seals values with its encryption key, which the backup leaves out: keep the key \
                     apart, or include it with --include-encryption-key"
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => failed("backup", &error),
    }
}

/// Restores a backup into an empty directory.
pub(crate) fn restore(args: &[String]) -> ExitCode {
    let [zip, dir] = match <[String; 2]>::try_from(args.to_vec()) {
        Ok(places) if !places.iter().any(|place| place.starts_with("--")) => places,
        _ => {
            eprintln!("{RESTORE_USAGE}");
            return ExitCode::from(2);
        }
    };
    match tinystore::backup::restore(&zip, &dir) {
        Ok(backup) => {
            println!("● {dir}  restored from {zip} · {} files, {} bytes", backup.files, backup.bytes);
            if backup.key_left_out {
                println!(
                    "  its encrypted buckets open with the store's key, which the backup left out: put it back first"
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => failed("restore", &error),
    }
}

fn failed(command: &str, error: &tinystore::Error) -> ExitCode {
    eprintln!("tinystore {command}: {error}");
    ExitCode::FAILURE
}
