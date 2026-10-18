# https://just.systems: the recipes a contributor runs. CI calls cargo itself.

# every recipe runs in PowerShell on Windows, so none needs Git's sh there
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

library := if os() == "windows" { "tinystore_ffi.dll" } else if os() == "macos" { "libtinystore_ffi.dylib" } else { "libtinystore_ffi.so" }
binary := if os() == "windows" { "tinystore.exe" } else { "tinystore" }

# the core the Bun SDK loads in its own process, and the tinystore it starts as a private child or a sidecar
export TINYSTORE_LIBRARY := justfile_directory() / "target" / "release" / library
export TINYSTORE_BIN := justfile_directory() / "target" / "release" / binary

# the core's library as Python's extension module, the name an abi3 build takes on each platform;
# it links no libpython, whose symbols the interpreter that loads it has
python_module := if os() == "windows" { "_core.pyd" } else { "_core.abi3.so" }
export PYO3_BUILD_EXTENSION_MODULE := "1"

# a go.work left from the Go engine names modules that are gone
export GOWORK := "off"

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
    cargo test -p tinystore --lib --locked --features replication-tls replication::link_tests::nodes_linked

# run clippy, a warning failing it, on the workspace and on the library built with each engine alone, the
# console alone, the wire alone and every engine without the wire; CI lints on each platform, since cfg
# hides one's code from another
lint: && (lint-alone "console") (lint-alone "typos") (lint-alone "kv") (lint-alone "config") (lint-alone "locks") (lint-alone "channels") (lint-alone "jobs") (lint-alone "sql") (lint-alone "blobs") (lint-alone "records") (lint-alone "metrics") (lint-alone "search") (lint-alone "wire") (lint-alone "kv,backup") (lint-alone "kv,channels") (lint-alone "kv,replication") (lint-alone "kv,replication,replication-tls") (lint-alone "kv,config,locks,channels,jobs,sql,blobs,records,metrics,search,backup,replication")
    cargo clippy --workspace --all-targets --locked -- -D warnings

# run clippy on the library built with these features alone
lint-alone features:
    cargo clippy -p tinystore --no-default-features --features {{features}} --all-targets --locked -- -D warnings

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

# read and write every vector through the Go codec, which its test holds to gofmt's layout
[working-directory: 'internal/wire']
codec-go:
    go vet .
    go test .

# read and write every vector through the Python codec
[working-directory: 'sdk/python']
codec-python:
    uv run pytest tests/test_protocol.py --noconftest -q

# build the core's library that bun:ffi loads, and tinystore
build:
    cargo build --release --locked -p tinystore-ffi -p tinystore-cli

# run the Bun SDK's kv, jobs, sql, files, events, metrics, configs, backups, locks, channels and search over the core in its own process, a private child, a sidecar and a remote server, its test clock, its codecs against the vectors, and its console lines
[working-directory: 'sdk/js']
sdk: build
    bun install --frozen-lockfile
    bun test test/pipe.test.ts test/kv.test.ts test/jobs.test.ts test/sql.test.ts test/blobs.test.ts test/queries.test.ts test/clock.test.ts test/direct.test.ts test/durability.test.ts test/encrypted.test.ts test/protocol.test.ts test/console.test.ts test/events.test.ts test/logger.test.ts test/background.test.ts test/metrics.test.ts test/config.test.ts test/backup.test.ts test/locks.test.ts test/channels.test.ts test/search.test.ts test/node.test.ts

# build the core's library as the Python SDK's tinystore._core, beside its sources: a build apart from Bun's, since it links Python
python-core:
    cargo build --release --locked -p tinystore-ffi --features python
    cp target/release/{{library}} sdk/python/src/tinystore/{{python_module}}

# run the Python SDK over the core in its own process, a private child, a sidecar and a remote server, and its codecs
[working-directory: 'sdk/python']
sdk-python: build python-core
    uv run --frozen pytest -q

# lint, format-check and typecheck the Python SDK
[working-directory: 'sdk/python']
sdk-python-check:
    uv run --frozen ruff check src tests
    uv run --frozen ruff format --check src tests
    uv run --frozen pyright

# lint and typecheck the Bun SDK
[working-directory: 'sdk/js']
sdk-check:
    bun install --frozen-lockfile
    bun run check

# lint and test every crate in a Linux container, for a host that is not Linux
test-linux:
    docker run --rm -v "{{justfile_directory()}}:/src" {{rust_caches}} -w /src {{rust_image}} sh -c 'cargo clippy --workspace --all-targets --locked -- -D warnings && for features in console typos kv config locks channels jobs sql blobs records metrics search wire kv,backup kv,channels kv,replication kv,replication,replication-tls kv,config,locks,channels,jobs,sql,blobs,records,metrics,search,backup,replication; do cargo clippy -p tinystore --no-default-features --features $features --all-targets --locked -- -D warnings || exit 1; done && cargo test --workspace --locked && cargo test -p tinystore --lib --locked --features replication-tls replication::link_tests::nodes_linked'

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
# a fuzz target of fuzz/ for `time` seconds, by hand, in a Linux container:
# libFuzzer needs nightly, and on Windows Visual Studio's AddressSanitizer too
fuzz target="records_rows" time="60":
    docker run --rm -v "{{justfile_directory()}}:/src" {{rust_caches}} -w /src/fuzz {{rust_image}} sh -c 'rustup toolchain install nightly --profile minimal >/dev/null && cargo +nightly install cargo-fuzz --locked --quiet && cargo +nightly fuzz run {{target}} -- -max_total_time={{time}}'

check: fmt-check lint test protocol-check codec-go codec-python sdk sdk-check sdk-python sdk-python-check lint-actions

# remove build output
clean:
    cargo clean
