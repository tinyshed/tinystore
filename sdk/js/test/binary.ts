// Builds tinystore from this repository once a test run, for the tests that
// run against a real server, unless TINYSTORE_BIN names one already.

import { mkdirSync } from 'node:fs'
import { join, resolve } from 'node:path'

// what a developer's shell may set would change the console lines the tests compare
for (const name of ['LOG_LEVEL', 'LOG_FORMAT', 'LOG_TIME']) {
	delete process.env[name]
}

if (process.env.TINYSTORE_BIN === undefined) {
	const repo = resolve(import.meta.dir, '../../..')
	const out = join(import.meta.dir, '..', '.bin')
	mkdirSync(out, { recursive: true })
	const binary = join(out, process.platform === 'win32' ? 'tinystore.exe' : 'tinystore')
	const built = Bun.spawnSync(['go', 'build', '-o', binary, '.'], {
		cwd: join(repo, 'cmd', 'tinystore'),
		env: { ...process.env, GOWORK: 'off', CGO_ENABLED: '0' },
		stderr: 'pipe',
	})
	if (built.exitCode !== 0) {
		throw new Error(`go build of tinystore failed: ${built.stderr.toString()}`)
	}
	process.env.TINYSTORE_BIN = binary
}
