# https://just.systems: the recipes a contributor runs. CI calls cargo itself.

# every recipe runs in PowerShell on Windows, so none needs Git's sh there
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

library := if os() == "windows" { "tinystore_ffi.dll" } else if os() == "macos" { "libtinystore_ffi.dylib" } else { "libtinystore_ffi.so" }
binary := if os() == "windows" { "tinystore.exe" } else { "tinystore" }

# the core the Bun SDK loads in its own process, and the tinystore it starts as a private child or a sidecar
export TINYSTORE_LIBRARY := justfile_directory() / "target" / "release" / library
export TINYSTORE_BIN := justfile_directory() / "target" / "release" / binary
# the core the SDK loads under Node, the same library built as its addon
export TINYSTORE_ADDON := justfile_directory() / "target" / "release" / "tinystore.node"

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

# check only what changed since the branch's upstream: fmt, clippy and the tests of each area touched,
# the lint sets that name it, and the SDK it reaches; minutes, where check takes half an hour
quick:
    bun tools/quick.ts

# run every crate's tests, the async API's among them, which no default feature turns on
test:
    cargo test --workspace --locked --features tinystore/async
    cargo test -p tinystore --lib --locked --features replication-tls -- replication::link_tests::nodes_linked replication::transport

# run clippy, a warning failing it, on the workspace and on the library built with each engine alone, the
# console alone, the wire alone, its client alone and beside metrics, the async API alone and beside each
# engine, and every engine with async and without the wire; CI lints on each platform, since cfg hides one's code from another
# contract: STORE-07
lint: && (lint-alone "console") (lint-alone "typos") (lint-alone "kv") (lint-alone "config") (lint-alone "locks") (lint-alone "channels") (lint-alone "jobs") (lint-alone "sql") (lint-alone "blobs") (lint-alone "records") (lint-alone "metrics") (lint-alone "system") (lint-alone "search") (lint-alone "wire") (lint-alone "client") (lint-alone "metrics,client") (lint-alone "kv,backup") (lint-alone "kv,channels") (lint-alone "sql,search") (lint-alone "wire,search") (lint-alone "kv,replication") (lint-alone "kv,replication,replication-tls") (lint-alone "async") (lint-alone "kv,async") (lint-alone "config,async") (lint-alone "locks,async") (lint-alone "channels,async") (lint-alone "jobs,async") (lint-alone "sql,async") (lint-alone "blobs,async") (lint-alone "records,async") (lint-alone "metrics,async") (lint-alone "search,async") (lint-alone "kv,config,locks,channels,jobs,sql,blobs,records,metrics,search,backup,replication,async")
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
[working-directory: 'sdk/go/internal/wire']
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
    bun test test/pipe.test.ts test/kv.test.ts test/jobs.test.ts test/sql.test.ts test/blobs.test.ts test/queries.test.ts test/session.test.ts test/clock.test.ts test/direct.test.ts test/durability.test.ts test/encrypted.test.ts test/protocol.test.ts test/console.test.ts test/events.test.ts test/logger.test.ts test/background.test.ts test/metrics.test.ts test/config.test.ts test/backup.test.ts test/locks.test.ts test/channels.test.ts test/search.test.ts test/node.test.ts test/open.test.ts test/values.test.ts

# run the Go SDK over the core in its own process, a private child, a sidecar and a remote server; gofmt checks its layout
[working-directory: 'sdk/go']
sdk-go: build
    test -z "$(gofmt -l .)"
    go vet ./...
    go test ./...

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

# build the core's library as Node's addon, tinystore.node: a build apart from Bun's, since Node's process gives it Node-API
node-core:
    cargo build --release --locked -p tinystore-ffi --features node
    cp target/release/{{library}} target/release/tinystore.node

# run the Bun SDK's suites under Node, bun:test answered by node:test and Jest's expect, and Node's own: the core in its own process through the addon, a private child, a sidecar and a remote server
[working-directory: 'sdk/js']
sdk-node: build node-core
    bun install --frozen-lockfile
    node --test --test-timeout=60000 --import ./test/node/register.ts test/pipe.test.ts test/kv.test.ts test/jobs.test.ts test/sql.test.ts test/blobs.test.ts test/queries.test.ts test/session.test.ts test/clock.test.ts test/direct.test.ts test/durability.test.ts test/encrypted.test.ts test/protocol.test.ts test/console.test.ts test/events.test.ts test/logger.test.ts test/background.test.ts test/metrics.test.ts test/config.test.ts test/backup.test.ts test/locks.test.ts test/channels.test.ts test/search.test.ts test/node.test.ts test/open.test.ts test/values.test.ts
    node --test test/under-node.ts

# build this machine's target of a release into native/<target>/, as the release workflow does on each:
# just release-native v0.1.0 win32-x64 x86_64-pc-windows-msvc; it stamps the version into Cargo.toml
# and Cargo.lock, so run it in a clone
release-native version target triple:
    sh -c "VERSION={{version}} TARGET={{target}} TRIPLE={{triple}} sh tools/native.sh"

# pack what native/ holds into dist/, as the release workflow does, and try it here from Node, Bun and Python
release-pack version target:
    bun tools/release.ts --version {{version}} --native native --out dist
    sh -c "VERSION={{version}} TARGET={{target}} sh tools/smoke.sh dist"

# lint and typecheck the Bun SDK
[working-directory: 'sdk/js']
sdk-check:
    bun install --frozen-lockfile
    bun run check

# lint and test every crate in a Linux container, for a host that is not Linux
test-linux:
    docker run --rm -v "{{justfile_directory()}}:/src" {{rust_caches}} -w /src {{rust_image}} sh -c 'cargo clippy --workspace --all-targets --locked -- -D warnings && for features in console typos kv config locks channels jobs sql blobs records metrics system search wire kv,backup kv,channels sql,search wire,search kv,replication kv,replication,replication-tls async kv,async config,async locks,async channels,async jobs,async sql,async blobs,async records,async metrics,async search,async kv,config,locks,channels,jobs,sql,blobs,records,metrics,search,backup,replication,async; do cargo clippy -p tinystore --no-default-features --features $features --all-targets --locked -- -D warnings || exit 1; done && cargo test --workspace --locked --features tinystore/async && cargo test -p tinystore --lib --locked --features replication-tls -- replication::link_tests::nodes_linked replication::transport'

# lint the GitHub workflows
lint-actions:
    docker run --rm -v "{{justfile_directory()}}:/repo" -w /repo rhysd/actionlint:1.7.12 -color

# run the docs site with live reload on :5173
[working-directory: 'web']
web-dev:
    bun install
    bun run dev

# every MUST of CONTRACTS.md named by a test, and no test naming an id it lacks
contracts:
    bun tools/contracts.ts

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

check: fmt-check contracts lint test protocol-check codec-go codec-python sdk sdk-check sdk-go sdk-python sdk-python-check sdk-node lint-actions

# remove build output
clean:
    cargo clean
