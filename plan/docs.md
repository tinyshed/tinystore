# The documentation for the Rust release

The guides in `docs/` describe the Go release candidates. This file is the
plan for replacing them: what the new documentation is made of, its contents,
what each old page becomes, what the site needs before the pages are written,
and what ships with `v0.1.0`. Draft, 19 October 2026, for the owner.

## Three kinds of page

| Kind      | Reader                                | Read                      | Written as                                                                     |
|-----------|---------------------------------------|---------------------------|--------------------------------------------------------------------------------|
| Handbook  | someone new, or curious how it works  | in order, start to finish | chapters that build one application, each from a problem to its guarantee      |
| Guides    | someone with a task: "limit sign-ins" | one page, from a search   | one page per feature or task, grouped by engine, as `docs/` pages are now      |
| Reference | someone who knows what they look for  | looked up, never read     | generated from the code: every name in four languages, errors, limits, the CLI |

The handbook explains ideas through scenarios, a guide answers one question,
the reference is exact. A handbook chapter links to the guides of its engine
and a guide links back to the chapter that explains why. GPT's proposal called
the middle kind recipes; the feature pages already are recipes, titled by what
a reader searches for, so they keep their place and their shape.

Every page keeps the voice of the `docs` skill: plain developer English.
"Handbook" names the structure, not a literary voice.

## Four languages

Every code block has four tabs, in the books' order: TypeScript (Bun and
Node), Python, Go, Rust. The reader picks one once and the site remembers it,
as it does today for three. A page where one language lacks the call says
why, as the skill requires.

## The handbook

One application through the whole book: the AI chat of `docs/tour.md`. Each
chapter of Part II adds one capability to it, so a reader who stops after any
chapter has a working program. Part IV moves the same chat onto three
machines.

Each chapter goes three levels deep on one page, and a reader stops at the
level they need:

| Level      | What it holds                                                                                 |
|------------|-----------------------------------------------------------------------------------------------|
| Learn      | the problem the chat meets, the code that solves it, what happens if the process dies halfway |
| Understand | `Under the hood`: a diagram and a few paragraphs on the mechanism, and `What can go wrong`    |
| Go deeper  | links to the source, the contracts it keeps by id, and the research rounds that measured it   |

A chapter of Parts III and IV adds `Why this design?`: the alternatives that
were weighed, such as RocksDB instead of SQLite, HNSW instead of RaBitQ and
clusters, logical operations instead of pages, and the measurement that
decided, with its environment and a link to its report. The models are the
Rust book and The Architecture of Open Source Applications: the reader learns
why, not only what to write.

**Part I. Start**, the first twenty minutes

| # | Chapter                     | Covers                                                                  | From                                |
|---|-----------------------------|-------------------------------------------------------------------------|-------------------------------------|
| 1 | Why TinyStore               | what it replaces, what it is not, the engines in one table              | `introduction.md`, rewritten        |
| 2 | Your first store            | install, `open`, a key, a job, a log line, look inside with the CLI     | `getting-started.md`, rewritten     |
| 3 | One directory, many engines | the directory, a file per engine, `close`; embedded, sidecar and remote | `concepts/files.md`, `languages.md` |
| 4 | The chat we build           | the application's plan, its routes, which engine does what              | `tour.md`'s plan                    |

**Part II. Building the chat**

| #  | Chapter                   | The chat gains                                                      | Engines          |
|----|---------------------------|---------------------------------------------------------------------|------------------|
| 5  | Messages                  | a database, migrations, writes, transactions, tables and queries    | sql              |
| 6  | Sessions and limits       | sessions with idle expiry, quotas per plan, requests sent twice     | kv               |
| 7  | Replies in the background | a queue, a worker, retries, steps, watching a reply                 | jobs             |
| 8  | Attachments               | uploads, serving with ranges, expiry                                | blobs            |
| 9  | Finding old messages      | an index of the messages table, words, a misspelt query, vectors    | search           |
| 10 | Live updates              | a new message to every open tab; one summary job per chat at a time | channels, locks  |
| 11 | Settings                  | the model's name and limits from a file, the environment, live      | config           |
| 12 | What it is doing          | the logger, events, a trace per request, reading them back          | records, console |
| 13 | How fast it is            | timers and counters, aggregates, the store's own metrics            | metrics          |
| 14 | Keeping it                | a backup while it runs, a restore, tests with a clock               | backup           |

**Part III. Under the hood**, for the curious

| #  | Chapter                     | Covers                                                                        | Interactive |
|----|-----------------------------|-------------------------------------------------------------------------------|-------------|
| 15 | The journey of a write      | SDK, pipe, core, the file's writer, grouped commits, the WAL, completions     | yes         |
| 16 | SQLite as TinyStore runs it | one writer per file, `immediate`, readers that close, the authorizer, pragmas | no          |
| 17 | One budget of memory        | the store's budget, page caches, readers, what a limit error says             | no          |
| 18 | Time and errors             | the store's clock, an engine's window, `ErrorKind` everywhere                 | no          |
| 19 | How a job lives             | due, claimed, lease, retry, step, done; what a crash does at each state       | yes         |
| 20 | How logs and metrics pack   | heads, segments, blocks, columns and codecs; a sample kept bit for bit        | yes         |
| 21 | How search finds            | postings, BM25 with block-max, the head and packing; RaBitQ and clusters      | no          |
| 22 | One core, four languages    | the wire protocol, the pipe and FFI, the sidecar and its `SERVE` proof        | yes         |

**Part IV. Beyond one machine**

| #  | Chapter            | Covers                                                                          | Interactive |
|----|--------------------|---------------------------------------------------------------------------------|-------------|
| 23 | The server         | `tinystore serve`, tokens, TLS, Docker, the CLI beside a running server         | no          |
| 24 | Readers in Europe  | nodes, a home per file, copies, pages instead of operations, writes passed home | yes         |
| 25 | When the home dies | epochs, `node promote` and `demote`, `move_home`, synchronous homes             | yes         |

The witness is a draft, so chapter 25 describes it as one or leaves it out.

**Part V. Production**

| #  | Chapter                 | Covers                                                                    |
|----|-------------------------|---------------------------------------------------------------------------|
| 26 | What TinyStore promises | durability, outcome unknown, at least once, damage contained; by contract |
| 27 | Limits and defaults     | every bound in one place, with links to the reference                     |
| 28 | Running it              | status, logs, self metrics, disk, the AI agents' MCP server               |

The interactive pages are small simulators, not animated arrows: the reader
acts and sees what TinyStore does. In chapter 25 the reader cuts America off,
sees which commits reached Europe, runs `promote`, and sees what was lost and
what the old home drops when it comes back. A simulator is a Svelte component
over a model of states written by hand; every state it shows is one that a
contract or a test names, and a test of the site walks its scenarios against
that list, so it cannot show behaviour that is not built. A markdown page
places one by a directive that GitHub shows as a link and the site as the
simulator. Chapter 25 shows a manual promotion, not an automatic failover.

## Scenes, by engine

The situations each engine's pages play, chosen by the docs skill's rule:
two parties or more, a guarantee at stake, and code on the page that handles
it. Each names the contract it shows.

| Engine      | Scene                                                                                 | Contract         |
|-------------|---------------------------------------------------------------------------------------|------------------|
| durability  | the power goes out at each stage of a write; what the app heard, what the disk holds  | DUR-01, DUR-04   |
| KV          | a buyer double-clicks Pay; the second request waits and gets the same receipt         | KV-11            |
| KV          | two tabs save one draft; the second write fails `conflict` and reads again            | KV-05            |
| KV          | a script floods the API; the fourth request gets `ok: false` and when to retry        | KV-09            |
| jobs        | a worker crashes mid-email; after `timeout` another worker sends it                   | JOB-05           |
| jobs        | a job of three steps crashes after the second; the retry skips the two that were kept | JOB-06           |
| sql         | six writes share a commit and one breaks a constraint; it fails alone                 | DUR-03, SQL-08   |
| blobs       | an upload is cut halfway; nothing is published, and the old file still reads          | BLOB-01          |
| channels    | a slow subscriber falls behind; it is told how many messages it missed                | CHAN-02          |
| locks       | a lock's holder stalls past its lease; its late write is refused by the fence         | LOCK-02, LOCK-05 |
| replication | the home is cut off; a promotion loses the writes no copy had, or warns in sync mode  | REPL-08, REPL-09 |
| replication | a write on a copy travels to the home and is answered once the copy holds it          | REPL-06          |

## The guides

Grouped by engine, in the handbook's order. An overview opens each group;
the pages after it are named as a reader would search for them.

| Group    | Pages                                                                                                                                |
|----------|--------------------------------------------------------------------------------------------------------------------------------------|
| Connect  | the sidecar, a private child, a remote server, embedded, errors and cancelling in each SDK                                           |
| SQL      | overview, migrations, writes and transactions, tables and queries, `include`, buckets and queues in a database, FTS5 and R*Tree      |
| KV       | overview, expiry, versions, branches, encrypted buckets, counters, rate limits, quotas, once, transactions, sessions, sign-in limits |
| Jobs     | overview, ids, schedules and cron, concurrency and rate, watching, steps, jobs in your database, testing                             |
| Blobs    | overview, uploads, serving files, copies and expiry, integrity                                                                       |
| Search   | overview, fields and weights, typos, filters, vectors, words and vectors together, a table's index, rebuilds                         |
| Channels | overview, subscriptions and missed messages, publishing from a transaction                                                           |
| Locks    | overview, holding and waiting, leases over a connection, fences, semaphores                                                          |
| Config   | overview, layers and files, secrets and required settings, live settings, watching, explain and validate                             |
| Records  | overview, logging, console output, events, reading, traces, tail and poll, other programs' output, repair                            |
| Metrics  | overview, instruments, reading, aggregates, ingesting, Prometheus, the store's own metrics, the `metrics` crate                      |
| Operate  | the command line, backups, nodes, AI agents, Docker, testing                                                                         |

## The reference

| Page                | Source                                                                                                  |
|---------------------|---------------------------------------------------------------------------------------------------------|
| API, per language   | generated: rustdoc's JSON for Rust, `go/doc` for `sdk/go`, the `.d.ts` for TypeScript, `ast` for Python |
| Errors              | `ErrorKind` and each SDK's spelling of it                                                               |
| Limits and defaults | the books' Bounds sections, checked against the constants                                               |
| The command line    | generated from the CLI's help table                                                                     |
| Contracts           | `CONTRACTS.md`                                                                                          |
| The wire protocol   | `docs/wire.md`, current today                                                                           |

## What each old page becomes

Every code block of `docs/` uses the Go release candidates' API
(`store.kv.bucket`, `kv.Open(ctx, store, …)`) and has no Rust, so no block is
kept as it is. The scenarios, headings and limits tables are worth keeping:
most pages are rewritten in place from their engine's book.

| Old page                                        | Becomes                                                     | Why                                                              |
|-------------------------------------------------|-------------------------------------------------------------|------------------------------------------------------------------|
| `introduction.md`                               | chapter 1                                                   | says only Go embeds; every SDK embeds now                        |
| `getting-started.md`                            | chapter 2                                                   | the scenario stays, the code is the old API                      |
| `languages.md`                                  | chapter 3 and the Connect guides                            | guests are not built; connecting stays                           |
| `tour.md`                                       | the handbook's thread, chapters 4–14                        | the same chat, grown into chapters                               |
| `sql/README.md`, `writes.md`                    | SQL guides, rewritten                                       | old API                                                          |
| `sql/schema.md`                                 | Tables and queries, in four languages                       | Go-only today; every SDK has tables and a query builder          |
| `sql/search.md`                                 | FTS5 and R*Tree guide; the search engine gets its own group | FTS5 still works, but a table's index is the answer now          |
| `kv/*` except `configs.md`                      | KV guides, rewritten; branches and encrypted buckets added  | old API                                                          |
| `kv/configs.md`                                 | the Config group                                            | config is its own engine                                         |
| `kv/your-data.md`, `jobs/your-data.md`          | "in your database" pages of KV and Jobs                     | `db.bucket` and `db.queue` replace the Go-only form              |
| `jobs/*`                                        | Jobs guides, rewritten                                      | old API                                                          |
| `blobs/*`                                       | Blobs guides, rewritten                                     | old API                                                          |
| `records/*`                                     | Records guides, rewritten; repair added                     | old API                                                          |
| `metrics/*`                                     | Metrics guides, rewritten; Prometheus and own metrics added | old API                                                          |
| `running/sidecar.md`, `server.md`               | Connect guides and chapter 23                               |                                                                  |
| `running/cli.md`                                | the Operate guide and the generated CLI reference           | the CLI was rebuilt to its book                                  |
| `running/agents.md`, `backups.md`, `testing.md` | Operate guides, rewritten                                   |                                                                  |
| `running/guests.md`                             | removed                                                     | guests are not built on this branch                              |
| `running/upgrading.md`                          | removed until `v0.1.0`                                      | nothing is compatible before it                                  |
| `concepts/durability.md`                        | chapter 26                                                  |                                                                  |
| `concepts/concurrency.md`                       | chapters 15 and 16                                          |                                                                  |
| `concepts/memory.md`                            | chapter 17                                                  |                                                                  |
| `concepts/time.md`, `errors.md`                 | chapter 18 and the Errors reference                         |                                                                  |
| `concepts/files.md`                             | chapter 3                                                   |                                                                  |
| `concepts/retention.md`                         | each engine's `keep`, in its guides                         | retention is a name's or a stream's own now                      |
| `reference/*` generated pages                   | generated again                                             | read from the Go RCs at the repository's root, last on 5 October |
| `reference/limits.md`                           | chapter 27 and the Limits reference                         |                                                                  |
| `wire.md`                                       | kept                                                        | maintained with the protocol                                     |

New pages with no old one: Search, Channels, Locks and Backup groups, nodes
and replication, encrypted buckets, branches, the store's own metrics, the
`metrics` crate, Prometheus, every Rust tab, and Part III's chapters on
packing and search.

## What the site needs first

Before chapters are written, so that none is written twice:

1. **A fourth tab.** `rust` in `web/src/lib/content/code.ts` and `api.ts`.
2. **The reference read from this branch.** Go from `sdk/go` instead of the
   root, Rust from rustdoc's JSON, and the engines added since: config,
   locks, channels, search, backup, replication.
3. **The chat as a program that runs.** `examples/chat/` holds the handbook's
   application in each of the four languages, and a chapter quotes it by a
   named region instead of carrying its own copy. CI runs each language's
   chat against the core, so the handbook's code is code that worked.
4. **Fragments that compile.** A guide's blocks are extracted and checked:
   Rust as a test crate, TypeScript with `tsc`, Go with `go vet`, Python with
   pyright. A fragment declares the names it uses from the page's first
   block. A type check proves the names, not the behaviour; what a guide
   promises about behaviour is kept by the SDK's tests that name its contract.
5. **Scenes.** A ` ```scene ` fence: actors, cables and steps as text, which
   the site plays beside the code it runs. Built; the format is in the docs
   skill.

6. **A signature on a call whose type is known.** Clicking a call in a block
   opens its signature, its doc and a link to the reference. A name is
   resolved in two ways, and anything else stays plain text:
   - a name that only one type in the reference has, such as `rateLimit`;
   - a method on a variable that the page opened, because every handle opens
     by a call whose return type the reference knows:
     `const sessions = store.bucket<Session>('sessions')` makes `sessions` a
     `Bucket`, so `sessions.get` resolves to `Bucket.get`. The same holds for
     `sessions = store.bucket(…)`, `sessions, err := kv.OpenBucket[…](…)` and
     `let sessions = store.bucket::<…>(…).open()?`.

   A call that resolves to no name of the reference fails the build. A name
   such as `get` on a variable whose type the page never shows stays plain:
   a wrong signature is worse than none.

## What ships with v0.1.0

The features that ship are the features the handbook explains, so a release
with `promote`, `move_home` and synchronous homes has the chapters on what
they guarantee. Short chapters are fine: a concept, a diagram and a link to
research, not every bit of RaBitQ.

| In `v0.1.0`                                                | After                                                       |
|------------------------------------------------------------|-------------------------------------------------------------|
| Part I, chapters 1–4                                       | chapters 16–21: SQLite, memory, time, jobs, packing, search |
| Part II, chapters 5–14                                     | the other simulators                                        |
| chapter 15, the journey of a write, with its simulator     |                                                             |
| chapter 22, one core, four languages                       |                                                             |
| Part IV, chapters 23–25, with the home that dies           |                                                             |
| Part V, chapters 26–28                                     |                                                             |
| every guide of the engines Part II uses, and the reference |                                                             |

The site builds from `main`, and `rust` replaces `main` when phase 4 ends. The
new pages are written here and reach the site with that merge, so the site
shows the Go guides until then.

## Open

- The order of the tabs: the books put TypeScript first and Rust last.
- Whether a chapter of Part III links to research's reports directly, which
  show the dense voice, or to a short page here that quotes their numbers with
  their environment.
- The chat's name in the examples.
