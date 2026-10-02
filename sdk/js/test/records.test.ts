// records and metrics through a real tinystore serve, the one test/binary.ts built.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import {
	type Condition,
	InvalidError,
	LimitError,
	noneOf,
	oneOf,
	open,
	prefix,
	type Store,
	TooOldError,
	withSignal,
	withTrace,
} from '../src/index.ts'

let dir: string
let store: Store

beforeAll(async () => {
	dir = mkdtempSync(join(tmpdir(), 'tinystore-records-'))
	store = await open(dir, { private: true })
})

afterAll(async () => {
	await store.close()
	await Bun.sleep(50)
	rmSync(dir, { recursive: true, force: true, maxRetries: 20, retryDelay: 50 })
})

async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

const second = 1_000_000_000n

describe('records', () => {
	test('a record comes back as it went in, its time to the nanosecond', async () => {
		const at = BigInt(Date.now()) * 1_000_000n + 123_456n
		await store.records.append({
			at,
			stream: 'web',
			name: 'click',
			level: 'info',
			body: 'bought',
			traceId: '0102030405060708090a0b0c0d0e0f10',
			context: { session: 's1' },
			attrs: [
				['element', 'buy'],
				['x', 812],
				['x', 813],
			],
		})
		const { items: records } = await store.records.scan({ streams: ['web'] })
		expect(records.length).toBe(1)
		const r = records[0]!
		expect(r.at).toBe(at)
		expect([r.stream, r.name, r.level, r.body]).toEqual(['web', 'click', 0, 'bought'])
		expect(r.traceId).toEqual(Uint8Array.from({ length: 16 }, (_, i) => i + 1))
		expect(r.context).toEqual([['session', '"s1"']])
		expect(r.attrs).toEqual([
			['element', '"buy"'],
			['x', '812'],
			['x', '813'],
		])
	})

	test('a query finds records by level, attribute and time, a page at a time', async () => {
		const base = BigInt(Date.now()) * 1_000_000n
		await store.records.append(
			Array.from({ length: 30 }, (_, i) => ({
				at: base + BigInt(i) * second,
				stream: 'api',
				name: 'log',
				level: i % 3 === 0 ? ('warn' as const) : ('info' as const),
				body: `line ${i}`,
				attrs: { route: i % 2 === 0 ? '/notes' : '/users' },
			})),
		)
		const warns = await store.records.scan({ streams: ['api'], minLevel: 'warn', since: '1m' })
		expect(warns.items.length).toBe(10)
		const query = { streams: ['api'], attrs: { route: '/notes' }, limit: 4 }
		const notes = await store.records.scan(query)
		expect(notes.items.length).toBe(4)
		const rest = await store.records.scan({ ...query, after: notes.next })
		expect(rest.items[0]?.body).toBe('line 8')
		expect(await caught(store.records.scan({ ...query, after: 'not a page' }))).toBeInstanceOf(
			InvalidError,
		)
		const all = []
		for await (const r of store.records.all({
			streams: ['api'],
			attrs: { route: '/notes' },
			limit: 4,
		})) {
			all.push(r.body)
		}
		expect(all.length).toBe(15)
		const newest = await store.records.scan({ streams: ['api'], newest: true, limit: 1 })
		expect(newest.items[0]?.body).toBe('line 29')
	})

	test('a record outside the store window is refused, naming it', async () => {
		const err = await caught(
			store.records.append({
				at: new Date(Date.now() - 100 * 86_400_000),
				stream: 'web',
				name: 'old',
			}),
		)
		expect(err).toBeInstanceOf(TooOldError)
	})

	test("another program's lines become records, a JSON line's fields kept and its level found", async () => {
		const lines = store.records.lines('worker')
		lines.write('{"level":40,"time":1,"msg":"slow request","ms":1200}\n')
		lines.write('plain text line\n')
		lines.write('panic: boom\n\tat main.go:1\n')
		await lines.end()
		expect(lines.dropped).toBe(0)
		// the server's writer hands its records to the engine at its next flush, a second at most
		let records = (await store.records.scan({ streams: ['worker'] })).items
		for (let i = 0; i < 60 && records.length < 3; i++) {
			await Bun.sleep(50)
			records = (await store.records.scan({ streams: ['worker'] })).items
		}
		expect(records.map(r => r.name)).toEqual(['json', 'log', 'log'])
		expect(records[0]?.level).toBe(4)
		expect(records[0]?.attrs).toContainEqual(['ms', '1200'])
		expect(records[2]?.body).toContain('at main.go:1')
	})

	test('an id of the wrong length is refused before anything leaves', async () => {
		expect(
			await caught(store.records.append({ stream: 'web', name: 'x', spanId: 'ff' })),
		).toBeInstanceOf(InvalidError)
	})
})

describe('logger', () => {
	test('a logger returns at once, keeps its context and reaches a scan once it has flushed', async () => {
		const log = store.records.logger('app')
		log.info('server started', { port: 3000 })
		log.with({ requestId: 'r1' }).warn('slow request', { ms: 1200 })
		log.event('user.created', { userId: 42 })
		log.error('payment failed', { err: new Error('boom') })
		await log.flush()
		const { items } = await store.records.scan({ streams: ['app'] })
		expect(items.map(r => [r.name, r.level, r.body])).toEqual([
			['log', 0, 'server started'],
			['log', 4, 'slow request'],
			['user.created', undefined, undefined],
			['log', 8, 'payment failed'],
		])
		expect([items[1]?.context, items[1]?.attrs]).toEqual([
			[['requestId', '"r1"']],
			[['ms', '1200']],
		])
		expect(String(items[3]?.attrs[0]?.[1])).toContain('boom')
	})

	test('a full logger drops and counts rather than wait, and keeps its least level', async () => {
		const log = store.records.logger('quiet', { buffer: 2, level: 'warn' })
		log.info('below its level')
		for (const n of ['one', 'two', 'three', 'four']) {
			log.warn(n)
		}
		await log.flush()
		expect(log.dropped).toBe(1)
		const { items } = await store.records.scan({ streams: ['quiet'] })
		expect(items.map(r => r.body)).toEqual(['one', 'two', 'three'])
	})
})

describe('status and cancelling', () => {
	test('a store says what its server is', async () => {
		const status = await store.status()
		expect(status.protocol).toBe(1)
		expect(status.engines).toContain('records')
		expect(status.capability).toBe('admin')
	})

	test('every call under an aborted signal is refused, and one aborted midway rejects', async () => {
		const stopped = new AbortController()
		stopped.abort()
		expect(await caught(withSignal(stopped.signal, () => store.records.scan({})))).toBeDefined()
		expect(
			await caught(withSignal(stopped.signal, () => store.kv.bucket('signals').get('k'))),
		).toBeDefined()
		const fine = await withSignal(new AbortController().signal, () =>
			store.kv.bucket('signals').get('k'),
		)
		expect(fine).toBeUndefined()
	})
})

describe('trace', () => {
	test("a logger's lines and appended records take the trace they were made in", async () => {
		const traceId = '0102030405060708090a0b0c0d0e0f10'
		const log = store.records.logger('traced')
		await withTrace({ traceId, spanId: '0102030405060708' }, async () => {
			log.info('charged')
			await store.records.append({ stream: 'traced', name: 'paid' })
		})
		log.info('outside')
		await log.flush()
		const found = await store.records.scan({ streams: ['traced'], traceId })
		expect(found.items.map(r => (r.name === 'log' ? r.body : r.name)).sort()).toEqual([
			'charged',
			'paid',
		])
	})
})

describe('search', () => {
	test('a search finds records by their text, the case ignored, a page at a time', async () => {
		const now = Date.now()
		const at = (ms: number) => new Date(now - ms)
		await store.records.append([
			{
				at: at(4),
				stream: 'search',
				name: 'log',
				level: 'warn',
				body: 'read: Connection reset by peer',
			},
			{ at: at(3), stream: 'search', name: 'log', level: 'info', body: 'all good' },
			{ at: at(2), stream: 'search', name: 'log', level: 'info', body: 'CONNECTION RESET again' },
			{ at: at(1), stream: 'search', name: 'user.created' },
		])
		const found = []
		for await (const r of store.records.all({
			streams: ['search'],
			search: 'connection reset',
			limit: 1,
		})) {
			found.push(r.body)
		}
		expect(found).toEqual(['read: Connection reset by peer', 'CONNECTION RESET again'])
		const events = await store.records.scan({ streams: ['search'], search: 'USER.created' })
		expect(events.items.map(r => r.name)).toEqual(['user.created'])
	})
})

describe('metrics', () => {
	test('a sample comes back bit for bit, -0 and a NaN payload included', async () => {
		const now = Date.now()
		const values = new Float64Array(3)
		const view = new DataView(values.buffer)
		view.setBigUint64(0, 0x8000000000000000n, true)
		view.setBigUint64(8, 0x7ff8000000000001n, true)
		values[2] = 0.5
		await store.metrics.ingest({
			name: 'cpu',
			kind: 'gauge',
			labels: { host: 'web-1' },
			samples: { times: [now - 2000, now - 1000, now], values },
		})
		const [series] = await store.metrics.read({ name: 'cpu', since: '1m' })
		expect([series?.name, series?.labels]).toEqual(['cpu', { host: 'web-1' }])
		expect(series?.times).toEqual([now - 2000, now - 1000, now])
		const read = new DataView(series!.values.buffer, series!.values.byteOffset)
		expect(read.getBigUint64(0, true)).toBe(0x8000000000000000n)
		expect(read.getBigUint64(8, true)).toBe(0x7ff8000000000001n)
		expect(series?.values[2]).toBe(0.5)
	})

	test("an aggregate's increase counts a counter's reset", async () => {
		const start = Date.now() - 50 * 60_000
		await store.metrics.ingest({
			name: 'requests_total',
			kind: 'counter',
			samples: [
				[start, 100],
				[start + 60_000, 110],
				[start + 120_000, 5],
				[start + 180_000, 20],
			],
		})
		const [agg] = await store.metrics.aggregate({
			name: 'requests_total',
			from: start,
			to: start + 3_600_000,
			width: '1h',
			op: 'increase',
		})
		expect(agg?.buckets.length).toBe(1)
		expect([agg?.buckets[0]?.value, agg?.buckets[0]?.resets, agg?.buckets[0]?.count]).toEqual([
			30, 1, 4,
		])
	})

	test('instruments are ingested at a flush, the same labels in any order one series', async () => {
		const requests = store.metrics.counter('http_requests_total')
		requests.with({ route: '/notes', method: 'POST' }).inc()
		requests.with({ method: 'POST', route: '/notes' }).add(2)
		store.metrics.gauge('inflight').set(5)
		store.metrics.gaugeFunc('queue_depth', () => 7)
		await store.metrics.flush()
		const [posts] = await store.metrics.read({
			name: 'http_requests_total',
			match: { route: '/notes' },
			since: '1m',
		})
		expect(posts?.values[0]).toBe(3)
		expect(posts?.labels).toEqual({ route: '/notes', method: 'POST' })
		const [depth] = await store.metrics.read({ name: 'queue_depth', since: '1m' })
		expect(depth?.values[0]).toBe(7)
	})

	test('conditions find series beyond equality, and noneOf alone is refused', async () => {
		const at = new Date()
		for (const [host, status, env] of [
			['api-1', '200', 'prod'],
			['api-2', '502', 'dev'],
			['web-1', '500', 'prod'],
			['web-2', '500', undefined],
		] as const) {
			const labels: Record<string, string> = { host, status }
			if (env !== undefined) {
				labels.env = env
			}
			await store.metrics.ingest({ name: 'requests', kind: 'counter', labels, samples: [[at, 1]] })
		}
		const hosts = async (where: Record<string, Condition | string>) =>
			(await store.metrics.read({ name: 'requests', where })).map(s => s.labels.host).sort()
		expect(await hosts({ status: oneOf('500', '502') })).toEqual(['api-2', 'web-1', 'web-2'])
		expect(await hosts({ env: noneOf('dev') })).toEqual(['api-1', 'web-1', 'web-2'])
		expect(await hosts({ host: prefix('api-'), status: '200' })).toEqual(['api-1'])
		expect(await hosts({ status: oneOf('418') })).toEqual([])
		expect(await caught(store.metrics.read({ where: { env: noneOf('dev') } }))).toBeInstanceOf(
			InvalidError,
		)
		expect(
			await caught(store.metrics.read({ name: 'requests', where: { env: prefix('') } })),
		).toBeInstanceOf(InvalidError)
	})

	test('an aggregate joins series by a label, exactly, and rates and deltas each', async () => {
		const start = Date.now() - 60_000
		const at = (s: number) => new Date(start + s * 1000)
		for (const [route, host, values] of [
			['/a', '1', [10, 15, 5, 8]],
			['/a', '2', [0, 2, 4, 6]],
			['/b', '1', [100, 100, 101, 103]],
		] as const) {
			await store.metrics.ingest({
				name: 'hits',
				kind: 'counter',
				labels: { route, host },
				samples: values.map((v, i) => [at(i), v] as [Date, number]),
			})
		}
		const range = { name: 'hits', from: at(0), to: at(4), width: 4000 } as const
		const byRoute = await store.metrics.aggregate({ ...range, op: 'increase', by: ['route'] })
		expect(byRoute.map(g => [g.labels.route, g.buckets[0]?.value, g.buckets[0]?.resets])).toEqual([
			['/a', 19, 1],
			['/b', 3, 0],
		])
		const all = await store.metrics.aggregate({ ...range, op: 'count', by: [] })
		expect(all.map(g => [g.labels, g.buckets[0]?.value])).toEqual([[{}, 12]])
		const rates = await store.metrics.aggregate({ ...range, op: 'rate', without: ['host'] })
		expect(rates.map(g => g.buckets[0]?.value)).toEqual([19 / 4, 3 / 4])
		expect(
			await caught(store.metrics.aggregate({ ...range, op: 'delta', by: ['route'] })),
		).toBeInstanceOf(InvalidError)
	})

	test('a limit says which it is, what the read wanted and the bound', async () => {
		const now = Date.now()
		await store.metrics.ingest({
			name: 'bounded',
			kind: 'gauge',
			samples: [
				[new Date(now - 3000), 1],
				[new Date(now - 2000), 2],
				[new Date(now - 1000), 3],
			],
		})
		const refused = await caught(
			store.metrics.read({ name: 'bounded', since: '1m', limits: { decoded: 1 } }),
		)
		expect(refused).toBeInstanceOf(LimitError)
		const limit = refused as LimitError
		expect([limit.limit, limit.bound]).toEqual(['decoded samples', 1])
		expect(limit.wanted).toBeGreaterThan(1)
	})

	test('a plan says what a read would spend, and where it would stop', async () => {
		const now = Date.now()
		await store.metrics.ingest({
			name: 'planned',
			kind: 'gauge',
			samples: [
				[new Date(now - 3000), 1],
				[new Date(now - 2000), 2],
				[new Date(now - 1000), 3],
			],
		})
		const plan = await store.metrics.explain({ name: 'planned', since: '1m' })
		expect([plan.series, plan.decoded, plan.stops]).toEqual([1, 3, undefined])
		const tight = await store.metrics.explain({
			name: 'planned',
			since: '1m',
			limits: { decoded: 1 },
		})
		expect([tight.stops?.limit, tight.stops?.bound, tight.limits.decoded]).toEqual([
			'decoded samples',
			1,
			1,
		])
		const buckets = await store.metrics.explain({
			name: 'planned',
			since: '1m',
			width: '1m',
			op: 'sum',
		})
		expect(buckets.series).toBe(1)
	})

	test('a drop removes a series', async () => {
		expect(await store.metrics.drop({ name: 'cpu', labels: { host: 'web-1' } })).toEqual({
			found: true,
			unreadableGroups: 0,
		})
		expect(await store.metrics.read({ name: 'cpu' })).toEqual([])
	})

	test("a label of the store's own, a range of nothing or of both kinds of start are refused", async () => {
		for (const refused of [
			() => store.metrics.read({ name: 'cpu', match: { __name__: 'other' } }),
			() => store.metrics.read({}),
			() => store.metrics.read({ name: 'cpu', since: '1h', from: 0 }),
			() => store.metrics.ingest({ name: 'cpu', kind: 'gauge', labels: { __x: 'y' }, samples: [] }),
		]) {
			expect(await caught(refused())).toBeInstanceOf(InvalidError)
		}
	})
})
