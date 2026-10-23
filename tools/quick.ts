// `just quick`: the checks of what changed since the branch's upstream, the
// working tree and new files included, in minutes rather than the half hour
// `just check` takes. CI runs every lint set on three platforms; quick lints
// the few sets most likely to break, since each set builds the library again.
//
// | changed                          | runs                                                         |
// |----------------------------------|--------------------------------------------------------------|
// | any .rs                          | cargo fmt --check                                            |
// | CONTRACTS.md, any test's source  | just contracts                                               |
// | crates/tinystore/src/<area>/...  | clippy, the tests of <area>, its set alone and the widest    |
// | crates/tinystore/src/<root>.rs   | clippy, every library test, the console, kv, wire and widest |
// | sqlite/, engine/, compress/      | as a root file: every engine stands on them                  |
// | crates/tinystore/src/wire/...    | as an area, and the Bun SDK; pipe.rs the Bun SDK too         |
// | crates/cli, ffi, protocol        | that crate's clippy and tests; ffi the Bun SDK too           |
// | protocol/*.wire                  | just protocol-check                                          |
// | sdk/js, sdk/python, Go           | that SDK's check and suite                                   |
// | .github/                         | just lint-actions                                            |
//
// The steps that build nothing run beside one another first; the builds
// then run one at a time, since cargo holds target/ for each.

import { spawn, spawnSync } from 'node:child_process'
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

/** What every engine stands on: a change there reaches every feature set, as a root file's does. */
const bases = new Set(['sqlite', 'engine', 'compress'])
/** The sets a change of a root file or a base is linted with: no store, the smallest store, the wire alone. */
const rootSets = ['console', 'kv', 'wire']

/** The features a file of the library belongs to: its area, and kv's config and locks as their own. */
function featuresOf(part: string[]): string[] {
	if (part[0] === 'kv' && (part[1] === 'config' || part[1] === 'locks')) return [part[0], part[1]]
	if (part[0] === 'metrics' && part[1] === 'system.rs') return [part[0], 'system']
	return [part[0]]
}

const library = 'crates/tinystore/src/'
const files = changed()
const beside: string[][] = []
const builds: string[][] = []
const add = (to: string[][], ...step: string[]) => {
	if (!to.some((known) => known.join(' ') === step.join(' '))) to.push(step)
}

if (files.some((file) => file.endsWith('.rs'))) add(beside, 'cargo', 'fmt', '--all', '--check')
if (files.some((file) => file === 'CONTRACTS.md' || /\.(rs|ts|py|go)$/.test(file))) add(beside, 'just', 'contracts')
if (files.some((file) => file.startsWith('sdk/js/'))) add(beside, 'just', 'sdk-check')
if (files.some((file) => file.startsWith('sdk/python/'))) add(beside, 'just', 'sdk-python-check')
if (files.some((file) => file.startsWith('.github/'))) add(beside, 'just', 'lint-actions')

let bunSdk = files.some((file) => file.startsWith('sdk/js/') || file.startsWith('crates/ffi/'))
const inLibrary = files.filter((file) => file.startsWith(library) && file.endsWith('.rs'))
if (inLibrary.length > 0) {
	const parts = inLibrary.map((file) => file.slice(library.length).split('/'))
	const nested = parts.filter((part) => part.length > 1)
	const areas = new Set(nested.map((part) => part[0]))
	const everywhere = parts.some((part) => part.length === 1) || [...areas].some((area) => bases.has(area))
	const sets = lintSets()
	const widest = sets.at(-1)
	// a feature without a set of its own, replication or backup, is linted with the smallest set holding it
	const own = (feature: string) => sets.find((set) => set === feature) ?? sets.find((set) => set.split(',').includes(feature))
	const features = [...new Set(nested.flatMap(featuresOf))].map(own)
	const wanted = everywhere ? new Set([...rootSets, widest]) : new Set([...features, widest])
	add(builds, 'cargo', 'clippy', '-p', 'tinystore', '--all-features', '--all-targets', '--locked', '--', '-D', 'warnings')
	for (const set of sets.filter((set) => wanted.has(set))) {
		add(builds, 'cargo', 'clippy', '-p', 'tinystore', '--no-default-features', '--features', set, '--all-targets', '--locked', '--', '-D', 'warnings')
	}
	const filters = everywhere ? [] : [...areas].map((area) => `${area}::`)
	add(builds, 'cargo', 'test', '-p', 'tinystore', '--all-features', '--lib', '--locked', '--', ...filters)
	bunSdk ||= areas.has('wire') || inLibrary.includes(`${library}pipe.rs`)
}

if (files.some((file) => file.startsWith('crates/cli/'))) {
	add(builds, 'cargo', 'clippy', '-p', 'tinystore-cli', '--all-targets', '--locked', '--', '-D', 'warnings')
	const tests = files.some((file) => file.startsWith('crates/cli/tests/')) ? [] : ['--bin', 'tinystore']
	add(builds, 'cargo', 'test', '-p', 'tinystore-cli', '--locked', ...tests)
}
if (files.some((file) => file.startsWith('crates/ffi/'))) {
	add(builds, 'cargo', 'clippy', '-p', 'tinystore-ffi', '--all-targets', '--locked', '--', '-D', 'warnings')
}
if (files.some((file) => file.startsWith('crates/protocol/') || file.startsWith('protocol/'))) {
	add(builds, 'cargo', 'test', '-p', 'tinystore-protocol', '--locked')
	add(builds, 'just', 'protocol-check')
}
if (bunSdk) add(builds, 'just', 'sdk')
if (files.some((file) => file.startsWith('sdk/python/'))) add(builds, 'just', 'sdk-python')
if (files.some((file) => file.startsWith('sdk/go/'))) add(builds, 'just', 'sdk-go')

const steps = beside.length + builds.length
if (steps === 0) {
	console.log('quick: nothing it checks changed')
	process.exit(0)
}
console.log(`quick: ${files.length} files changed, ${steps} steps`)
// QUICK_PLAN=1 says what would run and runs nothing
if (process.env.QUICK_PLAN === '1') {
	for (const step of [...beside, ...builds]) console.log(`  ${step.join(' ')}`)
	process.exit(0)
}
const began = Date.now()

function failed(step: string[], status: number | null): never {
	console.log(`\nquick: failed at ${step.join(' ')}`)
	process.exit(status ?? 1)
}

/** Runs a step with its output held, so that steps beside one another print one at a time. */
function held(step: string[]): Promise<{ step: string[]; status: number | null; output: string }> {
	return new Promise((done) => {
		const child = spawn(step[0], step.slice(1), { cwd: root, shell: process.platform === 'win32' })
		let output = ''
		child.stdout.on('data', (bytes) => (output += bytes))
		child.stderr.on('data', (bytes) => (output += bytes))
		child.on('close', (status) => done({ step, status, output }))
	})
}

for (const run of await Promise.all(beside.map(held))) {
	console.log(`\n> ${run.step.join(' ')}\n${run.output.trimEnd()}`)
	if (run.status !== 0) failed(run.step, run.status)
}
for (const step of builds) {
	console.log(`\n> ${step.join(' ')}`)
	const run = spawnSync(step[0], step.slice(1), { cwd: root, stdio: 'inherit', shell: process.platform === 'win32' })
	if (run.status !== 0) failed(step, run.status)
}
console.log(`\nquick: every step passed in ${Math.round((Date.now() - began) / 1000)} s; CI runs every lint set`)
