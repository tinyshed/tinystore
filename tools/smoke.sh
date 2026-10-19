#!/bin/sh
# Installs what tools/release.ts packed into empty projects, with nothing of
# the repository's to find, and opens a store through its core and through a
# private child from Node, Bun and Python: what a program that installs the
# release meets first.
#
#     VERSION=v0.1.0 TARGET=linux-x64 sh tools/smoke.sh dist
#
# Each runtime is tried where it is on PATH, and said when it is not.
set -eu

: "${VERSION:?the release, such as v0.1.0}"
: "${TARGET:?the target whose packages are tried, such as linux-x64}"
dist="$(cd "${1:?the directory tools/release.ts wrote}" && pwd)"
plain="${VERSION#v}"
python_version="$(echo "$plain" | sed 's/-alpha\./a/; s/-beta\./b/; s/-rc\./rc/')"
unset TINYSTORE_BIN TINYSTORE_LIBRARY TINYSTORE_ADDON

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
sdk="$dist/npm/tinyshed-tinystore-$plain.tgz"
platform="$dist/npm/tinyshed-tinystore-$TARGET-$plain.tgz"

cat >"$work/try.mjs" <<'EOF'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { open } from '@tinyshed/tinystore'

for (const how of [{ embedded: true }, { private: true }]) {
	const store = await open(mkdtempSync(join(tmpdir(), 'smoke-')), how)
	const notes = store.bucket('notes')
	await notes.set('a', 'packed')
	if ((await notes.get('a')) !== 'packed') throw new Error(`${JSON.stringify(how)}: the value did not come back`)
	await store.close()
	console.log(`${JSON.stringify(how)}: ok`)
}
EOF

cat >"$work/try.py" <<'EOF'
import asyncio
import tempfile

import tinystore


async def main() -> None:
    for how in ({"embedded": True}, {"private": True}):
        store = await tinystore.open(tempfile.mkdtemp(prefix="smoke-"), **how)
        notes = store.bucket("notes")
        await notes.set("a", "packed")
        assert await notes.get("a") == "packed", f"{how}: the value did not come back"
        await store.close()
        print(f"{how}: ok")


asyncio.run(main())
EOF

if command -v node >/dev/null 2>&1; then
	echo "node $(node --version)"
	mkdir "$work/node" && cd "$work/node"
	echo '{ "type": "module" }' >package.json
	npm install --no-audit --no-fund "$sdk" "$platform"
	# a module's imports are found from where it is: the project's
	cp ../try.mjs .
	node try.mjs
	./node_modules/.bin/tinystore version
else
	echo "node: not here, not tried"
fi

if command -v bun >/dev/null 2>&1; then
	echo "bun $(bun --version)"
	mkdir "$work/bun" && cd "$work/bun"
	echo '{}' >package.json
	bun add "$sdk" "$platform"
	cp ../try.mjs .
	bun try.mjs
else
	echo "bun: not here, not tried"
fi

if command -v uv >/dev/null 2>&1; then
	mkdir "$work/python" && cd "$work/python"
	uv venv --python 3.12 .venv
	uv pip install --python .venv --no-index --find-links "$dist/pypi" "tinyshed-tinystore==$python_version"
	if [ -x .venv/Scripts/python.exe ]; then python=.venv/Scripts/python.exe; else python=.venv/bin/python; fi
	"$python" -I ../try.py
else
	echo "python: uv is not here, not tried"
fi
