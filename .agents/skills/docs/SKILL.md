---
name: docs
description: Use when writing or changing a page of the guides in docs/, the landing page's words in web/landing.md, or the site in web/ that reads them - where a page goes, its shape and voice, code in Go, Bun and Python, what makes it read well to an AI agent, where each fact is checked, and how to see it render before pushing.
---

# Writing the docs

The guides in `docs/` are what people and AI agents read to build on
TinyStore. Every page has to work in two ways:

- **As part of a book.** The site's previous and next links follow the order
  of `docs/README.md`, so readers can go through the docs from start to
  finish. Each section starts with the common case and ends with the details.
- **As a single page.** Many readers arrive from a search, a link or an
  agent's `llms.txt`, and see nothing but this one page. It must make sense on
  its own.

A guide only describes what is built. The old design documents in research's
`tinystore/design/` are a good source of facts and examples, but never copy
their sentences: the guides use the plain voice described below. Read this
file to learn how to write a page, and check the sources in
[Where the facts are](#where-the-facts-are) for what to write.

## Where a page goes

`docs/README.md` is the table of contents and the site's sidebar. Each `##`
heading is a section, and each list item is a page, in reading order. An item
without a link is a page that hasn't been written yet, and the site shows it
greyed out. To add a page:

1. Create the file in its section's directory, using the table below.
2. Turn its item into a link: `- Quotas` becomes `- [Quotas](kv/quotas.md)`.
3. The page's address is its path: `docs/kv/quotas.md` becomes
   `/docs/kv/quotas`, and an engine's overview `docs/kv/README.md` becomes
   `/docs/kv`.

| Section | Files | What its pages cover |
|---|---|---|
| Start here | `docs/*.md` | what TinyStore is, a first program, how Go, Bun and Python connect to a store, a tour of every engine |
| SQL, KV, Jobs, Blobs, Records, Metrics | `docs/<engine>/` | `README.md` is the overview: what the engine is for, a short tour, and links to its other pages; then one page per feature |
| Running it | `docs/running/` | the sidecar, a remote server, the command line, AI agents, backups, testing, upgrading |
| How it works | `docs/concepts/` | durability, concurrency, time, memory and limits, errors, the files on disk: why the store behaves the way it does |
| Reference | `docs/reference/`, `docs/wire.md` | tables to look things up: limits and defaults, links to the API references, and the wire protocol last, for people who write a client |

A feature gets its own page if a reader would search for it by name, such as
Sessions, Quotas, Once or Steps. If a page needs more than six sections, split
it into two features. Don't add front matter, because GitHub shows it as a
table.

## The shape of a page

1. **`# Title`**: the name a reader would search for. `Quotas`, not
   `Several windows per key`.
2. **The lead**: one paragraph of two or three sentences that says what the
   feature does and when to use it, and names the engine. The site shows it
   under the title, in search results, in link previews and in `llms.txt`, so
   it has to make sense on its own.
3. **The shortest complete code** that does the job, in all three languages,
   near the top of the page. The first code block opens everything the page
   uses. Later blocks reuse those names, and the text says so.
4. **Sections that answer the reader's next questions**, from the common case
   to the edge cases: how to use it, what it guarantees, what happens when it
   fails, and what to watch out for. Write each heading as a task or a
   question, such as `## Refund uses` or `## What a crash loses`. Headings
   are also link anchors: if you rename one, the build tells you which links
   to fix.
5. **`## Limits and defaults`**: a table, if the feature has any.
6. **`## See also`**: the package's README for the full contract, and the
   design document in research for why it works this way.

Use a callout only for something that can lose data, surprise the reader or
stop a program, such as a limit, a rejected value or an operation that is not
atomic. Never use one for emphasis. Most pages need one callout or none.

## Code first

The code tells the story, and the text explains it. A reader should be able
to skim a page by its code blocks alone and still learn how to use the
feature. Getting started is the model.

- **Start each section with code**, or with one short sentence that leads into
  it. Explain after the code, in a few sentences.
- **Show behaviour in the code itself.** Put the result in a comment next to
  the call: `await codes.take('K7Q2') // undefined: a second click finds nothing`.
  A comment like that replaces a paragraph.
- **Prefer a small example over a description.** If a rule can be shown with
  three lines of code, show the code and state the rule in one sentence.
- **Use real-looking values**: a user's id, an email address, a code such as
  `K7Q2`. Avoid `foo`, `bar` and `x`.
- **Keep text between code blocks short.** Two or three sentences are usually
  enough. If a section needs more, the feature probably needs another example.

## Voice: plain developer English

Write the way good developer documentation is written, such as Stripe's,
Bun's or grammY's: simple, direct sentences that a reader whose first language
is not English understands on the first read.

The repository's AGENTS.md, its code comments and the old design documents
use a dense, literary style. That style is fine for contributors, but it is
wrong for the guides. Don't copy it.

**Do:**

- Address the reader as "you". Use the present tense, and the imperative for
  steps: "Call `allow` before each action."
- Put one idea in each sentence, usually 10 to 20 words. Split any sentence
  that is longer than 25 words or chains several clauses.
- Use subject, verb, object, with the actor as the subject: "`take` returns
  the value and deletes the key."
- Keep "that", "which" and "who" in relative clauses: "the limits that a plan
  sets".
- Use plain connecting words: "because", "so", "if", "when", "for example",
  "instead".
- Use common words: "sets", "stores", "contains", "deletes", "fails",
  "accepts".
- Say "per": "one page per feature", "100 messages per week".
- Use a list for three or more parallel items, and a table for values that
  differ between languages.
- Describe results literally: "`allow` returns `ok: false`", "it fails with a
  conflict error (`ErrConflict` in Go, `ConflictError` in Bun and Python)".
- Show a scenario first, then the code, then the rule behind it.
- Make the "wow" a plain statement of a guarantee: "If a user clicks the link
  twice, only one sign-in succeeds."
- Say what a feature doesn't do, in one sentence, and what to use instead.

**Don't:**

- Drop "that" or "which": not "the limits a paid plan states".
- Use "a X a Y" to mean "per": not "one job a key", not "a page a feature".
- Chain clauses with colons and semicolons. Use at most one colon per
  sentence, only before a list, an example or code, and no semicolons.
- Use "since" or "as" to mean "because".
- Make an abstract noun the subject when a person or a call acts: not "a use
  counts in every window or in none", but "`allow` counts the use in every
  window, or in none of them".
- Write aphorisms or rhetorical rhythm: not "a lease is its attempt", not
  "one call opens it, one call closes it, one budget bounds it".
- Use unusual verbs where a plain one exists: "states", "names", "holds" or
  "takes" when you mean "sets", "identifies", "contains" or "accepts".
- Use marketing words or filler: "simply", "just", "easy", "powerful",
  "seamless", or exclamation marks. Phrases such as "for example" or
  "note that" are fine when they help the reader.
- Describe anything that isn't built as if it works.

A number must come with its environment, meaning the machine and versions,
and a link to the report that measured it. Otherwise, leave it out.

Before and after, from the first drafts of these docs and from the design
documents:

| Dense | Plain |
|---|---|
| It is the kv engine's answer to the limits a paid plan states: a use counts in every window or in none. | Quotas are part of the KV engine and are designed for the limits of paid plans. `allow` counts a use in all windows, or in none of them. |
| `enqueue` returns once the job is on disk, so a job survives the process dying right after it. | `enqueue` returns after the job is saved to disk, so the job survives even if the process crashes right after. |
| Running until idle returns once no job is due, which suits a script or a test; a server runs `work` for as long as it runs. | With `untilIdle`, `work` returns as soon as no jobs are due. This is useful in scripts and tests. |
| Expired is absent to every operation. `Get`, `Has` and `Scan` do not see it, `SetIfAbsent` claims it, and `Add` starts again from zero. | When a key expires, TinyStore treats it as deleted. `get` returns nothing, `setIfAbsent` can write the key again, and a counter restarts from zero. You don't need a cleanup job. |
| A lease is its attempt. Settling a job names the attempt it claimed; one whose lease ended and whose job another worker has claimed since is `ErrConflict`. | When a worker claims a job, it gets a lease for a limited time. If the worker stalls and the lease runs out, another worker can claim the job. The first worker's late acknowledgement then fails with a conflict error and changes nothing. |
| A page ends where it can prove it is whole. | A page never splits records that have the same timestamp. The next page starts exactly where the previous one ended. |

Before you finish a page, read it aloud. If you have to read a sentence twice,
rewrite it.

## Code in three languages

Code blocks written one after another, with nothing between them, become one
block with a tab for each language. The site remembers the reader's choice on
every page.

````md
```ts
const ai = store.kv.quota('ai', { session: '100/5h', weekly: '300/7d' })
```

```python
ai = store.kv.quota("ai", session="100/5h", weekly="300/7d")
```

```go
ai, err := kv.OpenQuota(ctx, state, "ai",
	kv.Window("session", 100, 5*time.Hour), kv.Window("weekly", 300, 7*24*time.Hour))
```
````

- `ts` is the tab for Bun and Node, `python` for Python and `go` for Go. The
  switch always shows them in the order Bun, Python, Go.
- `title="…"` gives a block a file name, and several titled blocks of one
  language become files of one block. Leave the title off a single snippet.
  Two untitled blocks of the same language stay two separate blocks.
- A shell command joins a language's tab with `for`: ` ```sh for=bun `.
- Lines that a logger prints on a terminal go in a ` ```log ` fence. The site
  colours them as the console does: the time and `key=` dim, the level in its
  colour, the stream cyan. GitHub shows the fence as plain text.
- **Every block shows all three languages.** If one is missing, the page says
  why, for example because only Go declares a schema from structs. A reader
  who chose Python and only finds Go has learned nothing.
- **Use the same names for handles on every page**: `store` for the store; in
  Go, `state` for the KV engine, `queues` for jobs, `objects` for blobs,
  `logs` for records, `stats` for metrics and `db` for an SQL database. In Bun
  and Python the engines are properties of `store`, and a database is `db`
  too.
- **In Go, check the errors that the page is about**, and leave the others as
  plain `err` assignments, like the package READMEs do. A short tour may start
  its Go block with `// errors left out`.
- **In the text, describe the idea instead of one language's spelling**, because
  `with` and `labels` are the same idea. Where the page introduces an option,
  give its three spellings once: `DefaultTTL`, `defaultTtl`, `default_ttl`.
- **Write durations the way each language does**: `15*time.Minute` in Go,
  `'15m'` in Bun and Python. A bare number means milliseconds in Bun and
  seconds in Python.
- **Run the code if you can.** A complete program on a page, like the one in
  Getting started, should be one you have run in all three languages.

## Written for agents too

Agents read a page through `llms.txt`, `llms-full.txt` or the page's `.md`
copy, often without any other page, and write code from it.

- **The page stands alone.** It names its engine, opens its handles in the
  first code block or links to where they are opened, and never says "as
  shown above" about another page without a link.
- **Every name is exact and searchable.** Write calls, options and errors in
  code font, with each language's spelling where they differ, so that an
  agent writing Python finds `default_ttl` on the page.
- **Every limit and default is in a table, with its unit.**
- **Every error comes with what to do about it**: "If the version is stale,
  the write fails with a conflict error (`ErrConflict`, `ConflictError`).
  Read the key again and retry."
- **Nothing exists only in an image**, and code is never a screenshot.
- You don't need to write anything extra for agents: a page that reads well
  on its own reads well to them. The site generates `llms.txt`,
  `llms-full.txt` and each page's `.md` copy.

## Where the facts are

Check every call and every claim against its source before you put it on a
page. The example tests compile and run, so they are the safest to copy from.

| To check | Read |
|---|---|
| a Go call, its options, contracts and errors | the package README (`kv/README.md`…), `go doc`, and its `example_test.go` |
| a Bun or Node call | `sdk/js/README.md`, `sdk/js/src/<engine>.ts`, `sdk/js/test/<engine>.test.ts` |
| a Python call | `sdk/python/README.md`, `sdk/python/src/tinystore/<engine>.py`, `sdk/python/tests/` |
| a complete program that uses every engine | `examples/notes/main.go`, built and tested |
| what a guarantee rests on | the gates table in AGENTS.md: each promise and the test that checks it |
| a limit or a default | the package README's contracts, then the constant in the code |
| why it works this way, and what was measured | research's `tinystore/design/<engine>.md` and `reports/`. They are not updated anymore, so if they disagree with the code, the code is right |
| a good real-world example | the "Five cases" sections of the kv, jobs, blobs and sqldb design documents |
| the command line | `tinystore` without arguments lists its commands; the code is in `cmd/tinystore/` |
| the bytes on the wire | `docs/wire.md` |

## Callouts, tables, links

- A callout is a GitHub alert whose first line is bold. The bold line is the
  callout's label:

  ```md
  > [!NOTE]
  > **Counters**
  > `increase` is for counters.
  ```

  The kinds are GitHub's: NOTE, TIP, IMPORTANT, WARNING and CAUTION.
- A table with an empty header row, `| | |`, is drawn as rows without a
  header. Text in backticks is drawn in a monospace font.
- Write links relative to the file, as on GitHub. A link to a page stays on
  the site. A link to any other file of the repository opens on GitHub, at the
  commit the site was built from, and an image is served by the site. A link
  to a file or a heading that doesn't exist fails the build.
- `<!-- … -->` is a note for whoever edits the file. GitHub and the site don't
  show it, and the markdown copy leaves it out.

## The landing page

`web/landing.md` contains all the words of the landing page:

- the `#` heading is the headline, and the paragraph after it is the pitch;
- its code blocks are the sample, one per language;
- the table under `## Engines` is the list of engines;
- the section after it is the numbers: its heading, a legend table and a
  caveat paragraph.

The design lives in `web/src/routes/+page.svelte`. The benchmark cards are
read from the SVGs in the README, so their numbers are never typed twice. Link
each engine's row to its overview page once that page exists, and to its
package README until then.

## Before pushing

```sh
task web:dev   # the site on :5173, reloading as docs/ changes
task web       # what CI runs: the checks, the tests and the build
```

The build also checks every link. Then read the page once as a stranger
would, and look for filler words:

```sh
git grep -n -i -E '\b(simply|just|easy|easily|powerful|seamless)\b' -- docs/
```

- [ ] the lead makes sense on its own and names the engine
- [ ] the first code block works in all three languages, or the page says why not
- [ ] every call is checked against its source, and each option's spellings appear once
- [ ] limits and defaults are in a table
- [ ] nothing that isn't built is described as working
- [ ] the page's item in `docs/README.md` is a link
- [ ] every sentence reads well the first time
- [ ] `task web` passes
