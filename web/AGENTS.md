# TinyStore site

The documentation in `docs/` as a site: SvelteKit 2 with Svelte 5 runes,
every page prerendered by `adapter-static`, and a Bun server that hands the
files out and keeps the site's own analytics in a TinyStore of its own. The
root [AGENTS.md](../AGENTS.md) applies here too; this file covers what is
particular to the site. How a page is written is the `docs` skill.

## Status

Built: the landing page, a page for every file `docs/README.md` links, search, scenes, diagrams,
both themes and the looks a reader picks over them, the language switch, a markdown copy of each page,
which links the other pages' copies and opens with a line saying where `llms.txt` is, `llms.txt`,
also at `/.well-known/llms.txt`, `skill.md`, also at `/.well-known/agent-skills/tinystore/SKILL.md`
beside the discovery index that lists it with its SHA-256: what TinyStore is, how each language
installs it and the first examples of each engine's guide, whose calls the build checks,
a `robots.txt` whose `Content-Signal` lets the docs train a model, be searched and answer a question,
`llms-full.txt`, the API pages with `llms-<language>.txt`, a page per release, the sitemap,
and the server with its analytics. The docs
are written for Rust: the Handbook's
five parts in `docs/handbook/`, and the Guides in `docs/guides/`, every engine page by page, the command line by its overview.

Not built: a page of the site's own statistics.

It is served at https://tinyshed.org/tinystore: built with
`SITE_BASE=/tinystore`, behind the Caddy and the Cloudflare Tunnel that the
private `tinyshed/landing` repository runs on the server. Its `.well-known/`
is therefore `/tinystore/.well-known/`, not the host's: what tinyshed.org's
own `/.well-known/` answers is the landing's to route.

## The docs are the source, the site a reading of them

**`docs/README.md` is the sidebar.** Each second-level heading is a part, a
tab of the header with a sidebar of its own (Handbook, Guides, Reference),
each third-level heading a section of it, and each item of its list an entry
in order: a link to a file under `docs/` is a
page at the same path, `docs/handbook/start/why.md` → `/docs/handbook/start/why`; a link
anywhere else is a link out; an item without a link is a page not written yet,
shown and not linked. GitHub shows the same file as the table of contents.

**Whatever reads on GitHub reads here.** A page is plain GitHub markdown: no
front matter, which GitHub would draw as a table, and nothing GitHub would
show as noise. What the site adds it reads from what GitHub already ignores:

| Written                                       | On GitHub             | Here                                               |
|-----------------------------------------------|-----------------------|----------------------------------------------------|
| fences one after another, a language each     | one under another     | one block, the language the reader picked          |
| ` ```ts title="metrics.ts" `                  | a TypeScript block    | a tab named `metrics.ts`                           |
| ` ```sh for=bun `                             | a shell block         | the Bun variant of its block                       |
| ` ```toml for=rust `, no other language       | a TOML block          | the block, to a reader of Rust alone               |
| `> [!NOTE]` then a **bold** line              | GitHub's note         | a callout labelled by the bold line                |
| a table whose header row is empty, `\| \| \|` | a table               | a card of rows, no header                          |
| `[kv](kv.md#expiry)`                          | the file, its heading | `/docs/kv#expiry`, the heading checked             |
| `[code](../kv/kv.go)`                         | the file              | GitHub, at the commit the site was built from      |
| `<!-- a note -->`                             | nothing               | nothing, and left out of the markdown copy         |
| `store.bucket('sessions')` in a fence         | plain code            | a link to `Store.bucket`, its signature on a click |
| a ` ```scene ` fence                          | its text              | an animation beside its code, from `src/lib/scene` |
| a ` ```mermaid ` flowchart                    | Mermaid's drawing     | an SVG laid out at build, from `src/lib/diagram`   |
| a ` ```explain ` fence, a `tinystore` command | the command           | a field that reads a typed command word by word    |

**A link that leads nowhere fails the build.** Every relative link and image
is resolved at build time against the repository, and a heading against the
id GitHub gives it; the prerender then follows every link it renders. The dev
server prints the same problems as a warning and serves the pages anyway, so
that docs being rewritten can be read; a build fails on them.

**A call in a guide's block is the reference's.** `calls.ts` reads the
generated API pages back, follows each chain from `store` or a variable a
block assigned, and wraps every call it resolves in a link to its heading,
which the build checks as it checks any link; `code-blocks.ts` opens its
signature on a click. A call that a known type does not have fails the build.

**A scene is text that plays.** `src/lib/scene/parse.ts` reads a ```scene
fence at build time, where an error fails the page; `content/scene.ts` writes a
figure holding its code and its steps as a list, and `scenes.ts` mounts
`Scene.svelte` on it, whose `Stage` draws and moves the SVG. The format is in
the docs skill.

**A diagram is drawn once, when the site is built.** `src/lib/diagram/parse.ts`
reads the part of Mermaid's flowchart the docs use, and `render.ts` lays it
out with ELK's layered algorithm and writes an SVG whose classes take the
tokens, so both themes draw it and no Mermaid runs in the browser. An `LR`
diagram gets a `TB` twin that a phone shows instead, a tap enlarges either
(`diagrams.ts`), and a diagram that does not parse fails the page.
`diagram.test.ts` lays out every diagram of the docs and checks its width,
overlaps, crossings and order.

**The command line is read from its own help.** `just cli-json` writes
`src/lib/content/cli.json`, the help's table in `crates/cli/src/help.rs`, which
the CLI's tests hold the file to, and `cli-prints.json`, what each command's
example printed on a demo store. A shell fence colours a `tinystore` command's
words by them, and an explain (`src/lib/explain`) reads a typed command with
them, loaded only on a page that has one.

**Code is highlighted once, when the site is built.** Shiki writes both
themes' colours into each token, so the page highlights and changes theme with
no script. Every language of a block is in the HTML; `app.html` names the
reader's language before the first paint and the stylesheet shows it.

## The API pages are the source's

`docs/reference/<language>/<engine>.md` and their index `docs/reference/api.md`
are generated: `task reference` writes them, and `bun test` fails when one is
not what the source says. Edit the doc comments, never the pages.

| Language | Read by                                             | Public is                                                                    |
|----------|-----------------------------------------------------|------------------------------------------------------------------------------|
| Go       | `web/reference/go`, `go/doc` on `sdk/go`            | exported                                                                     |
| Bun      | `src/lib/content/reference.ts`, the emitted `.d.ts` | exported by `index.ts`, or a field of `Store`                                |
| Python   | `web/reference/python.py`, `ast` without importing  | in `__all__`, or set on `Store` in `__init__`                                |
| Rust     | `src/lib/content/rustdoc.ts`, rustdoc's JSON        | reachable from the crate's root, `Store`'s methods by the engine's directory |

An engine's modules in each language are `sources` in `reference.ts`, its
name and guide `src/lib/content/api.ts`. The sidebar lists the index alone;
the pages switch between languages and engines, and `llms-full.txt` leaves
them to `llms-bun.txt`, `llms-python.txt`, `llms-go.txt` and
`llms-rust.txt`, one per language, since an agent needs one of the four.
The test needs Go, Python 3.12 or later and the pinned Rust toolchain
beside Bun: rustdoc's JSON is asked of it through `RUSTC_BOOTSTRAP`, so no
nightly is needed.

## The server

In `server/`, `config.ts` reads the environment through zod, `bootstrap.ts`
is the only composition root, and `index.ts` only starts and stops. It serves `build/` from an index made at start, brotli or
gzip as the build made them, and counts what a reader asked for:

| Kept                                           | Where                     | What                                                                                                       |
|------------------------------------------------|---------------------------|------------------------------------------------------------------------------------------------------------|
| `site_views_total{page, via, agent, provider}` | metrics                   | a page, its data on navigation, or its markdown; a person, a crawler or an agent, and the agent's provider |
| `site_events_total{name}`                      | metrics                   | what a page's script reports: copies, searches, the language                                               |
| `view`, `copy-code`, …                         | records, stream `readers` | each of those, with the day's visitor id, where the reader came from, their language                       |
| the server's own lines                         | records, stream `site`    | and on stderr, as JSON in a container                                                                      |
| `salt/<day>`, `site-events`                    | kv                        | the day's salt, expiring; the limiter a reader's events go through                                         |

A page's address is its markdown to a request that would rather read it,
`Accept: text/markdown`, or `text/plain` and no HTML, with `Vary: Accept`;
the landing page's markdown is `llms.txt`. A request for markdown, by its
path or what it accepts, that finds nothing gets `404.md`, which says where
every page is listed. Every response carries a `Link` to `llms.txt` and
`llms-full.txt` and an `X-Llms-Txt`, at the address the build's sitemap gives.

No address is kept: a visitor is a hash of the address and the User-Agent under
a salt that lives a day and a half. No cookie is set. A view is counted when
the page's HTML or its `__data.json` is served, so the client sends nothing
for it; `data-sveltekit-preload-data="off"` keeps a hover from counting.

While it runs, `tinystore logs data -f` follows the views and `tinystore mcp
data` lets an agent read them: the store is the directory's sidecar, not a
private child.

## Rules

- Every route is prerendered. Nothing renders at request time, and a route
  that cannot be a file is not a route here.
- An internal link goes through `href()` and HTML made at build time through
  `rebase()`, `$lib/href.ts`, so that a build opens from any folder: a
  preview's, or a project page's base path.
- The tokens in `src/app.css` are the design system: a component reads
  `--ts-*` and nothing else. Both themes are the design's own; each look of
  the theme's menu, paper, black and an editor's theme such as Dracula, is a
  whole set of those tokens under `[data-look]`. An editor's look colours code
  by a token's kind: shiki writes `--shiki-look: var(--code-keyword)` once,
  and each look's `--code-*` in `prose.css` say what its keywords are, so a
  look more costs the HTML nothing.
- Fonts are installed, never fetched from a CDN: the site makes no request a
  reader did not ask for, analytics included.
- `src/lib/content/` runs at build time only and imports no `$app` module, so
  `bun test` runs it as it is.
- The landing page's numbers are read from the SVG cards the README shows,
  never typed again, so the two cannot disagree.
- The README's headline, pitch, sample and engines are the landing page's:
  `task readme` writes them from `web/landing.md` between `<!-- landing:… -->`
  markers, into the SDK packages' READMEs too in their own language, and the
  tests fail when they differ or when a link in one leads nowhere. A
  package's links go to GitHub, since npm and PyPI follow no relative link.
- The server takes the SDK from `../sdk/js`, unpublished, and `bun.lock`
  records the SDK's own dependencies: a change to `sdk/js/package.json` needs
  `bun install` here too, or CI's frozen install refuses.

## Commands

From the repository's top, each a task:

```sh
task web          # check, test and build, as CI's web job does
task web:dev      # the site on :5173, reloading as docs/ changes
task web:serve    # the last build as production serves it, into web/data
task web:image    # the image: tinystore, the server and the build
task readme       # the landing page's words, written into the README
task reference    # the API pages, written from the Go, Bun and Python source
```

`SITE_ORIGIN` is the address the canonical links, the sitemap and llms.txt
carry, and `SITE_BASE` the path a proxy serves the site under, which it strips
before the Bun server sees a request; `SITE_PORTABLE=1 SITE_OUT=build-preview` builds a copy that opens from
any folder. The Site workflow builds the image on each push to main that
changes what the site is built from and pushes it to GHCR, and the server
pulls it within a minute.
