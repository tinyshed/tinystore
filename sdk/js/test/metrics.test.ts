// Metrics as docs/guides/metrics/README.md has them over the protocol, through each way a
// store is reached: series ingested whole and read back bit for bit, the
// buckets of an aggregate, the newest samples, what a store holds and its
// removal, and instruments whose changes reach the store's totals once.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

import { type Connection, embedded, Link } from '../src/connection.ts'
import {
	exponentialBuckets,
	InvalidError,
	LimitError,
	type MetricsQuery,
	OutcomeUnknownError,
	open,
	type Store,
	UnavailableError,
	type ValueBucket,
} from '../src/index.ts'
import { binOf, Instruments } from '../src/instruments.ts'
import { Metrics } from '../src/metrics.ts'
import { bunRuntime } from '../src/runtime/bun.ts'
import { nodeRuntime } from '../src/runtime/node.ts'
import { caught, removed, until, ways } from './ways.ts'

/** The last value of each series of a name, by the labels given. */
async function last(store: Store, name: string): Promise<Record<string, number>> {
	const read = await store.metrics.read({ name, since: '1h' })
	return Object.fromEntries(
		read.map(series => [JSON.stringify(series.labels), series.values.at(-1)!]),
	)
}

for (const way of ways) {
	describe.skipIf(!way.ready)(`metrics through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-metrics-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		test('a series comes back bit for bit, its samples as two columns', async () => {
			const at = Date.now() - 60_000
			const values = new Float64Array([0.42, -0, Number.NaN, Number.POSITIVE_INFINITY])
			new DataView(values.buffer).setBigUint64(16, 0x7ff8_0000_dead_beefn, true)
			const times = [at, at + 1000, at + 2000, at + 3000]
			await store.metrics.ingest({
				name: 'cpu',
				kind: 'gauge',
				labels: { host: 'web-1' },
				times,
				values,
			})
			const [cpu] = await store.metrics.read({ name: 'cpu', where: { host: 'web-1' }, since: '1h' })
			expect(cpu?.labels).toEqual({ host: 'web-1' })
			expect(Array.from(cpu?.times ?? [])).toEqual(times)
			expect(new Uint8Array(cpu?.values.buffer ?? new ArrayBuffer(0))).toEqual(
				new Uint8Array(values.buffer),
			)
			const newest = await store.metrics.latest({ name: 'cpu', since: '1h' })
			expect(newest.map(sample => [sample.at.getTime(), sample.labels.host])).toEqual([
				[at + 3000, 'web-1'],
			])
		})

		test("an aggregate's buckets are exact by group, and a timer's percentile is its bounds", async () => {
			const at = Date.now() - 60_000
			const route = (name: string, values: number[]) => ({
				name: 'requests_total',
				kind: 'counter' as const,
				labels: { route: name },
				samples: values.map((value, i) => [at + i * 1000, value] as const),
			})
			await store.metrics.ingest([route('/a', [0, 10, 25]), route('/b', [0, 3, 1])])
			const traffic = await store.metrics.aggregate({
				name: 'requests_total',
				since: '1h',
				op: 'increase',
				by: ['route'],
			})
			const byRoute = Object.fromEntries(
				traffic.map(group => [group.labels.route, group.buckets[0]]),
			)
			expect(byRoute['/a']).toMatchObject({ value: 25, resets: 0 })
			expect(byRoute['/b']).toMatchObject({ value: 4, resets: 1 })
			const routes = async (where: NonNullable<MetricsQuery['where']>) => {
				const read = await store.metrics.read({ name: 'requests_total', where, since: '1h' })
				return read.map(series => series.labels.route).sort()
			}
			expect(await routes({ route: { not: '/a' } })).toEqual(['/b'])
			expect(await routes({ route: ['/a', '/b'] })).toEqual(['/a', '/b'])
			expect(await routes({ route: { prefix: '/' } })).toEqual(['/a', '/b'])
			const latency = store.metrics.timer('db_ms', { buckets: [5, 10, 25] })
			for (const took of [3, 7, 20, 100]) {
				latency.record(took)
			}
			await store.metrics.flush()
			const [p50] = await store.metrics.aggregate({ name: 'db_ms', since: '1h', op: 'p50' })
			expect(p50?.buckets[0]).toMatchObject({ lower: 5, upper: 10, samples: 4 })
			const [p99] = await store.metrics.aggregate({
				name: 'db_ms',
				since: '1h',
				op: 'p99.9',
				estimate: true,
			})
			expect(p99?.buckets[0]).toMatchObject({
				lower: 25,
				upper: Number.POSITIVE_INFINITY,
				estimate: 25,
			})
			const [mean] = await store.metrics.aggregate({ name: 'db_ms', since: '1h', op: 'avg' })
			expect(mean?.buckets[0]?.value).toBe(32.5)
			const unranked = store.metrics.aggregate({ name: 'db_ms', since: '1h', op: 'p' as 'p95' })
			expect(await caught(unranked)).toBeInstanceOf(InvalidError)
		})

		test("instruments count into the store's totals, a set of labels a series each", async () => {
			const requests = store.metrics.counter('http_requests_total', { help: 'Requests served.' })
			requests.with({ route: '/notes', status: 200 }).inc()
			requests.with({ status: 200, route: '/notes' }).add(2)
			requests.with({ route: '/users', status: 200 }).add(0)
			const depth = store.metrics.gauge('queue_depth', { unit: 'jobs' })
			depth.set(12)
			depth.inc()
			store.metrics.gaugeFunc('open_tickets', async () => 7)
			const sizes = store.metrics.histogram('response_bytes', {
				unit: 'bytes',
				buckets: [100, 1000],
			})
			sizes.record(50)
			sizes.record(5000)
			await store.metrics.flush()
			expect(await last(store, 'http_requests_total')).toEqual({
				'{"route":"/notes","status":"200"}': 3,
				'{"route":"/users","status":"200"}': 0,
			})
			expect(await last(store, 'queue_depth')).toEqual({ '{}': 13 })
			expect(await last(store, 'open_tickets')).toEqual({ '{}': 7 })
			expect(await last(store, 'response_bytes_count')).toEqual({ '{}': 2 })
			expect(await last(store, 'response_bytes_sum')).toEqual({ '{}': 5050 })
			expect(await store.metrics.description('http_requests_total')).toEqual({
				unit: '',
				help: 'Requests served.',
			})
		})

		test("a timer's measure answers, rethrows, and records either way", async () => {
			const took = store.metrics.timer('measured_ms')
			expect(await took.measure(async () => 7)).toBe(7)
			const thrown = took.measure(() => {
				throw new InvalidError('no')
			})
			expect(await caught(thrown)).toBeInstanceOf(InvalidError)
			await store.metrics.flush()
			expect(await last(store, 'measured_ms_count')).toEqual({ '{}': 2 })
		})

		test('an instrument the store refuses fails the next flush, writes nothing and keeps no other out', async () => {
			await store.metrics.ingest({
				name: 'taken',
				kind: 'gauge',
				samples: [[Date.now() - 1000, 1]],
			})
			store.metrics.counter('taken').inc()
			store.metrics.counter('others_total').inc()
			expect(await caught(store.metrics.flush())).toBeInstanceOf(InvalidError)
			await store.metrics.flush()
			expect(await last(store, 'others_total')).toEqual({ '{}': 1 })
			expect(await last(store, 'taken')).toEqual({ '{}': 1 })
		})

		test('a call that would take a sum past the largest number throws where the program made it', async () => {
			const huge = store.metrics.histogram('huge_bytes')
			huge.record(1e308)
			expect(() => huge.record(1e308)).toThrow(InvalidError)
			const added = store.metrics.counter('huge_total')
			added.add(1e308)
			expect(() => added.add(1e308)).toThrow(InvalidError)
			await store.metrics.flush()
			expect(await last(store, 'huge_bytes_count')).toEqual({ '{}': 1 })
			expect(await last(store, 'huge_total')).toEqual({ '{}': 1e308 })
		})

		test("a gauge's functions read a set of labels each, and a set is read by one", async () => {
			store.metrics.gaugeFunc('queue_depth_jobs', () => 3, { labels: { queue: 'emails' } })
			store.metrics.gaugeFunc('queue_depth_jobs', async () => 5, { labels: { queue: 'sms' } })
			const again = () =>
				store.metrics.gaugeFunc('queue_depth_jobs', () => 7, { labels: { queue: 'sms' } })
			expect(again).toThrow(InvalidError)
			expect(() => store.metrics.gauge('queue_depth_jobs')).toThrow(InvalidError)
			await store.metrics.flush()
			expect(await last(store, 'queue_depth_jobs')).toEqual({
				'{"queue":"emails"}': 3,
				'{"queue":"sms"}': 5,
			})
		})

		test('a call an instrument refuses throws where the program made it', () => {
			const counter = store.metrics.counter('refusals_total')
			expect(() => counter.add(-1)).toThrow(InvalidError)
			expect(() => counter.add(Number.NaN)).toThrow(InvalidError)
			expect(() => counter.with({ __store: 'a' })).toThrow(InvalidError)
			expect(() => store.metrics.gauge('refused_depth').set(Number.POSITIVE_INFINITY)).toThrow(
				InvalidError,
			)
			expect(() => store.metrics.timer('refused_ms').record(-5)).toThrow(InvalidError)
			expect(() => store.metrics.timer('refused_ms').with({ le: '5' })).toThrow(InvalidError)
			expect(() => store.metrics.timer('unordered_ms', { buckets: [10, 5] })).toThrow(InvalidError)
			expect(() => store.metrics.gauge('refusals_total')).toThrow(InvalidError)
			store.metrics.gaugeFunc('read_once', () => 1)
			expect(() => store.metrics.gaugeFunc('read_once', () => 2)).toThrow(InvalidError)
		})

		test('a call made while a batch is on its way counts in the next', async () => {
			const jobs = store.metrics.counter('jobs_total')
			for (let i = 0; i < 20; i++) {
				jobs.inc()
				const flushing = store.metrics.flush()
				jobs.inc()
				await flushing
			}
			await store.metrics.flush()
			expect(await last(store, 'jobs_total')).toEqual({ '{}': 40 })
		})

		test('what a store holds, a page at a time, and its removal', async () => {
			const page = await store.metrics.names({ prefix: 'http_' })
			const requests = page.names.find(name => name.name === 'http_requests_total')
			expect(requests).toMatchObject({
				kind: 'counter',
				seriesCount: 2,
				help: 'Requests served.',
				keep: '30d',
			})
			expect(requests?.retainedFrom.getTime()).toBeLessThanOrEqual(requests?.newest.getTime() ?? 0)
			expect(page.next).toBeUndefined()
			const first = await store.metrics.names({ limit: 1 })
			expect(first.names.length).toBe(1)
			const rest = await store.metrics.names({ after: first.next })
			expect(rest.names.map(name => name.name)).not.toContain(first.names[0]?.name)
			expect(await store.metrics.labelNames('http_requests_total')).toEqual({
				labels: ['route', 'status'],
			})
			expect(
				await store.metrics.labelValues('http_requests_total', 'route', { prefix: '/n' }),
			).toEqual({
				values: ['/notes'],
			})
			await store.metrics.describe('cpu', { unit: '%', help: 'Busy.' })
			expect(await store.metrics.description('cpu')).toEqual({ unit: '%', help: 'Busy.' })
			await store.metrics.describe('cpu', {})
			expect(await store.metrics.description('cpu')).toEqual({ unit: '', help: '' })
			expect(await store.metrics.delete({ name: 'cpu', labels: { host: 'web-1' } })).toBe(true)
			expect(await store.metrics.delete({ name: 'cpu', labels: { host: 'web-1' } })).toBe(false)
			expect(await store.metrics.damaged()).toEqual([])
			const plan = await store.metrics.explain({
				name: 'requests_total',
				since: '1h',
				every: '1m',
				op: 'rate',
			})
			expect(plan).toMatchObject({ series: 2, exceeds: undefined })
		})
	})
}

const library = process.env.TINYSTORE_LIBRARY
const runtime = typeof Bun === 'undefined' ? nodeRuntime : bunRuntime

test.skipIf(library === undefined)(
	'a batch whose answer a lost connection took counts once',
	async () => {
		const dir = mkdtempSync(join(tmpdir(), 'tinystore-metrics-cut-'))
		// a second handle keeps the store open while the connection under test is cut
		const keeper = await open(dir, { embedded: true, library })
		const dialled: Connection[] = []
		const dial = embedded(runtime, dir, () => library ?? '')
		const link = new Link(async () => {
			const connection = await dial()
			dialled.push(connection)
			return connection
		})
		const instruments = new Instruments(link)
		const metrics = new Metrics(link, instruments)
		const jobs = metrics.counter('cut_jobs_total')
		jobs.inc()
		await metrics.flush()
		for (let cut = 0; cut < 5; cut++) {
			jobs.add(2)
			const flushed = metrics.flush()
			await sleep(cut)
			dialled.at(-1)?.session.end(new Error('cut'))
			await flushed
		}
		expect(await last(keeper, 'cut_jobs_total')).toEqual({ '{}': 11 })
		await instruments.stop()
		await link.close()
		await keeper.close()
		await removed(dir)
	},
)

test.skipIf(library === undefined)(
	'a flush the store cannot be reached for fails, and what it carried goes once it can be',
	async () => {
		const dir = mkdtempSync(join(tmpdir(), 'tinystore-metrics-down-'))
		const keeper = await open(dir, { embedded: true, library })
		const dial = embedded(runtime, dir, () => library ?? '')
		const dialled: Connection[] = []
		let down = false
		const link = new Link(async () => {
			if (down) {
				throw new UnavailableError('the store is down')
			}
			const connection = await dial()
			dialled.push(connection)
			return connection
		})
		const instruments = new Instruments(link)
		const metrics = new Metrics(link, instruments)
		const jobs = metrics.counter('down_jobs_total')
		jobs.inc()
		await metrics.flush()
		down = true
		dialled.at(-1)?.session.end(new Error('cut'))
		jobs.add(2)
		expect(await caught(metrics.flush())).toBeInstanceOf(UnavailableError)
		down = false
		await metrics.flush()
		expect(await last(keeper, 'down_jobs_total')).toEqual({ '{}': 3 })
		await instruments.stop()
		await link.close()
		await keeper.close()
		await removed(dir)
	},
)

test('what a program writes wrong does not compile: a duration, an op, a label its instrument has not', () => {
	const written = async (metrics: Metrics) => {
		const requests = metrics.counter<'route' | 'status'>('typed_total')
		requests.with({ route: '/a', status: 200 }).inc()
		// @ts-expect-error: rout is not among the labels the counter was made with
		requests.with({ rout: '/a' })
		// @ts-expect-error: '1hr' spells no duration
		await metrics.read({ name: 'cpu', since: '1hr' })
		// @ts-expect-error: mean is no op; avg is
		await metrics.aggregate({ name: 'cpu', op: 'mean' })
		const [mean] = await metrics.aggregate({ name: 'cpu', since: '1h', op: 'avg' })
		const exact: number | undefined = mean?.buckets[0]?.value
		const [p95] = await metrics.aggregate({ name: 'cpu', since: '1h', op: 'p95' })
		// @ts-expect-error: a percentile may have bounds in place of a value
		const ranked: number = p95!.buckets[0]!.value
		const valued = (bucket: ValueBucket): number => bucket.value
		return [exact, ranked, valued(mean!.buckets[0]!)]
	}
	expect(typeof written).toBe('function')
})

test.skipIf(library === undefined)(
	'a flush whose batch a store that started again may have counted fails, its outcome unknown, and counts nothing twice',
	async () => {
		const [first, second] = [0, 1].map(() =>
			mkdtempSync(join(tmpdir(), 'tinystore-metrics-restart-')),
		)
		const keeper = await open(first!, { embedded: true, library })
		let dial = embedded(runtime, first!, () => library ?? '')
		// a connection that hears nothing: the store applies a batch whose answer never comes
		let deaf = false
		const dialled: Connection[] = []
		const link = new Link(async () => {
			const connection = await dial()
			const read = connection.session.read.bind(connection.session)
			connection.session.read = (bytes, length) => (deaf ? length : read(bytes, length))
			dialled.push(connection)
			return connection
		})
		const instruments = new Instruments(link)
		const metrics = new Metrics(link, instruments)
		const jobs = metrics.counter('restart_jobs_total')
		jobs.inc()
		await metrics.flush()
		deaf = true
		jobs.add(2)
		const flushed = caught(metrics.flush())
		await until(async () => (await last(keeper, 'restart_jobs_total'))['{}'] === 3)
		// the next connection reaches a store that knows no session, as one started again does
		dial = embedded(runtime, second!, () => library ?? '')
		deaf = false
		dialled.at(-1)?.session.end(new Error('restarted'))
		expect(await flushed).toBeInstanceOf(OutcomeUnknownError)
		jobs.inc()
		await metrics.flush()
		const restarted = await open(second!, { embedded: true, library })
		expect(await last(keeper, 'restart_jobs_total')).toEqual({ '{}': 3 })
		expect(await last(restarted, 'restart_jobs_total')).toEqual({ '{}': 1 })
		await instruments.stop()
		await link.close()
		await restarted.close()
		await keeper.close()
		await removed(first!)
		await removed(second!)
	},
)

test('a value falls in the interval of the grid where the store puts it', () => {
	expect([binOf(0), binOf(1), binOf(1.05), binOf(0.99), binOf(Math.SQRT2)]).toEqual([
		'zero',
		'3:0',
		'3:1',
		'3:0',
		'3:5',
	])
	const below = new DataView(new ArrayBuffer(8))
	below.setFloat64(0, Math.SQRT2)
	below.setBigUint64(0, below.getBigUint64(0) - 1n)
	expect(binOf(below.getFloat64(0))).toBe('3:4')
	expect(binOf(Number.MAX_VALUE)).toBe('3:8192')
	expect(exponentialBuckets(1, 2, 4)).toEqual([1, 2, 4, 8])
})

const binary = process.env.TINYSTORE_BIN
const measuredWays = [
	{
		name: 'the core in this process',
		ready: library !== undefined,
		open: (dir: string) => open(dir, { embedded: true, library, selfMetrics: true }),
	},
	{
		name: 'a private child',
		ready: binary !== undefined && binary !== 'unused',
		open: (dir: string) => open(dir, { private: true, selfMetrics: true }),
	},
]

for (const way of measuredWays) {
	test.skipIf(!way.ready)(
		`a store opened with selfMetrics through ${way.name} measures its commits`,
		async () => {
			const dir = mkdtempSync(join(tmpdir(), 'tinystore-self-'))
			const store = await way.open(dir)
			await store.bucket<string>('sessions').set('a', 'x')
			await store.metrics.flush()
			const [commits] = await store.metrics.latest({
				name: 'tinystore_commits_total',
				where: { file: 'kv' },
				since: '1h',
			})
			expect(commits?.value).toBeGreaterThan(0)
			await store.close()
			await removed(dir)
		},
	)
}

const boundedWays = [
	{
		name: 'the core in this process',
		ready: library !== undefined,
		open: (dir: string) => open(dir, { embedded: true, library, maxSeries: 2 }),
	},
	{
		name: 'a private child',
		ready: binary !== undefined && binary !== 'unused',
		open: (dir: string) => open(dir, { private: true, maxSeries: 2 }),
	},
]

for (const way of boundedWays) {
	test.skipIf(!way.ready)(
		`a store opened through ${way.name} holds the series maxSeries says`,
		async () => {
			const dir = mkdtempSync(join(tmpdir(), 'tinystore-series-'))
			const store = await way.open(dir)
			const host = (name: string) => ({
				name: 'cpu',
				kind: 'gauge' as const,
				labels: { host: name },
				times: [Date.now() - 1000],
				values: [1],
			})
			await store.metrics.ingest([host('a'), host('b')])
			const refused = await caught(store.metrics.ingest(host('c')))
			expect(refused).toBeInstanceOf(LimitError)
			expect((refused as Error).message).toContain('a series past the 2 a store holds')
			await store.close()
			await removed(dir)
		},
	)
}

test('a count of series no store can hold is refused before anything opens', async () => {
	const refused = await caught(
		open(mkdtempSync(join(tmpdir(), 'tinystore-series-')), { maxSeries: 0 }),
	)
	expect(refused).toBeInstanceOf(InvalidError)
})

test.skipIf(library === undefined)(
	'a store opened without selfMetrics holds none of its own names',
	async () => {
		const dir = mkdtempSync(join(tmpdir(), 'tinystore-self-'))
		const store = await open(dir, { embedded: true, library })
		await store.bucket<string>('sessions').set('a', 'x')
		await store.metrics.flush()
		expect(await store.metrics.latest({ name: 'tinystore_commits_total', since: '1h' })).toEqual([])
		await store.close()
		await removed(dir)
	},
)
