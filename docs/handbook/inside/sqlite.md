# SQLite as TinyStore runs it

Every engine of TinyStore stores its data in SQLite. SQLite is small, tested
to an extreme, and crash-safe, but it was built as a library for one program
at a time, with defaults from the early 2000s. This chapter lists the rules
TinyStore follows on every connection, and the failure each rule prevents.

## The build

TinyStore compiles its own SQLite into the core, pinned to one release, with
FTS5, R*Tree and JSON in. A program never depends on the SQLite of the system
it runs on, so the same file behaves the same on every machine. Every file is
in WAL mode, the write-ahead log: a commit appends to the `-wal` file beside
the database, and readers see a consistent snapshot while a writer works.

## One writer per file

| Rule                                          | What goes wrong without it                                                                      |
|-----------------------------------------------|-------------------------------------------------------------------------------------------------|
| one write connection per file, behind a mutex | two writers race for SQLite's file lock, and one gets `database is locked` under load           |
| every transaction begins `immediate`          | a deferred one that upgrades on its first write gets a busy that `busy_timeout` cannot wait out |
| writes known in advance share a commit        | each write pays a sync of its own, and a hundred writes cost a hundred syncs                    |
| a transaction holds the writer at most 5 s    | a stuck transaction would stop every write to the file                                          |

[The journey of a write](journey-of-a-write.md) follows the writer in detail.

## Readers that cannot write

Reads run on separate connections, each opened `query_only`: a read path
cannot write by mistake, even through a statement the program wrote. A file
has one reader per processor, between 4 and 16. One reader stays open, and
the others close after a minute without use, because a burst of reads would
otherwise keep their memory for good.

A read transaction covers fetching the bytes, not building the answer.
Decoding a block of logs, merging segments or turning rows into JSON happens
after the read transaction ends. The reason is the WAL: SQLite cannot
checkpoint past the oldest open reader, so the size of the `-wal` file is the
age of the oldest reader. A slow report that held its snapshot for a minute
would make the log grow for a minute.

## Every connection the same

Every pragma is set on every connection when it opens: the journal mode, the
sync mode, the foreign keys, the busy timeout, the cache. A reader closed and
opened again is the same reader. The durability a file was opened with sets
the sync mode:

| Durability | `synchronous` | A commit survives                                            |
|------------|---------------|--------------------------------------------------------------|
| `'full'`   | `FULL`        | the program crashing, and a power loss                       |
| `'os'`     | `NORMAL`      | the program crashing; a power loss may take the last commits |

A program's statement never changes its connection. SQLite's authorizer sees
each statement when it is compiled, and TinyStore refuses any that would:
`attach` and `detach`, `begin`, `commit`, `rollback` and savepoints, and a
`pragma` that sets a value (contract SQL-02). The writer is shared by every
write of a commit and a reader by whoever reads next, so a `pragma
foreign_keys = off` in one write would change the rules for its neighbours.
Pragmas that only read, such as `pragma table_info(messages)`, still work.

## Statements prepared once

The SQL of every engine is a named constant beside the function that runs
it, prepared once per connection and kept in a bounded cache. A prepared
statement is not a cached result: each read still starts a new snapshot.

A limit is always written `limit cast(? as integer)`. A bare `limit ?` lets
SQLite throw the compiled statement away on rebinding and compile it again
at every call, which costs more than the query on a small table.

## Mutexes and caches

SQLite runs on mutexes that the core gives it, set before SQLite starts.
SQLite's own mutexes park a thread the moment they find one held, and every
read takes its file's mutex twice, so two readers on SQLite's mutexes read
less than one. Each connection keeps its own page cache: the bundled build's
shared cache puts every connection's pages behind one mutex.

## Migrations that cannot drift

An application's database applies its migrations in one transaction when it
opens, with foreign keys off while they run, as SQLite's guide to rebuilding a
table requires, and checks them before the commit (contract SQL-04). The file
keeps a checksum of every applied migration. One that was edited, renamed,
deleted or put before another stops the file from opening, naming the
migration (contract SQL-03).

## Engines that share a file

A bucket, a queue or a search index opened from an application's database
lives in that database's file and commits with its rows. An engine kept in a
file can ask to run before each commit: a table's search index reads the rows
whose keys its triggers noted, inside each write's own savepoint, so that a
write the index cannot take fails alone and the row and its index change in
one commit.

## Free pages given back to the disk

When rows are deleted, SQLite keeps the pages they leave empty inside the
file and fills them with the next writes. It gives pages back to the disk
only in a file made with `auto_vacuum = incremental`, and only when asked.
`records.db` and `metrics.db` are made that way, because packing removes the
rows that logs were first written as, and expiry removes whole blocks.

Their maintenance gives back the pages that stayed free for ten minutes, at
most 4 MiB in a pass, which runs every minute. A stream that keeps writing
fills its free pages again before then, so a live file does not shrink and
grow every minute. A stream that stopped, or a shorter `keep`, starts giving
its pages back ten to twenty minutes later, 4 MiB a minute.

SQLite merges a page that is less than a third full only when a row is
deleted from it, not when a row on it is rewritten smaller. So when packing
or expiry shrinks a metric's head, the engine deletes the row and inserts it
again. Otherwise the heads would keep the pages they grew into during a bulk
load, which held thousands of samples per series before the first pass.

A file written by TinyStore 0.1.0 was made without the setting, and only a
`vacuum` of the whole file can add it. A vacuum holds the file's writer while
it rewrites the file. Maintenance vacuums such a file once, while it holds at
most 8 MiB of data. A larger one keeps its free pages for its next writes, as
0.1.0 did. A node never vacuums a file it logs, since its log would take the
whole file as one commit.

## Why this design?

**Why SQLite and not RocksDB or a custom format.** A log-structured store such
as RocksDB writes faster in some workloads, but it needs compaction tuning, has
no SQL, and every engine on top of it would rebuild what SQLite already has:
transactions, indexes, a query planner, and a file format that has stayed
readable for twenty years. TinyStore's engines that need a custom layout,
logs and metrics, pack their data into blocks stored as rows of SQLite, so
they get compression and still inherit crash safety from SQLite.

**Why one file per engine instead of one database.** A file has one writer.
Logs, metrics, jobs and sessions in one file would queue behind each other,
and a burst of log lines would make a sign-in wait. Separate files give each
engine its own writer and its own durability.

**Why the store refuses statements instead of trusting the program.** The
writer and the readers are shared, so a statement that changes its connection
changes it for every other caller. Rules that hold only when everyone
remembers them do not hold. The authorizer enforces them at compile time, for
every caller, the program in the same process and a client with a token alike.

## See also

- [The journey of a write](journey-of-a-write.md): the writer and its grouped
  commits.
- [Messages](../chat/messages.md): migrations and transactions from the
  program's side.
