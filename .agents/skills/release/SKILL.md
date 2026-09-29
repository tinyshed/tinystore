---
name: release
description: Use when preparing, building, checking or publishing a TinyStore release - a tag of the Go modules, the tinystore binaries, the npm packages or the Python wheels - or when changing internal/release, the release tasks or the release workflow.
---

# Releasing TinyStore

One version for everything: the three Go modules, the binary, npm and PyPI,
since they all speak one protocol. A release is a tag; nothing is tagged
before `task check`, `task sdk` and the race suite pass on the commit.

## What a release publishes

`task release:build -- vX.Y.Z` (`internal/release`) writes into `dist/`:

| | |
|---|---|
| `tinystore_<v>_<os>_<arch>.tar.gz`, `.zip` for Windows, `SHA256SUMS` | the GitHub release's assets |
| `npm/tinyshed-tinystore-<os>-<cpu>-<v>.tgz` | a package a platform, `os` and `cpu` set, the binary in `bin/` |
| `npm/tinystore-<v>.tgz` | `sdk/js`, with those six as `optionalDependencies` of the same version |
| `pypi/*.whl`, `pypi/*.tar.gz` | a wheel a platform with the binary in `tinystore/bin/`, a pure wheel and an sdist for the rest |

Six platforms: linux, darwin and windows, amd64 and arm64. The binary is
static, so one Linux wheel is tagged for glibc and musl alike, and Go 1.27
needs macOS 12. The SDKs look for the binary exactly there
(`packagedBinary`, `resources.files("tinystore") / "bin"`).

The build is reproducible: the same checkout gives the same bytes, archives,
tarballs and wheels included. `internal/release` writes the npm tarballs
itself, since `npm pack` on Windows drops the executable bit.

## Trying it

```sh
task release:snapshot -- v0.1.0
```

builds from the working tree, whatever version the binaries stamp. Then
install from `dist/` into an empty project, with no `TINYSTORE_BIN` and no
`tinystore` on PATH, and open a store privately and through its sidecar:
`bun add dist/npm/tinystore-<v>.tgz dist/npm/tinyshed-tinystore-<os>-<cpu>-<v>.tgz`,
and `uv pip install --no-index --find-links dist/pypi tinyshed-tinystore`,
which also shows that the platform's wheel is the one chosen. Do it on
Windows, in `debian:stable-slim` and in `python:3.12-alpine`, and run
`uvx twine check dist/pypi/*`.

## The Go tags

`server` and `cmd/tinystore` reach the root through a `replace`, which a
module that depends on them ignores and `go install …@version` refuses. So a
release requires real versions, in this order, each tag pushed before the
next step needs it:

1. The versions in `sdk/js/package.json` and `sdk/python/pyproject.toml` say
   the release (`0.1.0-rc.1` for npm is `0.1.0rc1` for PyPI); commit.
2. Tag the root `vX.Y.Z` there.
3. `server/go.mod` requires the root at `vX.Y.Z` without the `replace`,
   `go mod tidy`; commit; tag `server/vX.Y.Z`.
4. `cmd/tinystore/go.mod` requires both at `vX.Y.Z` without its `replace`,
   `go mod tidy`; commit; tag `cmd/tinystore/vX.Y.Z`.
5. Build the release from a clean checkout of `cmd/tinystore/vX.Y.Z`, whose
   binaries then stamp `vX.Y.Z`; `internal/release` refuses any other.
6. A commit that puts both `replace`s back, for development.

A tag is never moved or deleted once pushed: the module proxy and the
registries keep what they saw. A mistake is the next patch version.

## Publishing

Tagging and publishing are outward-facing: ask before each. npm packages
publish from their tarballs, the six platforms before `tinystore`, a
pre-release under `--tag next`; PyPI takes the wheels and the sdist. Both
publish through trusted publishing from the release workflow once it exists,
never with a token kept on a machine.
