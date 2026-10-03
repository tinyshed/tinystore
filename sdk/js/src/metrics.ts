// Series of samples in metrics.db, as Go's metrics package keeps them: a
// sample is kept bit for bit, a range read exactly, an aggregate computed
// exactly and rounded once. Instruments live in the SDK and are ingested
// every flush.

import { FailureLog, messageOf } from './background.ts'
import { download, type Link } from './connection.ts'
import { type Code, errorOf, InvalidError, type LimitError, TinystoreError } from './errors.ts'
import { sayOwn } from './logger.ts'
import { type Duration, ms, type Time, unixMs } from './time.ts'
import { byCodePoint } from './wire/codec.ts'
import {
	MetricsBatch,
	MetricsBuckets,
	MetricsDropped,
	MetricsLabels,
	MetricsPlan,
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

/** What a read or an aggregate would spend, each use beside its limit. */
export interface Plan {
	series: number
	blocks: number
	/** the blocks an aggregate answers from their summaries, decoding none of their samples */
	summarized: number
	bytes: number
	decoded: number
	limits: { series: number; blocks: number; bytes: number; decoded: number; answered: number }
	/** the limit the call would reach, which it would end with; undefined when it fits */
	stops: LimitError | undefined
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

/**
 * Each computed exactly and rounded once, a group's too: avg is the mean of
 * every sample, rate a counter's increase a second, delta a gauge's last
 * sample less its first.
 */
export type AggregateOp = 'count' | 'sum' | 'min' | 'max' | 'avg' | 'increase' | 'rate' | 'delta'

/**
 * An aggregate's buckets and, to join series, what groups them: by these
 * labels, or without them, each group one result. `by: []` joins every series
 * of a name. A group never joins two names or two kinds.
 */
export interface AggregateRange extends Range {
	width: Duration
	op: AggregateOp
	by?: string[]
	without?: string[]
}

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
	extra?: {
		width?: number
		op?: AggregateOp
		by?: string[] | undefined
		without?: string[] | undefined
	},
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
	readonly #timers = new Map<string, TimerInstrument>()
	#interval: ReturnType<typeof setInterval> | undefined
	readonly #failureLog = new FailureLog('metrics instruments', sayOwn)
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
	async aggregate(range: AggregateRange): Promise<Aggregate[]> {
		if (range.by !== undefined && range.without !== undefined) {
			throw new InvalidError('an aggregate groups by labels or without them, not both')
		}
		const grouping = { by: range.by, without: range.without }
		const got = await this.#link.run('read', connection =>
			download(
				connection,
				methods['metrics.aggregate'],
				MetricsRange.encode(rangeOf(range, { width: ms(range.width), op: range.op, ...grouping })),
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

	/**
	 * What read(range), or aggregate(range) when it names an op, would spend
	 * of its limits, found without a payload fetched or a sample decoded: the
	 * series, the blocks, those an aggregate answers from their summaries, the
	 * bytes and the samples, beside the limits; stops is the LimitError the
	 * call would end with, undefined when it fits.
	 */
	async explain(
		range: Range & Partial<Pick<AggregateRange, 'width' | 'op' | 'by' | 'without'>>,
	): Promise<Plan> {
		const extra =
			range.op === undefined
				? {}
				: { width: ms(range.width ?? 0), op: range.op, by: range.by, without: range.without }
		const body = await this.#link.run('read', connection =>
			connection.session.call(
				methods['metrics.explain'],
				MetricsRange.encode(rangeOf(range, extra)),
			),
		)
		const p = MetricsPlan.decode(body)
		const stops = p.stops === undefined || p.stops === null ? undefined : p.stops
		return {
			series: Number(p.series ?? 0),
			blocks: Number(p.blocks ?? 0),
			summarized: Number(p.summarized ?? 0),
			bytes: Number(p.bytes ?? 0),
			decoded: Number(p.decoded ?? 0),
			limits: {
				series: Number(p.limitSeries ?? 0),
				blocks: Number(p.limitBlocks ?? 0),
				bytes: Number(p.limitBytes ?? 0),
				decoded: Number(p.limitDecoded ?? 0),
				answered: Number(p.limitAnswered ?? 0),
			},
			stops:
				stops === undefined
					? undefined
					: (errorOf(stops.code ?? 'limit', stops.message ?? '', stops.what ?? {}) as LimitError),
		}
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

	/**
	 * A timer: how many durations it measured and their sum in milliseconds,
	 * ingested every flush as the counters name_count and name_sum, and the
	 * longest since the flush before as the gauge name_max, left out when it
	 * measured none. A range's mean is its sum's increase over its count's.
	 */
	timer(name: string): Timer {
		return new Timer(this.#timer(name), {})
	}

	#instrument(name: string, kind: SeriesKind): Instrument {
		let instrument = this.#instruments.get(name)
		if (instrument === undefined) {
			const timer = timerWriting(name, this.#timers)
			if (timer !== undefined) {
				throw new InvalidError(`${name} is written by the timer ${timer}`)
			}
			instrument = { name, kind, series: new Map(), refused: new Set(), lastRead: undefined }
			this.#instruments.set(name, instrument)
			this.#flushing()
		} else if (instrument.kind !== kind) {
			throw new InvalidError(`${name} is a ${instrument.kind}, not a ${kind}`)
		}
		return instrument
	}

	#timer(name: string): TimerInstrument {
		let timer = this.#timers.get(name)
		if (timer === undefined) {
			for (const { suffix } of timerSeries) {
				const writer = this.#instruments.get(name + suffix)
				if (writer !== undefined) {
					throw new InvalidError(
						`${name + suffix} is a ${writer.kind}; the timer ${name} would write it`,
					)
				}
			}
			timer = { name, series: new Map(), refused: new Set() }
			this.#timers.set(name, timer)
			this.#flushing()
		}
		return timer
	}

	#flushing(): void {
		this.#interval ??= setInterval(() => void this.#flushInBackground(), flushEvery)
		this.#interval.unref?.()
	}

	// a flush no caller awaits says its failure, as Go's background work does
	async #flushInBackground(): Promise<void> {
		try {
			await this.flush()
			this.#failureLog.succeeded()
		} catch (err) {
			this.#failureLog.failed(err)
		}
	}

	/**
	 * Ingests every instrument's value now, as the timer does every 15 s. A
	 * series the store refuses is left out from then on, said once, so that one
	 * bad instrument keeps no other out.
	 */
	async flush(): Promise<void> {
		const at = BigInt(Date.now())
		const batch = await this.#instrumentSamples(at)
		const taken = this.#takeTimers(at, batch)
		if (batch.length === 0) {
			return
		}
		try {
			await this.#ingestLeavingRefusedOut(batch)
		} catch (err) {
			for (const [timing, longest] of taken) {
				timing.longest = Math.max(timing.longest, longest)
				timing.measured = true
			}
			this.failures++
			this.lastFailure = err as Error
			throw err
		}
	}

	// every instrument's value now; a gauge whose function throws skips this
	// flush, said once while its error stays the same
	async #instrumentSamples(at: bigint): Promise<Owned[]> {
		const batch: Owned[] = []
		for (const instrument of this.#instruments.values()) {
			if (instrument.read !== undefined && !(await readGauge(instrument, instrument.read))) {
				continue
			}
			for (const [key, s] of instrument.series) {
				batch.push({
					sample: sampleAt(at, instrument.name, s.labels, instrument.kind, s.value),
					owner: {
						series: formatSeries(instrument.name, s.labels),
						refuse: () => refuse(instrument, key),
					},
				})
			}
		}
		return batch
	}

	// sends the batch until the store takes what is left of it: a refusal that
	// names one of its series leaves that series out, for good, and goes again
	async #ingestLeavingRefusedOut(batch: Owned[]): Promise<void> {
		let sending = batch
		while (sending.length > 0) {
			try {
				const series = sending.map(owned => owned.sample)
				await this.#link.run('write', connection =>
					connection.session.call(methods['metrics.ingest'], MetricsBatch.encode({ series })),
				)
				return
			} catch (err) {
				const refused = refusedIn(sending, err)
				if (refused === undefined) {
					throw err
				}
				refused.refuse()
				sayOwn('warn', 'instrument refused', { series: refused.series, error: messageOf(err) })
				sending = sending.filter(owned => owned.owner !== refused)
			}
		}
	}

	// adds what the timers ingest to batch and starts their longest again,
	// answering what it took so that a failed flush gives it back; a timer's
	// series of one label set share an owner, so a refusal leaves all three out
	#takeTimers(at: bigint, batch: Owned[]): [Timing, number][] {
		const taken: [Timing, number][] = []
		for (const timer of this.#timers.values()) {
			for (const [key, t] of timer.series) {
				const owner = {
					series: formatSeries(timer.name, t.labels),
					refuse: () => refuse(timer, key),
				}
				const add = (suffix: string, kind: SeriesKind, value: number) =>
					batch.push({ sample: sampleAt(at, timer.name + suffix, t.labels, kind, value), owner })
				add('_count', 'counter', t.count)
				add('_sum', 'counter', t.sum)
				if (t.measured) {
					add('_max', 'gauge', t.longest)
					taken.push([t, t.longest])
					t.longest = 0
					t.measured = false
				}
			}
		}
		return taken
	}

	/** Stops flushing every 15 s and ingests the instruments' last values; the store's close calls it. */
	async stop(): Promise<void> {
		if (this.#interval !== undefined) {
			clearInterval(this.#interval)
			this.#interval = undefined
		}
		if (this.instruments > 0) {
			await this.#flushInBackground()
		}
	}

	get instruments(): number {
		return this.#instruments.size + this.#timers.size
	}
}

const flushEvery = 15_000

interface Instrument {
	name: string
	kind: SeriesKind
	/** by the canonical spelling of each label set */
	series: Map<string, { labels: Labels; value: number }>
	/** the spellings of the label sets the store refused, whose writes are left out */
	refused: Set<string>
	read?: () => number | Promise<number>
	/** the error a gauge's function last threw, said once while it stays the same */
	lastRead: string | undefined
}

/** what a timer ingests, each series its name and a suffix */
const timerSeries = [
	{ suffix: '_count', kind: 'counter' },
	{ suffix: '_sum', kind: 'counter' },
	{ suffix: '_max', kind: 'gauge' },
] as const

interface TimerInstrument {
	name: string
	/** by the canonical spelling of each label set */
	series: Map<string, Timing>
	refused: Set<string>
}

interface Timing {
	labels: Labels
	count: number
	/** milliseconds */
	sum: number
	/** milliseconds, since the flush before */
	longest: number
	/** since the flush before */
	measured: boolean
}

/** the timer that writes the series name, if one does */
function timerWriting(name: string, timers: Map<string, TimerInstrument>): string | undefined {
	for (const { suffix } of timerSeries) {
		const base = name.slice(0, -suffix.length)
		if (name.endsWith(suffix) && timers.has(base)) {
			return base
		}
	}
	return undefined
}

/** one sample of a series, as metrics.ingest takes it */
interface Sampled {
	labels: Labels
	kind: SeriesKind
	times: BigInt64Array
	values: Float64Array
}

function sampleAt(
	at: bigint,
	name: string,
	labels: Labels,
	kind: SeriesKind,
	value: number,
): Sampled {
	return {
		labels: { ...labels, [wireName]: name },
		kind,
		times: BigInt64Array.of(at),
		values: Float64Array.of(value),
	}
}

function keyOf(labels: Labels): string {
	return JSON.stringify(Object.entries(labels).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)))
}

function seriesOf(instrument: Instrument, labels: Labels): { labels: Labels; value: number } {
	const key = keyOf(labels)
	let s = instrument.series.get(key)
	if (s === undefined) {
		s = { labels, value: 0 }
		// a refused series' writes go to a value no flush reads
		if (!instrument.refused.has(key)) {
			instrument.series.set(key, s)
		}
	}
	return s
}

/** a sample of an instrument's, and the series a refusal of it leaves out */
interface Owned {
	sample: Sampled
	owner: Owner
}

/** an instrument's series, or a timer's three of one label set */
interface Owner {
	/** as Go prints a series: name{route="/a"} */
	series: string
	refuse(): void
}

function refuse(instrument: Instrument | TimerInstrument, key: string): void {
	instrument.series.delete(key)
	instrument.refused.add(key)
}

/** the codes of a refusal that is the series' own doing, as Go's metrics tells them apart */
const seriesRefusals = new Set<Code>([
	'invalid',
	'limit',
	'too_old',
	'too_new',
	'conflict',
	'corrupt',
	'suspended',
])

/** the owner of the series a refusal names by its labels, when it names one of these */
function refusedIn(sending: Owned[], err: unknown): Owner | undefined {
	if (!(err instanceof TinystoreError) || !seriesRefusals.has(err.code)) {
		return undefined
	}
	const named = Object.entries(err.what)
	return sending.find(
		({ sample }) =>
			named.length === Object.keys(sample.labels).length &&
			named.every(([key, value]) => sample.labels[key] === value),
	)?.owner
}

/** a gauge's value from its function, or false when it threw: said once while the error stays the same */
async function readGauge(
	instrument: Instrument,
	read: () => number | Promise<number>,
): Promise<boolean> {
	if (instrument.refused.size > 0) {
		return false
	}
	try {
		instrument.series.set('{}', { labels: {}, value: await read() })
		instrument.lastRead = undefined
		return true
	} catch (err) {
		const error = messageOf(err)
		if (error !== instrument.lastRead) {
			sayOwn('warn', 'gauge read failed', { series: instrument.name, error })
			instrument.lastRead = error
		}
		return false
	}
}

/** a series as Go prints one: its name, then its labels sorted and quoted */
function formatSeries(name: string, labels: Labels): string {
	const keys = Object.keys(labels).sort()
	if (keys.length === 0) {
		return name
	}
	return `${name}{${keys.map(key => `${key}=${JSON.stringify(labels[key])}`).join(',')}}`
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

/** A timer of labels; record and measure add a duration to the next flush. */
export class Timer {
	readonly #timer: TimerInstrument
	readonly #labels: Labels

	constructor(timer: TimerInstrument, labels: Labels) {
		this.#timer = timer
		this.#labels = labels
	}

	/** The timer of these labels besides its own: the same labels in any order are one series. */
	with(labels: Labels): Timer {
		return new Timer(this.#timer, { ...this.#labels, ...labels })
	}

	/** Adds one duration: milliseconds, their fractions kept, or text such as '250ms'. */
	record(d: Duration): void {
		const millis = typeof d === 'number' ? d : ms(d)
		if (!(Number.isFinite(millis) && millis >= 0)) {
			throw new InvalidError(`a timer records ${d}; a duration is never negative`)
		}
		const key = keyOf(this.#labels)
		if (this.#timer.refused.has(key)) {
			return
		}
		let t = this.#timer.series.get(key)
		if (t === undefined) {
			t = { labels: this.#labels, count: 0, sum: 0, longest: 0, measured: false }
			this.#timer.series.set(key, t)
		}
		t.count++
		t.sum += millis
		t.longest = Math.max(t.longest, millis)
		t.measured = true
	}

	/**
	 * Runs fn and records how long it took, whether it returned or threw: its
	 * result comes back, and what it threw is thrown again.
	 *
	 *     const user = await latency.with({ route: '/users' }).measure(() => users.get(id))
	 */
	async measure<R>(fn: () => R | Promise<R>): Promise<R> {
		const start = performance.now()
		try {
			return await fn()
		} finally {
			this.record(performance.now() - start)
		}
	}
}
