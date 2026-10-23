# Backup and restore

`tinystore backup` writes the whole store into one checked zip while its
program runs. `tinystore restore` puts a zip back into an empty directory,
checking every byte, or leaves nothing.

```sh
tinystore backup ./data backups/2026-10-19.zip
tinystore backup ./data backups/full.zip --include app.json --include-encryption-key
tinystore restore backups/2026-10-19.zip ./restored
```

```text
● ./restored  restored from backups/2026-10-19.zip · 3 files, 52 KiB
```

## backup

| Flag                           | Does                                                         |
|--------------------------------|--------------------------------------------------------------|
| `--include <name>`             | a file of the program's own in the directory; again for more |
| `--include-encryption-key`     | the store's key too, which a backup leaves out by default    |
| `--encryption-key-file <file>` | where the store's key is, when not in the directory          |

It reads beside a server or a program that holds the directory, and alone
when none does. Each engine is copied as one moment of itself while it keeps
working, and the zip is written beside its name and renamed once synced: a
backup that fails leaves no zip.

A backup beside a running server reads the key only from the store's
directory. If the server keeps its key apart with `--encryption-key-file`, the
backup leaves that key out and says so, and so does a restore of it. With
`--include-encryption-key` it fails and writes no zip, so keep a copy of that
key apart yourself:

```text
● ./data  backed up to backups/2026-10-19.zip · 1 files, 52 KiB
  the store seals values with its encryption key, which the backup leaves out: keep the key apart
```

## restore

It writes only into an empty or absent directory, holding its `LOCK`
meanwhile, and checks each file's size, SHA-256 and CRC-32 before it names
it. A file that does not match fails the restore `corrupt` and leaves the
directory empty. A backup without the encryption key says so:

```text
● ./restored  restored from backups/2026-10-19.zip · 1 files, 52 KiB
  its encrypted buckets open with the store's key, which the backup left out: put it back first
```

## Every night

```sh
#!/bin/sh
set -e
zip="/backups/chat-$(date +%F).zip"
tinystore backup /srv/chat "$zip"
tinystore restore "$zip" /tmp/restore-check && rm -rf /tmp/restore-check
find /backups -name 'chat-*.zip' -mtime +30 -delete
```

A restore into a scratch directory checks every byte of last night's zip.
[Keeping it](../../handbook/chat/keeping-it.md) in the Handbook does the same
from a repeating job inside the program.

## See also

- [The command line](README.md).
- [Backup](../backup/README.md): the engine's guides, restore and the key.
