import { join, resolve } from 'node:path'

/**
 * Builds tinystore from this repository for the tests that run against a real
 * store, unless TINYSTORE_BIN names one already. Cargo's cache makes a second
 * build of an unchanged tree take a moment.
 */
export function tinystoreBinary(): string {
	if (process.env.TINYSTORE_BIN !== undefined) {
		return process.env.TINYSTORE_BIN
	}
	const repository = resolve(import.meta.dir, '../..')
	const binary = join(
		repository,
		'target',
		'debug',
		process.platform === 'win32' ? 'tinystore.exe' : 'tinystore',
	)
	const built = Bun.spawnSync(['cargo', 'build', '-q', '-p', 'tinystore-cli'], {
		cwd: repository,
		stderr: 'pipe',
	})
	if (built.exitCode !== 0) {
		throw new Error(`cargo build of tinystore failed: ${built.stderr.toString()}`)
	}
	process.env.TINYSTORE_BIN = binary
	return binary
}
