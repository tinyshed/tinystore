import { mkdirSync } from 'node:fs'
import { join, resolve } from 'node:path'

/**
 * Builds tinystore from this repository for the tests that run against a real
 * store, unless TINYSTORE_BIN names one already. Go's cache makes a second
 * build of an unchanged tree take a moment.
 */
export function tinystoreBinary(): string {
	if (process.env.TINYSTORE_BIN !== undefined) {
		return process.env.TINYSTORE_BIN
	}
	const repository = resolve(import.meta.dir, '../..')
	const out = join(import.meta.dir, '..', '.bin')
	mkdirSync(out, { recursive: true })
	const binary = join(out, process.platform === 'win32' ? 'tinystore.exe' : 'tinystore')
	const built = Bun.spawnSync(['go', 'build', '-o', binary, '.'], {
		cwd: join(repository, 'cmd', 'tinystore'),
		env: { ...process.env, GOWORK: 'off', CGO_ENABLED: '0' },
		stderr: 'pipe',
	})
	if (built.exitCode !== 0) {
		throw new Error(`go build of tinystore failed: ${built.stderr.toString()}`)
	}
	process.env.TINYSTORE_BIN = binary
	return binary
}
