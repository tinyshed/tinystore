// Streams of events as docs/guides/records/README.md has them over the protocol, through
// each way a store is reached: appends written whole, reads by time and by
// what events hold a page at a time, counts, what a store holds, a poll's
// cursor, and a store's logger keeping its lines.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { ClosedError, InvalidError, type Store, withTrace } from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

const traceId = '4bf92f3577b34da6a3ce929d0e0e4736'
const spanId = '00f067aa0ba902b7'

for (const way of ways) {
	describe.skipIf(!way.ready)(`events through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-events-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		test('an appended event comes back whole, an integer past 2^53 a bigint', async () => {
			const audit = store.events('audit')
			const changedAt = new Date(Date.now() - 60_000)
			await audit.append(
				'role.changed',
				{ user: 42, id: 9007199254740993n, tags: ['a', 'b'] },
				{ at: changedAt, context: { actor: 7 }, level: 'warn', message: 'role changed' },
			)
			await withTrace({ traceId, spanId }, () => audit.append('signed.in', { user: 42 }))
			const { events, next } = await audit.list()
			expect(next).toBeUndefined()
			expect(events.map(event => event.name)).toEqual(['role.changed', 'signed.in'])
			const [changed, signed] = events
			expect(changed?.at).toEqual(changedAt)
			expect(changed?.stream).toBe('audit')
			expect(changed?.fields).toEqual({ user: 42, id: 9007199254740993n, tags: ['a', 'b'] })
			expect(changed?.context).toEqual({ actor: 7 })
			expect([changed?.level, changed?.message]).toEqual(['warn', 'role changed'])
			expect([signed?.traceId, signed?.spanId]).toEqual([traceId, spanId])
		})

		test('an event whose id its stream holds is written as nothing, and its id read back', async () => {
			const deploys = store.events('deploys')
			const delivery = '72d3162e-cc78-11e3-81ab-4c9367dc0958'
			expect(await deploys.append('push', { sha: '26a0865' }, { id: delivery })).toBe(true)
			expect(await deploys.append('push', { sha: 'f00ba47' }, { id: delivery })).toBe(false)
			expect(await deploys.append('tick')).toBe(true)
			const list = [
				{ name: 'push', id: delivery },
				{ name: 'tag', id: 'v0.2.0' },
				{ name: 'tag', id: 'v0.2.0' },
				{ name: 'tick' },
			]
			expect(await deploys.append(list)).toBe(2)
			const { events } = await deploys.list()
			expect(events.map(event => [event.name, event.id])).toEqual([
				['push', delivery],
				['tick', undefined],
				['tag', 'v0.2.0'],
				['tick', undefined],
			])
			expect(events[0]?.fields).toEqual({ sha: '26a0865' })
			const empty = await caught(deploys.append('push', {}, { id: '' }))
			expect(empty).toBeInstanceOf(InvalidError)
		})

		test('a list is appended whole or not at all, and a refused event names its place', async () => {
			const clicks = store.events('clicks')
			const err = await caught(clicks.append([{ name: 'view', fields: { page: 1 } }, { name: '' }]))
			expect(err).toBeInstanceOf(InvalidError)
			expect((err as Error).message).toContain('event 1')
			const refused = await caught(
				clicks.append([
					{ name: 'view', fields: { page: 1 } },
					{ name: 'view', at: new Date(Date.now() + 3_600_000) },
				]),
			)
			expect(refused).toBeInstanceOf(Error)
			expect(await clicks.count()).toBe(0)
		})

		test('a query finds by what events hold, a page at a time', async () => {
			const api = store.events('api')
			// a millisecond apart: a page never splits a time, and thirty of one time fill no page of two
			const base = Date.now() - 60_000
			await api.append(
				Array.from({ length: 30 }, (_, n) => ({
					name: 'request',
					fields: { route: n % 3 === 0 ? '/login' : '/notes', ms: n },
					at: new Date(base + n),
					level: n % 10 === 0 ? ('error' as const) : ('info' as const),
				})),
			)
			expect((await api.list({ where: { route: '/login' } })).events).toHaveLength(10)
			expect((await api.list({ minLevel: 'error' })).events.map(event => event.fields.ms)).toEqual([
				0, 10, 20,
			])
			expect(
				(await api.list({ search: 'LOGIN', newest: true, limit: 2 })).events.map(e => e.fields.ms),
			).toEqual([27, 24])
			const first = await api.list({ limit: 12 })
			expect(first.events).toHaveLength(12)
			expect(first.next).toBeDefined()
			const seen: unknown[] = []
			for await (const event of api.all({ limit: 7 })) {
				seen.push(event.fields.ms)
			}
			expect(seen).toEqual(Array.from({ length: 30 }, (_, n) => n))
			expect(
				(await store.events().list({ streams: ['api'], names: ['request'] })).events,
			).toHaveLength(30)
		})

		test('a condition given as undefined is InvalidError, never a condition left out', async () => {
			const api = store.events('api')
			expect(await caught(api.list({ traceId: undefined as unknown as string }))).toBeInstanceOf(
				InvalidError,
			)
			expect(await caught(api.count({ where: { user: undefined } }))).toBeInstanceOf(InvalidError)
		})

		test('counts, a key’s top values and rows of a span with their stats', async () => {
			const checkout = store.events('checkout')
			await checkout.append(
				[10, 20, 30, 40].map(ms => ({
					name: 'paid',
					fields: { method: ms < 25 ? 'card' : 'cash', ms },
				})),
			)
			expect(await checkout.count({ names: ['paid'] })).toBe(4)
			expect(await checkout.top('method')).toEqual([
				{ value: 'card', count: 2 },
				{ value: 'cash', count: 2 },
			])
			const rows = await checkout.aggregate({ by: ['method'], stats: ['ms'], distinct: ['ms'] })
			expect(rows.map(row => [row.by, row.count, row.stats.ms, row.distinct])).toEqual([
				[{ method: 'card' }, 2, { samples: 2, sum: 30, min: 10, max: 20, avg: 15 }, { ms: 2 }],
				[{ method: 'cash' }, 2, { samples: 2, sum: 70, min: 30, max: 40, avg: 35 }, { ms: 2 }],
			])
		})

		test('a store says its streams, what each keeps, and the keys events hold', async () => {
			const kept = store.events('kept', { keep: '30d' })
			await kept.append('signup', { plan: 'pro', user: 1 })
			const stream = (await store.events().streams()).find(info => info.name === 'kept')
			expect(stream?.keep).toBe('30d')
			expect(stream?.retainedFrom.getTime()).toBeLessThanOrEqual(stream?.newest.getTime() ?? 0)
			expect(await kept.fieldNames({ prefix: 'p' })).toEqual(['plan'])
		})

		test('a poll gives a cursor, and nothing not packed yet', async () => {
			const batch = await store.events().poll()
			expect(batch.events).toEqual([])
			expect(batch.cursor).not.toBe('')
			expect(await caught(store.events().poll(batch.cursor, { limit: 0 }))).toBeInstanceOf(
				InvalidError,
			)
		})

		test('a tail gives the events written after it that its query finds, a batch as they come', async () => {
			const live = store.events('live')
			const tail = live.tail({ names: ['tick'] })[Symbol.asyncIterator]()
			const first = tail.next()
			let n = 0
			const ticking = setInterval(() => {
				live.append('tick', { n: n++ }).catch(() => {})
			}, 20)
			try {
				const read = await first
				if (read.done) {
					throw new Error('the tail ended')
				}
				expect(read.value.missed).toBe(0)
				expect(read.value.events.map(event => [event.stream, event.name])).toContainEqual([
					'live',
					'tick',
				])
			} finally {
				clearInterval(ticking)
				await tail.return(undefined)
			}
		})

		test('another program’s output becomes events, however it was cut', async () => {
			const out = store.events('child').lines()
			out.write('level=warn msg="slow request" ms=12')
			out.write(new TextEncoder().encode('00\n{"level":30,"msg":"ok"}\nthe last line'))
			await out.end()
			expect(out.dropped).toBe(0)
			const { events } = await store.events('child').list()
			expect(events.map(event => [event.name, event.level])).toEqual([
				['logfmt', 'warn'],
				['json', 'info'],
				['log', undefined],
			])
			expect(events[2]?.message).toBe('the last line')
			expect(() => out.write('late\n')).toThrow(ClosedError)
		})

		test('nothing damaged is listed, and what still reads is never dropped', async () => {
			const all = store.events()
			expect(await all.damaged()).toEqual([])
			const now = new Date()
			const reads = {
				part: 'head row',
				id: 1,
				stream: 'audit',
				first: now,
				last: now,
				error: '',
			} as const
			expect(await caught(all.drop(reads))).toBeInstanceOf(InvalidError)
		})

		test("a store's logger never waits: a full buffer drops and counts", async () => {
			const log = store.logger('busy', { console: 'off', buffer: 2 })
			for (let n = 0; n < 6; n++) {
				log.info('a line', { n })
			}
			expect(log.dropped).toBeGreaterThan(0)
			await log.flush()
			const kept = (await store.events('busy').list()).events.length
			expect(kept + log.dropped).toBe(6)
		})

		test("a store's logger keeps a buffer of more text than one request holds", async () => {
			const log = store.logger('stacks', { console: 'off', buffer: 64 })
			const stack = 'x'.repeat(100 * 1024)
			for (let n = 0; n < 24; n++) {
				log.error('failed', { stack })
			}
			await log.flush()
			expect([await store.events('stacks').count({ since: '1h' }), log.dropped]).toEqual([24, 0])
		})

		test("a store's logger keeps its lines, their fields and the context of with", async () => {
			const log = store.logger('worker', { console: 'off' })
			log.info('started', { port: 3000 })
			log.with({ job: 7 }).warn('slow', { ms: 1200 })
			log.event('signup', { plan: 'pro' })
			await log.flush()
			const { events } = await store.events('worker').list()
			expect(events.map(event => [event.name, event.level, event.message])).toEqual([
				['log', 'info', 'started'],
				['log', 'warn', 'slow'],
				['signup', undefined, undefined],
			])
			expect([events[1]?.fields, events[1]?.context]).toEqual([{ ms: 1200 }, { job: 7 }])
			expect(log.dropped).toBe(0)
		})

		// contract: LOG-09, REC-01
		test('a field given as undefined is left out of what is kept, and null stays null', async () => {
			const kept = store.events('kept')
			await kept.append(
				'happened',
				{ passed: undefined, owner: null, n: 1 },
				{ context: { who: undefined, user: 'ann' } },
			)
			await kept.append('alone', { passed: undefined }, { context: { who: undefined } })
			const log = store.logger('kept', { console: 'off' })
			log.with({ requestId: undefined }).info('model call', { passed: undefined, n: 2 })
			log.event('signup', { plan: undefined, n: 3 })
			await log.flush()
			const { events } = await store.events('kept').list()
			// a line's time is the program's clock to the millisecond, so the names say which is which
			const byName = Object.fromEntries(
				events.map(event => [event.name, [event.fields, event.context]]),
			)
			expect(byName).toEqual({
				happened: [{ owner: null, n: 1 }, { user: 'ann' }],
				alone: [{}, {}],
				log: [{ n: 2 }, {}],
				signup: [{ n: 3 }, {}],
			})
		})

		// contract: LOG-08
		test("a store's logger with a console part prints less than it keeps", async () => {
			const printed: string[] = []
			const to = { write: (text: string) => void printed.push(JSON.parse(text).msg) }
			const log = store.logger('quiet', {
				level: { store: 'info', console: 'warn' },
				console: 'json',
				to,
				env: false,
			})
			log.debug('cache miss')
			log.info('signed in')
			log.warn('slow request')
			await log.flush()
			const { events } = await store.events('quiet').list()
			expect(printed).toEqual(['slow request'])
			expect(events.map(event => event.message)).toEqual(['signed in', 'slow request'])
		})
	})
}
