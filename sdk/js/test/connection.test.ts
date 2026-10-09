// What a connection does with the sidecar it found: one of an older release
// is replaced with this SDK's binary, and another server is told of; and an
// address given where a directory belongs.

import { expect, test } from 'bun:test'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { Link, olderRelease, olderServer, replacedSidecar, sidecar } from '../src/connection.ts'
import { InvalidError, open } from '../src/index.ts'
import { bunRuntime } from '../src/runtime/bun.ts'

test('a server of an older release than its SDK is told apart, and only one', () => {
	const cases: [server: string, own: string, older: boolean][] = [
		['v0.1.0', '0.2.0', true],
		['v0.2.0-rc.1', '0.2.0', true],
		['v0.2.0-rc.2', '0.2.0-rc.10', true],
		['v0.2.0-beta.3', '0.2.0-rc.1', true],
		['v0.2.0', '0.2.0', false],
		['v0.3.0', '0.2.0', false],
		['v0.2.0', '0.2.0-rc.1', false],
		['(devel)', '0.2.0', false],
		['v0.0.0-20261002222701-fc080a056d05+dirty', '0.2.0', false],
		['v0.1.1-0.20261002222701-fc080a056d05', '0.2.0', false],
		['v0.1.0', '0.0.0', false],
	]
	for (const [server, own, older] of cases) {
		expect(olderRelease(server, own), `${server} against ${own}`).toBe(older)
	}
	expect(replacedSidecar('v0.1.0', '0.2.0', './data')).toContain(
		'the sidecar serving ./data was v0.1.0, older than',
	)
	expect(olderServer('v0.1.0', '0.2.0', './data')).toContain(
		"./data is served by v0.1.0, older than this SDK's 0.2.0",
	)
})

test('a sidecar of an older release is stopped, and every client moves to the one started in its place', async () => {
	const dir = mkdtempSync(join(tmpdir(), 'tinystore-replace-'))
	const serve = join(dir, 'server', 'SERVE')
	const first = await open(dir, { idle: '1s' })
	const notes = first.bucket<string>('notes')
	await notes.set('a', 'kept across the replacement')
	const old = JSON.parse(readFileSync(serve, 'utf8'))
	expect(old.sidecar).toBe(true)

	const told: string[] = []
	const write = process.stderr.write
	process.stderr.write = ((text: string) => told.push(text) > 0) as typeof process.stderr.write
	let replaced: Awaited<ReturnType<Link['connection']>>
	try {
		const binary = process.env.TINYSTORE_BIN ?? ''
		replaced = await new Link(
			sidecar(
				bunRuntime,
				dir,
				() => binary,
				1000,
				() => true,
			),
		).connection()
	} finally {
		process.stderr.write = write
	}
	const now = JSON.parse(readFileSync(serve, 'utf8'))
	expect(now.instance).not.toBe(old.instance)
	expect(now.pid).not.toBe(old.pid)
	expect(told.join('')).toContain(`the sidecar serving ${dir} was`)

	// the first client was told GOAWAY, and reaches the new sidecar as it connects again
	expect(await notes.get('a')).toBe('kept across the replacement')
	await replaced.close()
	await first.close()
	for (let tried = 0; ; tried++) {
		try {
			rmSync(dir, { recursive: true, force: true })
			break
		} catch (err) {
			if (tried === 100) {
				throw err
			}
			await Bun.sleep(50)
		}
	}
}, 30_000)

test('open refuses an address, and says to connect or to open the directory', async () => {
	const remote = await open('tcp://db.internal:7070').catch(e => e)
	expect(remote).toBeInstanceOf(InvalidError)
	expect(remote.message).toContain("connect('tcp://db.internal:7070', { token })")

	const local = await open('pipe:tinystore-d761f24b7e598066').catch(e => e)
	expect(local).toBeInstanceOf(InvalidError)
	expect(local.message).toContain('open the directory its server serves')
})
