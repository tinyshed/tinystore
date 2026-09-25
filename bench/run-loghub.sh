#!/bin/sh
# fetches the pinned loghub 2k samples, checks them and runs the template round
set -eu
corpus=${1:-bench/corpus/loghub}
revision=dd61d0952749ee7963bde24220d1be5ede023033
while read -r hash relative; do
	mkdir -p "$corpus/$(dirname "$relative")"
	[ -f "$corpus/$relative" ] ||
		curl -sSf -o "$corpus/$relative" "https://raw.githubusercontent.com/logpai/loghub/$revision/$relative"
	echo "$hash  $corpus/$relative" | sha256sum -c --quiet -
done < bench/loghub-sha256.txt
TINYSTORE_SPIKE=1 TINYSTORE_LOGHUB="$(cd "$corpus" && pwd)" go test ./spike -run '^TestLoghubTemplates$' -v -count=1
