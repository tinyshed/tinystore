// Numbers that move over time, as docs/guides/metrics/README.md has them, over
// the protocol: series by their name and labels, written whole by ingest and
// read back by range, in buckets and as their newest samples; what a store
// holds of them; and a program's instruments, whose totals the store keeps.
// A series' samples travel as two columns, an int64 time and a float64 value
// a sample, so that every value's bits come back as they were written.

import { type Connection, download, type Link, pieceOf } from './connection.ts'
import { InvalidError, OutcomeUnknownError } from './errors.ts'
import {
	Counter,
	Gauge,
	type GaugeFuncOptions,
	Histogram,
	type HistogramOptions,
	type InstrumentOptions,
	type Instruments,
	type LabelValue,
	labelsOf,
	Timer,
	type TimerOptions,
} from './instruments.ts'
import { ConnectionLostError, type Stream } from './session.ts'
import { type Calendar, type Duration, type DurationText, ms, type Time, unixMs } from './time.ts'
import {
	Empty,
	MetricsAggregateAsked,
	MetricsAggregates,
	MetricsDamages,
	MetricsDeleted,
	MetricsDescription,
	MetricsExposition,
	MetricsIngest,
	MetricsLatest,
	MetricsNameAsked,
	MetricsNamePage,
	MetricsPageAsked,
	MetricsPiece,
	MetricsPlan,
	MetricsQuery,
	MetricsRead,
	MetricsSeriesNamed,
	MetricsTextPage,
	MetricsUpload,
	methods,
} from './wire/protocol.ts'

export {
	type Buckets,
	Counter,
	exponentialBuckets,
	Gauge,
	type GaugeFuncOptions,
	Histogram,
	type HistogramOptions,
	type InstrumentLabels,
	type InstrumentOptions,
	type LabelValue,
	Timer,
	type TimerOptions,
	Timing,
} from './instruments.ts'

/** What a series' samples are. */
export type SeriesKind = 'gauge' | 'counter' | 'histogram'

/** What tells the series of one name apart: a host, a route, a status. */
export type Labels = Record<string, string>

/**
 * What a label's value must be: a value, one of a list, none of them with
 * `not` (or the label absent), or a prefix. A number or a boolean is its
 * text, as in an instrument's `with`.
 */
export type Condition =
	| LabelValue
	| readonly LabelValue[]
	| { not: LabelValue | readonly LabelValue[] }
	| { prefix: string }

/**
 * The series a call finds and its range: `{ name: 'cpu', where: { host:
 * 'web-1' }, since: '1h' }`. A query is data, the same in every language.
 */
export interface Query {
	/** the series of this name; a timer's name, the series it writes */
	name?: string | undefined
	where?: Record<string, Condition> | undefined
	/** the span before now the range covers; or from and to */
	since?: Duration | undefined
	/** the range's start, included */
	from?: Time | undefined
	/** the range's end, excluded; now, a sample taken now in the range, unless given */
	to?: Time | undefined
	/** a page of read or aggregate: this many samples or buckets at most, 100,000 at most; without it, the whole answer or a limit error */
	limit?: number | undefined
	/** the page after the one whose next this is, read with the same query and limit */
	after?: string | undefined
}

/** A series to write: its samples as [time, value] pairs, or as times and values columns. */
export interface SeriesInput {
	name: string
	kind: SeriesKind
	labels?: Record<string, LabelValue>
	samples?: readonly (readonly [time: Time, value: number])[]
	times?: readonly Time[] | Float64Array
	values?: readonly number[] | Float64Array
	/** how long the name's samples stay: its own, kept in the file; unless given, what it keeps already */
	keep?: Duration | undefined
}

/** What an ingest of an exporter's text gives every series of it. */
export interface PrometheusOptions {
	/** labels every series gets beside its own, such as the instance scraped; one the text gives too is refused, naming its line */
	labels?: Record<string, LabelValue> | undefined
	/** the time of every sample the text gives none; the store's clock unless given */
	at?: Time | undefined
	/** how long every name of the text keeps its samples */
	keep?: Duration | undefined
}

/** A series as a read gives it: its samples as two columns. */
export interface Series {
	name: string
	kind: SeriesKind
	labels: Labels
	/** unix milliseconds, in the order of their times */
	times: Float64Array
	/** the values, whose bytes keep each one's bits: -0 and a NaN's payload survive */
	values: Float64Array
	/** a page cut the series here: the page its next reads begins with the rest of it */
	more: boolean
}

/**
 * One page of a read: its series, and next while there is more. Walking it
 * with for await gives its series, then those of each page after it, one
 * page held at a time. Without limit it is the whole answer.
 */
export interface SeriesPage extends AsyncIterable<Series> {
	series: Series[]
	/** where the page after it begins; absent on the last */
	next?: string
}

/** p and a rank: 'p95', 'p99.9'. The ranks offered are those asked most; any other is written the same way. */
export type Percentile =
	| 'p50'
	| 'p75'
	| 'p90'
	| 'p95'
	| 'p99'
	| 'p99.9'
	// kept apart from the ranks offered, which it would otherwise fold in
	| (`p${number}` & Record<never, never>)

/** What a bucket answers: count … last, or a percentile; rate is a second's increase. */
export type Op =
	| 'count'
	| 'sum'
	| 'avg'
	| 'min'
	| 'max'
	| 'increase'
	| 'rate'
	| 'delta'
	| 'first'
	| 'last'
	| Percentile

/** An aggregate: a query cut into buckets of every, each one exact value of op, by groups. */
export interface AggregateQuery<O extends Op = Op> extends Query {
	/** the buckets' span, counted from 1970 in UTC; the range is one bucket without it */
	every?: Duration | undefined
	/** buckets of a calendar in timeZone, in place of every: 'day' begins each at a local midnight */
	calendar?: Calendar | undefined
	/** the IANA zone a calendar's buckets are in: 'Europe/Moscow', 'UTC' */
	timeZone?: string | undefined
	op: O
	/** the labels a group keeps; without by and without, every series of a name is one group */
	by?: readonly string[] | undefined
	/** the labels a group leaves out, keeping every other; [] keeps every series apart */
	without?: readonly string[] | undefined
	/** a timer's percentile also gives a point between its bounds, interpolated: an estimate */
	estimate?: boolean | undefined
}

/** What every bucket of an aggregate says of itself. */
interface Spanned {
	/** where it begins */
	at: Date
	/** how many samples it comes from; a timer's, how many durations */
	samples: number
	resets: number
	/** its value comes from less than its span: the range, the clock or retention cut it */
	partial: boolean
	/** its first step began before the bucket before it: a whole bucket between had no samples */
	gap: boolean
}

/** A bucket of count … last: its one exact value. */
export interface ValueBucket extends Spanned {
	value: number
}

/**
 * A bucket of a percentile: a gauge's sample at the rank, as value, or a
 * timer's bucket of its spread that holds it, from lower to upper, and with
 * estimate asked a point between them.
 */
export interface PercentileBucket extends Spanned {
	value?: number
	lower?: number
	/** Infinity past the last bound */
	upper?: number
	estimate?: number
}

/** A bucket of an aggregate of op: a value, or what a percentile gives. */
export type Bucket<O extends Op = Op> = O extends Percentile ? PercentileBucket : ValueBucket

export interface Aggregate<O extends Op = Op> {
	name: string
	/** the labels by kept or without left */
	labels: Labels
	/** a bucket without samples is left out */
	buckets: Bucket<O>[]
	/** a page cut the group here: the page its next reads begins with the rest of its buckets */
	more: boolean
}

/**
 * One page of an aggregate: its groups, and next while there is more,
 * walked as a SeriesPage is. A page ends between two whole buckets.
 */
export interface AggregatePage<O extends Op = Op> extends AsyncIterable<Aggregate<O>> {
	groups: Aggregate<O>[]
	/** where the page after it begins; absent on the last */
	next?: string
}

/** A series' newest sample in the range. */
export interface Newest {
	name: string
	labels: Labels
	at: Date
	value: number
}

/** What a read or an aggregate would spend of each bound, and the bound it would stop at. */
export interface Plan {
	series: number
	bytes: number
	decoded: number
	exceeds:
		| {
				/** series, bytes read, samples decoded or answer */
				limit: string
				wanted: number
				bound: number
		  }
		| undefined
}

/** A page's options: a prefix, how many, and where the page before ended. */
export interface PageOptions {
	prefix?: string | undefined
	/** 1000 unless given, 10,000 at most */
	limit?: number | undefined
	/** a page's next, so that a page's options can be the one before's */
	after?: string | undefined
}

/** A name the store holds samples of. */
export interface NameInfo {
	name: string
	kind: SeriesKind
	seriesCount: number
	unit: string
	help: string
	keep: DurationText
	/** its oldest sample, or where its keep begins while it holds samples from before */
	retainedFrom: Date
	newest: Date
}

export interface NamePage {
	names: NameInfo[]
	/** where the next page starts; none on the last */
	next?: string
}

export interface LabelPage {
	labels: string[]
	next?: string
}

export interface ValuePage {
	values: string[]
	next?: string
}

/** A name's unit and help; empty ones remove them. */
export interface Description {
	unit?: string
	help?: string
}

/** A series whose bytes no longer read, which delete removes. */
export interface Damage {
	name: string
	labels: Labels
	error: string
}

/**
 * A store's metrics: series by name and labels, their reads and aggregates,
 * what the store holds of them, and the program's instruments.
 */
export class Metrics {
	readonly #link: Link
	readonly #instruments: Instruments

	/** The store's close stops instruments, sending what they changed last. */
	constructor(link: Link, instruments: Instruments) {
		this.#link = link
		this.#instruments = instruments
	}

	/** Writes series and their samples, every one or none: a refused series names itself. */
	async ingest(series: SeriesInput | readonly SeriesInput[]): Promise<void> {
		const given = Array.isArray(series)
			? (series as readonly SeriesInput[])
			: [series as SeriesInput]
		const body = MetricsIngest.encode({ series: given.map(newSeries) })
		await this.#link.run('write', connection =>
			connection.session.call(methods['metrics.ingest'], body),
		)
	}

	/**
	 * Reads the text an exporter serves, in Prometheus' format or OpenMetrics', and writes its
	 * series, every one or none: a counter as a counter, a histogram's _bucket series as
	 * histogram. A line it cannot read is refused, naming the line.
	 *
	 *     const text = await (await fetch('http://web-1:9100/metrics')).text()
	 *     await store.metrics.ingestPrometheus(text, { labels: { instance: 'web-1' } })
	 */
	async ingestPrometheus(
		text: string | Uint8Array,
		options: PrometheusOptions = {},
	): Promise<void> {
		const labels: Labels = {}
		for (const [label, value] of Object.entries(options.labels ?? {})) {
			labels[label] = String(value)
		}
		const given: ExpositionFields = {
			labels,
			at: options.at === undefined ? undefined : unixMs(options.at),
			keep: options.keep === undefined ? undefined : ms(options.keep),
		}
		const bytes = typeof text === 'string' ? new TextEncoder().encode(text) : text
		await this.#link.run('write', async connection => {
			const body = oneMessage(connection, bytes, given)
			if (body !== undefined) {
				await connection.session.call(methods['metrics.ingestPrometheus'], body)
				return
			}
			const asked = MetricsUpload.encode({ size: bytes.length, ...given })
			const stream = await connection.session.open(
				methods['metrics.uploadPrometheus'],
				asked,
				false,
			)
			await uploaded(stream, bytes, pieceOf(connection))
		})
	}

	/**
	 * The samples of each series the query finds, in its range: without limit
	 * the whole answer, or a limit error naming its bound; with limit a page,
	 * which may hold fewer samples than limit and still have next.
	 */
	async read(query: Query): Promise<SeriesPage> {
		const parts = (await this.#download('metrics.read', MetricsQuery.encode(queryOf(query)))).map(
			part => MetricsRead.decode(part),
		)
		const series: Series[] = []
		let more = false
		for (const piece of parts.flatMap(part => part.series ?? [])) {
			const times = timesOf(piece.times ?? new Uint8Array(0))
			const values = valuesOf(piece.values ?? new Uint8Array(0))
			const last = series.at(-1)
			if (more && last !== undefined) {
				last.times = joined(last.times, times)
				last.values = joined(last.values, values)
				last.more = piece.more === true
			} else {
				const kind = (piece.kind ?? 'gauge') as SeriesKind
				const labels = piece.labels ?? {}
				series.push({
					name: piece.name ?? '',
					kind,
					labels,
					times,
					values,
					more: piece.more === true,
				})
			}
			more = piece.more === true
		}
		const next = parts.at(-1)?.next
		const page: { series: Series[]; next?: string } =
			next === undefined ? { series } : { series, next }
		return walked(
			page,
			at => at.series,
			after => this.read({ ...query, after }),
		)
	}

	/** The newest sample of each series the query finds, when it has one in the range. */
	async latest(query: Query): Promise<Newest[]> {
		const parts = await this.#download('metrics.latest', MetricsQuery.encode(queryOf(query)))
		return parts.flatMap(part =>
			(MetricsLatest.decode(part).series ?? []).map(newest => ({
				name: newest.name ?? '',
				labels: newest.labels ?? {},
				at: new Date(newest.at ?? 0),
				value: newest.value ?? 0,
			})),
		)
	}

	/**
	 * The buckets of each group of the series the query finds, each one exact
	 * value of op, rounded once; a bucket of count … last has its value.
	 */
	async aggregate<O extends Op>(query: AggregateQuery<O>): Promise<AggregatePage<O>> {
		const asked = MetricsAggregateAsked.encode(aggregateOf(query))
		const parts = (await this.#download('metrics.aggregate', asked)).map(part =>
			MetricsAggregates.decode(part),
		)
		const groups: Aggregate<O>[] = []
		let more = false
		for (const piece of parts.flatMap(part => part.groups ?? [])) {
			const buckets = (piece.buckets ?? []).map(bucket => bucketOf(bucket) as Bucket<O>)
			const last = groups.at(-1)
			if (more && last !== undefined) {
				last.buckets.push(...buckets)
				last.more = piece.more === true
			} else {
				groups.push({
					name: piece.name ?? '',
					labels: piece.labels ?? {},
					buckets,
					more: piece.more === true,
				})
			}
			more = piece.more === true
		}
		const next = parts.at(-1)?.next
		const page: { groups: Aggregate<O>[]; next?: string } =
			next === undefined ? { groups } : { groups, next }
		return walked(
			page,
			at => at.groups,
			after => this.aggregate({ ...query, after }),
		)
	}

	/** What a read, or an aggregate when given its op, would spend of each bound; it decodes no sample. */
	async explain(query: Query & Partial<Omit<AggregateQuery, keyof Query>>): Promise<Plan> {
		const asked =
			query.op === undefined ? { query: queryOf(query) } : aggregateOf(query as AggregateQuery)
		const answer = await this.#read('metrics.explain', MetricsAggregateAsked.encode(asked))
		const plan = MetricsPlan.decode(answer)
		const exceeds = plan.exceeds
		return {
			series: plan.series ?? 0,
			bytes: plan.bytes ?? 0,
			decoded: plan.decoded ?? 0,
			exceeds:
				exceeds === undefined
					? undefined
					: { limit: exceeds.limit ?? '', wanted: exceeds.wanted ?? 0, bound: exceeds.bound ?? 0 },
		}
	}

	/** A page of the names the store holds samples of, in the byte order of their text. */
	async names(options: PageOptions = {}): Promise<NamePage> {
		const answer = await this.#read('metrics.names', MetricsPageAsked.encode(pageOf(options)))
		const page = MetricsNamePage.decode(answer)
		const names = (page.names ?? []).map(name => ({
			name: name.name ?? '',
			kind: (name.kind ?? 'gauge') as SeriesKind,
			seriesCount: name.seriesCount ?? 0,
			unit: name.unit ?? '',
			help: name.help ?? '',
			keep: spelled(name.keep ?? 0),
			retainedFrom: new Date(name.retainedFrom ?? 0),
			newest: new Date(name.newest ?? 0),
		}))
		return page.next === undefined ? { names } : { names, next: page.next }
	}

	/** A page of a name's label names, but the store's own. */
	async labelNames(name: string, options: PageOptions = {}): Promise<LabelPage> {
		const page = await this.#texts('metrics.labelNames', { ...pageOf(options), name })
		return page.next === undefined
			? { labels: page.texts }
			: { labels: page.texts, next: page.next }
	}

	/** A page of the values a label of a name's series holds. */
	async labelValues(name: string, label: string, options: PageOptions = {}): Promise<ValuePage> {
		const page = await this.#texts('metrics.labelValues', { ...pageOf(options), name, label })
		return page.next === undefined
			? { values: page.texts }
			: { values: page.texts, next: page.next }
	}

	async #texts(
		method: 'metrics.labelNames' | 'metrics.labelValues',
		asked: Parameters<typeof MetricsPageAsked.encode>[0],
	): Promise<{ texts: string[]; next: string | undefined }> {
		const page = MetricsTextPage.decode(await this.#read(method, MetricsPageAsked.encode(asked)))
		return { texts: page.texts ?? [], next: page.next }
	}

	/**
	 * Sets a name's unit and help in place of what it had, as the Rust metrics
	 * crate's describe_counter! does: what is not given is removed, and `{}`
	 * removes both. They belong to the name, so a delete of its series leaves them.
	 */
	async describe(name: string, description: Description): Promise<void> {
		const body = MetricsDescription.encode({
			name,
			unit: description.unit ?? '',
			help: description.help ?? '',
		})
		await this.#link.run('write', connection =>
			connection.session.call(methods['metrics.describe'], body),
		)
	}

	/** A name's unit and help; both empty when nothing described it. */
	async description(name: string): Promise<{ unit: string; help: string }> {
		const answer = MetricsDescription.decode(
			await this.#read('metrics.description', MetricsNameAsked.encode({ name })),
		)
		return { unit: answer.unit ?? '', help: answer.help ?? '' }
	}

	/**
	 * Removes the one series of exactly this name and these labels, no more
	 * and no fewer, with every sample it holds, whether they still read or
	 * not; true when the store held it. `labels: {}` is the series without labels.
	 */
	async delete(series: { name: string; labels: Record<string, LabelValue> }): Promise<boolean> {
		const labels: Labels = {}
		for (const [label, value] of Object.entries(series.labels)) {
			labels[label] = String(value)
		}
		const body = MetricsSeriesNamed.encode({ name: series.name, labels })
		const answer = await this.#link.run('write', connection =>
			connection.session.call(methods['metrics.delete'], body),
		)
		return MetricsDeleted.decode(answer).found === true
	}

	/**
	 * The series whose bytes no longer read, 1000 at most, each with what
	 * broke; deleting them shows the next.
	 *
	 *     for (let damaged = await store.metrics.damaged(); damaged.length > 0; damaged = await store.metrics.damaged()) {
	 *         for (const series of damaged) await store.metrics.delete(series)
	 *     }
	 */
	async damaged(): Promise<Damage[]> {
		const answer = MetricsDamages.decode(await this.#read('metrics.damaged', Empty.encode({})))
		return (answer.damages ?? []).map(damage => ({
			name: damage.name ?? '',
			labels: damage.labels ?? {},
			error: damage.error ?? '',
		}))
	}

	/** Sends what every instrument changed and has the store write its totals now, as it does every 15 seconds. */
	async flush(): Promise<void> {
		if (await this.#instruments.flush()) {
			return
		}
		await this.#link.run('write', connection =>
			connection.session.call(methods['metrics.flush'], Empty.encode({})),
		)
	}

	/**
	 * A total that only goes up, which the store keeps: processes counting
	 * into one series count into one total. Its label names, when given,
	 * `counter<'route' | 'status'>(…)`, are the only ones `with` compiles with.
	 */
	counter<L extends string = string>(name: string, options: InstrumentOptions = {}): Counter<L> {
		return new Counter(this.#instruments, this.#instruments.declare(name, 'counter', options), {})
	}

	/** A value that goes up and down: a queue's length, a temperature. */
	gauge<L extends string = string>(name: string, options: InstrumentOptions = {}): Gauge<L> {
		return new Gauge(this.#instruments, this.#instruments.declare(name, 'gauge', options), {})
	}

	/**
	 * A gauge whose value read gives every 15 seconds, of the set of labels
	 * given; another set of the name takes a function of its own, and a set
	 * is read by one. It may be async, is never called again while a call of
	 * it runs, and an error or an answer that is not a number writes nothing
	 * that time, said once while it repeats.
	 *
	 *     store.metrics.gaugeFunc('queue_depth', () => emails.depth(), { labels: { queue: 'emails' } })
	 */
	gaugeFunc(
		name: string,
		read: () => number | Promise<number>,
		options: GaugeFuncOptions = {},
	): void {
		const { labels, ...told } = options
		this.#instruments.reader(name, labelsOf(labels ?? {}, false), told, read)
	}

	/** How many things took how long: their count, sum and longest in milliseconds, and with buckets their spread. */
	timer<L extends string = string>(name: string, options: TimerOptions = {}): Timer<L> {
		return new Timer(this.#instruments, this.#instruments.declare(name, 'timer', options), {})
	}

	/** A timer of numbers that are not durations: a response's size in bytes. */
	histogram<L extends string = string>(name: string, options: HistogramOptions = {}): Histogram<L> {
		return new Histogram(
			this.#instruments,
			this.#instruments.declare(name, 'histogram', options),
			{},
		)
	}

	#read(method: keyof typeof methods, body: Uint8Array): Promise<Uint8Array> {
		return this.#link.run('read', connection => connection.session.call(methods[method], body))
	}

	async #download(method: keyof typeof methods, body: Uint8Array): Promise<Uint8Array[]> {
		const got = await this.#link.run('read', connection =>
			download(connection, methods[method], body),
		)
		return got.items.length === 0 ? [got.trailer] : [...got.items, got.trailer]
	}
}

/**
 * A page that for await walks: its items, then those of each page after it,
 * the next page read only once the walk reaches the end of the one it holds.
 */
function walked<P extends { next?: string }, T>(
	page: P,
	items: (page: P) => readonly T[],
	readOn: (after: string) => Promise<P>,
): P & AsyncIterable<T> {
	return Object.assign(page, {
		async *[Symbol.asyncIterator](): AsyncGenerator<T> {
			for (let at = page; ; ) {
				yield* items(at)
				if (at.next === undefined) {
					return
				}
				at = await readOn(at.next)
			}
		},
	})
}

/** A query as the wire carries it; a condition given as undefined is refused, never left out. */
function queryOf(query: Query): Parameters<typeof MetricsQuery.encode>[0] {
	const where: Record<string, { values?: string[]; not?: boolean; prefix?: string }> = {}
	for (const [label, condition] of Object.entries(query.where ?? {})) {
		where[label] = conditionOf(label, condition)
	}
	if (query.since !== undefined && query.from !== undefined) {
		throw new InvalidError('a query starts since a span before now or from a time, not both')
	}
	return {
		name: query.name,
		where,
		since: query.since === undefined ? undefined : ms(query.since),
		from: query.from === undefined ? undefined : unixMs(query.from),
		to: query.to === undefined ? undefined : unixMs(query.to),
		limit: query.limit,
		after: query.after,
	}
}

function conditionOf(
	label: string,
	condition: Condition | undefined,
): { values?: string[]; not?: boolean; prefix?: string } {
	if (condition === undefined || condition === null) {
		throw new InvalidError(`where ${label}: a condition given as ${String(condition)}`)
	}
	if (Array.isArray(condition)) {
		return { values: (condition as readonly LabelValue[]).map(String) }
	}
	if (typeof condition === 'object') {
		if ('not' in condition) {
			return { values: [condition.not].flat().map(String), not: true }
		}
		if ('prefix' in condition) {
			return { prefix: condition.prefix }
		}
		throw new InvalidError(`where ${label}: a value, a list, { not } or { prefix }`)
	}
	return { values: [String(condition)] }
}

function aggregateOf(query: AggregateQuery): Parameters<typeof MetricsAggregateAsked.encode>[0] {
	return {
		query: queryOf(query),
		every: query.every === undefined ? undefined : ms(query.every),
		calendar: query.calendar,
		timeZone: query.timeZone,
		op: query.op,
		by: query.by === undefined ? undefined : [...query.by],
		without: query.without === undefined ? undefined : [...query.without],
		estimate: query.estimate === true,
	}
}

function pageOf(options: PageOptions): Parameters<typeof MetricsPageAsked.encode>[0] {
	return { prefix: options.prefix, limit: options.limit, after: options.after }
}

/** A bucket as the wire carries it. */
type WireBucket = NonNullable<
	NonNullable<ReturnType<typeof MetricsAggregates.decode>['groups']>[number]['buckets']
>[number]

function bucketOf(bucket: WireBucket): PercentileBucket {
	const answer: PercentileBucket = {
		at: new Date(bucket.at ?? 0),
		samples: bucket.samples ?? 0,
		resets: bucket.resets ?? 0,
		partial: bucket.partial === true,
		gap: bucket.gap === true,
	}
	if (bucket.value !== undefined) {
		answer.value = bucket.value
	}
	if (bucket.lower !== undefined) {
		answer.lower = bucket.lower
		answer.upper = bucket.upper ?? Infinity
	}
	if (bucket.estimate !== undefined) {
		answer.estimate = bucket.estimate
	}
	return answer
}

/** A series to write, its samples as the two columns the wire carries. */
function newSeries(
	given: SeriesInput,
): NonNullable<Parameters<typeof MetricsIngest.encode>[0]['series']>[number] {
	const labels: Labels = {}
	for (const [label, value] of Object.entries(given.labels ?? {})) {
		labels[label] = String(value)
	}
	const [times, values] = columnsOf(given)
	const timeBytes = new BigInt64Array(times.length)
	for (let i = 0; i < times.length; i++) {
		timeBytes[i] = BigInt(unixMs(times[i]!))
	}
	// a Float64Array's bytes go as they are, a NaN's payload with them
	const valueBytes =
		values instanceof Float64Array
			? new Uint8Array(
					values.buffer.slice(values.byteOffset, values.byteOffset + values.byteLength),
				)
			: new Uint8Array(Float64Array.from(values).buffer)
	return {
		name: given.name,
		kind: given.kind,
		labels,
		times: new Uint8Array(timeBytes.buffer),
		values: valueBytes,
		keep: given.keep === undefined ? undefined : ms(given.keep),
	}
}

/** What a text's ingest gives every series of it, one message or an upload. */
interface ExpositionFields {
	labels: Labels
	at: number | undefined
	keep: number | undefined
}

/** A text's one message, when it fits the body the connection agreed; a longer text goes in pieces. */
function oneMessage(
	connection: Connection,
	text: Uint8Array,
	given: ExpositionFields,
): Uint8Array | undefined {
	const most = connection.session.agreed?.maxBody ?? 0
	if (text.length > most) {
		return undefined
	}
	const body = MetricsExposition.encode({ text, ...given })
	return body.length <= most ? body : undefined
}

/**
 * Sends a text's pieces as DATA within the stream's credit, the last ending
 * them, and waits for the store to write it. A connection lost before that
 * last DATA left nothing written, so the text goes again on the next one;
 * lost after it, whether the text was written is unknown.
 */
async function uploaded(stream: Stream, text: Uint8Array, piece: number): Promise<void> {
	let ending = false
	try {
		for (let at = 0; !ending; at += piece) {
			ending = at + piece >= text.length
			await stream.send(MetricsPiece.encode({ bytes: text.subarray(at, at + piece) }), ending)
		}
		// the RESPONSE that began it, then the answer to its last piece
		await stream.next()
		await stream.next()
	} catch (err) {
		if (err instanceof ConnectionLostError && !ending) {
			// the store let go of what came and wrote nothing, so the whole text goes again
			throw new ConnectionLostError(err.message, false)
		}
		stream.cancel()
		if (err instanceof ConnectionLostError) {
			throw new OutcomeUnknownError(
				`the connection was lost as the text was written: ${err.message}`,
			)
		}
		throw err
	}
}

function columnsOf(given: SeriesInput): [ArrayLike<Time>, ArrayLike<number>] {
	if (given.samples !== undefined) {
		if (given.times !== undefined || given.values !== undefined) {
			throw new InvalidError(`metrics ${given.name}: samples, or times and values, not both`)
		}
		return [given.samples.map(([time]) => time), given.samples.map(([, value]) => value)]
	}
	if (
		given.times === undefined ||
		given.values === undefined ||
		given.times.length !== given.values.length
	) {
		throw new InvalidError(
			`metrics ${given.name}: samples, or times and values as long as each other`,
		)
	}
	return [given.times, given.values]
}

/** A column of int64 times, read as the unix milliseconds a Float64Array holds. */
function timesOf(bytes: Uint8Array): Float64Array {
	const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
	const times = new Float64Array(bytes.byteLength / 8)
	for (let i = 0; i < times.length; i++) {
		times[i] = Number(view.getBigInt64(i * 8, true))
	}
	return times
}

/**
 * A column of float64 values, its bytes copied as they came, so that each
 * value's bits stay. A socket's bytes are a Buffer, whose slice is a view of
 * a shared pool rather than a copy, so they are copied into a buffer of their own.
 */
function valuesOf(bytes: Uint8Array): Float64Array {
	const copy = new Uint8Array(bytes.byteLength)
	copy.set(bytes)
	return new Float64Array(copy.buffer)
}

function joined(a: Float64Array, b: Float64Array): Float64Array {
	const out = new Float64Array(a.length + b.length)
	out.set(a)
	out.set(b, a.length)
	return out
}

function spelled(millis: number): DurationText {
	for (const [unit, size] of [
		['d', 86_400_000],
		['h', 3_600_000],
		['m', 60_000],
		['s', 1000],
	] as const) {
		if (millis >= size && millis % size === 0) {
			return `${millis / size}${unit}` as DurationText
		}
	}
	return `${millis}ms` as DurationText
}
