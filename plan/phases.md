# Phases

Each phase ends at a gate a check can prove. New modules wait for parity:
nothing in a phase's list is a feature the Go engine did not have, unless it
says so. Mark a task done in the commit that does it.

## 0. Decisions, vocabulary, workspace

- [x] The research read and the owner's decisions written down ([decisions.md](decisions.md)).
- [x] This plan.
- [x] The Go engines leave the tree; `e81a050` stays the reference.
- [x] AGENTS.md rewritten for the Rust repository: shape, crates, `unsafe`, features, checks; `just` in place of `task`. The skills in `.agents/skills/` still speak of Go.
- [x] A Cargo workspace with the crates of [architecture.md](architecture.md): builds and tests on Windows and in a Linux container (`rust:1.99`).
- [x] CI building and testing it on Linux, macOS and Windows, with the Bun pipe on all three: green since the branch was first pushed, on 10 October.
- [x] The kv book's first draft and one newcomer check ([api/kv.md](api/kv.md)).
- [ ] The jobs book; the kv book's second check; the owner accepts both. The kv book's second check is done; the owner accepted the jobs book on 9 October, after a survey of job libraries and a second newcomer round.
- [x] Protocol 2: kv's schema in the new vocabulary, `protocol/*.wire`, and `crates/protocol`, which writes its codecs for Rust and TypeScript and a vector of every message ([protocol.md](protocol.md)).
- [ ] The generator's Python and Go codecs, with their SDKs.

**Gate:** the jobs and kv books pass the newcomer check and the owner accepts
them; the workspace builds and tests on three operating systems.

## 1. Vertical slice: core and KV through every layer

- [x] The SQLite adapter: the pinned build, one writer with grouped commits that callers lead or that answer submitted writes through a callback, readers with idle close, the statement cache, checked migrations.
- [ ] SQLite's memory in the store's budget through `SQLITE_CONFIG_MALLOC`; today only `sqlite3_memory_used` is read.
- [x] The runtime: Store, LOCK, clock, memory budget, background work, errors, the engine registry.
- [x] The logger in the core, a `tracing` layer; the console builds alone, without a store or SQLite.
- [ ] Config in the core, to its book ([api/config.md](api/config.md)), accepted on 13 October.
  Built, slice 1: the Rust core, a declaration of serde values with `Secret`, `Required` and `Live`, files of TOML,
  YAML 1.2 and JSON with comments parsed by the core, the environment with `NAME_FILE` and `.env` files, layers in
  the order written, a live setting's changes in kv.db, checked against every handle open and kept with a version,
  one hub a process that every handle and watcher follows, `explain`, `validate`, `strict`, and the rules of one
  value as data, which the wire's declarations will carry.
  Built, slice 2: `protocol/config.wire`, config an engine of the wire under `07`, its declaration and layers as data
  with what the program read itself, a change answered with the value it made, a stream a config that a lost
  connection follows again; the Bun SDK's `store.config(name, [declaration, ...layers])` with `secret`, `required`,
  `live`, `int`, `oneOf`, `duration`, `list`, `file` and `env`, the value typed from the declaration and a change
  from its live settings, `Secret` printing as `***`, `validate` run in the program, and a config opened again
  sharing its handle and its stream.
  Built, slice 3: a store's logger's options as the config `log` in Rust and in the Bun SDK, the code's options its
  defaults and `LOG_*` a layer over them, read as they were, its level live for every logger of the store and
  `trace` the level of a logger told none; records and kv's config meet in `log_config.rs`. Every slice of the book
  is built; Python and Go come with their bindings, and the book's Later waits for the owner.
- [x] kv's buckets: any serde type, branches, ttl and idle expiry, versions, sets, pages, large clears, maintenance.
- [x] kv's counters, rate limits, quotas, once, transactions; idle renewals that never wait for a commit.
- [x] The wire's session over frames and the MessagePack profile, every vector of `testdata/wire` passing; kv's every method of protocol 2, decoded by the generated messages.
- [x] kv's pages within the agreed body, transactions as a checked batch, `once` handed over to the client, and a `CANCEL` that lets a handed run go.
- [x] The server, `tinystore serve`: stdio for a private child, a Unix socket or a named pipe that `SERVE` names and its proof, `server.stop`, a private server's clock; the Bun SDK's kv suite passes through a private child and a sidecar.
- [x] The server's TCP and TLS, rustls on ring: a client's capability is its token's line, the tokens and the certificate read before the store opens, a HELLO awaited five seconds; the kv suite passes through a remote server.
- [ ] `tinystore stop`, `status` and `logs`; an owner-only DACL on the named pipe and on `server/` on Windows.
- [x] The pipe: the C ABI and bun:ffi; the Bun SDK's kv is the book's over protocol 2, and its suite passes `embedded`.
- [ ] napi-rs, PyO3 and cgo over the same four functions; the Python and Go SDKs.

A smoke comparison, not a research round, 9 October on Windows 11 with Bun
1.4.2, FULL durability, medians of five passes: through the existing Bun SDK,
a get took 11.5 µs embedded against 37 µs through the Go sidecar of
`e81a050`, a lone set 1.48 against 1.53 ms, and 2000 sets at once ran at about
63,000 against 42,000 a second. The same 2000 sets as raw frames through
bun:ffi ran at 72,000 to 97,000, and through the pipe from Rust at 138,000.

**Gate:** every kv line of GATES.md has a Rust test and it passes; the SDK
suites for kv pass over the pipe and over a socket on three operating
systems; phase 1's measurements in [ffi.md](ffi.md#what-phase-1-measures) are
taken against Go `e81a050` and written up as a research round.

## 2. sqldb, jobs, blobs

- [x] Downloads and uploads under credit, which blobs and records need: blobs' get and upload pace both ways in protocol 2.
- [ ] sqldb to its accepted book ([api/sqldb.md](api/sqldb.md)): migrations checked at open, SQL as text, typed tables, a query builder in every SDK, `batch` and `tx` in the protocol, FTS5 and R*Tree. The Rust core, protocol 2 and the Bun SDK are built to the book, `include` among them; FTS5 and R*Tree have no test yet, and Python and Go come with their bindings.
- [ ] jobs in the new vocabulary: queues, ids, repeats, `concurrency`, `rate`, steps, watching. The Rust core, protocol 2 and the Bun SDK are built to the accepted book, a queue in the store's transaction among them; Python and Go come with their bindings.
- [ ] blobs to its accepted book ([api/blobs.md](api/blobs.md)): files inline or a file each, uploads, conditions, copies, expiry, checked reads, the scrub. The Rust core, protocol 2 and the Bun SDK are built to the book, reads over the wire as S3's, on an ETag and a range, and the disk the store keeps free, `keep_free` and `serve --keep-free`; Python and Go come with their bindings.
- [x] kv and jobs inside an sql database: `db.bucket` and `db.queue`, which commit with its rows, in the Rust core, protocol 2 and the Bun SDK.

**Gate:** their GATES.md lines pass in Rust and through every SDK; Dashbin can
start on sqldb, kv and jobs.

### Dashbin's asks the plan had lost

Each is now in a book or closed.

In a book, and built with its engine: B5, aggregates over records, B6, the
names a store holds, and B11, a live tail, which the records book has as
`count`, `top` and `aggregate`, `streams` and `fieldNames`, and `tail`
([api/records.md](api/records.md)); B2, histograms and exact quantiles, and
B6 for metrics, which the metrics book has as a timer's `buckets` and its
percentile, a gauge's percentile by rank, and `names`, `labelNames` and
`labelValues` ([api/metrics.md](api/metrics.md)).

Closed: C2, the `read` token; B10, a memo, which `once` with a short `keep`
is, across processes too; C9, values kept encrypted, which an encrypted
bucket keeps ([api/kv.md](api/kv.md#encrypted-buckets)): the Rust core,
protocol 2 and the Bun SDK, one key a store, keys by name and a change of
key left for when someone needs them; C8, an SQL answer by its columns,
`db.columns` ([api/sqldb.md](api/sqldb.md#by-columns)), once a smoke measured
it against `db.all`: the core reads rows straight into columns, protocol 2
carries int64s and float64s with a bit a `NULL`, and the Bun SDK gives
arrays of what `all` gives; typed arrays are left for when someone needs
them.

## 3. records and metrics

- [ ] The codecs: huff0 and FSE in safe Rust or a format without them, the word-based reader, the exact accumulator.
  records: a format without FSE, which saved 0.15 B an event on the corpus, left out by the owner's choice on 11 October.
- [x] records: the head, segments, paged reads, follow, retention, redaction, `durability: 'os'` by default.
  Built: streams and their `keep`, appends into fresh rows, their packing into segments and blocks, reads a page
  at a time over both, expiry. An event more than ten seconds behind the newest its stream showed on time waits in
  a late head of its own, packed apart, so that the segments of those on time stay narrow in time; what each head
  holds is a row beside it, so that finding what is ready reads a row a head. A stream's small segments merge
  four of a size in the order of their times, into the one of lowest id, and the others stay as places that say
  where their events begin among its, which `poll` will follow. A read passes a block whose segment lacks a
  name or a key it asks for, or whose bloom filters of trace ids and of fields that look like ids rule out what it
  asks; in a block it fetches it reads the columns it asks about first and builds only the events they leave. On
  65,536 requests of one stream, packed (a request id, one of sixteen routes, a status and a duration each, a host
  in their context, a trace one in sixty-four; made by a scratch test of the Go engine at `e81a050` and appended
  to both), medians of fifteen runs of ten, release, the two one after the other on this Windows machine, 11 October: an exact filter (a name, a
  route and a status; 896 found) 13.61 ms against the Go engine's 18.47, a single trace 0.18 against 0.41, an
  absent context value 1.89 against 2.95 (`TINYSTORE_READS=<the lines> cargo test --release -p tinystore --lib
  three_reads_of_a_packed_corpus -- --ignored --nocapture`). Before it built only what it keeps, compared a
  column as it keeps its values and summed its checksum sixteen bytes at a time, the exact filter took 76.24.
  Counts, a key's most frequent values and aggregates by span and key, with stats and distinct values, walk
  the same reads a page at a time and hold only their answer; `streams` and `fieldNames` say what a store
  holds, the keys of packed segments from beside them. A tail gives the events appended from now on, from
  the last megabyte the store holds while one is open, and says what it missed; a poll gives every packed
  event once from a cursor of a segment and a place in it, a merged segment's where its holder keeps them,
  and counts the events `keep` removed before it, which the file keeps thirty days. A store's logger is a
  `tracing` layer: it prints each line as the console's vectors have it, and keeps it through a buffer of 1024
  lines that the store writes every second, sooner once half of it waits, and when it closes; a line that
  finds the buffer full is dropped, counted and said once in ten minutes. A writer of another program's
  lines joins a stack trace's, a traceback's or a JSON value's lines into one event, keeps a JSON or logfmt
  line as its fields when they spell it again, and finds its level where pino, glog, Redis, log4j and their
  kin write it, through a buffer of its own. Owed now: their time and
  peak memory on the production corpus, and then the fast paths that pay — a count from the counts of the
  blocks a range covers whole, a stat from its column without the events built, `top` and `distinct` from a
  column's dictionary. A block keeps its events by columns: integers by their steps, a steady one, a
  dictionary or the value most share; text by a dictionary or through zstd, with the times it spells cut out; a
  value's quotes taken off, uuids and hex as their bytes, integers that count time as how far behind their
  events they lie. On the 1,713,543 records two production hosts' containers wrote, 178,796 of them JSON and
  logfmt lines with fields, cut as packing cuts them, a block takes 17.50 B a record against the Go engine's
  17.64: 4.26 against 4.26 for the times, 11.66 against 11.85 for the text, 1.43 against 1.39 for the fields;
  on their 1,534,747 lines of text alone, 215 B each, 17.30 against 17.51 (11 October, one run each, sizes
  alone; `TINYSTORE_DUMP=<the records> cargo test --release -p tinystore --lib a_corpus_by_its_columns --
  --ignored --nocapture`, the records dumped by the Go engine's line writer at `e81a050`). The lines took 21.71
  before the times a text spells were cut out of them and 18.02 before integers had a rice code and a steady
  step; the records took 18.13 before values were typed and integers had dictionaries. Each of the Go
  engine's codings turned off in turn on the records cost it: FSE 0.15 B a record, quoted values typed 0.26,
  uuids among them 0.16, integers counting time 0.10, dictionaries of integers 0.02, radix words 0.001, hex
  0.001, integers with values apart nothing. Radix words are not ported. FSE is left for the entropy coder
  metrics brings, one more packer then. This engine weighs an event more than the Go engine's input size and
  so cuts more blocks, 2,525 against 2,323 here, which costs 0.02 B a record.
  Closed on 11 October, slices 1 to 8 pushed: what GATES still holds to Go alone is the format's readers,
  which wait for v0.1.0, the command's printer of phase 4, and follow's cache, level masks and an
  append's trace, which the owner left as they are; FSE is left out by the owner's choice.
- [x] metrics to its accepted book ([api/metrics.md](api/metrics.md)), built as [metrics-engine.md](metrics-engine.md) says, in its eight slices: series, instruments whose totals the store keeps, packing into blocks cut where hours begin, exact aggregates and percentiles, retention, the step rule and gaps; the `metrics` crate recorder.
  Built, slice 1: `metrics.db` and its schema, series registered with their dictionary pairs and postings within
  100,000, a name's kind while it has series and its own `keep`, `ingest` whole or not at all into heads that a
  write rewrites only where it reaches, `read` of heads through postings within the read's bounds, and expiry with
  the series it empties. The codec of a run is the Go engine's, but huff0, which waits for its measurement; what
  records and metrics both pack with is the root's `compress`.
  Built, slice 2: packing at the UTC ladder, a unit from the prefix's mean step, 240 samples a block at most; clocks
  shared by digest; values the shortest of the ordinary codec, changes and a decimal grid with residuals; directories
  of version 5, the clock's id in their checksum alone; exact summaries in a fixed accumulator of 35 words that rounds
  once; publication of eight series a transaction, a savepoint each, groups taking in the one before while it holds
  no more live blocks; reads across blocks and the head; blocks expiring whole. A series is reclaimed through the
  pairs its row names, since postings, which now carry no foreign key, have no index by series.
  Built, slice 3: `aggregate` with `count`, `sum`, `avg`, `min`, `max`, `increase`, `rate`, `delta`, `first` and
  `last`, buckets on the grid from 1970, the step rule with the first step from the newest sample before the range,
  `gap`, `partial` with a flag a series keeps once retention took samples of it, groups by `by` and `without`
  answered one at a time, whole blocks from their summaries; `latest` from one chunk or one directory a series;
  `explain`, the plan the calls carry out. The ladder now reaches 200 milliseconds, and a pass goes on while a series
  holds more complete units than a group takes: a series a sample a millisecond had filled its head.
  Built, slice 4: counters, gauges, a gauge's function, timers and histograms, a slot a set of labels whose calls
  add without a lock, their totals written every 15 seconds, at `flush` and at close, 256 slots a transaction and a
  savepoint a slot, each run's zero at the write before its first call and after what its series holds, 2000 sets an
  instrument and then `__overflow`, an hour without a call ending a run, gauges by connection, the `metrics` crate's
  recorder. The wire's connections declaring series come with slice 7.
  Built, slice 5: percentiles, `Op::Percentile`, a rank kept as the decimal the query wrote; a gauge's the sample at
  its nearest rank among its group's; a timer's the bucket of its spread that holds it, as `Value::Between`, a whole
  block from its summary, each member holding the bounds whose series counted as many samples as its `+Inf`, a group
  the bounds every member holds, an idle member none, and `estimate` as Prometheus' `histogram_quantile`; a timer read
  by its own name, `count`, `sum`, `avg`, `max`, `rate` and percentiles; an ingested histogram's `le` written as
  JavaScript writes it; and, as an experiment, exponential buckets on Prometheus' native grid, an interval a series
  under the store's `__bin`, the schema lowered past 128 intervals, two resolutions merged exactly.
  Built, slice 6: what a store holds, none of it reading a sample: `names`, each with its kind, series, unit, help,
  `keep`, oldest and newest sample, `labelNames` and `labelValues`, by prefix, `limit` and `after`, from a name's
  newest time on its row, an index of its series by their oldest sample and a table of its labels' values; `describe`
  and `description`; `delete` of a series with every sample it held, whether it still reads or not; a read that meets
  bytes that no longer read suspending the series, and `damaged`.
  Built, slice 7: `protocol/metrics.wire`, every call over the wire, a read's series, the newest samples and an
  aggregate's groups in parts within the client's credit, their values as float64 bits; a program's instruments on a
  stream of their own, in batches numbered in the SDK's session that the store applies once and in order, a lost
  answer found again when the stream opens again, and a store that started again sent nothing it may have counted;
  the Bun SDK's metrics over protocol 2 through each way a store is reached, its durations, ops and an instrument's
  labels offered and checked as they are written. After the owner's review: a gauge's function a set of labels, in
  Rust and the SDK, and a flush failing with what the store refused or a restart may have counted.
  Built, slice 8: fuzz targets of every decoder past its version and checksum, `metrics_runs` and `metrics_rows`,
  their seeds in the tests, which found a directory whose count past 32 shifted out of its word and panicked; GATES'
  lines in Rust: a failed open, one state while packing goes on, no engine imported, and what the Rust engine does
  not build said so.
  Closed by the owner on 12 October, slices 1 to 8 pushed. Left for later: the port measured against Go `e81a050` on
  the same inputs and huff0's ablation, which are a research round; the format's readers, which wait for v0.1.0; and
  what GATES still holds to Go alone: an append's
  due indexes, the store's memory, expired data spending no budget, the widest exact summaries in a directory, the
  watermark's strict edge, expiry's bound a pass and adjacent buckets fuzzed.
- [ ] Formats chosen by measurement: rowid groups for metrics, records' column layout.

**Gate:** their GATES.md lines pass; records' exact filter, single trace and
absent-context reads are no slower than Go's; every decoder has a fuzz target
that ran clean.

## 4. Backup, the command line, the release

- [ ] Backup and restore, to their book ([api/backup.md](api/backup.md)); a recovery tool's `openBeside`. A backup
  leaves the store's encryption key out unless told to take it, and says so when the store holds encrypted buckets,
  since their values open with no other key.
  Built, slice 1: the Rust core, each engine's snapshot while it keeps working, `vacuum into` on a connection of its
  own and blobs' bytes linked while none is removed, an engine no handle opened copied from its files, one zip of
  them stored as they are with ZIP64's fields and a manifest of each file's size and SHA-256, the program's files
  and the key only when asked for, and `restore` into an empty directory, every byte checked or nothing left.
  Built, slice 2: `backup.write`, the zip in pieces within the client's credit and what it holds after them, for an
  admin's connection; the Bun SDK's `store.backup(path, { files, encryptionKey })` through each way a store is
  reached; `tinystore backup` of a directory no server holds and `tinystore restore`. Left: `openBeside`, and a
  backup through a running server from the command line, with its client.
- [ ] `tinystore` serve, status, logs, mcp, backup, restore, migrate, schema.
- [ ] Self-metrics.
- [ ] The thin Go SDK on cgo and a sidecar, under the root module path.
- [ ] Actions builds the libraries, executables, npm packages, wheels and Go modules for seven targets.
- [ ] The guides in docs/ rewritten from the API books; `v0.1.0`.

**Gate:** the release workflow builds and smoke-tests every artifact on every
target.

## 5. Channels and locks

- [ ] Channels to their book ([api/channels.md](api/channels.md)), proposed on 16 October: publish and subscribe, after-commit delivery, missed messages said, through the server.
  Built, slices 1 to 3: hooks a commit runs once it is in the file, the writer still held, in the SQLite adapter's
  three commits and the store's transactions, a savepoint rolled back taking its back; the Rust core, `store.channel`,
  branches and `recursive`, a log a branch listened to and each subscriber's place, missed messages counted,
  `subscribe` on the store's delivery threads, `each`, `stop`, `on_missed` and `on_error`, the store's memory shed
  oldest first, `tx.with(&channel)` in `store.tx` and `db.tx`; `protocol/channels.wire`, a publish in a database's
  transaction handed over at its commit, a subscription's batches within the client's credit, a connection's output
  bounded and its subscriptions drained in turn; the Bun SDK's `store.channel`, `publish`, `subscribe`, `each`,
  `onMissed`, `onError`, a subscription made again after a lost connection, `tx.with(channel)` in both
  transactions. Python and Go come with their bindings; the book waits for the owner.
- [ ] Locks and semaphores to their book ([api/locks.md](api/locks.md)), proposed on 16 October: leases in memory, fences from an epoch kept in kv.db, the SDK's leases across connections.
  Built, slices 1 and 2: the Rust core, `store.lock` and `store.semaphore`, `hold` and `try_hold` waiting in line
  first come first served, `max_hold` revoking a lease at its time, `lost` and `in use`, a hold refused inside its
  own hold of the key and inside a transaction, fences of the epoch kv.db keeps and a count, the store's locks'
  timer; `protocol/locks.wire`, a hold answered once its key is given, a lease renewed and let go by its id from any
  connection and asked for again by its request; the Bun SDK's `store.lock` and `store.semaphore`, `hold`,
  `tryHold`, a lease renewed at a third of its ttl, its signal aborted at its own deadline or at `maxHold`, through
  each way a store is reached. Python and Go come with their bindings; the book waits for the owner.

**Gate:** a message published in a transaction that rolls back is never
delivered; a subscriber that falls behind receives a gap event.

## 6. Indexes

- [x] Search, accepted in [its book](api/search.md) on 16 October: an index's
  words and vectors in `search.db`, summed at a weight, filters, the typo
  corrector the command line and config share, and an SQL table's index
  kept in step in its own file, in eight slices.
  Closed by the owner on 17 October, slices 1 to 8 pushed. Left for later:
  what the book's Later lists, and the round that measures what a row costs
  a table's index's commit and how long its first read of a table takes.
- [ ] Full text and geo declared in sqldb's schema, rebuilt beside the old
  index and switched when ready.
- [ ] Vectors, and compute operations, for real callers.

**Gate:** a changed index definition rebuilds while reads keep answering from
the old one.

## 7. Replication

Taken before phase 4's command line and the SDKs at the owner's word on 17
October: they wait for the system's full bounds. The model and its slices are
in [scaling.md](scaling.md#order).

- [x] Capture: the after-commit seam in the adapter's three commits, checkpoints taken over, each file's log in segments, recovery at open.
  Built: `Options::node` makes a store a node, a draft until the links; `store.open_file` opens every engine's file
  with its log in `log/<file>/`. The writer's WAL hook says where the WAL ends after a commit, and the commit's
  frames are read back while the writer is held; the hook turns SQLite's checkpoints off, and the capture's own
  run every thousand frames once the log synced, so that the log needs no sync of its own a commit. An entry is a
  commit's pages, each once, chained by SHA-256 to the one before, in segments of 16 MiB within 256 MiB a file. At
  open a torn tail is cut and the WAL hands what the log lacks; a commit the log could not take answers
  `OutcomeUnknown` and the file takes no write until it opens again; a WAL started over beside the store is logged
  as a gap. Left: the log's bound by age and the disk the store keeps free, which the links' positions decide.
- [x] Apply: a copy's pages applied whole through `sqlite_dbpage`; engines on a copy with readers alone, background work following the file.
  Built: a copy's file is marked in its log's directory and opens as a copy, whose writes run on a reader so that
  finding a name its home made answers and a write is `Invalid`; its migrations only checked, `Unavailable` until
  they arrive; no engine's background work on it. Its entries are applied in order, each in one transaction of the
  applier's own connection, page 1 last and a truncation the last row, an entry that does not follow refused, one
  held taken as nothing, the position kept after each commit and the copy going on from it. Engines read again the
  names they keep once a commit applies. SQLite is built with `SQLITE_ENABLE_DBPAGE_VTAB`. Left for the links: the
  copy's own log of what it received, which it serves on, and a file's checksum that proves a copy its home's.
- [x] The first copy: a page-exact snapshot in short reads; blobs' objects before the commit that names them.
  Built: a snapshot reads a file's pages through `sqlite_dbpage` 256 at a time, each chunk in a read transaction of
  its own, between two points of its log read with the writer held, or a copy's position held; the taker writes
  them under a name of its own with plain writes, then the entries between the two points over them, and the file
  takes the copy's name, renamed durably, only once whole, its position kept after. Blobs' files too large to keep
  inline are fetched once a copy of `blobs.db` names them, from the node it copies, in pieces of a mebibyte written
  under a name of their own and renamed whole; a read meeting one not fetched yet fails, for that short while,
  rather than wait, and a copy keeps the files its home removed. Left: a copy that fetches a file before it applies
  the commit that names it.
- [x] Links: `from` and `serve` in the core, the files a node chooses, copies of copies, reconnects, a fresh copy for a node behind the log.
  Built: `Node { from, serve, files, token }`; a node serves over TCP, a thread accepting and two a link, the
  files it writes and its whole copies, each from where the other node's copy stands; a link follows every file
  served but the node's own records and metrics, or those `files` names, takes a snapshot where it holds none or
  its position no longer follows, applies each entry and keeps it in the copy's own log, which it serves on as its
  home wrote it; a broken link is made again to the next address of `from`, from each copy's position; a silent
  one is dropped after 30 seconds, pings every five keeping it; a token compared in constant time. A file a node
  copies opens once its copy is whole, 30 seconds at most, and with no link reads the copy it holds.
  `tinystore serve` takes `--node`, `--node-listen`, `--from`, `--node-files` and `--node-token-file`; the Bun
  SDK's `open(dir, { node: { from, serve, files, token } })` makes a store in the program's own process a node
  through the pipe's options, and a server it starts through those flags. Left: TLS,
  one zstd stream a link, a file's checksum that proves a copy its home's, and files a home makes after a node
  linked, which it finds by asking every five seconds.
- [ ] Writes: a write passed to its file's home, read your writes, a local queue of operations that commute, `unavailable` after a bound.
  Built so far: kv's buckets and counters, and the names their handles open, sql's statements and batches,
  search's documents set and deleted, on the index as its home holds it, and jobs' adds, sets, updates and
  cancels, on the queue as its home holds it open, pass from a copy to its home through each node on the way,
  each node running the call through the same handle; a file a copy waits for half a second that its home never
  made, the home makes, opening the engine that keeps it;
  the answer comes back with where the file's log stood, and each node answers once its own copy holds that entry,
  ten seconds at most. The home's error comes back as it said it, a conflict a conflict. Left: blobs', records'
  and metrics' writes, a transaction, an encrypted bucket's and what a database's file keeps of kv, jobs and
  search, a schedule, rate limits, quotas and `once`, the queue of operations that commute, and writes that wait
  for a link a bound before `unavailable`.
- [ ] Several homes: the catalog, files homed on different nodes, routing, moving a home that is alive.
- [ ] Promote: epochs, `tinystore promote`, an old home back as a copy, the command line's view of the links.
- [ ] The synchronous mode: a home answering once a chosen copy holds the commit.

**Gate:** a node three links away holds its home's files commit for commit; a
get after a set sees it on any node; a promotion loses only the commits not
shipped, counted, and the old home cannot write again.
