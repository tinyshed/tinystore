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
    let options = Options { background: false, encryption_key_file: key_file.clone(), ..Options::default() };
    asked::existing(&dir)?;
    let store = match Store::open(&dir, options) {
        Ok(store) => store,
        Err(error) if error.kind() == ErrorKind::InUse => Store::open_beside(&dir)?,
        Err(error) => return Err(error.into()),
    };
    // a store opened beside its holder reads its key in its directory alone
    let key_apart = key_file.filter(|file| store.is_beside() && file.exists());
    if let Some(file) = &key_apart
        && asked.has("--include-encryption-key")
    {
        return Err(failed(format!(
            "{}: a server or a program holds it, and a backup beside it takes no key from {}: keep that key \
             apart, or back up without --include-encryption-key",
            dir.display(),
            file.display()
        )));
    }
    let mut backup = store.backup();
    for name in asked.values("--include") {
        backup = backup.include(&name);
    }
    if asked.has("--include-encryption-key") {
        backup = backup.include_encryption_key();
    }
    let written = backup.write(&zip).and_then(|backup| store.close().map(|()| backup)).map_err(failed)?;
    outln!(
        "● {}  backed up to {} · {} files, {}",
        dir.display(),
        zip.display(),
        written.files,
        crate::on_disk::bytes(written.bytes)
    )?;
    if written.key_left_out {
        // beside its holder, a key kept apart is not the backup's to take
        let or_include = if key_apart.is_some() { "" } else { ", or include it with --include-encryption-key" };
        outln!(
            "  the store seals values with its encryption key, which the backup leaves out: keep the key \
             apart{or_include}"
        )?;
    }
    Ok(())
}

fn put_back(asked: Asked) -> Done {
    let [zip, dir] = &asked.words[..] else {
        return Err(wrong("a zip and an empty directory: restore backup.zip ./data2"));
    };
    let restored = tinystore::backup::restore(zip, dir)?;
    outln!("● {dir}  restored from {zip} · {} files, {}", restored.files, crate::on_disk::bytes(restored.bytes))?;
    if restored.key_left_out {
        outln!("  its encrypted buckets open with the store's key, which the backup left out: put it back first")?;
    }
    Ok(())
}
