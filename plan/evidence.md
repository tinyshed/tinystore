# Evidence

What the decisions stand on. Every number here comes from a dated round in
[tinyshed/research](https://github.com/tinyshed/research/tree/main/tinystore/reports),
measured against TinyStore `e307c48`, unless it says otherwise. The
[synthesis](https://github.com/tinyshed/research/blob/main/tinystore/reports/rust-research-summary-2026-10-08.md)
is the place to start.

## Go against Rust, same session, same data

Go is the public Go API; Rust is a prototype (rusqlite 0.40.1 on a static
SQLite 3.53.4). Medians of five or six balanced passes.

| Operation                                | Go          | Rust            | Ratio      | Round                           |
|------------------------------------------|-------------|-----------------|------------|---------------------------------|
| kv get                                   | 6.434 µs    | 3.614 µs        | 1.8×       | kv-optimization-2026-10-09      |
| kv has                                   | 5.874 µs    | 3.392 µs        | 1.7×       | kv-optimization-2026-10-09      |
| kv scan, 100 keys                        | 93.255 µs   | 30.192 µs       | 3.1×       | kv-optimization-2026-10-09      |
| SQL point read                           | 4.825 µs    | 1.855 µs        | 2.6×       | sqlite-native-2026-10-07        |
| SQL range, 240 rows                      | 107.811 µs  | 41.864 µs       | 2.6×       | sqlite-native-2026-10-07        |
| SQL count and sum, 240 rows              | 43.469 µs   | 16.944 µs       | 2.6×       | sqlite-native-2026-10-07        |
| metrics read16 (serial; PGO)             | 5.253 ms    | 1.975; 1.657 ms | 2.7×; 3.2× | metrics-deep-2026-10-08         |
| metrics cut avg (serial; PGO)            | 2.540 ms    | 0.730; 0.558 ms | 3.5×; 4.6× | metrics-deep-2026-10-08         |
| metrics head point read                  | 0.0559 ms   | 0.0164 ms       | 3.4×       | metrics-native-2026-10-08       |
| metrics ingest, scrape of 100            | 4.943 ms    | 2.267 ms        | 2.2×       | metrics-native-2026-10-08       |
| metrics maintenance (sealing)            | 84.77 ms    | 33.95 ms        | 2.5×       | metrics-native-2026-10-08       |
| records full read, sealed                | 8.005 ms    | 1.258 ms        | 6.4×       | records-optimization-2026-10-09 |
| records page, sealed                     | 1.445 ms    | 0.577 ms        | 2.5×       | records-optimization-2026-10-09 |
| records exact filter, sealed             | 1.294 ms    | 1.517 ms        | Go 1.2×    | records-optimization-2026-10-09 |
| records one trace, sealed                | 0.265 ms    | 0.362 ms        | Go 1.4×    | records-optimization-2026-10-09 |
| records absent context, sealed           | 0.244 ms    | 0.365 ms        | Go 1.5×    | records-optimization-2026-10-09 |
| kv set, FULL                             | 3442 µs     | 3165 µs         | 1.1×       | kv-optimization-2026-10-09      |
| SQL one update and FULL commit           | 188.5 µs    | 152.5 µs        | 1.2×       | sqlite-native-2026-10-07        |
| SQL 64 updates and FULL commit           | 1039 µs     | 724 µs          | 1.4×       | sqlite-native-2026-10-07        |
| records, 64 results of 4096 records held | 272–289 MiB | 141.5 MiB       | 1.9× less  | records-optimization-2026-10-09 |
| metrics, 16 read16 results held          | 48.97 MiB   | 23.39 MiB       | 2.1× less  | metrics-native-2026-10-08       |

The machines were a Ryzen 7 7700 under Docker Desktop and WSL2 (kv, records)
and a shared AMD EPYC 9V74 VM with two CPUs (metrics, SQL); Go 1.27.1, Rust
1.99.0, WAL with `synchronous=FULL`, synchronous single calls, warm caches.

## What follows from it

- **Durable writes are bound by the disk.** 98.2–99.4 % of a kv set's time is
  inside the FULL commit. No language changes that; group commit and the
  durability mode do ([decisions.md](decisions.md), entry 8).
- **Part of the gain is the algorithm, not the language.** The word-based
  residual reader made the Go decoder 8.9× faster too. The port takes the best
  algorithm found, not the Go code.
- **Go still wins three selective records reads.** The prototype parses a
  whole raw-attribute column before testing values and validates every field
  of a retained chunk; phase 3's gate is that these are no slower than Go.
- **Held memory halves through ownership.** A records result that keeps one
  decompressed buffer and ranges into it, instead of two strings a field, took
  335 MiB to 141.5 MiB; detaching it again gave 291 MiB, Go's level.
- **Native SQLite needs `HAVE_FDATASYNC` on Linux.** Without it the Unix VFS
  calls `fsync` where Go's translated SQLite calls `fdatasync`, and a 64 KiB
  set took 2637 µs native against 1737 µs in Go; the flag alone made native
  1.5–1.6× faster on 4 and 64 KiB sets.
- **Native SQLite needs `SQLITE_ENABLE_MEMORY_MANAGEMENT` undefined.**
  libsqlite3-sys's bundled build defines it, and with it every connection of
  the process shares one page cache behind one mutex, taken for each page a
  read fetches and lets go. Half the processor's time at sixteen callers went
  to waking and parking threads on it. The same commit without the flag reads
  a key 4.6× as fast at 8 callers, 9.3× at 16 and 7.9× at 64, which is 3.3–5.2×
  Go where it had been 0.42–0.82×; one caller is as before, and sixteen
  readers hold some 20 MiB more, a cache each (rust-slice-2026-10-10).
- **A commit's leader does nothing a caller at a time.** One thread woke each
  of 64 callers after their commit, and where a wake lands on a sleeping
  processor it costs tens of microseconds: 45% of the processor's samples
  were in `futex_wake`, the callers came back over as long as a commit takes,
  and a commit carried 34 writes of 64 where Go's carried 47. The leader now
  wakes two, each caller the firsts of the halves it was given, and no write
  wakes a gathering leader but the one it waits for: a commit carries 46, and
  64 callers write 1.30–1.53× as fast, 1.02–1.12× Go where they had been
  0.67–0.84×, one to four as before (rust-slice-2026-10-10).
- **SQLite's own mutexes park a thread the moment one is held.** Every read
  takes the mutex of its file's WAL index twice, and two threads reading one
  file read 0.7× of what one did. With the core's mutexes, which spin first,
  and a readers' pool that wakes nobody for nothing, two readers read 3.7×
  through kv and 4.5× through sql, and four 2.3× and 3.0×. On a KVM server,
  where a wake costs less, the same change gives 1.31× and 1.08×
  (rust-slice-2026-10-10).
- **A connection wakes one thread for a crowd of calls, not one a call.**
  Every call handed to the store's threads woke one, and a point read took
  its turn on the thread that read it. With one wake that the woken thread
  passes on, and a crowd's reads on those threads, 64 calls in flight through
  Bun are 1.04–1.28× Go's sidecar where they were 0.5–0.95×: kv get 1.4–1.5×
  of before, sql point read 1.6–2.5×, jobs add 1.3× (rust-slice-2026-10-10).

## Not measured

- An idle store in Rust. Go's, measured 2026-10-04 in a scratch probe (not a
  research round; Alpine under Docker Desktop, VmRSS three seconds after work
  and a GC): 5.7 MiB for the bare store, 14.9 MiB with sqldb, kv, jobs, records
  and metrics open, of which 8.8 MiB are file-backed pages of the translated
  SQLite code, and a 2.74 MiB zstd encoder held by records after its first
  flush.
- Concurrent load, a cold disk, Windows and macOS, a complete records port,
  records and metrics writes, lifecycle, cancellation, recovery.
- The prototypes have no admission, memory budget, cancellation, group commit
  or reader pool; production adds their costs.

## The sidecar's cost

From [rpc-server-2026-09-29](https://github.com/tinyshed/research/blob/main/tinystore/reports/rpc-server-2026-09-29.md),
one kv get in flight: 39–61 µs over a Windows named pipe, 41–86 µs over
stdio, 64–110 µs over TCP, 185–215 µs over a Unix socket in a Linux container
under Docker Desktop; an embedded Go get took 6.7–6.9 µs. This is the gap the
byte pipe closes for Bun, Node, Python and Go.

## The prototypes

Twelve directories in research, about 45,000 lines of Rust with much of it
copied between them, 154 tests. They are the evidence for algorithms and the
method of measuring; their code is not carried over.

| Take                                     | What it gave                                                | Where in research                                        |
|------------------------------------------|-------------------------------------------------------------|----------------------------------------------------------|
| Word-based residual reader and writer    | 17-bit decode 4.06 → 0.46 µs in Go; up to 17.6× at width 64 | `metrics-max-bench/rust/src/codec/residuals.rs`          |
| 35-word exact accumulator for sum, avg   | cut-block sum 2.02 → 0.82 ms, exact merges kept             | `metrics-opt-bench/rust/src/exact.rs`                    |
| Regular clocks filled, payloads borrowed | read16 2.57 → 2.19 ms                                       | `metrics-max-bench/rust/src/codec/sealed.rs`             |
| One owned buffer per records result      | full read 2.9×, held memory 335 → 141.5 MiB                 | `records-opt-bench/rust/src/optimized.rs`                |
| Rowid groups for metrics                 | file −21.4 % on Alibaba, +0.45 % on TSBS                    | `metrics-layout-bench`, report metrics-layout-2026-10-09 |
| `HAVE_FDATASYNC` in the SQLite build     | 64 KiB set 1.6×                                             | `kv-opt-bench`, report kv-optimization-2026-10-09        |
| No shared page cache in the SQLite build | a read by 16 callers 9.3×, 5.2× Go                          | `slice-bench`, report rust-slice-2026-10-10              |
| A commit's callers wake one another      | 64 writers 1.30–1.53×, 1.02–1.12× Go                        | `slice-bench`, report rust-slice-2026-10-10              |
| Mutexes for SQLite that spin first       | two readers 3.7–4.5×, four 2.3–3.0×                         | `slice-bench`, report rust-slice-2026-10-10              |
| One wake for a crowd on a connection     | Bun, 64 in flight: 1.3–2.5×, past Go's sidecar              | `slice-bench`, report rust-slice-2026-10-10              |

Measured and not worth it: an owned batch arena and lookaside (read16 0.96×),
a no-result kv set (0.99–1.00×), immutable payload packs (sparse reads 8–33 %
slower, dead bodies kept until the last reference dies).

What is wrong with the code, and why none of it is copied:

- the metrics codec calls zstd's private `HUF_compress1X_repeat` through
  `extern "C"`; the Go codec uses klauspost's huff0 and FSE, so the port needs
  its own entropy coders in safe Rust, or a format without them;
- about a hundred `unsafe` blocks without a SAFETY comment, and no fuzz target;
- ROLLBACK errors are discarded and a connection is not recovered after one;
- kv's set-if-absent reads outside its transaction;
- errors are strings; process-wide switches change how a decoder behaves; the
  metrics codec exists in six directories in three versions;
- 125 narrowing casts in one codec, 52 functions over 100 lines, hard-coded
  `/work` and `/src` paths in the runners.

## The byte pipe's neighbours

- **Prisma** ran its Rust query engine first as a separate binary, then as a
  Node-API library, and in v7 (2025) replaced it with TypeScript and WASM. The
  cost was converting data across the Rust–JavaScript boundary: a `findMany`
  of 25,000 records went from about 185 ms to 55 ms, and the bundle from 14 MB
  to 1.6 MB, by Prisma's own numbers
  ([without Rust](https://www.prisma.io/blog/prisma-orm-without-rust-latest-performance-benchmarks),
  [from Rust to TypeScript](https://www.prisma.io/blog/from-rust-to-typescript-a-new-chapter-for-prisma-orm)).
  Their engine sat between JavaScript and a remote Postgres, a hop they could
  remove; TinyStore's core is the database, and its data crosses once.
- **bun:ffi** costs a few nanoseconds a call: Bun's own figure is 4.67 ns for
  an empty function, and a third-party run on an M1 measured Bun 12.5 ns against
  Node-API 11.5 ns. Bun's documentation calls bun:ffi experimental and Node-API
  the stable way, and threadsafe `JSCallback` experimental too
  ([bun:ffi](https://bun.com/docs/runtime/ffi)).

## Crates and their traps

Checked 9 October 2026 on crates.io and in each project's sources.

- rusqlite 0.40.2 with libsqlite3-sys 0.38.2 bundles SQLite 3.53 with FTS5,
  R*Tree and JSON already enabled; GEOPOLY and the math functions are added
  through `LIBSQLITE3_FLAGS`. `preupdate_hook` and `session` need bindgen and
  libclang. There are no wrappers for `sqlite3_config`, `sqlite3_status64`,
  `sqlite3_db_status` or the heap limits: the adapter calls them through the
  `ffi` module.
- `sqlite3_config` works only before SQLite initializes; the hard heap limit is
  process-wide and turns on memory statistics, a mutex per allocation.
- The bundled build sets `SQLITE_DEFAULT_FOREIGN_KEYS=1`.
- On macOS a sync reaches the disk only with `PRAGMA fullfsync=ON`.
- `update_hook` skips WITHOUT ROWID tables, which kv and metrics use, and
  `commit_hook` runs before the commit is final: engine events are emitted by
  the engine's own code after the commit returns.
- zstd 0.14 (libzstd 1.5.7): a decoder accepts windows up to 128 MB unless
  `window_log_max` is set.
- `std::fs::File::lock` is stable since Rust 1.89, so the directory's LOCK
  needs no crate.
- Miri does not run C, so it checks the codecs and not the SQLite adapter.
