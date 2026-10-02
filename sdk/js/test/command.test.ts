// The tinystore command this package installs, bin/tinystore.js, run as bunx
// and npx run it: under Bun, and under Node where Node is installed.

import { expect, test } from 'bun:test'
import { join } from 'node:path'

const command = join(import.meta.dir, '..', 'bin', 'tinystore.js')
const runtimes = [process.execPath, Bun.which('node')].filter(runtime => runtime !== null)

function run(
	runtime: string,
	args: string[],
	env: Record<string, string | undefined> = process.env,
) {
	const ran = Bun.spawnSync([runtime, command, ...args], { env, stdout: 'pipe', stderr: 'pipe' })
	return { code: ran.exitCode, stdout: ran.stdout.toString(), stderr: ran.stderr.toString() }
}

test('the tinystore command runs the binary, with its output and its exit code', () => {
	for (const runtime of runtimes) {
		const version = run(runtime, ['version'])
		expect(version.code, `${runtime}: ${version.stderr}`).toBe(0)
		expect(version.stdout).toStartWith('tinystore ')

		const unknown = run(runtime, ['nothing'])
		expect(unknown.code).toBe(1)
		expect(unknown.stderr).toContain('no command "nothing"')
	}
})

test('the tinystore command without its platform package says so', () => {
	const none = run(process.execPath, ['version'], { ...process.env, TINYSTORE_BIN: '' })
	expect(none.code).toBe(1)
	expect(none.stderr).toContain(`@tinyshed/tinystore-${process.platform}-${process.arch}`)
	expect(none.stderr).toContain('was not installed')
})
