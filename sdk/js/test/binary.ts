// Builds tinystore from this repository once a test run, for the tests that
// run against a real server, unless TINYSTORE_BIN names one already.

import { join, resolve } from 'node:path'

// what a developer's shell may set would change the console lines the tests compare
for (const name of ['LOG_LEVEL', 'LOG_FORMAT', 'LOG_TIME']) {
	delete process.env[name]
}

if (process.env.TINYSTORE_BIN === undefined) {
	const repo = resolve(import.meta.dir, '../../..')
	const built = Bun.spawnSync(['cargo', 'build', '--locked', '-p', 'tinystore-cli'], {
		cwd: repo,
		stderr: 'pipe',
	})
	if (built.exitCode !== 0) {
		throw new Error(`cargo build of tinystore failed: ${built.stderr.toString()}`)
	}
	const binary = process.platform === 'win32' ? 'tinystore.exe' : 'tinystore'
	process.env.TINYSTORE_BIN = join(repo, 'target', 'debug', binary)
}
