// kv through a real tinystore serve, the one test/binary.ts built.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import {
	ConflictError,
	CorruptError,
	InvalidError,
	open,
	type StandardSchemaV1,
	type Store,
} from '../src/index.ts'
import { bunRuntime } from '../src/runtime/bun.ts'
import type { Runtime } from '../src/runtime.ts'
import { openWith } from '../src/store.ts'

/**
 * What a promise rejected with. Bun's expect(...).rejects waits for a
 * promise without running the event loop's I/O, so a call answered by a
 * server never settles inside it.
 */
async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

let dir: string
let store: Store

beforeAll(async () => {
	dir = mkdtempSync(join(tmpdir(), 'tinystore-kv-'))
	store = await open(dir, { private: true })
})

afterAll(async () => {
	await store.close()
	rmSync(dir, { recursive: true, force: true })
})

interface Session {
	device: string
	seenAt: number
}

describe('a bucket of JSON', () => {
	test('a value comes back as it went in, under its owner', async () => {
		const sessions = store.kv.bucket<Session>('sessions')
		await sessions.of(7).set('token', { device: 'phone', seenAt: 1 })
		expect(await sessions.of(7).get('token')).toEqual({ device: 'phone', seenAt: 1 })
		expect(await sessions.of('7').has('token')).toBe(true)
		expect(await sessions.get('token')).toBeUndefined()
		expect(await sessions.of(8).get('token')).toBeUndefined()
	})

	test('a stale version is a conflict naming its bucket and key', async () => {
		const sessions = store.kv.bucket<Session>('sessions')
		const first = await sessions.setEntry('k', { device: 'a', seenAt: 1 })
		await sessions.set('k', { device: 'b', seenAt: 2 }, { ifVersion: first.version })
		const err = await sessions
			.set('k', { device: 'c', seenAt: 3 }, { ifVersion: first.version })
			.catch(e => e)
		expect(err).toBeInstanceOf(ConflictError)
		expect(err.what).toMatchObject({ bucket: 'sessions', key: 'k' })
		expect((await sessions.get('k'))?.device).toBe('b')
	})

	test('an expiry holds the key until its time', async () => {
		const drafts = store.kv.bucket<string>('drafts')
		await drafts.set('d', 'hello', { ttl: '1h' })
		const entry = await drafts.getEntry('d')
		expect(entry?.expires?.getTime()).toBeGreaterThan(Date.now() + 59 * 60_000)
		expect(await drafts.touch('d', { ttl: '2h' })).toBe(true)
		expect((await drafts.getEntry('d'))?.expires?.getTime()).toBeGreaterThan(
			Date.now() + 119 * 60_000,
		)
		await drafts.set('gone', 'x', { expireAt: Date.now() - 1000 })
		expect(await drafts.get('gone')).toBeUndefined()
	})

	test('setIfAbsent claims a key once and hands back the live one after', async () => {
		const seen = store.kv.bucket('stripe-events', 'none')
		expect(await seen.setIfAbsent('evt_1', null, { ttl: '7d' })).toBe(true)
		const again = await seen.setEntryIfAbsent('evt_1', null)
		expect(again.created).toBe(false)
		expect(again.entry.version).not.toBe('')
	})

	test('take reads and burns', async () => {
		const codes = store.kv.bucket('login-codes', 'int', { defaultTtl: '15m' })
		await codes.set('123456', 42)
		expect(await codes.take('123456')).toBe(42)
		expect(await codes.take('123456')).toBeUndefined()
	})

	test('clear empties a branch and those under it', async () => {
		const carts = store.kv.bucket<number>('carts')
		await carts.of('u1').set('a', 1)
		await carts.of('u1', 'saved').set('b', 2)
		await carts.of('u2').set('a', 3)
		await carts.of('u1').clear()
		expect(await carts.of('u1').get('a')).toBeUndefined()
		expect(await carts.of('u1', 'saved').get('b')).toBeUndefined()
		expect(await carts.of('u2').get('a')).toBe(3)
	})

	test('all walks a branch a page at a time in the order of its keys', async () => {
		const pages = store.kv.bucket<number>('pages').of('p')
		for (let i = 0; i < 25; i++) {
			await pages.set(`k${String(i).padStart(2, '0')}`, i)
		}
		const first = await pages.scan({ limit: 10 })
		expect(first.items.map(e => e.key)).toEqual(Array.from({ length: 10 }, (_, i) => `k0${i}`))
		expect(first.next).toBe('k09')
		const all = []
		for await (const e of pages.all({ limit: 7 })) {
			all.push(e.value)
		}
		expect(all).toEqual(Array.from({ length: 25 }, (_, i) => i))
	})
})

describe('kinds', () => {
	test('every kind keeps its values as Go keeps its types', async () => {
		const strings = store.kv.bucket('strings', 'string')
		await strings.set('s', 'héllo \u{1F600}')
		expect(await strings.get('s')).toBe('héllo \u{1F600}')

		const bytes = store.kv.bucket('bytes', 'bytes')
		await bytes.set('b', new Uint8Array([0, 255, 7]))
		expect(await bytes.get('b')).toEqual(new Uint8Array([0, 255, 7]))

		const floats = store.kv.bucket('floats', 'float')
		await floats.set('f', -0)
		expect(Object.is(await floats.get('f'), -0)).toBe(true)
		await floats.set('inf', Number.POSITIVE_INFINITY)
		expect(await floats.get('inf')).toBe(Number.POSITIVE_INFINITY)

		const flags = store.kv.bucket('flags', 'bool')
		await flags.set('on', true)
		expect(await flags.get('on')).toBe(true)

		const big = store.kv.bucket('big', 'bigint')
		await big.set('n', 2n ** 62n)
		expect(await big.get('n')).toBe(2n ** 62n)
	})

	test('a kind that reads another kind refuses it as corrupt', async () => {
		await store.kv.bucket('mixed', 'string').set('k', 'text')
		expect(await caught(store.kv.bucket('mixed', 'int').get('k'))).toBeInstanceOf(CorruptError)
	})

	test('an integer past what a number holds is refused rather than rounded', async () => {
		await store.kv.bucket('wide', 'bigint').set('k', 2n ** 60n)
		expect(await caught(store.kv.bucket('wide', 'int').get('k'))).toBeInstanceOf(CorruptError)
		expect(await caught(store.kv.bucket('wide', 'int').set('j', 2 ** 60))).toBeInstanceOf(
			InvalidError,
		)
	})

	test('a schema checks each value as it is read', async () => {
		const positive: StandardSchemaV1<unknown, { n: number }> = {
			'~standard': {
				version: 1,
				vendor: 'test',
				validate: value => {
					const n = (value as { n?: unknown }).n
					return typeof n === 'number' && n > 0
						? { value: { n } }
						: { issues: [{ message: 'n must be positive', path: ['n'] }] }
				},
			},
		}
		const checked = store.kv.bucket('checked', positive)
		await checked.set('good', { n: 1 })
		expect(await checked.get('good')).toEqual({ n: 1 })
		await checked.set('bad', { n: -1 })
		const err = await checked.get('bad').catch(e => e)
		expect(err).toBeInstanceOf(CorruptError)
		expect(err.message).toContain('n: n must be positive')
	})
})

describe('counters', () => {
	test('add and max change a counter in one write, 0 when absent', async () => {
		const attempts = store.kv.counters('login-attempts', { defaultTtl: '15m' })
		expect(await attempts.of('ip').get('10.0.0.1')).toBe(0)
		expect(await attempts.of('ip').add('10.0.0.1')).toBe(1)
		expect(await attempts.of('ip').add('10.0.0.1', 4)).toBe(5)
		expect(await attempts.of('ip').max('10.0.0.1', 3)).toBe(5)
		expect(await attempts.of('ip').max('10.0.0.1', 9)).toBe(9)
		await attempts.of('ip').delete('10.0.0.1')
		expect(await attempts.of('ip').get('10.0.0.1')).toBe(0)
	})

	test('a name holding counters does not open for values', async () => {
		await store.kv.counters('hits').add('x')
		expect(await caught(store.kv.bucket('hits').get('x'))).toBeInstanceOf(InvalidError)
	})
})

describe('batches', () => {
	test('a batch writes every call or none', async () => {
		const accounts = store.kv.bucket<number>('accounts')
		const log = store.kv.bucket<string>('log')
		await store.kv.batch(tx => {
			accounts.withTx(tx).set('a', 10)
			log.withTx(tx).set('1', 'opened a')
		})
		expect(await accounts.get('a')).toBe(10)
		expect(await log.get('1')).toBe('opened a')

		const stale = (await accounts.getEntry('a'))!.version
		await accounts.set('a', 11)
		const failed = store.kv.batch(tx => {
			log.withTx(tx).set('2', 'moved a')
			accounts.withTx(tx).set('a', 12, { ifVersion: stale })
		})
		const err = await failed.catch(e => e)
		expect(err).toBeInstanceOf(ConflictError)
		expect(err.what).toMatchObject({ call: '1' })
		expect(await log.get('2')).toBeUndefined()
	})

	test('a view reads from one snapshot, its answers settling with it', async () => {
		const accounts = store.kv.bucket<number>('accounts')
		await accounts.set('x', 1)
		let x: Promise<number | undefined> | undefined
		let y: Promise<boolean> | undefined
		await store.kv.view(tx => {
			x = accounts.withTx(tx).get('x')
			y = accounts.withTx(tx).has('nothing')
		})
		expect(await x).toBe(1)
		expect(await y).toBe(false)
	})
})

describe('the directory sidecar', () => {
	test('a second open finds the sidecar the first started, through SERVE', async () => {
		const shared = mkdtempSync(join(tmpdir(), 'tinystore-sidecar-'))
		const first = await open(shared, { idle: '300ms' })
		await first.kv.bucket<string>('notes').set('a', 'from the first')
		const second = await open(shared)
		expect(await second.kv.bucket<string>('notes').get('a')).toBe('from the first')
		const served = JSON.parse(await Bun.file(join(shared, 'server', 'SERVE')).text())
		expect(served.endpoints.length).toBe(1)
		await first.close()
		await second.close()
		// the sidecar leaves once idle, taking SERVE and LOCK with it
		for (let i = 0; i < 100 && (await Bun.file(join(shared, 'server', 'SERVE')).exists()); i++) {
			await Bun.sleep(50)
		}
		expect(await Bun.file(join(shared, 'server', 'SERVE')).exists()).toBe(false)
		expect(await Bun.file(join(shared, 'server', 'serve.log')).text()).toContain('msg=serving')
		// SERVE goes first and LOCK once the sidecar's store has closed, and
		// Bun's rmSync ignores maxRetries, so a held file is tried again here
		for (let tried = 0; ; tried++) {
			try {
				rmSync(shared, { recursive: true, force: true })
				break
			} catch (err) {
				if (tried === 100) {
					throw err
				}
				await Bun.sleep(50)
			}
		}
	}, 20_000)
})

describe('a private child', () => {
	test('close returns once the child has exited, its directory free', async () => {
		const own = mkdtempSync(join(tmpdir(), 'tinystore-private-'))
		let exited = false
		const runtime: Runtime = {
			...bunRuntime,
			spawnPrivate(argv, events) {
				const child = bunRuntime.spawnPrivate(argv, events)
				void child.exited.then(() => {
					exited = true
				})
				return child
			},
		}
		const opened = await openWith(runtime, own, { private: true })
		await opened.kv.bucket<string>('notes').set('a', 'kept')
		await opened.close()
		expect(exited).toBe(true)
		// on Windows a child still running holds its files, and this fails with EBUSY
		rmSync(own, { recursive: true, force: true })
	})
})
