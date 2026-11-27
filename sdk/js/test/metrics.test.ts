// Metrics as docs/guides/metrics/README.md has them over the protocol, through each way a
// store is reached: series ingested whole and read back bit for bit, the
// buckets of an aggregate, the newest samples, what a store holds and its
// removal, and instruments whose changes reach the store's totals once.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

import { type Connection, embedded, Link } from '../src/connection.ts'
import type { Code, TinystoreError } from '../src/errors.ts'
import {
	exponentialBuckets,
	InvalidError,
	type Labels,
	LimitError,
	type MetricsQuery,
	OutcomeUnknownError,
	open,
	type Series,
	type Store,
	UnavailableError,
	type ValueBucket,
} from '../src/index.ts'
import { binOf, Instruments } from '../src/instruments.ts'
import { Metrics } from '../src/metrics.ts'
import { bunRuntime } from '../src/runtime/bun.ts'
import { nodeRuntime } from '../src/runtime/node.ts'
import type { Duration } from '../src/time.ts'
import { caught, removed, until, ways } from './ways.ts'

/** Texts an exporter serves and the series each writes, which every language reads alike. */
const prometheus = JSON.parse(
	readFileSync(
		join(import.meta.dirname, '..', '..', '..', 'testdata', 'metrics', 'prometheus.json'),
		'utf8',
	),
) as {
	at: number
	keep: Duration
	cases: {
		name: string
		text: string
		labels?: Record<string, string>
		series?: { name: string; kind: string; labels: Labels; samples: [number, string][] }[]
		error?: { kind: string; has: string }
	}[]
}

/** Labels in the order of their names, so that two sets compare as text. */
function sorted(labels: Labels): Labels {
	return Object.fromEntries(Object.entries(labels).sort(([a], [b]) => (a < b ? -1 : 1)))
}

/** A value's bits, as the vectors spell it and the text format does: NaN, +Inf and -0 among them. */
function bitsOf(spelled: string): string {
	const value = spelled === '+Inf' ? Infinity : spelled === '-Inf' ? -Infinity : Number(spelled)
	const view = new DataView(new ArrayBuffer(8))
	view.setFloat64(0, value, true)
	return view.getBigUint64(0, true).toString(16)
}

/** A series as a vector compares it: its samples' times and their values' bits. */
function seen(series: Series): string {
	const view = new DataView(
		series.values.buffer,
		series.values.byteOffset,
		series.values.byteLength,
	)
	const samples = Array.from(series.times, (at, i) => [
		at,
		view.getBigUint64(i * 8, true).toString(16),
	])
	return JSON.stringify([series.name, series.kind, series.labels, samples])
}

/** The last value of each series of a name, by the labels given. */
async function last(store: Store, name: string): Promise<Record<string, number>> {
	const { series: read } = await store.metrics.read({ name, since: '1h' })
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
			const {
				series: [cpu],
			} = await store.metrics.read({ name: 'cpu', where: { host: 'web-1' }, since: '1h' })
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

		// contract: MET-19
		test("a gauge's stretch is replaced whole, sent again changes nothing, and a counter's is refused", async () => {
			const day = 86_400_000
			const today = Math.floor(Date.now() / day) * day
			const days = [5, 4, 3, 2, 1, 0].map(back => today - back * day)
			const labels = { shop: 'msk' }
			await store.metrics.ingest({
				name: 'revenue',
				kind: 'gauge',
				labels,
				samples: days.map(at => [at, 1000] as const),
			})
			const corrected = {
				name: 'revenue',
				kind: 'gauge',
				labels,
				samples: [
					[days[2]!, 1000],
					[new Date(days[4]!), 900],
				],
			} as const
			const range = { from: new Date(days[2]!), to: days[5]! }
			await store.metrics.replace(corrected, range)
			await store.metrics.replace(corrected, range)
			const {
				series: [revenue],
			} = await store.metrics.read({ name: 'revenue', since: '7d' })
			expect(Array.from(revenue?.times ?? [])).toEqual(days.filter((_, i) => i !== 3))
			expect(Array.from(revenue?.values ?? [])).toEqual([1000, 1000, 1000, 900, 1000])
			const counter = { name: 'orders_total', kind: 'counter', samples: [] } as const
			expect(await caught(store.metrics.replace(counter, range))).toBeInstanceOf(InvalidError)
		})

		// contract: MET-17, MET-28
		test('a read and an aggregate given limit come in pages that walk on to the whole answer', async () => {
			const at = Date.now() - 600_000
			const samples = Array.from({ length: 500 }, (_, i) => [at + i * 1000, i] as const)
			for (const host of ['a', 'b']) {
				await store.metrics.ingest({ name: 'paged', kind: 'gauge', labels: { host }, samples })
			}
			const query = { name: 'paged', since: '1h' } as const
			const whole = await store.metrics.read(query)
			expect([whole.series.length, whole.next]).toEqual([2, undefined])
			const first = await store.metrics.read({ ...query, limit: 300 })
			expect([first.series.at(-1)?.more, first.next === undefined]).toEqual([true, false])
			const more = await store.metrics.read({ ...query, limit: 300, after: first.next })
			expect(more.series[0]?.labels).toEqual(first.series.at(-1)?.labels)
			const walked: Series[] = []
			for await (const part of first) {
				walked.push(part)
			}
			const read: Record<string, number> = {}
			for (const part of walked) {
				read[part.labels.host ?? ''] = (read[part.labels.host ?? ''] ?? 0) + part.times.length
			}
			expect(read).toEqual({ a: 500, b: 500 })
			expect(walked.every(part => part.times.length <= 300)).toBe(true)
			const groups = []
			const paged = await store.metrics.aggregate({
				...query,
				every: '1m',
				op: 'avg',
				by: ['host'],
				limit: 3,
			})
			expect(paged.groups.at(-1)?.more).toBe(true)
			for await (const group of paged) {
				groups.push(group)
			}
			const once = await store.metrics.aggregate({ ...query, every: '1m', op: 'avg', by: ['host'] })
			// each group once, its parts joined
			expect(groups).toEqual(once.groups)
			expect(JSON.parse(JSON.stringify(first))).toEqual({
				series: expect.any(Array),
				next: first.next,
			})
		})

		test('every text of testdata/metrics/prometheus.json writes its series or is refused whole', async () => {
			for (const each of prometheus.cases) {
				const options = { labels: each.labels, at: prometheus.at, keep: prometheus.keep }
				const ingested = store.metrics.ingestPrometheus(each.text, options)
				if (each.error !== undefined) {
					const error = (await caught(ingested)) as TinystoreError
					expect([each.name, error.code]).toEqual([each.name, each.error.kind as Code])
					expect(error.message).toContain(each.error.has)
					continue
				}
				await ingested
				const series = each.series ?? []
				const want = series.map(one => {
					const samples = one.samples.map(([at, value]) => [at, bitsOf(value)])
					return JSON.stringify([one.name, one.kind, sorted(one.labels), samples])
				})
				const got: string[] = []
				for (const name of new Set(series.map(one => one.name))) {
					const { series: read } = await store.metrics.read({ name, since: prometheus.keep })
					got.push(...read.map(one => seen({ ...one, labels: sorted(one.labels) })))
				}
				expect([each.name, ...got.sort()]).toEqual([each.name, ...want.sort()])
			}
			const bytes = new TextEncoder().encode('# TYPE bytes_given gauge\nbytes_given 1\n')
			await store.metrics.ingestPrometheus(bytes, { keep: '1d' })
			expect(await last(store, 'bytes_given')).toEqual({ '{}': 1 })
		})

		// a private child's answers once left through std's Stdout, where on
		// Windows under Node, once a text had come in pieces, one now and then
		// stopped part way and its read waited for good: within thirty reads
		test('after a text that came in pieces, answers of 400 KiB arrive whole, however many in a row', async () => {
			const long = 'x'.repeat(640)
			const lines = Array.from(
				{ length: 6000 },
				(_, n) => `wide_answer_${n % 10}{n="${n}",long="${long}"} ${n}`,
			)
			await store.metrics.ingestPrometheus(`${lines.join('\n')}\n`)
			for (let read = 0; read < 200; read++) {
				const { series: answered } = await store.metrics.read({
					name: `wide_answer_${read % 10}`,
					since: '1h',
				})
				expect([read, answered.length]).toEqual([read, 600])
			}
		})

		// contract: MET-15
		test('a text past one message goes in pieces and is written whole, and one past 32 MiB is a limit', async () => {
			const long = 'x'.repeat(640)
			const lines = Array.from(
				{ length: 6000 },
				(_, n) => `wide_${n % 10}{n="${n}",long="${long}"} ${n}`,
			)
			const text = `${lines.join('\n')}\n`
			expect(text.length).toBeGreaterThan(3 << 20)
			await store.metrics.ingestPrometheus(text, { labels: { instance: 'web-1' } })
			for (let name = 0; name < 10; name++) {
				const { series: read } = await store.metrics.read({ name: `wide_${name}`, since: '1h' })
				const wrong = read.filter(
					series =>
						series.labels.instance !== 'web-1' ||
						series.labels.long !== long ||
						series.values[0] !== Number(series.labels.n),
				)
				expect([read.length, wrong.length]).toEqual([600, 0])
			}
			const past = await caught(store.metrics.ingestPrometheus('#'.repeat((32 << 20) + 1)))
			expect(past).toBeInstanceOf(LimitError)
		})

		// contract: MET-30
		test('an ingest of 98,000 samples is one call, and one past a message goes up in pieces', async () => {
			const at = Date.now() - 20 * 3_600_000
			const times = Float64Array.from({ length: 14_000 }, (_, i) => at + i * 5000)
			const values = Float64Array.from({ length: 14_000 }, (_, i) => i)
			const hosts = ['a', 'b', 'c', 'd', 'e', 'f', 'g'].map(host => ({
				name: 'seeded',
				kind: 'gauge' as const,
				labels: { host },
				times: Array.from(times),
				values,
			}))
			await store.metrics.ingest(hosts)
			let samples = 0
			for await (const series of await store.metrics.read({
				name: 'seeded',
				since: '1d',
				limit: 100_000,
			})) {
				samples += series.times.length
			}
			expect(samples).toBe(98_000)
			// 600 series of a 4,000-byte label pass one message, and go up in pieces
			const wide = 'w'.repeat(4000)
			const recent = Date.now() - 60_000
			await store.metrics.ingest(
				Array.from({ length: 600 }, (_, n) => ({
					name: 'seeded_wide',
					kind: 'gauge' as const,
					labels: { n: `${n}${wide}` },
					samples: [[recent, 1]] as const,
				})),
			)
			const { series: widened } = await store.metrics.read({ name: 'seeded_wide', since: '1h' })
			expect(widened.length).toBe(600)
			const past = await caught(
				store.metrics.ingest([...hosts, { ...hosts[0]!, labels: { host: 'h' } }]),
			)
			expect(past).toBeInstanceOf(LimitError)
		})

		test("a calendar's buckets begin at the local midnight of their time zone", async () => {
			// a Moscow day ten days ago, a sample an hour after its midnight and one an hour before its end
			const midnight =
				Math.floor(Date.now() / 86_400_000) * 86_400_000 - 10 * 86_400_000 - 3 * 3_600_000
			const samples = [midnight + 3_600_000, midnight + 23 * 3_600_000].map(at => [at, 1] as const)
			await store.metrics.ingest({ name: 'orders', kind: 'gauge', labels: {}, samples })
			const range = {
				name: 'orders',
				from: new Date(midnight),
				to: new Date(midnight + 86_400_000),
			}
			const {
				groups: [local],
			} = await store.metrics.aggregate({
				...range,
				op: 'sum',
				calendar: 'day',
				timeZone: 'Europe/Moscow',
			})
			expect(local?.buckets.map(bucket => [bucket.at.getTime(), bucket.value])).toEqual([
				[midnight, 2],
			])
			const {
				groups: [utc],
			} = await store.metrics.aggregate({ ...range, op: 'sum', every: '1d' })
			expect(utc?.buckets.map(bucket => bucket.value)).toEqual([1, 1])
			// contract: MET-27
			const moscowDay = new Intl.DateTimeFormat('en-CA', { timeZone: 'Europe/Moscow' })
			expect(local?.buckets.map(bucket => bucket.date)).toEqual([moscowDay.format(midnight)])
			expect(utc?.buckets.map(bucket => bucket.date)).toEqual([undefined, undefined])
		})

		// contract: MET-26
		test('an aggregate given fill answers every bucket of the range, an empty one with no value', async () => {
			const hour = Math.floor(Date.now() / 3_600_000) * 3_600_000
			const samples = [hour - 5 * 3_600_000 + 60_000, hour - 3_600_000 + 60_000].map(
				(at, i) => [at, i + 1] as const,
			)
			await store.metrics.ingest({ name: 'filled_errors', kind: 'counter', labels: {}, samples })
			const query = { name: 'filled_errors', since: '1d', every: '1h', op: 'increase' } as const
			const sparse = await store.metrics.aggregate(query)
			expect(sparse.groups[0]?.buckets.length).toBe(2)
			const { groups } = await store.metrics.aggregate({ ...query, fill: true })
			const buckets = groups[0]?.buckets ?? []
			expect(buckets.length).toBe(25)
			const starts = buckets.map(bucket => bucket.at.getTime())
			expect(starts.every((at, i) => i === 0 || at - (starts[i - 1] ?? 0) === 3_600_000)).toBe(true)
			const empty = buckets.filter(bucket => bucket.samples === 0)
			expect(empty.length).toBe(23)
			expect(empty.every(bucket => bucket.value === undefined)).toBe(true)
			// a value an empty bucket may lack is the type's: a chart chooses its zero
			const drawn: number[] = buckets.map(bucket => bucket.value ?? 0)
			expect(drawn.length).toBe(25)
			expect(buckets.filter(bucket => bucket.value !== undefined).length).toBe(2)
			const unbounded = await caught(
				store.metrics.aggregate({ name: 'filled_errors', every: '1h', op: 'increase', fill: true }),
			)
			expect(unbounded).toBeInstanceOf(InvalidError)
		})

		// contract: MET-23
		test('an aggregate that skips NaN says how many each bucket passed over', async () => {
			const minute = Math.floor(Date.now() / 60_000) * 60_000 - 180_000
			const quantile = (q: string, values: number[]) => ({
				name: 'scraped_seconds',
				kind: 'gauge' as const,
				labels: { quantile: q },
				samples: values.map((value, i) => [minute + i * 15_000, value] as const),
			})
			// an idle summary's quantiles: NaN in the second minute, and 0.99 never computed
			const nan = Number.NaN
			await store.metrics.ingest([
				quantile('0.5', [1, 2, nan, 3, nan, nan, nan, nan, 4, nan, 4, 4]),
				quantile('0.99', new Array(12).fill(nan)),
			])
			const range = {
				name: 'scraped_seconds',
				from: new Date(minute),
				to: new Date(minute + 180_000),
			}
			const { groups } = await store.metrics.aggregate({
				...range,
				every: '1m',
				op: 'avg',
				by: ['quantile'],
				skipNaN: true,
			})
			expect(
				groups.map(group => [
					group.labels.quantile,
					group.buckets.map(bucket => [bucket.value, bucket.samples, bucket.skipped]),
				]),
			).toEqual([
				[
					'0.5',
					[
						[2, 3, 1],
						[4, 3, 1],
					],
				],
			])
			const refused = await caught(store.metrics.aggregate({ ...range, op: 'avg' }))
			expect(refused).toBeInstanceOf(InvalidError)
			const infinite = await caught(
				store.metrics
					.ingest({
						name: 'scraped_level',
						kind: 'gauge',
						labels: {},
						samples: [[minute, Infinity]],
					})
					.then(() =>
						store.metrics.aggregate({ ...range, name: 'scraped_level', op: 'max', skipNaN: true }),
					),
			)
			expect(infinite).toBeInstanceOf(InvalidError)
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
			const { groups: traffic } = await store.metrics.aggregate({
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
				const { series: read } = await store.metrics.read({
					name: 'requests_total',
					where,
					since: '1h',
				})
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
			const {
				groups: [p50],
			} = await store.metrics.aggregate({ name: 'db_ms', since: '1h', op: 'p50' })
			expect(p50?.buckets[0]).toMatchObject({ lower: 5, upper: 10, samples: 4 })
			const {
				groups: [p99],
			} = await store.metrics.aggregate({
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
			const {
				groups: [mean],
			} = await store.metrics.aggregate({ name: 'db_ms', since: '1h', op: 'avg' })
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

		// contract: MET-24
		test("a read sees what the program's instruments counted, with no flush", async () => {
			const calls = store.metrics.counter('read_back_total')
			calls.with({ op: 'a' }).inc()
			const { groups } = await store.metrics.aggregate({
				name: 'read_back_total',
				since: '1h',
				op: 'increase',
			})
			expect(groups[0]?.buckets[0]?.value).toBe(1)
			calls.with({ op: 'a' }).inc()
			calls.with({ op: 'a' }).inc()
			const [newest] = await store.metrics.latest({ name: 'read_back_total', since: '1h' })
			expect(newest?.value).toBe(3)
			const took = store.metrics.timer('read_back_ms')
			took.record('5ms')
			expect(await last(store, 'read_back_ms_count')).toEqual({ '{}': 1 })
		})

		// contract: MET-29
		test('maxSeries counts past its sets into __overflow and says so once in TinyStore lines', async () => {
			const told: string[] = []
			const write = process.stderr.write
			process.stderr.write = ((text: string) => told.push(text) > 0) as typeof process.stderr.write
			const people = store.metrics.counter('op_people', { maxSeries: 2 })
			try {
				for (let user = 1; user <= 5; user++) {
					people.with({ user }).inc()
				}
			} finally {
				process.stderr.write = write
			}
			const said = told.filter(line => line.includes('counting into __overflow'))
			expect(said.length).toBe(1)
			expect(said[0]).toContain('op_people')
			const { groups } = await store.metrics.aggregate({
				name: 'op_people',
				since: '1h',
				op: 'last',
				by: ['user'],
			})
			const overflow = groups.find(group => '__overflow' in group.labels)
			expect([groups.length, overflow?.buckets[0]?.value]).toEqual([3, 3])
			expect(() => store.metrics.counter('op_people', { maxSeries: 3 })).toThrow(InvalidError)
			expect(() => store.metrics.counter('op_bounded', { maxSeries: 0 })).toThrow(InvalidError)
			expect(() => store.metrics.counter('op_bounded', { maxSeries: 2001 })).toThrow(InvalidError)
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

		// contract: MET-33
		test('compare sets the newest window against the median of the windows before it', async () => {
			const now = Date.now()
			await store.metrics.ingest([
				{
					name: 'compared_ms',
					kind: 'gauge',
					labels: { source: 'server' },
					samples: [
						[now - 150_000, 2],
						[now - 90_000, 4],
						[now - 30_000, 12],
					],
				},
			])
			const asked = {
				name: 'compared_ms',
				op: 'max',
				window: '1m',
				where: { source: 'server' },
				against: '10m',
			} as const
			expect(await store.metrics.compare(asked)).toEqual({
				value: 12,
				usual: 3,
				ratio: 4,
				samples: 1,
				windows: 2,
			})
			const none = await store.metrics.compare({ ...asked, name: 'compared_nothing' })
			expect(none).toEqual({ samples: 0, windows: 0 })
			const many = await caught(store.metrics.compare({ ...asked, against: '1002m' }))
			expect(many).toBeInstanceOf(InvalidError)
			expect(String(many)).toContain('1000 at most')
			const hours = await caught(store.metrics.compare({ ...asked, sameHour: true }))
			expect(String(hours)).toContain('a day at least')
			const ranged = await caught(store.metrics.compare({ ...asked, since: '1h' } as never))
			expect(String(ranged)).toContain('no field `since`')
		})

		// contract: MET-25
		test('a query field the SDK does not know is refused by name, never dropped', async () => {
			const names = await caught(
				store.metrics.aggregate({ names: ['calls'], since: '1h', op: 'increase' } as never),
			)
			expect(names).toBeInstanceOf(InvalidError)
			expect(String(names)).toContain('no field `names`; did you mean `name`?')
			const every = await caught(store.metrics.read({ name: 'cpu', every: '1h' } as never))
			expect(String(every)).toContain('`every` is an aggregate')
			const typo = await caught(store.metrics.latest({ name: 'cpu', snice: '1h' } as never))
			expect(String(typo)).toContain('no field `snice`')
			expect(await store.metrics.read({ name: 'cpu', since: '1h', to: undefined })).toBeDefined()
			// a query of no condition reaches the store, which says so in its own words
			const none = await caught(store.metrics.aggregate({ since: '1h', op: 'increase' }))
			expect(none).toBeInstanceOf(InvalidError)
			expect(String(none)).toContain("by `name` or by a label's value")
			expect(String(none)).not.toContain('Not')
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
		const link = new Link(async dialling => {
			const connection = await dial(dialling)
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
		const link = new Link(async dialling => {
			if (down) {
				throw new UnavailableError('the store is down')
			}
			const connection = await dial(dialling)
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
		const {
			groups: [mean],
		} = await metrics.aggregate({ name: 'cpu', since: '1h', op: 'avg' })
		const exact: number | undefined = mean?.buckets[0]?.value
		const {
			groups: [p95],
		} = await metrics.aggregate({ name: 'cpu', since: '1h', op: 'p95' })
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
		const link = new Link(async dialling => {
			const connection = await dial(dialling)
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
