# The release

What a release of the Rust branch builds, from which machines, into which
packages, and how it reaches each registry. It replaces the Go release
candidates' tool, `internal/release`, which built Go binaries and went with
the Go engine. [ffi.md](ffi.md)'s "What ships" is the promise this keeps.

## One run

A release is one run of `.github/workflows/release.yml`, dispatched from
`main` with its version and whether to
publish. Nothing is tagged or published by hand.

1. CI runs on the commit unless a push passed it already.
2. Seven builds, one a target, each on the machine below, make the target's
   executable and its three libraries.
3. One job packs them into the archives, npm's packages and PyPI's wheels,
   with `tools/release.ts`, and writes `SHA256SUMS` and the notes.
4. A smoke test a platform installs what was packed into an empty project,
   with nothing of the repository's on `PATH`, and opens a store through its
   core and through a private child, from Bun, Node and Python.
5. Asked to publish, the run waits for a reviewer of the `release`
   environment; once approved it pushes the tags, publishes npm, PyPI,
   crates.io and the server image, and makes the GitHub release last.

A run that does not publish keeps `dist` as its artifact, to try by hand.

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
  core's static library, which [ffi.md](ffi.md) once promised, is not built.

## Tags

`vX.Y.Z` on the commit CI passed, and `sdk/go/vX.Y.Z`, the Go SDK's module
in its directory, pushed together with `git push --atomic`. A tag is never
moved: a mistake is the next patch.

## Once, before the first release

- The environments `release`, with the maintainers as its reviewers, `npm`
  and `pypi`, each allowing the branch releases are dispatched from.
- npm: the scope `@tinyshed`; the first publish takes `NPM_TOKEN`, then each
  of the eight packages trusts `release.yml` and the token goes.
- PyPI: a pending trusted publisher for `tinyshed-tinystore`, the workflow
  `release.yml`, the environment `pypi`.
- crates.io: the environment `crates`; the first publish takes
  `CARGO_REGISTRY_TOKEN`, a token scoped to publishing new crates, then
  `tinystore` and `tinystore-cli` each trust `release.yml` in the
  environment `crates` and the token goes.
- GHCR: the package made public after its first push.
- A tag ruleset for `v*` and `sdk/go/v*` that refuses moving or deleting
  them.
