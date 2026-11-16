# Backup and restore

`tinystore backup` writes the whole store into one checked zip while its
program runs. `tinystore restore` puts a zip back into an empty directory,
checking every byte, or leaves nothing. `tinystore verify` checks a zip as a
restore would, and writes nothing.

```sh
tinystore backup ./data backups/2026-10-19.zip
tinystore backup ./data backups/full.zip --include app.json --include-encryption-key
tinystore verify backups/2026-10-19.zip
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
when none does. It copies the whole store as one moment while the store keeps
working (contract BAK-01). The zip is written beside its name and renamed once
synced, so a backup that fails leaves no zip.

A backup that leaves out the key that seals values says so:

```text
● ./data  backed up to backups/2026-10-19.zip · 1 files, 52 KiB
  the store seals values with its encryption key, which the backup leaves out: keep the key apart, or include it with --include-encryption-key
```

A backup beside a running server reads the key only from the store's
directory. So a key that the server keeps apart with `--encryption-key-file`
is always left out, and `--include-encryption-key` fails and writes no zip.
[The encryption key](../backup/encryption-key.md) says where to keep it.

## restore

It writes only into an empty or absent directory and checks every byte
before it names a file, or fails and leaves the directory empty, as
[What a restore checks](../backup/restore.md#what-a-restore-checks) lists
(contract BAK-02). A backup without the encryption key says so:

```text
● ./restored  restored from backups/2026-10-19.zip · 1 files, 52 KiB
  its encrypted buckets open with the store's key, which the backup left out: put it back first
```

## verify

It makes every check of a restore that reads the zip, its directory, its
manifest and each file's size, CRC-32 and SHA-256, and on Windows the names
Windows reads as devices, and writes nothing (contract BAK-06). It prints
the bytes a restore would write, so the disk can be checked before one:

```text
● backups/2026-10-19.zip  ok · 3 files · 52 KiB · checked in 0.1s
  a restore of it writes 53248 bytes
```

A file that does not match exits 1, naming it and what broke, `corrupt`.

## Every night

```sh
#!/bin/sh
set -e
zip="/backups/chat-$(date +%F).zip"
tinystore backup /srv/chat "$zip"
tinystore verify "$zip"
tinystore restore "$zip" /tmp/restore-check && rm -rf /tmp/restore-check
find /backups -name 'chat-*.zip' -mtime +30 -delete
```

## See also

- [The command line](README.md).
- [Backup](../backup/README.md): the engine's guides, restore and the key.
- [Keeping it](../../handbook/chat/keeping-it.md) in the Handbook: the same
  nightly backup from a repeating job inside the program.
