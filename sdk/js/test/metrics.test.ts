// records and metrics through a real tinystore serve, the one test/binary.ts built.

import { afterAll, beforeAll, describe, expect, spyOn, test } from 'bun:test'
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
} from '../src/index.ts'

let dir: string
let store: Store

beforeAll(async () => {
	dir = mkdtempSync(join(tmpdir(), 'tinystore-metrics-'))
	store = await open(dir, { private: true })
})

afterAll(async () => {
	await store.close()
	rmSync(dir, { recursive: true, force: true })
})

async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

/** What was written to stderr while fn ran, where TinyStore says its own lines. */
async function stderrWhile(fn: () => Promise<void>): Promise<string> {
	const spy = spyOn(process.stderr, 'write')
	try {
		await fn()
		return spy.mock.calls.map(([text]) => String(text)).join('')
	} finally {
		spy.mockRestore()
	}
}

function times(text: string, said: string): number {
	return text.split(said).length - 1
}

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

	test('an increase counts the step into each bucket, the first from before the range', async () => {
		const start = Date.now() - 50 * 60_000
		await store.metrics.ingest({
			name: 'steps_total',
			kind: 'counter',
			samples: [
				[start, 100],
				[start + 60_000, 110],
				[start + 120_000, 5],
				[start + 180_000, 20],
			],
		})
		const range = {
			name: 'steps_total',
			from: start + 60_000,
			to: start + 240_000,
			op: 'increase',
		} as const
		const [minutes] = await store.metrics.aggregate({ ...range, width: '1m' })
		expect(minutes?.buckets.map(b => [b.value, b.resets, b.lookback])).toEqual([
			[10, 0, true],
			[5, 1, false],
			[15, 0, false],
		])
		const [halves] = await store.metrics.aggregate({ ...range, width: '30s' })
		expect([halves?.buckets[0]?.value, halves?.buckets[0]?.lookback]).toEqual([0, false])
		const [looked] = await store.metrics.aggregate({ ...range, width: '30s', lookback: '1m' })
		expect([looked?.buckets[0]?.value, looked?.buckets[0]?.lookback]).toEqual([10, true])
	})

	test("latest is each series' newest sample, and first and last a bucket's ends", async () => {
		const now = Date.now()
		await store.metrics.ingest([
			{
				name: 'depth',
				kind: 'gauge',
				labels: { queue: 'mail' },
				samples: [
					[now - 3000, 4],
					[now - 1000, 7],
				],
			},
			{ name: 'depth', kind: 'gauge', labels: { queue: 'sms' }, samples: [[now - 600_000, 2]] },
		])
		const latest = await store.metrics.latest({ name: 'depth', since: '1m' })
		expect(latest.map(s => [s.labels.queue, s.times, Array.from(s.values)])).toEqual([
			['mail', [now - 1000], [7]],
		])
		const range = { name: 'depth', match: { queue: 'mail' }, since: '1m', width: '1m' } as const
		const [first] = await store.metrics.aggregate({ ...range, op: 'first' })
		const [last] = await store.metrics.aggregate({ ...range, op: 'last' })
		expect([first?.buckets[0]?.value, last?.buckets[0]?.value]).toEqual([4, 7])
	})

	test("a description is kept by name, and an instrument's is written at its flush", async () => {
		await store.metrics.describe('query_ms', { unit: 'ms', help: 'How long a query took.' })
		expect(await store.metrics.description('query_ms')).toEqual({
			unit: 'ms',
			help: 'How long a query took.',
		})
		expect(await store.metrics.description('never_described')).toEqual({ unit: '', help: '' })
		store.metrics.counter('described_total', { help: 'Requests served.' }).inc()
		store.metrics.timer('described_query', { help: 'Queries.' }).record(2)
		await store.metrics.flush()
		expect(await store.metrics.description('described_total')).toEqual({
			unit: '',
			help: 'Requests served.',
		})
		expect(await store.metrics.description('described_query_sum')).toEqual({
			unit: 'ms',
			help: 'Queries.',
		})
		expect(() => store.metrics.gauge('described_gauge', { unit: 'x'.repeat(33) })).toThrow(
			InvalidError,
		)
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

	test('a refused instrument keeps no other out, and says so once', async () => {
		const refused = store.metrics.counter('refused_total').with({ '': 'x' })
		const said = await stderrWhile(async () => {
			store.metrics.counter('kept_total').inc()
			refused.inc()
			store.metrics.timer('refused_ms').with({ '': 'y' }).record(5)
			store.metrics.gaugeFunc('unread', () => {
				throw new Error('no reading')
			})
			await store.metrics.flush()
			store.metrics.counter('kept_total').inc()
			refused.inc()
			await store.metrics.flush()
		})
		const [kept] = await store.metrics.read({ name: 'kept_total', since: '1m' })
		expect(kept?.values).toEqual(Float64Array.of(1, 2))
		expect(times(said, 'instrument refused')).toBe(2)
		expect(said).toContain('refused_total')
		expect(said).toContain('refused_ms')
		expect(times(said, 'gauge read failed')).toBe(1)
		expect(store.metrics.failures).toBe(0)
	})

	test('a timer writes its count, sum and longest at each flush; measure answers and rethrows', async () => {
		const latency = store.metrics.timer('timed_ms')
		latency.record(10)
		latency.record('30ms')
		latency.with({ route: '/a' }).record(2.5)
		expect(await latency.measure(() => 42)).toBe(42)
		const boom = new Error('boom')
		expect(
			await caught(
				latency.measure(async () => {
					throw boom
				}),
			),
		).toBe(boom)
		expect(() => latency.record(-1)).toThrow(InvalidError)
		expect(() => store.metrics.counter('timed_ms_count')).toThrow(InvalidError)
		store.metrics.counter('busy_ms_sum').inc()
		expect(() => store.metrics.timer('busy_ms')).toThrow(InvalidError)
		await store.metrics.flush()
		await Bun.sleep(2)
		await store.metrics.flush() // nothing measured since: the totals again, no longest

		const last = async (name: string, route?: string) => {
			const series = await store.metrics.read({ name, since: '1m' })
			const s = series.find(s => s.labels.route === route)
			return s?.values
		}
		expect((await last('timed_ms_count'))?.at(-1)).toBe(4)
		const sum = (await last('timed_ms_sum'))?.at(-1) ?? 0
		expect(sum >= 40 && sum < 1040).toBe(true)
		expect(await last('timed_ms_max')).toEqual(Float64Array.of(30))
		expect(await last('timed_ms_count', '/a')).toEqual(Float64Array.of(1, 1))
		expect(await last('timed_ms_sum', '/a')).toEqual(Float64Array.of(2.5, 2.5))
		expect(await last('timed_ms_max', '/a')).toEqual(Float64Array.of(2.5))
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

	test('an invalid description cannot prevent good instruments from flushing', async () => {
		for (const name of ['', 'x'.repeat(4097), String.fromCharCode(0xd800)]) {
			expect(() => store.metrics.counter(name, { help: 'requests' })).toThrow(InvalidError)
		}
		const description = { unit: 'count', help: 'requests' }
		store.metrics.counter('described-good', description).inc()
		description.unit = 'x'.repeat(100)
		await store.metrics.flush()
		expect((await store.metrics.read({ name: 'described-good', since: '1h' })).length).toBe(1)
		expect(await store.metrics.description('described-good')).toEqual({
			unit: 'count',
			help: 'requests',
		})
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
