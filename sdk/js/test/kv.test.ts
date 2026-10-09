// kv through the core in this process, as plan/api/kv.md has it: buckets,
// counters, limits, once and transactions over protocol 2. Runs when
// TINYSTORE_LIBRARY names the library `cargo build -p tinystore-ffi` made.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import {
	ConflictError,
	CorruptError,
	type Duration,
	InvalidError,
	open,
	type Rate,
	type Store,
} from '../src/index.ts'

const library = process.env.TINYSTORE_LIBRARY

/**
 * What a promise rejected with. Bun's expect(...).rejects waits for a
 * promise without running the event loop's I/O, so a call answered by the
 * core never settles inside it.
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
	if (library === undefined) {
		return
	}
	dir = mkdtempSync(join(tmpdir(), 'tinystore-kv-'))
	store = await open(dir, { embedded: true, library })
})

afterAll(async () => {
	if (library === undefined) {
		return
	}
	await store.close()
	rmSync(dir, { recursive: true, force: true })
})

interface Session {
	device: string
	seenAt: number
}

describe.skipIf(!library)('a bucket', () => {
	test('a value comes back as it went in, under its owner', async () => {
		const sessions = store.bucket<Session>('sessions')
		await sessions.under(7).set('token', { device: 'phone', seenAt: 1 })
		expect(await sessions.under(7).get('token')).toEqual({ device: 'phone', seenAt: 1 })
		expect(await sessions.under('7').has('token')).toBe(true)
		expect(await sessions.get('token')).toBeUndefined()
		expect(await sessions.under(8).get('token')).toBeUndefined()
	})

	test('a stale version is a conflict naming its bucket and key', async () => {
		const sessions = store.bucket<Session>('sessions')
		await sessions.set('k', { device: 'a', seenAt: 1 })
		const first = await sessions.entry('k')
		if (first === undefined) {
			throw new Error('the key just set is not there')
		}
		await sessions.set('k', { device: 'b', seenAt: 2 }, { ifVersion: first.version })
		const err = await caught(
			sessions.set('k', { device: 'c', seenAt: 3 }, { ifVersion: first.version }),
		)
		expect(err).toBeInstanceOf(ConflictError)
		expect((err as Error).message).toContain('kv bucket sessions: key "k"')
		expect((await sessions.get('k'))?.device).toBe('b')
	})

	test('a version that may be absent does not compile, so a write is never unchecked by accident', async () => {
		const sessions = store.bucket<Session>('sessions')
		const missing = await sessions.entry('never-set')
		const write = () =>
			// @ts-expect-error: undefined would write without a check; an absent key is create's
			sessions.set('never-set', { device: 'x', seenAt: 0 }, { ifVersion: missing?.version })
		expect(typeof write).toBe('function')
		expect(await sessions.create('never-set', { device: 'x', seenAt: 0 })).toBe(true)
	})

	test('an expiry holds the key until its time, and expire moves it either way', async () => {
		const drafts = store.bucket<string>('drafts', { type: 'string' })
		await drafts.set('d', 'hello', { ttl: '1h' })
		const entry = await drafts.entry('d')
		expect(entry?.expiresAt?.getTime()).toBeGreaterThan(Date.now() + 59 * 60_000)
		expect(await drafts.expire('d', '2h')).toBe(true)
		expect((await drafts.entry('d'))?.expiresAt?.getTime()).toBeGreaterThan(
			Date.now() + 119 * 60_000,
		)
		expect(await drafts.expire('d', new Date(Date.now() + 60_000))).toBe(true)
		expect((await drafts.entry('d'))?.expiresAt?.getTime()).toBeLessThan(Date.now() + 61_000)
		await drafts.set('gone', 'x', { expiresAt: Date.now() - 1000 })
		expect(await drafts.get('gone')).toBeUndefined()
		expect(await drafts.expire('never-set', '1h')).toBe(false)
	})

	test('create and add write a key only where none is, and say so', async () => {
		const seen = store.bucket('stripe-events', { ttl: '7d' })
		expect(await seen.add('evt_1')).toBe(true)
		expect(await seen.add('evt_1')).toBe(false)
		expect(await seen.has('evt_1')).toBe(true)
		const names = store.bucket<string>('names', { type: 'string' })
		expect(await names.create('ada', 'Lovelace')).toBe(true)
		expect(await names.create('ada', 'Byron')).toBe(false)
		expect(await names.get('ada')).toBe('Lovelace')
	})

	test('take reads and burns, and delete says whether the key was there', async () => {
		const codes = store.bucket<number>('login-codes', { ttl: '15m', type: 'int' })
		await codes.set('123456', 42)
		expect(await codes.take('123456')).toBe(42)
		expect(await codes.take('123456')).toBeUndefined()
		await codes.set('654321', 7)
		expect(await codes.delete('654321')).toBe(true)
		expect(await codes.delete('654321')).toBe(false)
	})

	test('clear empties a branch and those under it', async () => {
		const carts = store.bucket<number>('carts')
		await carts.under('u1').set('a', 1)
		await carts.under('u1', 'saved').set('b', 2)
		await carts.under('u2').set('a', 3)
		await carts.under('u1').clear()
		expect(await carts.under('u1').get('a')).toBeUndefined()
		expect(await carts.under('u1', 'saved').get('b')).toBeUndefined()
		expect(await carts.under('u2').get('a')).toBe(3)
	})

	test('list reads a page in the order of the keys, and all walks every page', async () => {
		const pages = store.bucket<number>('pages').under('p')
		for (let i = 0; i < 25; i++) {
			await pages.set(`k${String(i).padStart(2, '0')}`, i)
		}
		const first = await pages.list({ limit: 10 })
		expect(first.entries.map(e => e.key)).toEqual(Array.from({ length: 10 }, (_, i) => `k0${i}`))
		expect(first.next).toBe('k09')
		const second = await pages.list({ limit: 10, after: first.next })
		expect(second.entries[0]?.key).toBe('k10')
		const all = []
		for await (const e of pages.all({ limit: 7 })) {
			all.push(e.value)
		}
		expect(all).toEqual(Array.from({ length: 25 }, (_, i) => i))
	})

	test('ttl and idle are one or the other, and a duration is checked before anything leaves', () => {
		expect(() => store.bucket('both', { ttl: '1h', idle: '1h' })).toThrow(InvalidError)
		store.bucket('spelled', { idle: '2w3d4h5m6s7ms' })
		// @ts-expect-error: '1hr' spells no duration, which the type sees as it is written
		expect(() => store.bucket('typos', { ttl: '1hr' })).toThrow(InvalidError)
		const fromEnvironment = '90mins'
		expect(() => store.bucket('typos', { ttl: fromEnvironment as Duration })).toThrow(InvalidError)
		expect(() => store.bucket('Bad Name')).toThrow(InvalidError)
	})
})

describe.skipIf(!library)('value types', () => {
	test('each type keeps its values as the core keeps a Rust type', async () => {
		const strings = store.bucket<string>('strings', { type: 'string' })
		await strings.set('s', 'héllo \u{1F600}')
		expect(await strings.get('s')).toBe('héllo \u{1F600}')

		const bytes = store.bucket<Uint8Array>('bytes', { type: 'bytes' })
		await bytes.set('b', new Uint8Array([0, 255, 7]))
		expect(await bytes.get('b')).toEqual(new Uint8Array([0, 255, 7]))

		const floats = store.bucket<number>('floats', { type: 'float' })
		await floats.set('f', -0)
		expect(Object.is(await floats.get('f'), -0)).toBe(true)
		await floats.set('inf', Number.POSITIVE_INFINITY)
		expect(await floats.get('inf')).toBe(Number.POSITIVE_INFINITY)

		const flags = store.bucket<boolean>('flags', { type: 'bool' })
		await flags.set('on', true)
		expect(await flags.get('on')).toBe(true)

		const big = store.bucket<bigint>('big', { type: 'bigint' })
		await big.set('n', 2n ** 62n)
		expect(await big.get('n')).toBe(2n ** 62n)
	})

	test('a value of another type is corrupt, and an integer past a number is refused rather than rounded', async () => {
		await store.bucket('mixed', { type: 'string' }).set('k', 'text')
		expect(await caught(store.bucket('mixed', { type: 'int' }).get('k'))).toBeInstanceOf(
			CorruptError,
		)
		await store.bucket('wide', { type: 'bigint' }).set('k', 2n ** 60n)
		expect(await caught(store.bucket('wide', { type: 'int' }).get('k'))).toBeInstanceOf(
			CorruptError,
		)
		expect(await caught(store.bucket('wide', { type: 'int' }).set('j', 2 ** 60))).toBeInstanceOf(
			InvalidError,
		)
	})

	test("a set's member reads as null from a bucket of JSON", async () => {
		const tags = store.bucket('tags')
		await tags.add('rust')
		expect(await tags.get('rust')).toBeNull()
	})
})

describe.skipIf(!library)('counters', () => {
	test('add gives the new count, each branch apart, and an absent counter is 0', async () => {
		const attempts = store.counters('login-attempts', { ttl: '15m' })
		expect(await attempts.add('10.0.0.1')).toBe(1)
		expect(await attempts.add('10.0.0.1', 4)).toBe(5)
		expect(await attempts.under('eu').add('10.0.0.1')).toBe(1)
		expect(await attempts.get('10.0.0.1')).toBe(5)
		expect(await attempts.get('10.0.0.2')).toBe(0)
		expect(await attempts.delete('10.0.0.1')).toBe(true)
		expect(await attempts.get('10.0.0.1')).toBe(0)
		await attempts.clear()
		expect(await attempts.under('eu').get('10.0.0.1')).toBe(0)
	})

	test('counters kept in memory count all the same', async () => {
		const hits = store.counters('page-hits', { flushEvery: '1s' })
		const counts = await Promise.all(Array.from({ length: 20 }, () => hits.add('/')))
		expect(Math.max(...counts)).toBe(20)
		expect(await hits.get('/')).toBe(20)
	})
})

describe.skipIf(!library)('limits', () => {
	test('a rate limit lets its burst through, then says when to try again, each key and branch apart', async () => {
		const api = store.rateLimit('api', { rate: '2/m' })
		const tenant = api.under('tenant-7')
		expect(await tenant.allow('user-1')).toEqual({
			ok: true,
			left: 1,
			retryAt: undefined,
			windows: {},
		})
		expect((await tenant.allow('user-1')).left).toBe(0)
		const third = await tenant.allow('user-1')
		expect(third.ok).toBe(false)
		expect(third.retryAt?.getTime()).toBeGreaterThan(Date.now() + 29_000)
		expect((await api.allow('user-1')).ok).toBe(true)
		expect(await caught(tenant.allow('user-2', 3))).toBeInstanceOf(InvalidError)
		expect((await tenant.peek('user-1')).ok).toBe(false)
		await tenant.reset('user-1')
		expect((await tenant.allow('user-1')).ok).toBe(true)
	})

	test('a rate it cannot read is refused, its type before its call', () => {
		// @ts-expect-error: 'fast' spells no rate, which the type sees as it is written
		expect(() => store.rateLimit('bad', { rate: 'fast' })).toThrow(InvalidError)
		expect(() => store.rateLimit('bad', { rate: '100/5x' as Rate })).toThrow(InvalidError)
		expect(() => store.rateLimit('bad', { rate: '5/10s', burst: 0 })).toThrow(InvalidError)
	})

	test('a quota counts in every window or in none, and gives uses back', async () => {
		const ai = store.quota('ai', { daily: '2/1d', weekly: '3/7d' })
		const first = await ai.allow('user-1')
		expect(first.ok).toBe(true)
		expect(first.windows.daily).toMatchObject({ used: 1, limit: 2, left: 1 })
		expect(first.windows.weekly).toMatchObject({ used: 1, limit: 3, left: 2 })
		expect(first.windows.daily.resetsAt?.getTime()).toBeGreaterThan(Date.now())
		expect((await ai.allow('user-1')).ok).toBe(true)
		const refused = await ai.allow('user-1')
		expect(refused.ok).toBe(false)
		expect(refused.retryAt).toBeInstanceOf(Date)
		expect((await ai.peek('user-1')).windows.weekly.used).toBe(2)
		await ai.refund('user-1')
		expect((await ai.allow('user-1')).ok).toBe(true)
		await ai.reset('user-1')
		expect((await ai.peek('user-1')).windows.daily.used).toBe(0)
	})
})

describe.skipIf(!library)('once', () => {
	test('a run keeps its answer, and the next run of the key gets it without running', async () => {
		const charges = store.once<{ receipt: string }>('charges')
		let ran = 0
		const charge = async () => {
			ran++
			return { receipt: `r-${ran}` }
		}
		expect(await charges.run('order-1', charge)).toEqual({ receipt: 'r-1' })
		expect(await charges.run('order-1', charge)).toEqual({ receipt: 'r-1' })
		expect(ran).toBe(1)
		expect(await charges.get('order-1')).toEqual({ receipt: 'r-1' })
		expect(await charges.delete('order-1')).toBe(true)
		expect(await charges.run('order-1', charge)).toEqual({ receipt: 'r-2' })
	})

	test('an error keeps nothing, so the next run runs again', async () => {
		const charges = store.once<string>('declines', { keep: '1h' })
		const err = await caught(
			charges.run('order-2', () => {
				throw new Error('the card network timed out')
			}),
		)
		expect((err as Error).message).toBe('the card network timed out')
		expect(await charges.run('order-2', () => 'charged')).toBe('charged')
	})

	test('runs of one key at once run its function once, and each gets its answer', async () => {
		const charges = store.once<number>('at-once').under('shop-1')
		let ran = 0
		const slow = () =>
			new Promise<number>(resolve => {
				ran++
				setTimeout(() => resolve(99), 50)
			})
		const answers = await Promise.all([charges.run('order-3', slow), charges.run('order-3', slow)])
		expect(answers).toEqual([99, 99])
		expect(ran).toBe(1)
	})
})

describe.skipIf(!library)('transactions', () => {
	test('reads decide the writes, which commit together', async () => {
		const stock = store.bucket<number>('stock', { type: 'int' })
		const orders = store.bucket<string>('orders', { type: 'string' })
		await stock.set('sku-1', 1)
		const placed = await store.tx(async tx => {
			const left = (await tx.with(stock).get('sku-1')) ?? 0
			if (left < 1) {
				return false
			}
			await tx.with(stock).set('sku-1', left - 1)
			await tx.with(orders).set('o-1', 'sku-1')
			expect(await tx.with(stock).get('sku-1')).toBe(0)
			return true
		})
		expect(placed).toBe(true)
		expect(await stock.get('sku-1')).toBe(0)
		expect(await orders.get('o-1')).toBe('sku-1')
	})

	test('a key read that changed before the commit runs the function again', async () => {
		const stock = store.bucket<number>('contended', { type: 'int' })
		await stock.set('sku-2', 10)
		let runs = 0
		await store.tx(async tx => {
			runs++
			const left = (await tx.with(stock).get('sku-2')) ?? 0
			if (runs === 1) {
				await stock.set('sku-2', 5)
			}
			await tx.with(stock).set('sku-2', left - 1)
		})
		expect(runs).toBe(2)
		expect(await stock.get('sku-2')).toBe(4)
	})

	test('a throw writes nothing', async () => {
		const notes = store.bucket<string>('notes', { type: 'string' })
		const err = await caught(
			store.tx(async tx => {
				await tx.with(notes).set('n', 'draft')
				throw new Error('changed my mind')
			}),
		)
		expect((err as Error).message).toBe('changed my mind')
		expect(await notes.get('n')).toBeUndefined()
	})

	test('take, create, delete and clear answer as the commit will leave the keys', async () => {
		const codes = store.bucket<number>('tx-codes', { type: 'int' })
		await codes.set('c1', 1)
		await codes.under('u1').set('c2', 2)
		const attempts = store.counters('tx-attempts')
		await store.tx(async tx => {
			expect(await tx.with(codes).take('c1')).toBe(1)
			expect(await tx.with(codes).get('c1')).toBeUndefined()
			expect(await tx.with(codes).create('c1', 3)).toBe(true)
			expect(await tx.with(codes).create('c1', 4)).toBe(false)
			expect(await tx.with(codes).delete('missing')).toBe(false)
			await tx.with(codes).under('u1').clear()
			expect(await tx.with(codes).under('u1').get('c2')).toBeUndefined()
			await tx.with(attempts).add('ip', 2)
		})
		expect(await codes.get('c1')).toBe(3)
		expect(await codes.under('u1').get('c2')).toBeUndefined()
		expect(await attempts.get('ip')).toBe(2)
	})

	test('a handle of another store is refused', async () => {
		const otherDir = mkdtempSync(join(tmpdir(), 'tinystore-kv-other-'))
		const other = await open(otherDir, { embedded: true, library })
		try {
			const theirs = other.bucket<number>('stock', { type: 'int' })
			const err = await caught(store.tx(async tx => tx.with(theirs).get('sku-1')))
			expect(err).toBeInstanceOf(InvalidError)
		} finally {
			await other.close()
			rmSync(otherDir, { recursive: true, force: true })
		}
	})
})
