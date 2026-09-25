# Log templates on Loghub samples — 2026-09-24

Whether splitting text log lines into a template and its variables, as
`docs/architecture.md` plans for records, stores them smaller than zstd over
the plain lines. Payload and file size only; no speed, no memory, no search.

## Environment

- Commit `db34377` on `research/records`, Go 1.27.1 linux/amd64,
  `modernc.org/sqlite` v1.59.0, `klauspost/compress` zstd at `SpeedDefault`.
- Cloud container, Intel Xeon @ 2.80GHz, 4 vCPU.
- Corpus: the ten `*_2k.log` samples of Loghub at `dd61d09`, 2 000 lines each,
  pinned by `bench/loghub-sha256.txt`.

```sh
bench/run-loghub.sh <corpus>
```

## What was compared

- **zstd**: blocks of 1 000 lines, joined by newlines, one zstd frame each.
- **templates**: a small Drain — lines of the same token count join a template
  when at least half of the words agree, a token holding a digit is a variable
  from the start, differing positions become `<*>`. Mining runs over the whole
  file first, then each block stores varint template ids and one column per
  template variable, NUL-separated, under one zstd frame. The template
  dictionary, zstd-compressed, is counted once per dataset.
- Lines split on single spaces, so every line is decoded and compared byte for
  byte; the round fails if one differs.
- File: the blocks (and the dictionary) as rows of a vacuumed SQLite file.

## Result, bytes a line

| dataset | templates | raw | zstd | templates | zstd, file | templates, file |
|---|---:|---:|---:|---:|---:|---:|
| Android | 73 | 139.5 | 12.3 | 14.6 | 18.4 | 20.5 |
| Apache | 6 | 85.6 | 5.6 | 3.4 | 8.2 | 8.2 |
| BGL | 49 | 158.6 | 31.5 | 26.2 | 34.8 | 32.8 |
| HDFS | 13 | 143.9 | 26.9 | 24.0 | 32.8 | 26.6 |
| Hadoop | 68 | 192.5 | 9.1 | 7.7 | 12.3 | 14.3 |
| Linux | 59 | 108.2 | 7.5 | 7.2 | 10.2 | 14.3 |
| OpenSSH | 22 | 112.6 | 8.5 | 5.6 | 14.3 | 12.3 |
| Spark | 22 | 98.1 | 7.3 | 5.5 | 12.3 | 8.2 |
| Thunderbird | 24 | 162.6 | 15.9 | 15.9 | 18.4 | 20.5 |
| Zookeeper | 23 | 139.9 | 12.6 | 11.8 | 18.4 | 18.4 |

Summed over the ten, the template payload is 11 % smaller than zstd's
(121.9 against 137.2 bytes a line).

## Reading

- zstd over 1 000 plain lines already takes these logs from 86–193 bytes a
  line to 6–32: it finds the repetition templates name. Templates with
  untyped string columns add 0–40 %, and lose on Android (+19 %), whose 73
  templates cost more as a dictionary than they save.
- Where the lines are left is the variables: timestamps, block ids and IPs
  stored as text. Typing them — a timestamp as a delta, a number as a varint —
  is what the CLP line of work credits for most of its gain, and it is not in
  this round.
- The file columns are not a result. Two blocks and a dictionary per dataset
  are a handful of 4 KiB pages, and one page is two bytes a line here; the
  file sizes differ by page rounding, not by layout.

## Not measured

Typed variable columns, the full-size Loghub 2.0 datasets, online mining where
templates change while blocks are written, lines holding NUL, and the cost of
mining and decoding.
