// `just quick`: the checks of what changed since the branch's upstream, the
// working tree and new files included, in minutes rather than the half hour
// `just check` takes. `just check` still runs before a push.
//
// | changed                          | runs                                                     |
// |----------------------------------|----------------------------------------------------------|
// | any .rs                          | cargo fmt --check                                        |
// | CONTRACTS.md, any test's source  | just contracts                                           |
// | crates/tinystore/src/<area>/...  | clippy, the tests of <area>, the lint sets naming it     |
// | crates/tinystore/src/<root>.rs   | clippy, every library test, every lint set               |
// | crates/tinystore/src/wire/...    | as an area, and the Bun SDK                              |
// | crates/cli, ffi, protocol        | that crate's clippy and tests                            |
// | protocol/*.wire                  | just protocol-check                                      |
// | sdk/js, sdk/python, Go           | that SDK's check and suite                               |
// | .github/                         | just lint-actions                                        |

import { spawnSync } from 'node:child_process'
import { readFileSync } from 'node:fs'

const root = new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')

function git(...args: string[]): string[] {
	const run = spawnSync('git', args, { cwd: root, encoding: 'utf8' })
	return run.status === 0 ? run.stdout.split('\n').filter(Boolean) : []
}

/** What differs from the upstream, or from HEAD on a branch with none, and the files git does not track yet. */
function changed(): string[] {
	const base = git('rev-parse', '--abbrev-ref', '@{upstream}').length > 0 ? '@{upstream}' : 'HEAD'
	const files = [...git('diff', '--name-only', base), ...git('ls-files', '--others', '--exclude-standard')]
	return [...new Set(files)].filter((file) => !file.startsWith('.scratch/'))
}

/** The feature sets `just lint` builds the library with, read from the justfile so that one list holds them. */
function lintSets(): string[] {
	const justfile = readFileSync(`${root}/justfile`, 'utf8')
	return [...justfile.matchAll(/\(lint-alone "([^"]+)"\)/g)].map((found) => found[1])
}

const library = 'crates/tinystore/src/'
const files = changed()
const steps: string[][] = []
const add = (...step: string[]) => {
	if (!steps.some((known) => known.join(' ') === step.join(' '))) steps.push(step)
}

if (files.some((file) => file.endsWith('.rs'))) add('cargo', 'fmt', '--all', '--check')
if (files.some((file) => file === 'CONTRACTS.md' || /\.(rs|ts|py|go)$/.test(file))) add('just', 'contracts')

const inLibrary = files.filter((file) => file.startsWith(library) && file.endsWith('.rs'))
if (inLibrary.length > 0) {
	const parts = inLibrary.map((file) => file.slice(library.length).split('/'))
	const areas = new Set(parts.filter((part) => part.length > 1).map((part) => part[0]))
	const rootTouched = parts.some((part) => part.length === 1)
	add('cargo', 'clippy', '-p', 'tinystore', '--all-features', '--all-targets', '--locked', '--', '-D', 'warnings')
	const sets = lintSets().filter((set) => rootTouched || set.split(',').some((feature) => areas.has(feature)))
	for (const set of sets) {
		add('cargo', 'clippy', '-p', 'tinystore', '--no-default-features', '--features', set, '--all-targets', '--locked', '--', '-D', 'warnings')
	}
	const filters = rootTouched ? [] : [...areas].map((area) => `${area}::`)
	add('cargo', 'test', '-p', 'tinystore', '--all-features', '--lib', '--locked', '--', ...filters)
	if (areas.has('wire') || rootTouched) add('just', 'sdk')
}

if (files.some((file) => file.startsWith('crates/cli/'))) {
	add('cargo', 'clippy', '-p', 'tinystore-cli', '--all-targets', '--locked', '--', '-D', 'warnings')
	const tests = files.some((file) => file.startsWith('crates/cli/tests/')) ? [] : ['--bin', 'tinystore']
	add('cargo', 'test', '-p', 'tinystore-cli', '--locked', ...tests)
}
if (files.some((file) => file.startsWith('crates/ffi/'))) {
	add('cargo', 'clippy', '-p', 'tinystore-ffi', '--all-targets', '--locked', '--', '-D', 'warnings')
}
if (files.some((file) => file.startsWith('crates/protocol/') || file.startsWith('protocol/'))) {
	add('cargo', 'test', '-p', 'tinystore-protocol', '--locked')
	add('just', 'protocol-check')
}
if (files.some((file) => file.startsWith('sdk/js/'))) {
	add('just', 'sdk-check')
	add('just', 'sdk')
}
if (files.some((file) => file.startsWith('sdk/python/'))) {
	add('just', 'sdk-python-check')
	add('just', 'sdk-python')
}
if (files.some((file) => file.startsWith('sdk/go/'))) add('just', 'sdk-go')
if (files.some((file) => file.startsWith('.github/'))) add('just', 'lint-actions')

if (steps.length === 0) {
	console.log('quick: nothing it checks changed')
	process.exit(0)
}
console.log(`quick: ${files.length} files changed, ${steps.length} steps`)
const began = Date.now()
for (const step of steps) {
	console.log(`\n> ${step.join(' ')}`)
	const run = spawnSync(step[0], step.slice(1), { cwd: root, stdio: 'inherit', shell: process.platform === 'win32' })
	if (run.status !== 0) {
		console.log(`\nquick: failed at ${step.join(' ')}`)
		process.exit(run.status ?? 1)
	}
}
console.log(`\nquick: every step passed in ${Math.round((Date.now() - began) / 1000)} s; just check runs before a push`)
