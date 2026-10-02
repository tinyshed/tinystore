// Series of samples in metrics.db, as Go's metrics package keeps them: a
// sample is kept bit for bit, a range read exactly, an aggregate computed
// exactly and rounded once. Instruments live in the SDK and are ingested
// every flush.

import { download, type Link } from './connection.ts'
import { InvalidError } from './errors.ts'
import { type Duration, ms, type Time, unixMs } from './time.ts'
import { byCodePoint } from './wire/codec.ts'
import {
	MetricsBatch,
	MetricsBuckets,
	MetricsDropped,
	MetricsLabels,
	MetricsRange,
	MetricsSeries,
	methods,
} from './wire/messages.ts'

export type SeriesKind = 'gauge' | 'counter'

/**
 * What tells the series of one name apart: a host, a route, a status. A label
 * whose name begins with __ is the store's own, and refused.
 */
export type Labels = Record<string, string>

export interface SeriesInput {
	/** the metric: 'http_requests_total' */
	name: string
	kind: SeriesKind
	labels?: Labels
	/** samples as [time, value] pairs, or as columns: unix milliseconds and their values */
	samples:
		| readonly (readonly [time: Time, value: number])[]
		| { times: readonly number[] | BigInt64Array; values: readonly number[] | Float64Array }
}

export interface Series {
	name: string
	kind: SeriesKind
	labels: Labels
	/** unix milliseconds, in time order */
	times: number[]
	/** the values, their bits as they were ingested: -0 and a NaN's payload survive */
	values: Float64Array
}

/**
 * The series of a name, of labels, or of both, each matched exactly, and
 * their samples over the last `since` or from `from` to `to`.
 *
 * ```ts
 * { name: 'cpu', since: '1h' }                         // the last hour of every cpu series
 * { match: { host: 'web-1' }, from: start, to: end }   // every series of web-1
 * ```
 */
/**
 * What a label's value must be beyond equality, made by oneOf, noneOf or
 * prefix: an object of the SDK's own, so that a stored value is never read as
 * a query.
 */
export class Condition {
	readonly kind: 'one_of' | 'none_of' | 'prefix'
	readonly values: readonly string[]

	constructor(kind: Condition['kind'], values: readonly string[]) {
		this.kind = kind
		this.values = values
	}
}

/** The label is one of the values. */
export function oneOf(...values: string[]): Condition {
	return new Condition('one_of', values)
}

/** The label is none of the values, or the series has none. */
export function noneOf(...values: string[]): Condition {
	return new Condition('none_of', values)
}

/** The label's value begins with the prefix. */
export function prefix(start: string): Condition {
	return new Condition('prefix', [start])
}

export interface Range {
	name?: string
	/** labels a series holds, each exactly */
	match?: Labels
	/**
	 * labels under a condition: `{ status: oneOf('500', '502'), env: noneOf('dev'), host: prefix('api-') }`;
	 * a plain value is equality. A range of noneOf alone is refused, since it would scan every series.
	 */
	where?: Record<string, Condition | string>
	/** the span before now the range covers: '1h', '15m', or milliseconds */
	since?: Duration
	/** the range's start, included; the oldest sample kept when absent */
	from?: Time
	/** excluded; the open end when absent */
	to?: Time
	/** each narrows the server's: series matched, blocks decoded, bytes fetched, samples decoded, answered */
	limits?: { series?: number; blocks?: number; bytes?: number; decoded?: number; answered?: number }
}

export type AggregateOp = 'count' | 'sum' | 'min' | 'max' | 'increase'

export interface Bucket {
	from: Date
	to: Date
	/** the samples it counted */
	count: number
	/** the resets among them, a counter's */
	resets: number
	/** computed exactly and rounded once */
	value: number
	/** the value overflowed to an infinity */
	overflow: boolean
	/** retention cut the bucket, which counted only its samples from the cutoff on */
	partial: boolean
}

export interface Aggregate {
	name: string
	kind: SeriesKind
	labels: Labels
	/** only buckets holding samples */
	buckets: Bucket[]
}

const openEnd = 0x7fff_ffff_ffff_ffffn

/** The label the wire carries a series' name as, the store's own spelling. */
const wireName = '__name__'

/** A series' name and labels as the wire spells them, its name among its labels. */
function wireLabels(name: string | undefined, labels: Labels | undefined): Labels {
	const spelled: Labels = {}
	for (const [key, value] of Object.entries(labels ?? {})) {
		if (key.startsWith('__')) {
			throw new InvalidError(`the label ${key}: a name beginning with __ is the store's own`)
		}
		spelled[key] = value
	}
	if (name !== undefined) {
		spelled[wireName] = name
	}
	return spelled
}

/** A series as the wire spelled it: its name apart from its labels. */
function namedSeries(spelled: Labels | undefined): { name: string; labels: Labels } {
	const { [wireName]: name = '', ...labels } = spelled ?? {}
	return { name, labels }
}

function rangeOf(
	r: Range,
	extra?: { width?: number; op?: AggregateOp },
): Parameters<typeof MetricsRange.encode>[0] {
	const { match, where } = conditionsOf(r)
	if (r.name === undefined && Object.keys(match).length === 0 && !where.some(finds)) {
		throw new InvalidError('a range names a series, a label to match, or a oneOf or a prefix')
	}
	if (r.since !== undefined && r.from !== undefined) {
		throw new InvalidError('a range starts since a span before now or from a time, not both')
	}
	const from =
		r.since !== undefined ? Date.now() - ms(r.since) : r.from === undefined ? 0 : unixMs(r.from)
	return {
		matchers: wireLabels(r.name, match),
		where: where.length > 0 ? where : undefined,
		from: BigInt(from),
		to: r.to === undefined ? openEnd : BigInt(unixMs(r.to)),
		limitSeries: r.limits?.series,
		limitBlocks: r.limits?.blocks,
		limitBytes: r.limits?.bytes,
		limitDecoded: r.limits?.decoded,
		limitAnswered: r.limits?.answered,
		...extra,
	}
}

/** A range's equality, plain values of where included, and its conditions in the order of their labels. */
function conditionsOf(r: Range): {
	match: Labels
	where: { label: string; kind: Condition['kind']; values: string[] }[]
} {
	const match: Labels = { ...r.match }
	const where = []
	for (const [label, condition] of Object.entries(r.where ?? {}).sort(([a], [b]) =>
		byCodePoint(a, b),
	)) {
		if (label in match) {
			throw new InvalidError(`label ${label} is matched and has a condition`)
		}
		if (typeof condition === 'string') {
			match[label] = condition
		} else if (condition instanceof Condition) {
			where.push({ label, kind: condition.kind, values: [...condition.values] })
		} else {
			throw new InvalidError(`label ${label}: a condition is made by oneOf, noneOf or prefix`)
		}
	}
	return { match, where }
}

function finds(condition: { kind: Condition['kind'] }): boolean {
	return condition.kind !== 'none_of'
}

function columnsOf(samples: SeriesInput['samples']): {
	times: BigInt64Array
	values: Float64Array
} {
	if (Array.isArray(samples)) {
		const pairs = samples as readonly (readonly [Time, number])[]
		return {
			times: BigInt64Array.from(pairs, ([t]) => BigInt(unixMs(t))),
			values: Float64Array.from(pairs, ([, v]) => v),
		}
	}
	const columns = samples as {
		times: readonly number[] | BigInt64Array
		values: readonly number[] | Float64Array
	}
	return {
		times:
			columns.times instanceof BigInt64Array
				? columns.times
				: BigInt64Array.from(columns.times, t => BigInt(t)),
		values:
			columns.values instanceof Float64Array ? columns.values : Float64Array.from(columns.values),
	}
}

/** Joins the pieces a series longer than a body came in, one after another with its name and labels. */
function joined<T extends { name: string; labels: Labels }>(
	pieces: T[],
	join: (into: T, piece: T) => void,
): T[] {
	const out: T[] = []
	for (const piece of pieces) {
		const last = out.at(-1)
		if (last !== undefined && last.name === piece.name && sameLabels(last.labels, piece.labels)) {
			join(last, piece)
		} else {
			out.push(piece)
		}
	}
	return out
}

function sameLabels(a: Labels, b: Labels): boolean {
	const keys = Object.keys(a)
	return keys.length === Object.keys(b).length && keys.every(k => a[k] === b[k])
}

function concatFloats(a: Float64Array, b: Float64Array): Float64Array {
	const out = new Float64Array(a.length + b.length)
	out.set(a)
	out.set(b, a.length)
	return out
}

export class Metrics {
	readonly #link: Link
	readonly #instruments = new Map<string, Instrument>()
	#timer: ReturnType<typeof setInterval> | undefined
	/** instruments' flushes that failed, and why the last did */
	failures = 0
	lastFailure: Error | undefined

	constructor(link: Link) {
		this.#link = link
	}

	/** Stores series and their samples, all or none; a refused series is named by its labels. */
	async ingest(series: SeriesInput | readonly SeriesInput[]): Promise<void> {
		const batch = (Array.isArray(series) ? series : [series as SeriesInput]).map(s => ({
			labels: wireLabels(s.name, s.labels),
			kind: s.kind,
			...columnsOf(s.samples),
		}))
		await this.#link.run('write', connection =>
			connection.session.call(methods['metrics.ingest'], MetricsBatch.encode({ series: batch })),
		)
	}

	/** Every sample of the series a range matches, exactly, read whole before the first leaves the server. */
	async read(range: Range): Promise<Series[]> {
		const got = await this.#link.run('read', connection =>
			download(connection, methods['metrics.read'], MetricsRange.encode(rangeOf(range))),
		)
		const pieces = got.items.map(item => {
			const s = MetricsSeries.decode(item)
			return {
				...namedSeries(s.labels as Labels | undefined),
				kind: (s.kind ?? 'gauge') as SeriesKind,
				times: Array.from(s.times ?? [], Number),
				values: s.values ?? new Float64Array(0),
			}
		})
		return joined(pieces, (into, piece) => {
			into.times.push(...piece.times)
			into.values = concatFloats(into.values, piece.values)
		})
	}

	/** Buckets of a width from the range's start, each computed exactly: a counter's increase counts its resets. */
	async aggregate(range: Range & { width: Duration; op: AggregateOp }): Promise<Aggregate[]> {
		const got = await this.#link.run('read', connection =>
			download(
				connection,
				methods['metrics.aggregate'],
				MetricsRange.encode(rangeOf(range, { width: ms(range.width), op: range.op })),
			),
		)
		const pieces = got.items.map(item => {
			const b = MetricsBuckets.decode(item)
			const values = b.values ?? new Float64Array(0)
			const buckets: Bucket[] = Array.from(values, (value, i) => ({
				from: new Date(Number(b.from?.[i] ?? 0n)),
				to: new Date(Number(b.to?.[i] ?? 0n)),
				count: Number(b.count?.[i] ?? 0n),
				resets: Number(b.resets?.[i] ?? 0n),
				value,
				overflow: ((b.flags?.[i] ?? 0) & 1) !== 0,
				partial: ((b.flags?.[i] ?? 0) & 2) !== 0,
			}))
			return {
				...namedSeries(b.labels as Labels | undefined),
				kind: (b.kind ?? 'gauge') as SeriesKind,
				buckets,
			}
		})
		return joined(pieces, (into, piece) => {
			into.buckets.push(...piece.buckets)
		})
	}

	/** Removes one series and everything it holds, whether it still reads or not. */
	async drop(series: {
		name: string
		labels?: Labels
	}): Promise<{ found: boolean; unreadableGroups: number }> {
		const labels = wireLabels(series.name, series.labels)
		const body = await this.#link.run('write', connection =>
			connection.session.call(methods['metrics.drop'], MetricsLabels.encode({ labels })),
		)
		const dropped = MetricsDropped.decode(body)
		return { found: dropped.found === true, unreadableGroups: dropped.unreadableGroups ?? 0 }
	}

	/** A counter: its total since this process started, ingested every flush; a restart is a reset. */
	counter(name: string): Counter {
		return new Counter(this.#instrument(name, 'counter'), {})
	}

	/** A gauge: its value at each flush. */
	gauge(name: string): Gauge {
		return new Gauge(this.#instrument(name, 'gauge'), {})
	}

	/** A gauge read by a function at each flush; a function that throws skips that sample. */
	gaugeFunc(name: string, read: () => number | Promise<number>): void {
		this.#instrument(name, 'gauge').read = read
	}

	#instrument(name: string, kind: SeriesKind): Instrument {
		let instrument = this.#instruments.get(name)
		if (instrument === undefined) {
			instrument = { name, kind, series: new Map() }
			this.#instruments.set(name, instrument)
			this.#timer ??= setInterval(() => void this.flush().catch(() => {}), flushEvery)
			this.#timer.unref?.()
		} else if (instrument.kind !== kind) {
			throw new InvalidError(`${name} is a ${instrument.kind}, not a ${kind}`)
		}
		return instrument
	}

	/** Ingests every instrument's value now, as the timer does every 15 s. */
	async flush(): Promise<void> {
		const at = BigInt(Date.now())
		const batch: {
			labels: Labels
			kind: SeriesKind
			times: BigInt64Array
			values: Float64Array
		}[] = []
		for (const instrument of this.#instruments.values()) {
			if (instrument.read !== undefined) {
				try {
					instrument.series.set('{}', { labels: {}, value: await instrument.read() })
				} catch {
					continue
				}
			}
			for (const s of instrument.series.values()) {
				batch.push({
					labels: { ...s.labels, [wireName]: instrument.name },
					kind: instrument.kind,
					times: BigInt64Array.of(at),
					values: Float64Array.of(s.value),
				})
			}
		}
		if (batch.length === 0) {
			return
		}
		try {
			await this.#link.run('write', connection =>
				connection.session.call(methods['metrics.ingest'], MetricsBatch.encode({ series: batch })),
			)
		} catch (err) {
			this.failures++
			this.lastFailure = err as Error
			throw err
		}
	}

	/** Stops the timer; the store flushes once more as it closes. */
	stop(): void {
		if (this.#timer !== undefined) {
			clearInterval(this.#timer)
			this.#timer = undefined
		}
	}

	get instruments(): number {
		return this.#instruments.size
	}
}

const flushEvery = 15_000

interface Instrument {
	name: string
	kind: SeriesKind
	/** by the canonical spelling of each label set */
	series: Map<string, { labels: Labels; value: number }>
	read?: () => number | Promise<number>
}

function keyOf(labels: Labels): string {
	return JSON.stringify(Object.entries(labels).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)))
}

function seriesOf(instrument: Instrument, labels: Labels): { labels: Labels; value: number } {
	const key = keyOf(labels)
	let s = instrument.series.get(key)
	if (s === undefined) {
		s = { labels, value: 0 }
		instrument.series.set(key, s)
	}
	return s
}

export class Counter {
	readonly #instrument: Instrument
	readonly #labels: Labels

	constructor(instrument: Instrument, labels: Labels) {
		this.#instrument = instrument
		this.#labels = labels
	}

	/** The counter of these labels besides its own: the same labels in any order are one series. */
	with(labels: Labels): Counter {
		return new Counter(this.#instrument, { ...this.#labels, ...labels })
	}

	inc(): void {
		this.add(1)
	}

	add(n: number): void {
		if (!(n >= 0)) {
			throw new InvalidError(`a counter adds ${n}; it only grows`)
		}
		seriesOf(this.#instrument, this.#labels).value += n
	}
}

export class Gauge {
	readonly #instrument: Instrument
	readonly #labels: Labels

	constructor(instrument: Instrument, labels: Labels) {
		this.#instrument = instrument
		this.#labels = labels
	}

	with(labels: Labels): Gauge {
		return new Gauge(this.#instrument, { ...this.#labels, ...labels })
	}

	set(value: number): void {
		seriesOf(this.#instrument, this.#labels).value = value
	}

	add(n: number): void {
		seriesOf(this.#instrument, this.#labels).value += n
	}

	inc(): void {
		this.add(1)
	}

	dec(): void {
		this.add(-1)
	}
}
