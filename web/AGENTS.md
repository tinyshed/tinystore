# TinyStore site

The documentation in `docs/` as a site: SvelteKit 2 with Svelte 5 runes,
every page prerendered by `adapter-static`, and a Bun server that hands the
files out and keeps the site's own analytics in a TinyStore of its own. The
root [AGENTS.md](../AGENTS.md) applies here too; this file covers what is
particular to the site. How a page is written is the `docs` skill.

## Status

Built: the landing page, a page for every file `docs/README.md` links, search,
both themes, the language switch, a markdown copy of each page, `llms.txt`,
`llms-full.txt`, the API pages with `llms-<language>.txt`, the sitemap, and
the server with its analytics. The docs
themselves are the design documents until the guides are written; `Metrics`
under Store is the one guide, and the shape the others follow.

Not built: a page of the site's own statistics.

It is served at https://tinyshed.org/tinystore: built with
`SITE_BASE=/tinystore`, behind the Caddy and the Cloudflare Tunnel that the
private `tinyshed/landing` repository runs on the server.

## The docs are the source, the site a reading of them

**`docs/README.md` is the sidebar.** Each second-level heading is a section,
each item of its list an entry in order: a link to a file under `docs/` is a
page at the same path, `docs/store/metrics.md` → `/docs/store/metrics`; a link
anywhere else is a link out; an item without a link is a page not written yet,
shown and not linked. GitHub shows the same file as the table of contents.

**Whatever reads on GitHub reads here.** A page is plain GitHub markdown: no
front matter, which GitHub would draw as a table, and nothing GitHub would
show as noise. What the site adds it reads from what GitHub already ignores:

| Written                                       | On GitHub             | Here                                          |
|-----------------------------------------------|-----------------------|-----------------------------------------------|
| fences one after another, a language each     | one under another     | one block, the language the reader picked     |
| ` ```ts title="metrics.ts" `                  | a TypeScript block    | a tab named `metrics.ts`                      |
| ` ```sh for=bun `                             | a shell block         | the Bun variant of its block                  |
| `> [!NOTE]` then a **bold** line              | GitHub's note         | a callout labelled by the bold line           |
| a table whose header row is empty, `\| \| \|` | a table               | a card of rows, no header                     |
| `[kv](kv.md#expiry)`                          | the file, its heading | `/docs/kv#expiry`, the heading checked        |
| `[code](../kv/kv.go)`                         | the file              | GitHub, at the commit the site was built from |
| `<!-- a note -->`                             | nothing               | nothing, and left out of the markdown copy    |

**A link that leads nowhere fails the build.** Every relative link and image
is resolved at build time against the repository, and a heading against the
id GitHub gives it; the prerender then follows every link it renders.

**Code is highlighted once, when the site is built.** Shiki writes both
themes' colours into each token, so the page highlights and changes theme with
no script. Every language of a block is in the HTML; `app.html` names the
reader's language before the first paint and the stylesheet shows it.

## The API pages are the source's

`docs/reference/<language>/<engine>.md` and their index `docs/reference/api.md`
are generated: `task reference` writes them, and `bun test` fails when one is
not what the source says. Edit the doc comments, never the pages.

| Language | Read by                                               | Public is                                     |
|----------|-------------------------------------------------------|-----------------------------------------------|
| Go       | `web/reference/go`, `go/doc` and its markdown printer | exported                                      |
| Bun      | `src/lib/content/reference.ts`, the emitted `.d.ts`   | exported by `index.ts`, or a field of `Store` |
| Python   | `web/reference/python.py`, `ast` without importing    | in `__all__`, or set on `Store` in `__init__` |

An engine's modules in each language are `sources` in `reference.ts`, its
name and guide `src/lib/content/api.ts`. The sidebar lists the index alone;
the pages switch between languages and engines, and `llms-full.txt` leaves
them to `llms-bun.txt`, `llms-python.txt` and `llms-go.txt`, one per
language, since an agent needs one of the three. The test needs Go and
Python 3.12 or later beside Bun, as CI's web job has them.

## The server

In `server/`, `config.ts` reads the environment through zod, `bootstrap.ts`
is the only composition root, and `index.ts` only starts and stops. It serves `build/` from an index made at start, brotli or
gzip as the build made them, and counts what a reader asked for:

| Kept                                 | Where                     | What                                                                                 |
|--------------------------------------|---------------------------|--------------------------------------------------------------------------------------|
| `site_views_total{page, via, agent}` | metrics                   | a page, its data on navigation, or its markdown; a person, a crawler or an agent     |
| `site_events_total{name}`            | metrics                   | what a page's script reports: copies, searches, the language                         |
| `view`, `copy-code`, …               | records, stream `readers` | each of those, with the day's visitor id, where the reader came from, their language |
| the server's own lines               | records, stream `site`    | and on stderr, as JSON in a container                                                |
| `salt/<day>`, `site-events`          | kv                        | the day's salt, expiring; the limiter a reader's events go through                   |

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
  `--ts-*` and nothing else. Both themes are the design's own.
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
