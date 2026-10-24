// Packs a release from what its seven builds made, as the release skill says:
// the archives, npm's packages and PyPI's wheels, `SHA256SUMS` and the notes.
//
//     bun tools/release.ts --version v0.1.0 --native native --out dist
//     bun tools/release.ts --version v0.1.1 --notes-only --out dist     # NOTES.md alone
//
// `native/<target>/` holds what the release workflow's build of that target
// copied out: `bin/` the executable, `lib/` the core's library and the Node
// addon, `python/` Python's module. A target missing there is left out and
// said, so that a build of one platform can be packed and tried alone.
//
// It writes nothing outside `--out`: npm packs from a directory of its own
// under it, and every manifest is stamped with the version there, never in
// the repository.

import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { chmodSync, cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { join, relative } from 'node:path'

const root = new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')

/** A target as npm, PyPI and the archives name it. */
interface Target {
	name: string
	os: 'linux' | 'darwin' | 'win32'
	cpu: 'x64' | 'arm64'
	libc?: 'glibc' | 'musl'
	wheel: string
}

const targets: Target[] = [
	{ name: 'linux-x64', os: 'linux', cpu: 'x64', libc: 'glibc', wheel: 'manylinux_2_28_x86_64' },
	{ name: 'linux-arm64', os: 'linux', cpu: 'arm64', libc: 'glibc', wheel: 'manylinux_2_28_aarch64' },
	{ name: 'linux-x64-musl', os: 'linux', cpu: 'x64', libc: 'musl', wheel: 'musllinux_1_2_x86_64' },
	{ name: 'linux-arm64-musl', os: 'linux', cpu: 'arm64', libc: 'musl', wheel: 'musllinux_1_2_aarch64' },
	{ name: 'darwin-arm64', os: 'darwin', cpu: 'arm64', wheel: 'macosx_11_0_arm64' },
	{ name: 'darwin-x64', os: 'darwin', cpu: 'x64', wheel: 'macosx_10_13_x86_64' },
	{ name: 'win32-x64', os: 'win32', cpu: 'x64', wheel: 'win_amd64' },
]

const executable = (target: Target) => (target.os === 'win32' ? 'tinystore.exe' : 'tinystore')
const library = (target: Target) =>
	({ linux: 'libtinystore_ffi.so', darwin: 'libtinystore_ffi.dylib', win32: 'tinystore_ffi.dll' })[target.os]
const pythonModule = (target: Target) => (target.os === 'win32' ? '_core.pyd' : '_core.abi3.so')

const homepage = 'https://tinyshed.org/tinystore'
const repository = { type: 'git', url: 'git+https://github.com/tinyshed/tinystore.git' }

function main(): void {
	const asked = flags(process.argv.slice(2))
	const version = asked.version.replace(/^v/, '')
	if (!/^\d+\.\d+\.\d+(-(alpha|beta|rc)\.\d+)?$/.test(version)) {
		fail(`version ${asked.version}: vX.Y.Z, or with -alpha.N, -beta.N or -rc.N`)
	}
	const out = join(root, asked.out)
	if (asked.notesOnly) {
		// a release's notes before it is dispatched, from the commits it would carry
		mkdirSync(out, { recursive: true })
		return notes(targets, version, out)
	}
	rmSync(out, { recursive: true, force: true })
	mkdirSync(join(out, 'npm'), { recursive: true })
	mkdirSync(join(out, 'pypi'), { recursive: true })
	const built = targets.filter((target) => {
		const there = existsSync(join(root, asked.native, target.name))
		if (!there) {
			console.log(`${target.name}: not built, left out`)
		}
		return there
	})
	if (built.length === 0) {
		fail(`nothing built in ${asked.native}`)
	}
	for (const target of built) {
		const native = join(root, asked.native, target.name)
		archive(target, native, version, out)
		npmPlatform(target, native, version, out)
		wheel(target, native, version, out)
	}
	npmSdk(built, version, out)
	sums(out)
	notes(built, version, out)
}

/** `bin/` and `lib/` with the license and the readme, a `.tar.gz`, or a `.zip` for Windows. */
function archive(target: Target, native: string, version: string, out: string): void {
	const name = `tinystore-${version}-${target.name}`
	const staged = join(out, 'stage', name)
	cpSync(join(native, 'bin'), join(staged, 'bin'), { recursive: true })
	markExecutable(join(staged, 'bin'))
	cpSync(join(native, 'lib'), join(staged, 'lib'), { recursive: true })
	cpSync(join(root, 'LICENSE'), join(staged, 'LICENSE'))
	cpSync(join(root, 'README.md'), join(staged, 'README.md'))
	if (target.os === 'win32') {
		zip(join(out, `${name}.zip`), entriesOf(staged, name))
	} else {
		// relative paths: Git Bash's tar takes `D:` for a host to connect to
		run('tar', ['-czf', `../${name}.tar.gz`, name], join(out, 'stage'))
	}
}

/** `@tinyshed/tinystore-<target>`: the executable, the core's library and the addon, for one platform. */
function npmPlatform(target: Target, native: string, version: string, out: string): void {
	const staged = join(out, 'stage', 'npm', target.name)
	cpSync(join(native, 'bin'), join(staged, 'bin'), { recursive: true })
	markExecutable(join(staged, 'bin'))
	cpSync(join(native, 'lib'), join(staged, 'lib'), { recursive: true })
	cpSync(join(root, 'LICENSE'), join(staged, 'LICENSE'))
	const manifest = {
		name: `@tinyshed/tinystore-${target.name}`,
		version,
		description: `TinyStore's executable, core library and Node addon for ${target.name}`,
		license: 'Apache-2.0',
		homepage,
		repository,
		os: [target.os],
		cpu: [target.cpu],
		...(target.libc === undefined ? {} : { libc: [target.libc] }),
		files: ['bin', 'lib'],
		// a package manager that zips what it installs, as Yarn's PnP does,
		// would leave the executable where nothing can run it
		preferUnplugged: true,
	}
	writeFileSync(join(staged, 'package.json'), `${JSON.stringify(manifest, null, '\t')}\n`)
	pack(staged, out)
}

/** `@tinyshed/tinystore`: the SDK, compiled for Node, naming every platform's package as optional. */
function npmSdk(built: Target[], version: string, out: string): void {
	const sdk = join(root, 'sdk', 'js')
	run('bun', ['run', 'build'], sdk)
	const staged = join(out, 'stage', 'npm', 'sdk')
	for (const part of ['src', 'dist', 'bin', 'README.md']) {
		cpSync(join(sdk, part), join(staged, part), { recursive: true })
	}
	cpSync(join(root, 'LICENSE'), join(staged, 'LICENSE'))
	const manifest = JSON.parse(readFileSync(join(sdk, 'package.json'), 'utf8'))
	manifest.version = version
	delete manifest.devDependencies
	delete manifest.scripts
	manifest.optionalDependencies = Object.fromEntries(
		built.map((target) => [`@tinyshed/tinystore-${target.name}`, version]),
	)
	writeFileSync(join(staged, 'package.json'), `${JSON.stringify(manifest, null, '\t')}\n`)
	pack(staged, out)
}

/** One abi3 wheel for CPython 3.12 on: the SDK's sources, its core and the executable. */
function wheel(target: Target, native: string, version: string, out: string): void {
	const python = pythonVersion(version)
	const distInfo = `tinyshed_tinystore-${python}.dist-info`
	const sources = join(root, 'sdk', 'python', 'src', 'tinystore')
	const entries: Entry[] = []
	for (const file of walk(sources)) {
		const name = relative(sources, file).replaceAll('\\', '/')
		if (/(^|\/)__pycache__\//.test(name) || /^_core\./.test(name) || name.startsWith('bin/')) {
			continue
		}
		entries.push({ name: `tinystore/${name}`, from: file })
	}
	entries.push({ name: `tinystore/${pythonModule(target)}`, from: join(native, 'python', pythonModule(target)) })
	entries.push({ name: `tinystore/bin/${executable(target)}`, from: join(native, 'bin', executable(target)), mode: 0o755 })
	entries.push({ name: `${distInfo}/METADATA`, text: metadata(python) })
	entries.push({
		name: `${distInfo}/WHEEL`,
		text: `Wheel-Version: 1.0\nGenerator: tinystore tools/release.ts\nRoot-Is-Purelib: false\nTag: cp312-abi3-${target.wheel}\n`,
	})
	// pip writes the `tinystore` command from this, so that `uvx --from tinyshed-tinystore tinystore` runs
	entries.push({ name: `${distInfo}/entry_points.txt`, text: '[console_scripts]\ntinystore = tinystore._command:main\n' })
	entries.push({ name: `${distInfo}/licenses/LICENSE`, from: join(root, 'LICENSE') })
	const file = `tinyshed_tinystore-${python}-cp312-abi3-${target.wheel}.whl`
	zip(join(out, 'pypi', file), entries, `${distInfo}/RECORD`)
}

/** The wheel's metadata, from the project's own `pyproject.toml`. */
function metadata(version: string): string {
	const project = readFileSync(join(root, 'sdk', 'python', 'pyproject.toml'), 'utf8')
	const field = (name: string) => new RegExp(`^${name} = "([^"]*)"`, 'm').exec(project)?.[1] ?? ''
	const classifiers = [...project.matchAll(/^ {4}"([^"]+ :: [^"]+)",$/gm)].map((match) => match[1])
	const readme = readFileSync(join(root, 'sdk', 'python', 'README.md'), 'utf8')
	return [
		'Metadata-Version: 2.4',
		'Name: tinyshed-tinystore',
		`Version: ${version}`,
		`Summary: ${field('description')}`,
		'License-Expression: Apache-2.0',
		'License-File: LICENSE',
		`Requires-Python: ${field('requires-python')}`,
		`Project-URL: Homepage, ${homepage}`,
		'Project-URL: Repository, https://github.com/tinyshed/tinystore',
		...classifiers.map((classifier) => `Classifier: ${classifier}`),
		'Description-Content-Type: text/markdown',
		'',
		readme,
	].join('\n')
}

/** PyPI's spelling of a version: `0.1.0rc1` for `0.1.0-rc.1`. */
function pythonVersion(version: string): string {
	return version.replace('-alpha.', 'a').replace('-beta.', 'b').replace('-rc.', 'rc')
}

/** `SHA256SUMS` of every file a release carries, as `sha256sum -c` reads it. */
function sums(out: string): void {
	rmSync(join(out, 'stage'), { recursive: true, force: true })
	const files = walk(out).map((file) => relative(out, file).replaceAll('\\', '/'))
	const lines = files.sort().map((file) => {
		const hash = createHash('sha256').update(readFileSync(join(out, file))).digest('hex')
		return `${hash}  ${file}`
	})
	writeFileSync(join(out, 'SHA256SUMS'), `${lines.join('\n')}\n`)
}

function notes(built: Target[], version: string, out: string): void {
	const lines = [
		`TinyStore ${version}`,
		'',
		'```sh',
		`bun add @tinyshed/tinystore@${version}`,
		`npm install @tinyshed/tinystore@${version}`,
		`pip install tinyshed-tinystore==${pythonVersion(version)}`,
		'```',
		'',
		`Built for ${built.map((target) => `\`${target.name}\``).join(', ')}. Each archive holds the`,
		'`tinystore` executable in `bin/`, and in `lib/` the core\'s library and the Node addon; the Go',
		'SDK fetches its platform\'s at the first open. `SHA256SUMS` checks them all.',
		...changes(version),
	]
	writeFileSync(join(out, 'NOTES.md'), `${lines.join('\n')}\n`)
}

const sections: [type: string, heading: string][] = [
	['feat', 'Features'],
	['fix', 'Fixes'],
	['perf', 'Performance'],
]

/** A commit by its Conventional Commits subject; `!` marks one that changes what a program writes. */
interface Commit {
	type: string
	scope?: string
	changesNames: boolean
	text: string
}

function commitOf(subject: string): Commit[] {
	const [, type, scope, bang, text] = subject.match(/^(\w+)(?:\(([^)]+)\))?(!)?: (.+)$/) ?? []
	return type && text ? [{ type, scope, changesNames: bang === '!', text }] : []
}

/**
 * The commits since the release before: first those marked `!`, the names a
 * program must change before it upgrades, then a section a type.
 */
function changes(version: string): string[] {
	const previous = previousTag(version)
	if (!previous) {
		return []
	}
	const subjects = git(['log', '--no-merges', '--format=%s', `${previous}..HEAD`]).split('\n')
	const commits = subjects.flatMap(commitOf)
	const groups: [heading: string, commits: Commit[]][] = [
		['Changed names', commits.filter((commit) => commit.changesNames)],
		...sections.map(([type, heading]): [string, Commit[]] => [
			heading,
			commits.filter((commit) => !commit.changesNames && commit.type === type),
		]),
	]
	const lines = groups.flatMap(([heading, own]) =>
		own.length === 0 ? [] : ['', `### ${heading}`, '', ...own.map(itemOf)],
	)
	if (lines.length === 0) {
		return []
	}
	return ['', `## Changes since ${previous}`, ...lines, '', `Full history: \`${previous}...v${version}\`.`]
}

function itemOf(commit: Commit): string {
	return `- ${commit.scope ? `${commit.scope}: ` : ''}${commit.text}`
}

/** The newest tag below `version`; a stable release skips the candidates, a candidate follows the last of any. */
function previousTag(version: string): string | undefined {
	const order = (tag: string): number[] => {
		const [, major, minor, patch, candidate] = tag.match(/^v(\d+)\.(\d+)\.(\d+)(?:-rc\.(\d+))?$/) ?? []
		return [Number(major), Number(minor), Number(patch), candidate === undefined ? Infinity : Number(candidate)]
	}
	const stable = !version.includes('-')
	const here = order(`v${version}`)
	return git(['tag', '--list', 'v*'])
		.split('\n')
		.filter((tag) => /^v\d+\.\d+\.\d+(-rc\.\d+)?$/.test(tag) && !(stable && tag.includes('-')))
		.filter((tag) => compare(order(tag), here) < 0)
		.sort((a, b) => compare(order(b), order(a)))[0]
}

function compare(a: number[], b: number[]): number {
	for (let i = 0; i < a.length; i++) {
		if (a[i] !== b[i]) {
			return a[i] < b[i] ? -1 : 1
		}
	}
	return 0
}

function git(args: string[]): string {
	const ran = spawnSync('git', args, { cwd: root, encoding: 'utf8' })
	return ran.status === 0 ? ran.stdout.trim() : ''
}

interface Entry {
	name: string
	from?: string
	text?: string
	mode?: number
}

/** Every file under `dir`, an executable `bin/` file marked so. */
function entriesOf(dir: string, prefix: string): Entry[] {
	return walk(dir).map((file) => {
		const name = `${prefix}/${relative(dir, file).replaceAll('\\', '/')}`
		return { name, from: file, ...(name.includes('/bin/') ? { mode: 0o755 } : {}) }
	})
}

function zip(path: string, entries: Entry[], record?: string): void {
	const python = process.platform === 'win32' ? 'python' : 'python3'
	const input = JSON.stringify({ out: path, entries, record })
	const ran = spawnSync(python, ['-I', join(root, 'tools', 'wheel.py')], { input, encoding: 'utf8' })
	if (ran.status !== 0) {
		fail(`writing ${relative(root, path)}: ${ran.stderr}`)
	}
}

function pack(dir: string, out: string): void {
	run('npm', ['pack', '--pack-destination', join(out, 'npm')], dir)
}

/**
 * Marks every file of `dir` executable, as an artifact's download or a copy
 * may have left it otherwise. Packed on Windows, whose files keep no such
 * mark, a Unix package's executable is not one: a release packs on Linux.
 */
function markExecutable(dir: string): void {
	for (const file of walk(dir)) {
		chmodSync(file, 0o755)
	}
}

function walk(dir: string): string[] {
	return readdirSync(dir).flatMap((name) => {
		const path = join(dir, name)
		return statSync(path).isDirectory() ? walk(path) : [path]
	})
}

function run(command: string, args: string[], cwd = root): void {
	const ran = spawnSync(command, args, { cwd, stdio: 'inherit', shell: process.platform === 'win32' })
	if (ran.status !== 0) {
		fail(`${command} ${args.join(' ')} failed in ${relative(root, cwd) || '.'}`)
	}
}

function flags(args: string[]): { version: string; native: string; out: string; notesOnly: boolean } {
	const value = (name: string, otherwise?: string) => {
		const at = args.indexOf(`--${name}`)
		const given = at >= 0 ? args[at + 1] : otherwise
		if (given === undefined) {
			fail(`--${name} is needed`)
		}
		return given as string
	}
	return {
		version: value('version'),
		native: value('native', 'native'),
		out: value('out', 'dist'),
		notesOnly: args.includes('--notes-only'),
	}
}

function fail(message: string): never {
	console.error(`release: ${message}`)
	process.exit(1)
}

main()
