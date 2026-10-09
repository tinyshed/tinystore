# https://just.systems: the recipes a contributor runs. CI calls cargo itself.

# every recipe runs in PowerShell on Windows, so none needs Git's sh there
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

library := if os() == "windows" { "tinystore_ffi.dll" } else if os() == "macos" { "libtinystore_ffi.dylib" } else { "libtinystore_ffi.so" }

# the SDK's suite builds a Go server in its preload unless this names one
export TINYSTORE_BIN := "unused"
export TINYSTORE_LIBRARY := justfile_directory() / "target" / "release" / library

# the Linux the container recipes run in; named volumes keep cargo's registry
# and the build between runs, so a second run compiles only what changed
rust_image := "rust:1.99"
rust_caches := "-v tinystore-cargo:/usr/local/cargo/registry -v tinystore-target:/target -e CARGO_TARGET_DIR=/target"

# list the recipes
default:
    @just --list

# run every crate's tests
test:
    cargo test --workspace --locked

# run clippy, a warning failing it; CI lints on each platform, since cfg hides one's code from another
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# format the code
fmt:
    cargo fmt --all

# verify formatting without writing changes
fmt-check:
    cargo fmt --all --check

# build the core's library that bun:ffi loads
library:
    cargo build --release --locked -p tinystore-ffi

# run the Bun SDK over the core in its own process, through bun:ffi
[working-directory: 'sdk/js']
pipe: library
    bun install --frozen-lockfile
    bun test test/pipe.test.ts

# lint and test every crate in a Linux container, for a host that is not Linux
test-linux:
    docker run --rm -v "{{justfile_directory()}}:/src" {{rust_caches}} -w /src {{rust_image}} sh -c "cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked"

# lint the GitHub workflows
lint-actions:
    docker run --rm -v "{{justfile_directory()}}:/repo" -w /repo rhysd/actionlint:1.7.12 -color

# run the docs site with live reload on :5173
[working-directory: 'web']
web-dev:
    bun install
    bun run dev

# align every markdown table of the repository as an IDE formats it
[working-directory: 'web']
tables:
    bun install --frozen-lockfile
    bun run tables

# everything CI gates on
check: fmt-check lint test pipe lint-actions

# remove build output
clean:
    cargo clean
