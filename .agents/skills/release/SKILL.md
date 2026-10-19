---
name: release
description: Use when preparing, trying or publishing a TinyStore release - the Release workflow, the seven targets, tools/native.sh, tools/stamp.sh, tools/release.ts and tools/smoke.sh, npm's packages, PyPI's wheels, the crates on crates.io, the GitHub release, the server image and the tags - or when changing any of them.
---

# Releasing TinyStore

This skill is the contract of a release, the targets, the machines that
build them, what each package carries and how it reaches its registry, and
how to work on it without publishing by mistake.

One version covers everything: the executable, the core's library, the Node
addon, Python's module, the npm packages, the wheels, the crates, the image
and the Go SDK's tag. Nothing is released yet, and the Go engine's candidates
`0.1.0-rc.1` to `rc.5` already sit on npm and PyPI under the same names, so
the first Rust release comes after them: `v0.1.0-rc.6` or `v0.1.0`.

Tagging and publishing are outward-facing and cannot be taken back. Dispatch
with publish on, or approve the `release` environment, only when the user has
said to, for that version.

## Targets

| Target             | Rust's triple                | Machine                                         | Linked against               |
|--------------------|------------------------------|-------------------------------------------------|------------------------------|
| `linux-x64`        | `x86_64-unknown-linux-gnu`   | `ubuntu-24.04`, in `manylinux_2_28`'s container | glibc 2.28                   |
| `linux-arm64`      | `aarch64-unknown-linux-gnu`  | `ubuntu-24.04-arm`, in `manylinux_2_28`'s       | glibc 2.28                   |
| `linux-x64-musl`   | `x86_64-unknown-linux-musl`  | `ubuntu-24.04`, in `alpine:3.22`                | musl, dynamically            |
| `linux-arm64-musl` | `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm`, in `alpine:3.22`            | musl, dynamically            |
| `darwin-arm64`     | `aarch64-apple-darwin`       | `macos-15`                                      | macOS 11                     |
| `darwin-x64`       | `x86_64-apple-darwin`        | `macos-15`, cross                               | macOS 10.13                  |
| `win32-x64`        | `x86_64-pc-windows-msvc`     | `windows-2025`                                  | the MSVC runtime, statically |

- glibc 2.28 is RHEL 8 and Debian 10: `manylinux_2_28` is the oldest image
  that still builds, and its wheels say so in their tag.
- musl's builds are native, in Alpine, with `-C target-feature=-crt-static`:
  a `cdylib` needs musl dynamic, and Bun, Node and Python on Alpine load it
  so.
- macOS on Intel is built on Apple silicon: the C of SQLite and of zstd
  cross-compile, and GitHub's Intel machines are being retired.
- Windows on ARM and 32-bit systems have no build.

## What a target builds

From one checkout, with the workspace's version stamped first, `cargo
build --release --locked`, three times for `tinystore-ffi`, once for each
feature set, each copied out before the next writes over it:

| File                                                   | Built as                             | Carried by                        |
|--------------------------------------------------------|--------------------------------------|-----------------------------------|
| `tinystore`, `tinystore.exe`                           | `-p tinystore-cli`                   | every package                     |
| `libtinystore_ffi.so` · `.dylib` · `tinystore_ffi.dll` | `-p tinystore-ffi`                   | npm, for Bun; the archive, for Go |
| `tinystore.node`                                       | `-p tinystore-ffi --features node`   | npm, for Node                     |
| `_core.abi3.so`, `_core.pyd`                           | `-p tinystore-ffi --features python` | the wheel                         |

## Packages

| Channel | What                                                                                                                                          |
|---------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| GitHub  | `tinystore-<v>-<target>.tar.gz`, `.zip` on Windows: `bin/` the executable, `lib/` the core's library and the Node addon; `SHA256SUMS`         |
| npm     | `@tinyshed/tinystore-<target>` a target, `bin/` and `lib/`, with `os`, `cpu` and `libc`; `@tinyshed/tinystore`, naming the seven as optional  |
| PyPI    | `tinyshed-tinystore`, one abi3 wheel a target for CPython 3.12 on: the SDK, `tinystore/_core` and `tinystore/bin/`                            |
| crates  | `tinystore`, the library, and `tinystore-cli`, the command, which names it at the same version exactly; `ffi` and `protocol` stay unpublished |
| GHCR    | `ghcr.io/tinyshed/tinystore:<v>`, the glibc executables for amd64 and arm64 on `gcr.io/distroless/cc-debian12`                                |

- An SDK finds what its platform's package carries before it asks
  `PATH`: the executable for a sidecar or a private child, and, for
  `embedded`, the core's library under Bun and the addon under Node. Linux's
  package is chosen by the C library the process runs on, glibc or musl,
  as npm chose it at install.
- A wheel's tag is the target's: `manylinux_2_28_x86_64`,
  `manylinux_2_28_aarch64`, `musllinux_1_2_x86_64`, `musllinux_1_2_aarch64`,
  `macosx_11_0_arm64`, `macosx_10_13_x86_64`, `win_amd64`. There is no
  sdist and no pure wheel: without the core the SDK runs nothing.
- The manifests say `0.0.0`; the release stamps its version into the
  workspace before it builds and into every package it writes. PyPI spells a
  pre-release its own way, `0.1.0rc1` for `v0.1.0-rc.1`.
- The Go SDK fetches its release's archive for its platform at the first
  open that finds no executable or library given, in `TINYSTORE_BIN` and
  `TINYSTORE_LIBRARY` or on `PATH`, checks it against `SHA256SUMS` and keeps
  `bin/` and `lib/` in the user's cache; `TINYSTORE_RELEASES` names a mirror
  or `off`, `TINYSTORE_CACHE` another cache. A module a platform with the
  core's static library, is not built.

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

## Set up once

Done on 19 October: the environments `release`, with the owner as its
reviewer, `npm` with `NPM_TOKEN`, `pypi` with PyPI's trusted publisher,
`crates` with `CARGO_REGISTRY_TOKEN`; GHCR's package public; the ruleset
`release tags`, which refuses moving or deleting `v*` and `sdk/go/v*`.

Left until the first release has published: trusted publishing on npm for
each of the eight packages and on crates.io for `tinystore` and
`tinystore-cli`, each trusting `release.yml` in its environment, and then
the two tokens deleted. A job uses its token while one is set, and OIDC
once it is gone.

## Where a Go program finds its core

A released Go SDK fetches `tinystore-<v>-<target>` from the release at the
first open that finds no executable or library given, in `TINYSTORE_BIN`
and `TINYSTORE_LIBRARY` or on `PATH`, checks it against `SHA256SUMS`, and
keeps `bin/` and `lib/` in the user's cache (`sdk/go/internal/core/fetch.go`).
An archive's name, its top folder and the two files' names are therefore a
promise to every released Go SDK: changing one breaks the versions before.
`go get tinyshed.org/tinystore` reads the module from `sdk/go` through the
`go-import` tag the site's server answers (`web/server/http.ts`).

## Not built yet

Go's modules a platform with the core's static library (the SDK fetches the archive instead),
Windows on ARM.
