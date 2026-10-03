---
name: docs
description: Use when writing or changing a page of the documentation in docs/, or the site in web/ that reads it - a page's place in docs/README.md, its code in Go, Bun and Python, callouts, tables, links and images, what agents get from it, and how to check it renders before pushing.
---

# Writing a page of the docs

A page is a markdown file under `docs/`, read on GitHub as it is and built
into the site by `web/`. Write it for GitHub first; the site reads the same
file and adds only what GitHub ignores. `web/AGENTS.md` has the site's side.

## Where a page lives

1. Put the file where its section's pages live: `docs/store/kv.md`.
2. In `docs/README.md`, turn its item into a link: `- KV` becomes
   `- [KV](store/kv.md)`. An item without a link is a page not written yet,
   and the site shows it greyed out; the order of the list is the sidebar's.
3. The page's address is its path: `/docs/store/kv`, and `/docs/store/kv.md`
   for its markdown.

No front matter: GitHub draws it as a table at the top of the page.

## Its shape

- `# Title`, then one paragraph that says what the page is about. That
  paragraph is the lead under the title, the description search engines
  show and the text a link preview carries.
- `##` for each section, which "On this page" lists; `###` within one.
- A heading's anchor is the one GitHub gives it, `## Read it back` →
  `#read-it-back`, so links written for GitHub land here too. Renaming a
  heading breaks the links to it; the build names them.

## Code in three languages

Fences written one after another, with nothing between them, are one block:
a tab for each language, the reader's choice kept for every page.

````md
```ts title="metrics.ts"
export const requests = store.metrics.counter('http_requests_total')
```

```python title="metrics.py"
requests = store.metrics.counter("http_requests_total")
```

```go title="metrics.go"
requests := store.Counter("http_requests_total")
```
````

- `ts` is the Bun tab, `python` Python's, `go` Go's, in whatever order they
  are written; the switch shows Bun, Python, Go.
- `title="…"` names the file. Several titled fences of one language are files
  of the same block; two untitled fences of one language stay two blocks.
- A shell command joins a language when it says which: ` ```sh for=bun `.
- Every call is the API as it is. Check each against `docs/sdk.md` and the
  engine's README, and give all three languages or say why one is missing:
  a reader who picked Python and finds only Go has been told nothing.
- Prose names no one language's method: `counter`, `with` and `labels` are
  three spellings of one idea, so say what it does.

## Callouts, tables, links

- A callout is GitHub's alert, labelled by a bold first line:

  ```md
  > [!NOTE]
  > **Counters**
  > `increase` is for counters.
  ```

  The kinds are GitHub's: NOTE, TIP, IMPORTANT, WARNING, CAUTION.
- A table whose header row is empty, `| | |`, is drawn as rows without a
  header. A value in backticks is drawn in mono.
- A link is relative to the file, as on GitHub. A page stays on the site,
  any other file of the repository opens on GitHub at the commit the site was
  built from, and an image is served by the site. A link that leads nowhere,
  or a heading that is not there, fails the build.
- `<!-- … -->` is a note to whoever edits the file: GitHub and the site show
  nothing, and the markdown copy leaves it out.

## What an agent gets

Every page's markdown, links made absolute, at its address with `.md`;
`/llms.txt` lists them by section, `/llms-full.txt` holds them all. The
"Copy as Markdown" button copies the same text. Nothing extra is written for
it: a page that reads well reads well to an agent.

## Checking it

```sh
task web:dev   # the site on :5173, reloading as docs/ changes
task web       # what CI runs: the checks, the tests and the build
```

The build is the link checker: run `task web` before pushing a page. The
prose follows the root `AGENTS.md`, and a page never describes what is not
built as though it works.
