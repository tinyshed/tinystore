---
name: release
description: Use when preparing, trying or publishing a TinyStore release - the Release workflow, the seven targets, tools/native.sh, tools/stamp.sh, tools/release.ts and tools/smoke.sh, npm's packages, PyPI's wheels, the crates on crates.io, the GitHub release, the server image and the tags - or when changing any of them.
---

# Releasing TinyStore

[plan/release.md](../../../plan/release.md) is the contract: the targets,
the machines that build them, what each package carries and how it reaches
its registry. This skill is how to work on it without publishing by mistake.

One version covers everything: the executable, the core's library, the Node
addon, Python's module, the npm packages, the wheels, the image and the Go
SDK's tag. Nothing is released yet, and the Go engine's candidates
`0.1.0-rc.1` to `rc.5` already sit on npm and PyPI under the same names, so
the first Rust release comes after them: `v0.1.0-rc.6` or `v0.1.0`.

Tagging and publishing are outward-facing and cannot be taken back. Dispatch
with publish on, or approve the `release` environment, only when the user has
said to, for that version.

## The run

A release is one run of `.github/workflows/release.yml`, dispatched from
`main` with the version and publish.

1. `version` refuses a version npm, PyPI, crates.io or a tag has already, and one
   published from another branch.
2. `ci` runs on the commit unless a push passed it already.
3. `build`, seven jobs: `tools/native.sh` stamps the version into the
   workspace through `tools/stamp.sh` and builds the target's `tinystore` and its core three times,
   plain, with `node` and with `python`, into `native/<target>/`. Linux's run
   in `manylinux_2_28` and `alpine` through `docker run`.
4. `package`: `tools/release.ts` packs `native/` into `dist/`: the archives,
   `npm/*.tgz`, `pypi/*.whl`, `SHA256SUMS`, `NOTES.md`.
5. `smoke` and `smoke-musl`: `tools/smoke.sh` installs `dist/` into empty
   projects and opens a store embedded and as a private child, from Node, Bun
   and Python. Intel macOS is not tried: no machine runs it.
6. With publish: `tags` waits for a reviewer, then pushes `vX.Y.Z` and
   `sdk/go/vX.Y.Z` atomically; `npm`, `pypi`, `crates` and `image` publish; `github`
   makes the release last, with the archives and `SHA256SUMS`.

Without publish the run stops after the smoke tests and keeps `dist` as an
artifact: that is how a release is tried in CI.

## Trying it here

- `just release-native <version> <target> <triple>` builds this machine's
  target into `native/`. It rewrites `Cargo.toml` and `Cargo.lock`'s
  version, so run it in a clone or a worktree (`git worktree add`), never in
  the checkout you work in.
- Linux from Windows or macOS: in the worktree,
  `MSYS_NO_PATHCONV=1 docker run --rm -v "<worktree>:/src" -w /src -e VERSION -e TARGET -e TRIPLE quay.io/pypa/manylinux_2_28_x86_64 sh tools/native.sh`;
  Alpine for musl. A bind mount from Windows is slow: twenty minutes and
  more for three builds.
- `just release-pack <version> <target>` packs `native/` into `dist/` and
  runs the smoke test for `<target>` here.
- Try with a version nobody published, `v9.9.9-rc.1`: Bun and npm fetch an
  optional dependency the registry has under the same name and version, and
  `@tinyshed/tinystore-win32-x64@0.1.0-rc.1` is the Go engine's executable,
  which answers the Rust SDK `invalid message`.
- With no `node_modules`, Bun installs an import from the registry on its
  own: a test that found the old published SDK instead of the one packed
  failed with `store.bucket is not a function`. Look in `node_modules`
  before believing a result.
- `bunx tinystore` may run another `tinystore` than the package's: call
  `./node_modules/.bin/tinystore`.
- `uvx twine check dist/pypi/*` before a wheel reaches PyPI, which refuses
  what pip installs: a RECORD that lists a file after its bytes, a tag it
  does not know.
- Before pushing a workflow: `just lint-actions`. A release workflow that
  does not parse fails only when a release needs it.

## Why it is safe

- Nothing leaves the runners before every target built and the smoke tests
  passed on what was packed; the reviewers of `release` see that run first.
- The two tags go in one `git push --atomic`, both or neither, on the commit
  CI passed. A tag is never moved: a mistake is the next patch.
- A publish that failed is run again with "Re-run failed jobs", which reuses
  `dist`: npm skips what it has, PyPI skips files it has, `crates` skips a
  crate whose version crates.io has, the GitHub release
  is made once. Never build again once the tags are pushed: what the build
  made wrong is the next version.
- The repository keeps `0.0.0` everywhere; only a build stamps a version, and
  each package reads its own back (`package.json`, `importlib.metadata`).

## Once, before the first release

plan/release.md lists it: the environments `release`, `npm` and `pypi`;
npm's first publish through `NPM_TOKEN`, then trusted publishing for each of
the eight packages; PyPI's pending trusted publisher; GHCR's package made
public; the environment `crates` and crates.io's first publish through
`CARGO_REGISTRY_TOKEN`, then trusted publishing for `tinystore` and
`tinystore-cli`; a tag ruleset for `v*` and `sdk/go/v*`.

## Not built yet

Go's modules a platform with the core's static library (the SDK fetches the archive instead),
a `tinystore` console script in the wheel, Windows on ARM.
