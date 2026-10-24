// `just contracts`: every contract of CONTRACTS.md that says MUST is kept by a
// test naming its id, `// contract: KV-04` (or `#` in Python and the justfile),
// and no comment names an id the file lacks. A contract that only says MAY,
// and the NOT- ones, promise nothing a test could break.

import { spawnSync } from 'node:child_process'
import { readFileSync } from 'node:fs'

const root = new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')

// Contracts no test keeps yet. One that gains a test leaves this list.
const untested = new Set(['DUR-10', 'BLOB-09'])

function contracts(): Map<string, string> {
	const found = new Map<string, string>()
	for (const line of readFileSync(`${root}/CONTRACTS.md`, 'utf8').split('\n')) {
		const m = line.match(/^- \*\*([A-Z]+-\d+)\*\* (.*)$/)
		if (!m) continue
		if (found.has(m[1])) fail(`CONTRACTS.md: ${m[1]} is given twice`)
		found.set(m[1], m[2])
	}
	return found
}

/** Each id a comment names, with where. */
function claims(): Map<string, string[]> {
	const run = spawnSync('git', ['ls-files', '--cached', '--others', '--exclude-standard'], { cwd: root, encoding: 'utf8' })
	const files = run.stdout.split('\n').filter((f) => /\.(rs|ts|py|go)$|^justfile$/.test(f) && !f.startsWith('.scratch/'))
	const found = new Map<string, string[]>()
	for (const file of files) {
		if (file === 'tools/contracts.ts') continue
		readFileSync(`${root}/${file}`, 'utf8')
			.split('\n')
			.forEach((line, i) => {
				const m = line.match(/^\s*(?:\/\/|#) contract: ([A-Z]+-\d+(?:, [A-Z]+-\d+)*)\s*$/)
				if (!m) return
				for (const id of m[1].split(', ')) found.set(id, [...(found.get(id) ?? []), `${file}:${i + 1}`])
			})
	}
	return found
}

function fail(message: string): never {
	console.error(message)
	process.exit(1)
}

const promised = contracts()
const kept = claims()
const problems: string[] = []
for (const [id, places] of kept) {
	if (!promised.has(id)) problems.push(`${places[0]}: names ${id}, which CONTRACTS.md does not hold`)
}
for (const [id, text] of promised) {
	const needsTest = !id.startsWith('NOT-') && /\bMUST\b/.test(text)
	if (needsTest && !kept.has(id) && !untested.has(id)) problems.push(`${id}: no test names it`)
	if (untested.has(id) && kept.has(id)) problems.push(`${id}: a test names it now; take it off the untested list`)
}
if (problems.length > 0) fail(problems.join('\n'))

const tests = [...kept.values()].reduce((sum, places) => sum + places.length, 0)
console.log(`contracts: ${promised.size} held, ${kept.size} named by ${tests} tests, ${untested.size} untested`)
