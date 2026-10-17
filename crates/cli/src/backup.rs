//! `tinystore backup` and `tinystore restore`: a store's directory as one
//! checked zip, and that zip back into an empty directory. A backup reads
//! beside a program or a server that holds the directory, and alone when
//! none does.
//!
//! ```text
//! tinystore backup ./data backup-2026-10-13.zip [--include secret.key]… [--include-encryption-key]
//! tinystore restore backup-2026-10-13.zip ./data
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use tinystore::{ErrorKind, Options, Store};

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;

pub(crate) fn backup(args: &[String]) -> ExitCode {
    asked::run(&help::BACKUP, args, back_up)
}

pub(crate) fn restore(args: &[String]) -> ExitCode {
    asked::run(&help::RESTORE, args, put_back)
}

fn back_up(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_needed("backup")?;
    let [zip] = &rest[..] else {
        return Err(wrong("a directory and a zip: backup ./data backup.zip"));
    };
    let zip = PathBuf::from(zip);
    let key_file = asked.value("--encryption-key-file").map(PathBuf::from);
    let options = Options { background: false, encryption_key_file: key_file, ..Options::default() };
    let store = match Store::open(&dir, options) {
        Ok(store) => store,
        Err(error) if error.kind() == ErrorKind::InUse => Store::open_beside(&dir)?,
        Err(error) => return Err(error.into()),
    };
    let mut backup = store.backup();
    for name in asked.values("--include") {
        backup = backup.include(&name);
    }
    if asked.has("--include-encryption-key") {
        backup = backup.include_encryption_key();
    }
    let written = backup.write(&zip).and_then(|backup| store.close().map(|()| backup)).map_err(failed)?;
    println!(
        "● {}  backed up to {} · {} files, {}",
        dir.display(),
        zip.display(),
        written.files,
        crate::files::bytes(written.bytes)
    );
    if written.key_left_out {
        println!(
            "  the store seals values with its encryption key, which the backup leaves out: keep the key apart, or \
             include it with --include-encryption-key"
        );
    }
    Ok(())
}

fn put_back(asked: Asked) -> Done {
    let [zip, dir] = &asked.words[..] else {
        return Err(wrong("a zip and an empty directory: restore backup.zip ./data2"));
    };
    let restored = tinystore::backup::restore(zip, dir)?;
    println!("● {dir}  restored from {zip} · {} files, {}", restored.files, crate::files::bytes(restored.bytes));
    if restored.key_left_out {
        println!("  its encrypted buckets open with the store's key, which the backup left out: put it back first");
    }
    Ok(())
}
