// Streams of events as docs/guides/records/README.md has them over the protocol, through
// each way a store is reached: appends written whole, reads by time and by
// what events hold a page at a time, counts, what a store holds, a poll's
// cursor, and a store's logger keeping its lines.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { ClosedError, InvalidError, type Store, sql, withTrace } from '../src/index.ts'
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

		test('a where list is one of, { not } finds an absent key, { prefix } a text', async () => {
			// contract: REC-18
			const calls = store.events('calls')
			await calls.append([
				{ name: 'call', fields: { userId: 1, op: 'review.list' } },
				{ name: 'call', fields: { userId: 2, op: 'drafts.get' } },
				{ name: 'call', fields: { userId: 3, op: 'courses.list', env: 'test' } },
				{ name: 'call', fields: { userId: 4, tags: ['a', 'b'] } },
			])
			const users = async (where: Record<string, unknown>) =>
				(await calls.list({ where })).events.map(event => event.fields.userId)
			expect(await users({ op: ['review.list', 'drafts.get'] })).toEqual([1, 2])
			expect(await users({ userId: [1, 3, 99] })).toEqual([1, 3])
			expect(await users({ env: { not: 'test' } })).toEqual([1, 2, 4])
			expect(await users({ op: { not: ['review.list', 'drafts.get'] } })).toEqual([3, 4])
			expect(await users({ op: { prefix: 'courses.' } })).toEqual([3])
			expect(await users({ tags: [['a', 'b']] })).toEqual([4])
			const rows = await calls.aggregate({
				by: ['op'],
				where: { op: ['review.list', 'drafts.get'] },
			})
			expect(rows).toHaveLength(2)
			for (const where of [{ op: [] }, { op: { a: 1 } }, { op: { prefix: '' } }]) {
				expect(await caught(calls.count({ where }))).toBeInstanceOf(InvalidError)
			}
		})

		test('sessions cut a read by a pause: two visits 90 minutes apart are two', async () => {
			// contract: REC-23
			const visits = store.events('visits')
			const start = Date.now() - 4 * 3_600_000
			const at = (minutes: number) => new Date(start + minutes * 60_000)
			await visits.append([
				{ name: 'open', fields: { userId: 1 }, at: at(0) },
				{ name: 'click', fields: { userId: 1 }, at: at(10) },
				{ name: 'open', fields: { userId: 2 }, at: at(5) },
				{ name: 'open', fields: { userId: 1 }, at: at(100) },
			])
			const sessions = await visits.sessions({ since: '5h', by: 'userId', gap: '30m', events: 1 })
			expect(
				sessions.map(session => [session.by.userId, session.count, session.events.length]),
			).toEqual([
				[1, 2, 1],
				[2, 1, 1],
				[1, 1, 1],
			])
			expect(sessions[0]?.from).toEqual(at(0))
			expect(sessions[0]?.to).toEqual(at(10))
			expect(sessions[0]?.partial).toBe(false)
			const first = await visits.sessions({ since: '5h', by: ['userId'], gap: '30m', first: 1 })
			expect(first.map(session => [session.by.userId, session.events])).toEqual([
				[1, []],
				[2, []],
			])
			expect(
				await caught(visits.sessions({ by: 'userId', gap: '30m', events: 1001 })),
			).toBeInstanceOf(InvalidError)
		})

		test('a where keeps to the values a database’s statement gives, read as the query starts', async () => {
			// contract: REC-22
			const db = await store.database('school', {
				migrations: {
					'0001_teachers.sql': 'create table teachers (id integer primary key) strict',
				},
			})
			await db.exec`insert into teachers (id) values (2)`
			const events = store.events('used')
			await events.append([
				{ name: 'open', fields: { userId: 1 } },
				{ name: 'open', fields: { userId: 2 } },
			])
			const teachers = db.statement`select id from teachers`
			expect(await events.count({ since: '1d', where: { userId: teachers } })).toBe(1)
			expect(await events.count({ where: { userId: { not: teachers } } })).toBe(1)
			const rows = await events.aggregate({
				by: ['name'],
				distinct: ['userId'],
				where: { userId: teachers },
			})
			expect(rows.map(row => [row.count, row.distinct.userId])).toEqual([[1, 1]])
			const above = db.statement('select id from teachers where id > ?', 0)
			const { events: found } = await events.list({ where: { userId: above } })
			expect(found.map(event => event.fields.userId)).toEqual([2])
			expect(await store.events().count({ streams: ['used'], where: { userId: teachers } })).toBe(1)
			const bare = await caught(events.count({ where: { userId: sql`select id from teachers` } }))
			expect(bare).toBeInstanceOf(InvalidError)
			expect((bare as Error).message).toContain('db.statement')
			const blob = await caught(events.count({ where: { userId: db.statement`select x'07'` } }))
			expect(blob).toBeInstanceOf(InvalidError)
		})

		test('largest gives the events of a key’s largest numbers; top reads the message', async () => {
			// contract: REC-19
			const pages = store.events('pages')
			for (const ms of [120, 9000, 300]) {
				await pages.append('page-open', { ms, path: `/p${ms}` })
			}
			await pages.append('page-open', { ms: 'slow' })
			const slowest = await pages.largest('ms', { names: ['page-open'], since: '1h', limit: 2 })
			expect(slowest.map(event => event.fields.path)).toEqual(['/p9000', '/p300'])
			expect(await caught(pages.largest('ms', { limit: 1001 }))).toBeInstanceOf(InvalidError)
			const errors = store.events('client-errors')
			await errors.append('error', {}, { message: 'TypeError: x' })
			await errors.append('error', {}, { message: 'TypeError: x' })
			await errors.append('error', { message: 'copied' })
			expect(await errors.top('message')).toEqual([
				{ value: 'TypeError: x', count: 2 },
				{ value: 'copied', count: 1 },
			])
		})

		test('delete takes a person’s events out of every stream named, once, and takes a where', async () => {
			// contract: REC-20
			// a store of its own: a delete packs what waits, which the poll's test below expects nothing of
			const own = mkdtempSync(join(tmpdir(), 'tinystore-delete-'))
			const store = await way.open(own)
			try {
				await deleting(store)
			} finally {
				await store.close()
				await way.leave(own)
				await removed(own)
			}
		})

		async function deleting(store: Store): Promise<void> {
			const activity = store.events('activity')
			const errors = store.events('client-errors-of-users')
			for (const userId of [97516, 97517, 97516]) {
				await activity.append('click', { userId })
				await errors.append('error', {}, { context: { userId } })
			}
			const streams = ['activity', 'client-errors-of-users']
			expect(await store.events().delete({ streams, where: { userId: 97516 } })).toBe(4)
			expect(await store.events().delete({ streams, where: { userId: 97516 } })).toBe(0)
			const later = new Date(Date.now() + 60_000)
			expect(await activity.delete({ where: { userId: [97516, 97517] }, to: later })).toBe(1)
			expect(await errors.count()).toBe(1)
			// each started only as it is awaited: one refused before its turn is an unhandled rejection under Node
			const refused = [
				() => activity.delete({ where: {} }),
				() => activity.delete({ where: { userId: 1 }, limit: 1 } as never),
				() => activity.delete({ where: { userId: 1 }, search: 'x' } as never),
			]
			for (const deleted of refused) {
				expect(await caught(deleted())).toBeInstanceOf(InvalidError)
			}
		}

		test('fill gives a row for every span, and a calendar’s rows their local date', async () => {
			// contract: REC-16, REC-17
			const repro = store.events('repro')
			await repro.append('call', { op: 'a' })
			const hours = await repro.aggregate({ names: ['call'], since: '6h', every: '1h', fill: true })
			expect(hours.length).toBeGreaterThanOrEqual(6)
			expect(hours.map(row => row.count).reduce((a, b) => a + b)).toBe(1)
			expect(hours.every(row => row.date === undefined)).toBe(true)
			const days = await repro.aggregate({
				since: '3d',
				calendar: 'day',
				timeZone: 'Europe/Moscow',
				fill: true,
			})
			expect(days.length).toBeGreaterThanOrEqual(3)
			for (const row of days) {
				expect(row.date).toMatch(/^\d{4}-\d{2}-\d{2}$/)
			}
			expect(await caught(repro.aggregate({ every: '1h', fill: true }))).toBeInstanceOf(
				InvalidError,
			)
		})

		test('a field no query has is InvalidError, named, never dropped', async () => {
			// contract: REC-15
			const api = store.events('api')
			const named = await caught(api.count({ name: 'request' } as never))
			expect(named).toBeInstanceOf(InvalidError)
			expect((named as Error).message).toContain(
				'records query: no field `name`; did you mean `names`?',
			)
			const stream = await caught(store.events().list({ stream: 'api' } as never))
			expect((stream as Error).message).toContain('did you mean `streams`?')
			const other = await caught(api.aggregate({ op: 'increase' } as never))
			expect((other as Error).message).toContain('no field `op`')
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

		// contract: REC-14
		test('a stream says which names and fields its events carry, with their kinds and counts', async () => {
			const orders = store.events('orders')
			for (let orderId = 0; orderId < 100; orderId++) {
				await orders.append('order.paid', {
					orderId,
					amount: 12.5,
					currency: 'EUR',
					paid: orderId % 3 !== 0,
				})
			}
			await orders.append('order.refunded', { orderId: 3, note: null }, { context: { actor: 7 } })
			expect(await store.events().fields('orders')).toEqual({
				eventNames: [
					{ name: 'order.paid', count: 100 },
					{ name: 'order.refunded', count: 1 },
				],
				fields: [
					{ name: 'amount', kinds: ['float'], count: 100 },
					{ name: 'currency', kinds: ['text'], count: 100 },
					{ name: 'note', kinds: ['null'], count: 1 },
					{ name: 'orderId', kinds: ['integer'], count: 101 },
					{ name: 'paid', kinds: ['bool'], count: 100 },
				],
				context: [{ name: 'actor', kinds: ['integer'], count: 1 }],
			})
			expect(await store.events().fields('nothing')).toEqual({
				eventNames: [],
				fields: [],
				context: [],
			})
			expect(await caught(store.events().fields('Not A Name'))).toBeInstanceOf(InvalidError)
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

		// contract: LOG-10
		test('a key a nested with gives again is kept and found by its new value alone', async () => {
			const log = store.logger('access', { console: 'off' })
			log
				.with({ component: 'http', user: 7 })
				.with({ component: 'access' })
				.info('request completed')
			await log.flush()
			const access = store.events('access')
			const { events } = await access.list()
			expect(events.map(event => event.context)).toEqual([{ component: 'access', user: 7 }])
			const found = async (component: string) =>
				(await access.list({ where: { component } })).events.length
			expect([await found('access'), await found('http')]).toEqual([1, 0])
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
