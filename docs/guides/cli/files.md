# files

`tinystore files` puts, gets, lists and deletes a store's files from a shell,
with no SDK, and changes a cache's ceiling or frees its room. `put`,
`delete`, `set-max-size` and `trim` write through a server, as
[every write](README.md#who-writes) of the command line does. `get` and `list`
read beside the program or server that holds the directory, and renew no
file nor record a use of it.

```sh
tinystore files ./data put avatars users/42/avatar.png ./avatar.png
tinystore files ./data get avatars users/42/avatar.png ./avatar.png
tinystore files ./data list avatars users/42/
tinystore files ./data delete avatars users/42/avatar.png
tinystore files ./data set-max-size pages 1GiB --if-current 512MiB
tinystore files ./data trim pages --to 300MiB
```

## Put a file

```sh
tinystore files ./data put avatars users/42/avatar.png ./avatar.png
pg_dump app | tinystore files ./data put dumps app.sql -
```

```text
put users/42/avatar.png in files avatars, 12.4 KiB image/png
```

- The words are the set, the file's path in it, and the local file. `-` reads
  the bytes from stdin.
- It replaces whatever the path held, so the same command sent again leaves
  the same file.
- `--type image/png` gives the content type. Without it, a common extension
  gives one, `.png`, `.jpg`, `.json`, `.txt`, `.html` and a few more, and any
  other file is bytes, `application/octet-stream`.
- A file of any size goes: one past 2 MiB is sent in pieces.

## Get a file

```sh
tinystore files ./data get avatars users/42/avatar.png ./avatar.png
```

```text
wrote ./avatar.png, 12.4 KiB
```

- It writes to the path it is given, never to stdout, since PowerShell's `>`
  writes a program's bytes back as text.
- A file already at the path is refused unless `--force`.
- It writes beside the path and renames once the file is whole, and the
  bytes are checked against their SHA-256 as they are read, so a failure
  leaves no half-written file.

## List a set

```sh
tinystore files ./data list avatars
tinystore files ./data list avatars users/42/
```

```text
users/42/avatar.png   12.4 KiB  26 Oct 14:32:07  image/png
users/43/avatar.png    9.1 KiB  26 Oct 14:35:51  image/png
```

- A second word lists the paths that start with it.
- A page holds 100 files unless `--limit` says, 1000 at most, and `--after`
  reads the next, as [Pages](README.md#pages) says.
- `--json` gives a line per file, with its size, ETag, type and times whole
  and UTC.

## Delete a file

```sh
tinystore files ./data delete avatars users/42/avatar.png
```

```text
deleted users/42/avatar.png from files avatars
```

A path that holds no file says `files avatars holds no users/42/avatar.png`
and exits 0.

## Change a set's ceiling

```sh
tinystore files ./data set-max-size pages 1GiB --if-current 512MiB
tinystore files ./data set-max-size pages none
```

```text
files pages holds 1.0 GiB at most
```

- The words are the set and its new ceiling, in bytes or with `KiB`, `MiB`
  or `GiB`; `none` removes it.
- `--if-current` changes it only while the set keeps that ceiling; another
  fails `conflict`, so two deploys never undo each other.
- `--when-full evict` or `refuse` says what a write past it does; without it
  the set keeps what it does.
- A lower ceiling removes the files used least recently at once, unless the
  set refuses. [Caching](../blobs/caching.md) has the rules.

## Free room now

```sh
tinystore files ./data trim pages --to 300MiB
```

```text
trimmed files pages: 184 files removed, 210 MiB freed
```

It removes files in the eviction's order until the set holds at most
`--to`, and leaves the ceiling where it was. A set whose ceiling refuses is
refused.

## See also

- [blobs](../blobs/README.md): files from a program, uploads, conditions and
  expiry.
