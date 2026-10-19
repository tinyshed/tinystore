#!/bin/sh
# Stamps a release's version into the workspace: its own, and the exact one
# through which the command line and the FFI name the library, which
# crates.io reads. The lockfile follows offline, the dependencies fetched
# first. tools/native.sh runs it before a build, and the release before
# cargo publish.
#
#     VERSION=v0.1.0 sh tools/stamp.sh
set -eu

: "${VERSION:?the release, such as v0.1.0}"
plain="${VERSION#v}"

cargo fetch --locked
sed -e "s/^version = \"0.0.0\"$/version = \"$plain\"/" \
	-e "s/^tinystore = { path = \"crates\/tinystore\", version = \"0.0.0\" }$/tinystore = { path = \"crates\/tinystore\", version = \"=$plain\" }/" \
	Cargo.toml >Cargo.toml.stamped
mv Cargo.toml.stamped Cargo.toml
# a line the patterns no longer meet would publish 0.0.0
if [ "$(grep -c -e "^version = \"$plain\"$" -e "version = \"=$plain\" }$" Cargo.toml)" != 2 ]; then
	echo "Cargo.toml took the version $plain in fewer than its two lines" >&2
	exit 1
fi
cargo update --workspace --offline
