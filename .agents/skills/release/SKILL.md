---
name: release
description: Use when preparing, trying or publishing a TinyStore release - the Release workflow, the three Go tags, the binaries, npm, PyPI and the server image - or when changing internal/release, the release tasks or the workflows.
---

# Releasing TinyStore

One version for everything, since everything speaks one protocol: the three Go
modules, the binary, npm, PyPI and the server image. A release is one run of
the Release workflow (`.github/workflows/release.yml`); nothing is tagged or
published by hand.

Tagging and publishing are outward-facing. Dispatch with publish on, or approve
the run, only when the user has said to, for that version.

## Releasing

1. CI is green on main, and the documents describe nothing unbuilt as working.
2. Try it: Actions, Release, run from main with the version and publish off.
   It runs CI on the commit, makes the three tags in its runner, builds
   everything from them, and keeps `dist` and `tags` as the run's artifacts.
   Try `dist` as below.
3. Publish: the same with publish on. Once the build has passed, the run waits
   for a reviewer of the `release` environment. Approving pushes the three tags
   at once; npm, PyPI and the image publish; the GitHub release comes last,
   with `dist/NOTES.md`, once what it announces is there.

A version is `vX.Y.Z` or `vX.Y.Z-alpha.N`, `-beta.N`, `-rc.N`, the
pre-releases every registry spells (PyPI's `0.1.0rc1` is stamped for it). It
comes after every release before it and is on neither npm nor PyPI yet; the
workflow checks both before it tags.

## What a release is

| | |
|---|---|
| `vX.Y.Z` | the root module, at the commit CI passed |
| `server/vX.Y.Z` | that commit, `server/go.mod` requiring the root at `vX.Y.Z` without its `replace` |
| `cmd/tinystore/vX.Y.Z` | then `cmd/tinystore/go.mod` requiring both; the binaries build here and stamp `vX.Y.Z` |
| `tinystore_<v>_<os>_<arch>.tar.gz`, `.zip`, `SHA256SUMS` | the GitHub release's assets |
| `@tinyshed/tinystore-<os>-<cpu>` and `tinystore` on npm | the binary a platform, and the SDK naming those six as optional dependencies |
| `tinyshed-tinystore` on PyPI | a wheel a platform with the binary in `tinystore/bin/`, a pure wheel, an sdist |
| `ghcr.io/tinyshed/tinystore:<v>` | the linux binaries, amd64 and arm64, on distroless static (`internal/release/Dockerfile`) |

Six platforms: linux, darwin and windows, amd64 and arm64. The binary is
static, so one Linux wheel serves glibc and musl alike, and Go 1.27 needs
macOS 12. The SDKs look for the binary exactly there (`packagedBinary`,
`resources.files("tinystore") / "bin"`).

A release never writes main. The `replace` directives that development needs,
and that a required module ignores and `go install` refuses, are dropped by two
commits that exist only under their tags. The SDKs' manifests say `0.0.0`:
`internal/release` stamps the version into what it builds, and each SDK reads
its own back (`package.json`, `importlib.metadata`), so a package cannot say
another version than the one it was published as.

## Why it is safe

- Nothing leaves the runner before CI has passed on the commit and everything
  has built from the tags. `go mod tidy` reads the root and the server at
  their new tags from the runner's repository through git, `GOPRIVATE` and a
  `url.<repo>.insteadOf` in its environment, so `go.sum` holds what the module
  proxy computes once the tags are pushed. It tidies in a module cache of its
  own, since a cache that once held another commit under the same version
  would hand that back.
- The three tags go in one `git push --atomic`, all or none, from the bundle
  the build made, and the root tag is checked to be the commit CI tested.
- Tagging is deterministic: the two commits and three tags take HEAD's date,
  so tagging the same HEAD again makes the same objects.
- A tag is never moved or deleted once pushed: the module proxy and the
  registries keep what they saw. A mistake is the next patch version.
- A publish that failed is run again with "Re-run failed jobs", which reuses
  the build's artifacts: npm skips the packages it has, PyPI skips the files
  it has, and the GitHub release is made once. Never run the build again once
  its tags are pushed.

## Trying it locally

- `task release:snapshot -- v0.1.0` builds `dist/` from the working tree,
  whatever version the binaries stamp.
- `task release:tag -- v0.0.1-rc.1`, in a scratch clone, makes the tags there
  and prints how to build from them and how to undo it. Give the build a
  `GOMODCACHE` of its own: a version made to try must never reach a cache a
  real release would later read.
- Install from `dist/` into an empty project, with no `TINYSTORE_BIN` and no
  `tinystore` on `PATH`, and open a store privately and through its sidecar:
  `bun add dist/npm/tinystore-<v>.tgz dist/npm/tinyshed-tinystore-<os>-<cpu>-<v>.tgz`,
  and `uv pip install --no-index --find-links dist/pypi tinyshed-tinystore`,
  which also shows that the platform's wheel is the one chosen. Do it on
  Windows, in `debian:stable-slim` and in `python:3.12-alpine`, and run
  `uvx twine check dist/pypi/*`.
- The image: `linux-amd64/tinystore` and an empty `data/` beside
  `internal/release/Dockerfile`, `docker build`, then run
  `serve --dir /data --listen tcp://0.0.0.0:7443 --tokens /etc/tinystore/tokens`
  with a tokens file mounted, and connect an SDK with `connect`.
- `task lint:actions` before a workflow change is pushed: a release workflow
  that does not parse fails only when a release needs it.

## Once, before the first release

- Environments: `release` with the maintainers as required reviewers, the
  approval that pushes the tags; `npm` and `pypi`, each deploying from main
  only.
- npm: the organisation `tinyshed` owns the scope. npm can trust a workflow
  only for a package that exists, so the first release publishes with the
  `NPM_TOKEN` secret of the `npm` environment; then set each of the seven
  packages to trust `release.yml` on npmjs.com and delete the secret.
- PyPI: a pending trusted publisher for `tinyshed-tinystore`: the repository
  `tinyshed/tinystore`, the workflow `release.yml`, the environment `pypi`.
- GHCR: after the first push, make the package `tinystore` public if it is not.
- A tag ruleset for `v*`, `server/v*` and `cmd/tinystore/v*` that forbids
  moving or deleting them, GitHub Actions allowed to create them.
