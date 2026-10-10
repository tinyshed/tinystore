# https://just.systems: the recipes a contributor runs. CI calls cargo itself.

# every recipe runs in PowerShell on Windows, so none needs Git's sh there
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

library := if os() == "windows" { "tinystore_ffi.dll" } else if os() == "macos" { "libtinystore_ffi.dylib" } else { "libtinystore_ffi.so" }
binary := if os() == "windows" { "tinystore.exe" } else { "tinystore" }

# the core the Bun SDK loads in its own process, and the tinystore it starts as a private child or a sidecar
export TINYSTORE_LIBRARY := justfile_directory() / "target" / "release" / library
export TINYSTORE_BIN := justfile_directory() / "target" / "release" / binary

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

# write the protocol's codecs and vectors again from protocol/*.wire
protocol:
    cargo run --locked -p tinystore-protocol

# fail when a file the schema writes is not what it writes
protocol-check:
    cargo run --locked -p tinystore-protocol -- --check

# build the core's library that bun:ffi loads, and tinystore
build:
    cargo build --release --locked -p tinystore-ffi -p tinystore-cli

# run the Bun SDK's kv and jobs over the core in its own process, a private child, a sidecar and a remote server, its test clock, and its codecs against the vectors
[working-directory: 'sdk/js']
sdk: build
    bun install --frozen-lockfile
    bun test test/pipe.test.ts test/kv.test.ts test/jobs.test.ts test/sql.test.ts test/queries.test.ts test/clock.test.ts test/direct.test.ts test/durability.test.ts test/protocol.test.ts

# lint and typecheck the Bun SDK
[working-directory: 'sdk/js']
sdk-check:
    bun install --frozen-lockfile
    bun run check

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
check: fmt-check lint test protocol-check sdk sdk-check lint-actions

# remove build output
clean:
    cargo clean
