# Contracts

What TinyStore promises the program that opens it. A change that breaks one of
these is a regression, whatever the tests say; a change that means to move one
changes this file in the same commit, and says so in its subject.

A contract describes what a program can observe, never how it is built: no
SQLite, no codec, no function name. How each engine is built is in its own
module; the calls a program writes are in the guides.

**MUST** and **MUST NOT** are absolute. **MAY** says what a program cannot rely
on either way. Each contract has an id that never changes meaning; a contract
removed leaves its id unused. A test that keeps a contract names its id in a
comment, `// contract: KV-04`, and `just contracts` fails when a contract that
says MUST is named by no test, or a comment names an id this file lacks.

Until `1.0`, a minor release may still change names, and its notes say which.
These contracts are what does not.

## 1. The store

- **STORE-01** One store MUST hold a directory: a second open of it in another process MUST be refused or join as a guest that takes no lock, makes no file it does not hold, and refuses writes.
- **STORE-02** Each engine MUST keep its own file in the store's directory, and no engine MUST wait on another engine's writer.
- **STORE-03** Engines MUST close last opened first, each once, when the store closes or its last handle is dropped; a handle that outlives its store MUST fail `closed`.
- **STORE-04** A store dropped without `close` MUST close its engines, keep what they held, and let go of its directory.
- **STORE-05** The store's memory MUST stay within the budget its program gives, and a store given none MUST bound each call alone, never reading a container's limit on its own; work that holds a writer MUST NOT wait for memory, and a large reservation MUST NOT be passed over forever by small ones.
- **STORE-06** Background work MUST stop with `background: false`; a failure of it MUST be logged once and its recovery once, never a line a run.
- **STORE-07** A program that links some engines MUST NOT pay for the others: an engine is a cargo feature, and a program that only logs links no store and no SQLite.
- **STORE-08** A time an engine keeps MUST lie inside `[now − retention, now + skew]`, `now` read once a call from the store's clock; outside it MUST be `too_old` or `too_new`, naming the item.
- **STORE-09** A read whose caller cancelled it, or whose timeout passed, MUST stop where it is, waiting or running, free at once what it held and fail `cancelled`; a cancel MUST NOT stop another caller's read, nor a write that runs.
- **STORE-10** An awaited call MUST NOT commit, sync the disk or wait on the thread that polls its future, whatever it waits for: a reader, memory, a file's writer, a lock, a run under way, another node's answer. That work runs on the store's own threads, or waits on no thread at all.

## 2. Durability and transactions

- **DUR-01** A write that returned success MUST survive an abrupt exit of the process, at the durability the store or the database was opened with: unless told, `full` for kv, jobs, files, search and every database, `os` for records and metrics.
- **DUR-02** A transaction MUST commit all its changes or leave none observable; a call that fails inside it MUST leave it as it was before the call, unless that call ended the whole transaction, as a statement's `or rollback`, a trigger's `raise(rollback)` or a failing disk does: then the transaction MUST leave nothing observable, and every later call and its commit MUST fail `invalid`.
- **DUR-03** Writes grouped into one commit MUST each fail alone: one refused write MUST NOT fail its neighbours, unless it ended the whole commit, as a statement's `or rollback`, a trigger's `raise(rollback)` or a failing disk does: it then fails with its own error, the writes before it in that commit that had not failed MUST fail `unavailable` and write nothing, and those after it MUST run in the next commit.
- **DUR-04** A write whose caller left before its turn MUST write nothing; one that has started MUST finish with its group. An awaited write's turn is its future's first poll, or its read's finding that it writes (DUR-11), so one dropped after it still commits.
- **DUR-05** A write whose outcome cannot be established MUST answer `outcome_unknown`, never success and never an ordinary failure.
- **DUR-06** Atomicity MUST hold within one file. A write across two files is not atomic, unless the program opened one engine inside another's file (`db.bucket`, `db.queue`, `db.search_index`), which then commits with that file's rows.
- **DUR-07** A call on a file from inside that file's own transaction, on the same thread, MUST fail `invalid` rather than wait for itself or miss what the transaction wrote.
- **DUR-08** A reader MUST see one consistent snapshot: every commit whole, and each item once, while packing, merging or expiry go on.
- **DUR-09** Maintenance — packing, merging, purging, expiry — MUST NOT change an answer, and a crash between its steps MUST leave the last commit and go on from it.
- **DUR-10** A long read or transaction MUST NOT fail a grouped write behind it.
- **DUR-11** An awaited write whose future was polled MUST commit with its group whether or not its future is still awaited, and one whose future was dropped before its first poll MUST write nothing. A statement the database does not know writes, as one with `returning` is until it first runs, has its turn when its read finds that it writes: from then on it MUST commit as a polled write does.

## 3. Errors and limits

- **ERR-01** Every failure MUST be one `Error` whose `ErrorKind` means the same in every engine, on the wire and in every SDK.
- **ERR-02** An error MUST name what failed — engine, item, key — and keep its cause.
- **ERR-03** A key already held MUST be `conflict`; a request that breaks a rule `invalid`; bytes that no longer read `corrupt`, naming the invariant that broke.
- **ERR-04** A decoder given any bytes MUST answer what they hold or fail `corrupt`, and MUST NOT panic.
- **ERR-05** A call past a bound MUST fail `limit`, naming the bound, what the call wanted and the bound's value, and MUST NOT return a smaller answer.
- **ERR-06** A call that fails `limit` or `invalid` MUST change nothing.
- **ERR-07** Damage to one item MUST NOT stop its neighbours: a damaged series, segment, head or row is named, listed by `damaged` and removable by `drop`, and work goes past it.

## 4. Engines

### KV

- **KV-01** A value MUST come back as it went in, bit for bit, for every serde type; a value read as another kind than it was written MUST be `corrupt`, never a plausible wrong value.
- **KV-02** A row MUST be the same bytes whether Rust, Bun, Go or Python wrote it.
- **KV-03** An expired or cleared key MUST be absent to every operation.
- **KV-04** A counter's overflow MUST be refused and leave its value as it was.
- **KV-05** A version MUST never repeat, and a write on a stale version MUST be `conflict`.
- **KV-06** A clear MUST empty a branch and every branch under it, and MUST NOT bring back a counter waiting to flush.
- **KV-07** A key with idle expiry read in its last minute MUST be renewed; a renewal MUST NOT extend a newer incarnation of the key.
- **KV-08** Counters kept in memory MUST reach the file at a flush, at close and when the store is dropped unclosed; `LoseAtMost` MUST lose no more than its interval.
- **KV-09** A rate limit MUST let its burst through and then its rate, racing callers included.
- **KV-10** A quota MUST count a use in every window or in none; `peek` MUST count nothing, and a refund MUST NOT go below zero.
- **KV-11** `once` MUST run a key's function once and keep its answer; other callers MUST wait for it and get its answer, one too large to keep included, and a run that failed or was abandoned MUST let the next caller run.
- **KV-12** Of two transactions taking the last item, one MUST get it.
- **KV-13** A page MUST NOT hold a snapshot between pages, and MUST end before the value that passes its bytes.

### Config

- **CFG-01** A config MUST be its declaration, then each layer in the order written, each overriding what came before, then a live setting's kept change over them all, across a restart.
- **CFG-02** Only a setting marked live MAY change while the program runs.
- **CFG-03** A required setting missing, a value that does not read, or a key that names no setting MUST keep the config from opening, every problem said at once, each naming the layer that gave it and the setting it may have meant.
- **CFG-04** A change MUST reach every handle and watcher before it returns, versions in order; `if_version` MUST refuse a change made since.
- **CFG-05** A change that `validate` or any open handle's declaration refuses MUST keep nothing.
- **CFG-06** A secret MUST print as `***` wherever it goes — debug, logs, errors, JSON — and only `reveal` gives it; a secret MUST NOT be changed at run time.
- **CFG-07** A kept change that no longer fits the declaration MUST be ignored and said, and applied again once it fits; `strict` MUST refuse to open instead.
- **CFG-08** Files, variables, `NAME_FILE` and `.env` MUST read the same in every language.

### Locks

- **LOCK-01** A key MUST be held by one holder at a time, and given to waiters in the order they came.
- **LOCK-02** Each lease's fence MUST be larger than every earlier lease's fence of the store, across restarts and whatever the clock does.
- **LOCK-03** A hold past `max_hold` MUST lose its lease and fail `lost`, even if its function succeeded.
- **LOCK-04** A hold inside its own hold of the key, or inside a transaction, MUST be refused at once.
- **LOCK-05** A lease over a connection MUST last its ttl from its last renewal; one that ran out MUST NOT be renewed nor release a key another holds.
- **LOCK-06** A panic, a cancel or the store's close MUST let the key go and answer every waiter.

### Channels

- **CHAN-01** A subscriber MUST receive what is published after it subscribes, in order, and nothing after it stops.
- **CHAN-02** A publisher MUST NOT wait for a subscriber; a subscriber that falls behind MUST be told exactly how many messages it missed before the next.
- **CHAN-03** A message published inside a transaction MUST be handed over after its commit, in commit order, and never after a rollback.
- **CHAN-04** A handler that fails or a message that does not decode MUST be reported, and the next message MUST follow.

### Jobs

- **JOB-01** A job MUST NOT run before its time, and jobs due MUST start in the order of their time.
- **JOB-02** A job added in a transaction or a batch MUST commit with its rows or not at all, and a worker MUST NOT see it before the commit.
- **JOB-03** An id MUST name one job; `add` MUST add only to a free id, and `set` MUST make the id's job its value whatever it was.
- **JOB-04** `update` MUST change only a job that has not started; `cancel` MUST tell a running job's handler to stop.
- **JOB-05** A job whose lease ended MUST run again, and its stale lease MUST settle nothing.
- **JOB-06** A step MUST run once in a run, across its attempts.
- **JOB-07** A failure MUST be retried with growing waits and then fail for good; a snooze MUST count no attempt; a job that kills its process MUST fail after its attempts.
- **JOB-08** A repeating job MUST NOT overlap itself nor pile up; a cron MUST keep its zone across daylight saving and MUST be given one.
- **JOB-09** Concurrency in all, by group, and rate limits MUST hold across workers and claims; a full group MUST NOT hold back another.
- **JOB-10** `maxWaiting` MUST hold under concurrent adds.
- **JOB-11** A stopping worker MUST wait for its handlers and give back, uncounted, the jobs it had not started.
- **JOB-12** A value that no longer decodes MUST fail its job, not its queue.

### SQL

- **SQL-01** A read MUST run on a connection that cannot write; a write with `returning` MUST run on the writer and answer once its commit is durable.
- **SQL-02** A statement MUST NOT change its connection: attaching a file, beginning or ending a transaction, or setting a pragma MUST be `invalid`, for every caller and in a migration too.
- **SQL-03** Migrations MUST apply in the order of their numbers; an applied migration that changed, was renamed, went missing or was put before another MUST stop the database from opening.
- **SQL-04** A migration that leaves a row without its parent MUST be refused.
- **SQL-05** A value MUST come back as it went in; a value SQLite would change MUST be refused; a placeholder without its value MUST be refused, not read as `NULL`.
- **SQL-06** An integer MUST travel whole in every SDK, as a bigint past what a number holds.
- **SQL-07** A transaction MUST see its own writes, roll back on an error, and roll back past five seconds, an awaited one's counted from its future's first poll.
- **SQL-08** A broken constraint MUST name it: a unique key `conflict`, anything else `invalid`, with its table and columns or its name.
- **SQL-09** An update or a delete with an empty condition MUST be refused.
- **SQL-10** Every SDK's query builder MUST write the same statement from the same steps.
- **SQL-11** A database's name MUST NOT leave `sql/` nor meet another's on a file system that folds case.

### Blobs

- **BLOB-01** A file MUST appear whole at its commit or not at all; an upload that does not commit, is cancelled, or disagrees with its size MUST leave nothing.
- **BLOB-02** A whole read MUST be checked against the file's SHA-256 and fail `corrupt` before its end if a byte changed; the scrub MUST find each change once.
- **BLOB-03** A reader MUST keep what it opened through a replace or a delete, on every platform.
- **BLOB-04** Of two writes that read one ETag, one MUST conflict unless the first put back the very bytes it read, since an ETag names a file's bytes alone; `create` MUST write once.
- **BLOB-05** A copy MUST share the bytes and outlive its source.
- **BLOB-06** Opening the store MUST remove what a process that died left half written.
- **BLOB-07** A file past `max_file_size`, or one that would leave the disk less free than the store keeps, MUST be refused and leave nothing.
- **BLOB-08** A path MUST be its own bytes on every file system.
- **BLOB-09** Memory MUST NOT follow a file's size.

### Records

- **REC-01** An event MUST come back as it was written: each value as it was spelled, its fields, its time.
- **REC-02** A list of events MUST be appended whole or not at all; a refused one names its place.
- **REC-03** An appended event MUST be readable at once, before it is packed, and MUST survive an abrupt exit.
- **REC-04** A read MUST return events in event time, those of one time in the order they were appended, within a stream and across streams, each once, across packed segments and what waits; events of one time that v0.1.0 packed keep the order v0.1.0 read them in (NOT-12).
- **REC-05** A page MUST NOT split one timestamp nor lose one; a budget MUST end a page rather than fail it.
- **REC-06** A count MUST find what a list finds; a sum of integers MUST be exact past 2⁶³.
- **REC-07** A poll MUST give every packed event once from its cursor, across merges, and MUST say how many `keep` removed before it reached them.
- **REC-08** A tail MUST give the events appended after it opened, and say how many it missed when slower than the writers.
- **REC-09** A stream's `keep` MUST be its own; another `keep` for an open stream MUST be refused, naming both.
- **REC-10** Another program's lines MUST become events however they were cut; a stack trace's or a JSON value's lines MUST become one event.

### Logger and console

- **LOG-01** A log call MUST NOT wait for the file: a full buffer drops the line, counts it and says so once a quiet period.
- **LOG-02** A logger or writer dropped or closed MUST lose none of the lines it holds.
- **LOG-03** A console line MUST be the same bytes in Rust, Bun, Python and Go.
- **LOG-04** A redacted field MUST be kept nowhere; a secret MUST be hidden whatever its key's spelling, URL passwords included.
- **LOG-05** `LOG_LEVEL`, `LOG_FORMAT` and `LOG_TIME` MUST win over the code and MUST NOT turn a console on; a value no one can mean is ignored and said once.
- **LOG-06** Writing a log line MUST NOT log again.
- **LOG-07** A store's logger MUST follow the config `log`, its level live for every logger of the store.

### Metrics

- **MET-01** A sample MUST be kept bit for bit — `-0`, NaN payloads and infinities included — through every codec, file and restart.
- **MET-02** A series MUST be its name and its labels; names and labels beginning `tinystore_` or `__` are the store's and MUST be refused from a program.
- **MET-03** An ingest MUST be applied whole; a late sample behind what is sealed MUST be refused and counted, never reopen a block.
- **MET-04** An aggregate MUST be exact and rounded once, across blocks, counter resets and retention; buckets of any width MUST add up to the range.
- **MET-05** A bucket the range, the clock or retention cut MUST say `partial`; a step across a whole empty bucket MUST say `gap`; no other bucket says either.
- **MET-06** A summary MUST answer only for a whole block inside one bucket and the range; anything else MUST be decoded.
- **MET-07** A read MUST be bounded before it starts — samples, bytes, blocks, series — and a plan MUST say what the call will spend.
- **MET-08** A gauge's percentile MUST be a sample at its nearest rank; a timer's MUST be the bucket that holds the rank.
- **MET-09** Instruments MUST count into one total across threads and connections; a batch sent again MUST change nothing, and one whose answer a lost connection took MUST count once.
- **MET-10** A call an instrument refuses MUST change nothing; one refused instrument MUST NOT keep the others out of a flush.
- **MET-11** A set of labels past 2000 an instrument MUST count into `__overflow`, and groups MUST still add up to the total.
- **MET-12** A name's `keep` MUST be its own; retention MUST clip a read before a summary is chosen.
- **MET-13** A series MUST be deletable whole, damaged or not, its neighbours untouched.
- **MET-14** `self_metrics` MUST be off unless asked for, and a store opened without it holds none of its own names.
- **MET-15** A text in Prometheus' format or OpenMetrics' MUST be written whole, each sample as the kind its family's type gives and its value bit for bit, or refused whole, naming the line it cannot read.

### Search

- **SRCH-01** A search by words MUST answer what scoring every document answers, under sets, deletes, packing and purging; of equal scores the document set first comes first.
- **SRCH-02** A search by a vector MUST answer the exact best by cosine unless `recall` is asked for; with `recall`, each hit's score MUST still be its exact cosine.
- **SRCH-03** A filtered search MUST answer what scoring every document the filter passes answers.
- **SRCH-04** A set MUST be seen by the next search; a deleted document MUST never answer again.
- **SRCH-05** `set_many` MUST set every document in one commit or none.
- **SRCH-06** A change that needs the documents read again MUST NOT expose a half-built index: the old one answers and takes writes until the new one switches in, and a restart goes on.
- **SRCH-07** A table's index MUST change in the commit that changes the row, and a rollback MUST change neither; a search in the transaction that wrote MUST see its writes.
- **SRCH-08** A write from beside the store MUST reach a table's index at the store's next write to the file, or within a second.
- **SRCH-09** A misspelt word MUST be read as its nearest and `corrected` MUST say so; a word that matches MUST never be changed.
- **SRCH-10** An index MUST answer after a restart as before.

## 5. Replication

These hold for a node, `Options::node`; a store alone keeps no log.

- **REPL-01** Every commit of a home's file MUST be handed to its log once, in commit order, and the log MUST rebuild the file byte for byte.
- **REPL-02** A commit its log could not take MUST answer `outcome_unknown`, and the file MUST take no write until it opens again.
- **REPL-03** A process that died MUST lose no commit to its log.
- **REPL-04** A copy MUST be its home's file page for page after each commit it applies; a reader of a copy MUST NOT see half a commit.
- **REPL-05** A copy MAY be behind its home: reads on it are stale by its lag.
- **REPL-06** A copy MUST NOT write its file: kv's, sql's, search's, jobs' and blobs' writes MUST pass to the home and answer once the copy holds them; a transaction on a copy MUST be `unavailable`.
- **REPL-07** A copy MUST refuse an entry that does not follow, and MUST NOT apply one twice after a crash.
- **REPL-08** A file MUST have at most one home taking writes at an epoch: a home MUST refuse writes once a later epoch reaches it, and a judged home MUST write only while it holds its lease.
- **REPL-09** A synchronous home's success MUST mean a copy it names holds the write; a write no copy held in time MUST answer `committed_locally` and stand.
- **REPL-10** A live move MUST hand a file's home over with no commit lost.
- **REPL-11** A link beyond loopback MUST use TLS unless the node says `insecure`; a node whose token is wrong MUST take nothing.
- **REPL-12** A counter's add on a copy MUST count once at its home however often it is sent.

## 6. Wire and SDKs

- **WIRE-01** Every SDK MUST give the same answer as the Rust core for the same call, whether the core runs in its process, as a private child, as a sidecar or remote.
- **WIRE-02** A request MUST be understood whole or refused, naming the field and the server's version; a frame past its agreed size MUST be refused unread.
- **WIRE-03** A client past its credit MUST be cut off; a server MUST NOT wait on a slow reader, and a connection's output MUST stay bounded.
- **WIRE-04** Every stream MUST end with one final frame.
- **WIRE-05** A write whose connection ended before its answer MUST surface as `outcome_unknown`; a read MAY be sent again.
- **WIRE-06** A transaction whose client stalls, cancels or leaves MUST be rolled back by the server, freeing the writer.
- **WIRE-07** A closing server MUST say `GOAWAY`, let running streams finish, and answer later requests `unavailable` unrun.
- **WIRE-08** Every codec and every SDK MUST read the protocol's vectors as the same messages.
- **WIRE-09** A private child MUST leave when its stdin ends; a sidecar MUST leave once idle with its `SERVE` and lock.
- **WIRE-10** A read whose client cancels its stream or leaves MUST stop on the server as a cancelled read does, its stream ending `cancelled`.

## 7. Security

- **SEC-01** A read token MUST read only: a method the protocol calls `write` or `admin`, a write statement, an `ATTACH` or a `PRAGMA` MUST be `permission` and make no file.
- **SEC-02** A remote connection MUST present its token; a `SERVE` server MUST prove itself to a local challenge, and an endpoint taken after its server left MUST NOT.
- **SEC-03** An encrypted bucket's file MUST show no value; a sealed value MUST open only at its own place under its own key, and a moved one MUST be `corrupt`.
- **SEC-04** A store opened with another key MUST read and write nothing of an encrypted bucket until it is cleared; a key lost MUST NOT be replaced while values sealed with it remain.
- **SEC-05** `status`, logs and errors MUST NOT print `SERVE`'s secret, a token or a config secret.
- **SEC-06** On Linux, macOS and Windows alike, a local user other than the one a local server runs as, short of root or an administrator, MUST NOT read its `SERVE` nor connect to its socket or named pipe, whatever the store's directory lets other users do.

## 8. Backup and recovery

- **BAK-01** A backup MUST hold one moment of each engine while the store keeps working.
- **BAK-02** A restore MUST check every byte against the manifest and the zip, write only into an empty directory, and leave nothing when it fails.
- **BAK-03** The store's encryption key and the program's own files MUST be left out unless asked for, and a backup without the key MUST say so.
- **BAK-04** A backup that fails MUST leave no zip and no snapshot.

## 9. The command line

- **CLI-01** A command that reads beside a running server MUST NOT write the store nor make one of a directory.
- **CLI-02** A misspelt command, flag or setting MUST be told the nearest one.
- **CLI-03** `stop` MUST return once `SERVE` is gone, and fail when no server runs.

## 10. What TinyStore does not promise

- **NOT-01** No write is atomic across two files, unless the program joined them (DUR-06).
- **NOT-02** Channels deliver at most once, in memory: a message published while no one listens, or before a restart, is gone.
- **NOT-03** Locks and semaphores live in the memory of the process that holds the store; they do not coordinate two stores.
- **NOT-04** A copy's reads are stale by its lag (REPL-05).
- **NOT-05** A `node promote` made while the old home still writes — during a network split — may lose the commits the old home took after the promotion; the new home counts them as lost and the old one is fenced once it hears of it.
- **NOT-06** `committed_locally` means no copy held the write: a failover may lose it.
- **NOT-07** `LoseAtMost` counters may lose up to their interval at a crash.
- **NOT-08** A metrics sample behind its series' sealed frontier is refused, not stored.
- **NOT-09** A log line that finds its buffer full is dropped and counted, never waited for.
- **NOT-10** A search with `recall` may miss some of the exact best; without it a vector search is exact.
- **NOT-11** Nothing before `v0.1.0` is compatible: formats, schemas and the protocol change without migration.
- **NOT-12** Records' events of one time that v0.1.0 packed keep the order v0.1.0 read them in, which is not always the order they were appended: a stream's late events apart from its others, and the streams by the blocks their events lie in (REC-04).
