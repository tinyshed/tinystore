// Builds tinystore from this repository once a test run, for the tests that
// run against a real server, unless TINYSTORE_BIN names one already.

import { spawnSync } from 'node:child_process'
import { join, resolve } from 'node:path'

// what a developer's shell may set would change the console lines the tests compare
for (const name of ['LOG_LEVEL', 'LOG_FORMAT', 'LOG_TIME']) {
	delete process.env[name]
}

if (process.env.TINYSTORE_BIN === undefined) {
	const repo = resolve(import.meta.dirname, '../../..')
	const built = spawnSync('cargo', ['build', '--locked', '-p', 'tinystore-cli'], {
		cwd: repo,
		encoding: 'utf8',
	})
	if (built.status !== 0) {
		throw new Error(`cargo build of tinystore failed: ${built.stderr}`)
	}
	const binary = process.platform === 'win32' ? 'tinystore.exe' : 'tinystore'
	process.env.TINYSTORE_BIN = join(repo, 'target', 'debug', binary)
}
