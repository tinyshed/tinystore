#!/bin/sh
# Builds one target of a release, as plan/release.md says: the version stamped
# into the workspace, then `tinystore` and the core three times, copied into
# native/<target>/ as tools/release.ts reads them.
#
#     VERSION=v0.1.0 TARGET=linux-x64 TRIPLE=x86_64-unknown-linux-gnu sh tools/native.sh
#
# It runs on macOS and Windows' runners as they are, and inside the image of a
# Linux target, manylinux's or Alpine's, which it gives a C compiler and Rust
# first. Plain sh, since Alpine has no bash.
set -eu

: "${VERSION:?the release, such as v0.1.0}"
: "${TARGET:?the target, such as linux-x64}"
: "${TRIPLE:?the Rust triple, such as x86_64-unknown-linux-gnu}"

if [ -f /etc/alpine-release ]; then
	apk add --no-cache bash curl gcc g++ make musl-dev perl >/dev/null
fi
if ! command -v cargo >/dev/null 2>&1; then
	# rust-toolchain.toml names the release; rustup installs it at the first cargo
	curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
	. "$HOME/.cargo/env"
fi
rustup target add "$TRIPLE" >/dev/null

case "$TRIPLE" in
*-linux-musl)
	# a cdylib needs musl linked dynamically, as Alpine's Bun, Node and Python load it
	export RUSTFLAGS="-C target-feature=-crt-static"
	;;
*-windows-msvc)
	# the MSVC runtime inside every file, so that no machine needs it installed
	export RUSTFLAGS="-C target-feature=+crt-static"
	;;
aarch64-apple-darwin) export MACOSX_DEPLOYMENT_TARGET=11.0 ;;
x86_64-apple-darwin) export MACOSX_DEPLOYMENT_TARGET=10.13 ;;
esac

sh tools/stamp.sh

built="target/$TRIPLE/release"
out="native/$TARGET"
rm -rf "$out"
mkdir -p "$out/bin" "$out/lib" "$out/python"
case "$TRIPLE" in
*-windows-*) exe=tinystore.exe library=tinystore_ffi.dll module=_core.pyd ;;
*-apple-*) exe=tinystore library=libtinystore_ffi.dylib module=_core.abi3.so ;;
*) exe=tinystore library=libtinystore_ffi.so module=_core.abi3.so ;;
esac

cargo build --release --locked --target "$TRIPLE" -p tinystore-cli -p tinystore-ffi
cp "$built/$exe" "$out/bin/$exe"
cp "$built/$library" "$out/lib/$library"

# each feature set writes the same file name: copied out before the next
cargo build --release --locked --target "$TRIPLE" -p tinystore-ffi --features node
cp "$built/$library" "$out/lib/tinystore.node"

PYO3_BUILD_EXTENSION_MODULE=1 cargo build --release --locked --target "$TRIPLE" -p tinystore-ffi --features python
cp "$built/$library" "$out/python/$module"

# a build for another processor than this machine is not run here; macOS's
# sh, bash 3.2, misreads a quoted pattern inside "$(...)", so none is there
host=$(rustc -vV | grep '^host:' | cut -d ' ' -f 2)
if [ "$host" = "$TRIPLE" ]; then
	"$out/bin/$exe" version
fi
ls -l "$out/bin" "$out/lib" "$out/python"
