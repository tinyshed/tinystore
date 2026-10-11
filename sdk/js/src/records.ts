// Streams of events, as plan/api/records.md has them, over protocol 2: a
// stream opened by name, its events appended whole and read back by time and
// by what they hold, a page at a time, and every stream read together. A
// store's logger keeps its lines in a stream through the same append.

import type { Link } from './connection.ts'
import { type ConsoleLine, jsonOf } from './console.ts'
import { InvalidError } from './errors.ts'
import { checkName, type Via, viaLink } from './handles.ts'
import { parseJson } from './json.ts'
import { encodeFields, type Logger, type LoggerOptions, newLogger } from './logger.ts'
import { type Duration, type DurationText, ms } from './time.ts'
import { currentTrace, type Trace } from './trace.ts'
import {
	Empty,
	methods,
	RecordsAggregateAsked,
	RecordsAppend,
	RecordsBatch,
	RecordsCount,
	type RecordsEvent,
	RecordsFieldNamesAsked,
	RecordsNames,
	type RecordsNewEvent,
	RecordsOpen,
	RecordsPage,
	RecordsPollAsked,
	RecordsQuery,
	RecordsRows,
	RecordsStreams,
	RecordsTopAsked,
	RecordsTops,
} from './wire/protocol.ts'

/** A level a logger logs at: one of its four names, or slog's number between them. */
export type Level = 'debug' | 'info' | 'warn' | 'error' | number

/** An event's level: what loggers and the programs whose lines are kept write. */
export type EventLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error' | 'fatal'

/** What happened, or what it happened in: an object, or its pairs in their order. */
export type Fields = Record<string, unknown> | readonly (readonly [string, unknown])[]

/** An event as a read gives it. */
export interface Event {
	/** its time, which the file keeps to the nanosecond */
	at: Date
	stream: string
	/** what happened; `log` for a logger's line */
	name: string
	level?: EventLevel
	message?: string
	/** each value as JSON.parse reads it, an integer past 2^53 a bigint */
	fields: Record<string, unknown>
	context: Record<string, unknown>
	/** 32 hex digits */
	traceId?: string
	/** 16 hex digits */
	spanId?: string
}

/** An event to append: its name, what happened, and what it carries beside. */
export interface NewEvent {
	name: string
	fields?: Fields
	/** its time: now unless given, from what the stream keeps to ten minutes ahead */
	at?: Date
	context?: Fields
	level?: EventLevel | number
	message?: string
	/** a trace's 32 hex digits, with its span's 16: the current trace's unless given */
	traceId?: string
	spanId?: string
}

export type AppendOptions = Omit<NewEvent, 'name' | 'fields'>

export interface EventsOptions {
	/** how long the stream keeps its events: its own, kept in the file; 14 days until one says */
	keep?: Duration
}

/** What a read finds: every condition given holds; one given as undefined is InvalidError. */
export interface Query {
	/** the span before now, '1h'; or from and to */
	since?: Duration
	/** the range's start, included */
	from?: Date
	/** the range's end, excluded */
	to?: Date
	/** any of these streams, for every stream's read */
	streams?: readonly string[]
	/** any of these names: ['log'] is the lines */
	names?: readonly string[]
	/** this level and worse; an event without a level is not found */
	minLevel?: EventLevel
	traceId?: string
	/** each key with this value, by the JSON it was written as, in fields or in context */
	where?: Record<string, unknown>
	/** this text in the message, the name or a value, its case ignored */
	search?: string
	/** newest first; oldest first without it */
	newest?: boolean
	/** the events of a page: 1000, and 10,000 at most */
	limit?: number
	/** a page's next, with the rest of the query as it was */
	after?: string
}

/** A query of every event it finds, for what counts them. */
export type Counted = Omit<Query, 'newest' | 'limit' | 'after'>

export interface Page {
	events: Event[]
	/** the page after it; the last page has none */
	next?: string
}

export interface Top {
	value: unknown
	count: number
}

/** A field's numbers in a row: a sum of integers exact, a bigint past 2^53. */
export interface Stats {
	samples: number
	sum: number | bigint
	min: number | bigint
	max: number | bigint
	avg: number
}

export interface Row {
	/** where the row's span begins, counted from 1970 in UTC */
	at: Date
	count: number
	by: Record<string, unknown>
	stats: Record<string, Stats>
	distinct: Record<string, number>
}

export interface Aggregate extends Counted {
	/** the span of a row: the range is one without it */
	every?: Duration
	/** keys whose every value has rows of its own */
	by?: readonly string[]
	/** fields whose numbers a row sums */
	stats?: readonly string[]
	/** fields whose different values a row counts */
	distinct?: readonly string[]
}

export interface StreamInfo {
	name: string
	keep: DurationText
	oldest: Date
	newest: Date
}

export interface PollBatch {
	events: Event[]
	/** where the next poll begins, kept once these events are safe */
	cursor: string
	/** the events keep removed before the cursor reached them */
	expired: number
}

type Method = keyof typeof methods

const levelNumbers: Record<EventLevel, number> = {
	trace: -8,
	debug: -4,
	info: 0,
	warn: 4,
	error: 8,
	fatal: 12,
}
const levelNames: readonly EventLevel[] = ['trace', 'debug', 'info', 'warn', 'error', 'fatal']

/** What every read takes: a stream's handle, or every stream without one. */
abstract class Reads {
	protected abstract ask(
		method: Method,
		body: (handle: number | undefined) => Uint8Array,
	): Promise<Uint8Array>

	/** One page, from one moment of the file: `{ events, next }`. */
	async list(query?: Query): Promise<Page> {
		const asked = queryOf(query)
		const answer = await this.ask('records.list', handle =>
			RecordsQuery.encode({ ...asked, handle }),
		)
		const page = RecordsPage.decode(answer)
		const events = (page.events ?? []).map(eventOf)
		return page.next === undefined ? { events } : { events, next: page.next }
	}

	/** Every event the query finds, a page after another. */
	async *all(query?: Query): AsyncGenerator<Event> {
		let after = query?.after
		for (;;) {
			const page = await this.list(after === undefined ? query : { ...query, after })
			yield* page.events
			if (page.next === undefined) {
				return
			}
			after = page.next
		}
	}

	/** How many events the query finds. */
	async count(query?: Counted): Promise<number> {
		const asked = queryOf(query)
		const answer = await this.ask('records.count', handle =>
			RecordsQuery.encode({ ...asked, handle }),
		)
		return RecordsCount.decode(answer).count ?? 0
	}

	/**
	 * A key's most frequent values and how many events hold each: ten unless
	 * `limit` asks for more, at most 1000. The key is a field's or a
	 * context's, or `stream`, `name` or `level`.
	 */
	async top(key: string, query?: Counted & { limit?: number }): Promise<Top[]> {
		const asked = queryOf(query)
		const answer = await this.ask('records.top', handle =>
			RecordsTopAsked.encode({ query: { ...asked, handle }, key }),
		)
		return (RecordsTops.decode(answer).values ?? []).map(top => ({
			value: parseJson(top.value ?? 'null'),
			count: top.count ?? 0,
		}))
	}

	/** A row for each span of `every` and each value of the keys of `by`, with its count. */
	async aggregate(query: Aggregate = {}): Promise<Row[]> {
		const { every, by, stats, distinct, ...counted } = query
		const asked = queryOf(counted)
		const answer = await this.ask('records.aggregate', handle =>
			RecordsAggregateAsked.encode({
				query: { ...asked, handle },
				every: every === undefined ? undefined : ms(every),
				by: by === undefined ? undefined : [...by],
				stats: stats === undefined ? undefined : [...stats],
				distinct: distinct === undefined ? undefined : [...distinct],
			}),
		)
		return (RecordsRows.decode(answer).rows ?? []).map(row => ({
			at: dateOf(row.at ?? 0n),
			count: row.count ?? 0,
			by: mapValues(row.by, json => parseJson(json)),
			stats: mapValues(row.stats, kept => ({
				samples: kept.samples ?? 0,
				sum: parseJson(kept.sum ?? '0') as number | bigint,
				min: parseJson(kept.min ?? '0') as number | bigint,
				max: parseJson(kept.max ?? '0') as number | bigint,
				avg: kept.avg ?? 0,
			})),
			distinct: mapValues(row.distinct, count => count),
		}))
	}

	/** The keys the events hold, in fields and in context, sorted; `prefix` narrows them. */
	async fieldNames(
		query?: Omit<Query, 'newest' | 'after'> & { prefix?: string },
	): Promise<string[]> {
		const { prefix, ...rest } = query ?? {}
		const asked = queryOf(rest)
		const answer = await this.ask('records.fieldNames', handle =>
			RecordsFieldNamesAsked.encode({ query: { ...asked, handle }, prefix }),
		)
		return RecordsNames.decode(answer).names ?? []
	}
}

/** Opens a stream of events: lazily, at its first call on a connection. */
export function openEvents(link: Link, stream: string, options?: EventsOptions): Events {
	checkName(stream, 'stream')
	const keep = options?.keep === undefined ? undefined : ms(options.keep)
	return new Events(link, stream, RecordsOpen.encode({ stream, keep }))
}

/** A stream of events: written with `append`, which waits for the file, and read back. */
export class Events extends Reads {
	readonly stream: string
	readonly #via: Via
	readonly #open: Uint8Array

	/** Events come from `store.events(stream)`. */
	constructor(link: Link, stream: string, open: Uint8Array) {
		super()
		this.stream = stream
		this.#via = viaLink(link)
		this.#open = open
	}

	/**
	 * Writes an event, or a list of them whole or not at all: it returns once
	 * they are written, and a read finds them at once. A refused event names
	 * its place in the list and its name.
	 */
	append(name: string, fields?: Fields, options?: AppendOptions): Promise<void>
	append(events: readonly NewEvent[]): Promise<void>
	async append(
		first: string | readonly NewEvent[],
		fields?: Fields,
		options?: AppendOptions,
	): Promise<void> {
		const events = typeof first === 'string' ? [oneEvent(first, fields, options)] : first
		const encoded = events.map(newEventOf)
		await this.#call(
			'records.append',
			handle => RecordsAppend.encode({ handle, events: encoded }),
			'write',
		)
	}

	/** A logger's lines, each an event of this stream. */
	appendLines(lines: readonly ConsoleLine[]): Promise<void> {
		const events = lines.map(line => ({
			name: line.event ?? 'log',
			at: nanosOf(line.at),
			level: line.level,
			message: line.msg,
			fields: objectOf(line.attrs),
			context: objectOf(line.context),
			...wireTrace(line.traceId, line.spanId),
		}))
		return this.#call(
			'records.append',
			handle => RecordsAppend.encode({ handle, events }),
			'write',
		).then(() => {})
	}

	protected ask(
		method: Method,
		body: (handle: number | undefined) => Uint8Array,
	): Promise<Uint8Array> {
		return this.#call(method, body, 'read')
	}

	#call(
		method: Method,
		body: (handle: number) => Uint8Array,
		idempotence: 'read' | 'write',
	): Promise<Uint8Array> {
		return this.#via.call(methods['records.open'], this.#open, methods[method], body, idempotence)
	}
}

/** Every stream's events, to read: `store.events()`. */
export class AllEvents extends Reads {
	readonly #link: Link

	/** AllEvents come from `store.events()`. */
	constructor(link: Link) {
		super()
		this.#link = link
	}

	/** Every stream with events, by name, with what it keeps and its oldest and newest event's time. */
	async streams(): Promise<StreamInfo[]> {
		const answer = await this.ask('records.streams', () => Empty.encode({}))
		return (RecordsStreams.decode(answer).streams ?? []).map(stream => ({
			name: stream.name ?? '',
			keep: spelled(stream.keep ?? 0),
			oldest: dateOf(stream.oldest ?? 0n),
			newest: dateOf(stream.newest ?? 0n),
		}))
	}

	/**
	 * Every event once, a batch a call, from a cursor the program keeps, the
	 * oldest event kept without one: `{ events, cursor, expired }`. With no
	 * event past the cursor, the batch is empty and its cursor the one given.
	 */
	async poll(cursor?: string, options?: { limit?: number }): Promise<PollBatch> {
		const answer = await this.ask('records.poll', () =>
			RecordsPollAsked.encode({ cursor, limit: options?.limit }),
		)
		const batch = RecordsBatch.decode(answer)
		return {
			events: (batch.events ?? []).map(eventOf),
			cursor: batch.cursor ?? '',
			expired: batch.expired ?? 0,
		}
	}

	protected ask(
		method: Method,
		body: (handle: number | undefined) => Uint8Array,
	): Promise<Uint8Array> {
		return this.#link.run('read', connection =>
			connection.session.call(methods[method], body(undefined)),
		)
	}
}

/** What a store's logger is told: a logger's options, and how long its stream keeps. */
export interface StoreLoggerOptions extends LoggerOptions {
	/** how long the stream keeps its events, as store.events takes it */
	keep?: Duration
}

/** A logger of `stream` that keeps its lines there through an append of their own. */
export function storeLogger(link: Link, stream: string, options: StoreLoggerOptions = {}): Logger {
	const { keep, ...told } = options
	const events = openEvents(link, stream, keep === undefined ? undefined : { keep })
	return newLogger(lines => events.appendLines(lines), stream, told)
}

/** The event an append of a name, its fields and its options writes. */
function oneEvent(
	name: string,
	fields: Fields | undefined,
	options: AppendOptions | undefined,
): NewEvent {
	return fields === undefined ? { ...options, name } : { ...options, name, fields }
}

/** An event as the wire carries it, the current trace's ids when it gives none. */
function newEventOf(event: NewEvent, place: number): Parameters<typeof RecordsNewEvent.encode>[0] {
	const named = `event ${place} ${JSON.stringify(event.name)}`
	if (typeof event.name !== 'string' || event.name === '') {
		throw new InvalidError(`${named}: an event has a name`)
	}
	if (event.at !== undefined && Number.isNaN(event.at.getTime())) {
		throw new InvalidError(`${named}: a time that is no date`)
	}
	const given =
		event.traceId === undefined
			? traceOf(currentTrace())
			: { traceId: event.traceId, spanId: event.spanId }
	return {
		name: event.name,
		at: event.at === undefined ? undefined : nanosOf(event.at),
		level: event.level === undefined ? undefined : levelNumber(event.level, named),
		message: event.message,
		fields:
			event.fields === undefined ? undefined : objectOf(encodeFields(event.fields, undefined)),
		context:
			event.context === undefined ? undefined : objectOf(encodeFields(event.context, undefined)),
		...wireTrace(given.traceId, given.spanId),
	}
}

/** A query as the wire carries it; a condition given as undefined reads nothing it was not asked. */
function queryOf(query: Query | undefined): Parameters<typeof RecordsQuery.encode>[0] {
	const q = query ?? {}
	for (const [key, value] of Object.entries(q)) {
		if (value === undefined) {
			throw new InvalidError(`a query's ${key} given as undefined: leave it out to read without it`)
		}
	}
	const where = q.where === undefined ? undefined : Object.entries(q.where)
	for (const [key, value] of where ?? []) {
		if (value === undefined) {
			throw new InvalidError(`a query's where ${JSON.stringify(key)} given as undefined`)
		}
	}
	return {
		since: q.since === undefined ? undefined : ms(q.since),
		from: q.from === undefined ? undefined : nanosOf(q.from),
		to: q.to === undefined ? undefined : nanosOf(q.to),
		streams: q.streams === undefined ? undefined : [...q.streams],
		names: q.names === undefined ? undefined : [...q.names],
		minLevel: q.minLevel === undefined ? undefined : levelNumber(q.minLevel, 'minLevel'),
		traceId: q.traceId === undefined ? undefined : idBytes(q.traceId, 16, 'traceId'),
		where:
			where === undefined
				? undefined
				: Object.fromEntries(where.map(([key, value]) => [key, jsonOf(value)])),
		search: q.search,
		newest: q.newest,
		limit: q.limit,
		after: q.after,
	}
}

function eventOf(read: ReturnType<typeof RecordsEvent.decode>): Event {
	const event: Event = {
		at: dateOf(read.at ?? 0n),
		stream: read.stream ?? '',
		name: read.name ?? '',
		fields: read.fields === undefined ? {} : (parseJson(read.fields) as Record<string, unknown>),
		context: read.context === undefined ? {} : (parseJson(read.context) as Record<string, unknown>),
	}
	if (read.level !== undefined) {
		event.level = levelNames[Math.min(5, Math.max(0, Math.floor(read.level / 4) + 2))] as EventLevel
	}
	if (read.message !== undefined) {
		event.message = read.message
	}
	if (read.traceId !== undefined) {
		event.traceId = Buffer.from(read.traceId).toString('hex')
	}
	if (read.spanId !== undefined) {
		event.spanId = Buffer.from(read.spanId).toString('hex')
	}
	return event
}

/** Key, JSON, key, JSON… as the JSON object they spell; none for no field. */
function objectOf(flat: readonly string[]): string | undefined {
	if (flat.length === 0) {
		return undefined
	}
	const pairs: string[] = []
	for (let at = 0; at < flat.length; at += 2) {
		pairs.push(`${JSON.stringify(flat[at])}:${flat[at + 1]}`)
	}
	return `{${pairs.join(',')}}`
}

function levelNumber(level: EventLevel | number, what: string): number {
	if (typeof level === 'number') {
		if (!Number.isInteger(level)) {
			throw new InvalidError(`${what}: the level ${level}, where slog counts whole numbers`)
		}
		return level
	}
	const number = levelNumbers[level]
	if (number === undefined) {
		throw new InvalidError(
			`${what}: the level ${JSON.stringify(level)}, not one of ${levelNames.join(', ')}`,
		)
	}
	return number
}

/** A trace's ids as hex, when the trace has both. */
function traceOf(trace: Trace | undefined): {
	traceId?: string | undefined
	spanId?: string | undefined
} {
	if (trace === undefined) {
		return {}
	}
	const hex = (id: Uint8Array | string | undefined) =>
		typeof id === 'string' || id === undefined ? id : Buffer.from(id).toString('hex')
	return { traceId: hex(trace.traceId), spanId: hex(trace.spanId) }
}

/** A trace and its span as the wire carries them: both, or neither, since the store keeps a span with its trace. */
function wireTrace(
	traceId: string | undefined,
	spanId: string | undefined,
): { traceId?: Uint8Array; spanId?: Uint8Array } {
	if (traceId === undefined || spanId === undefined) {
		return {}
	}
	return { traceId: idBytes(traceId, 16, 'traceId'), spanId: idBytes(spanId, 8, 'spanId') }
}

function idBytes(hex: string, size: number, what: string): Uint8Array {
	if (!new RegExp(`^[0-9a-fA-F]{${2 * size}}$`).test(hex)) {
		throw new InvalidError(`${what} ${JSON.stringify(hex)}: ${2 * size} hex digits`)
	}
	return Uint8Array.from(Buffer.from(hex, 'hex'))
}

function nanosOf(date: Date): bigint {
	return BigInt(date.getTime()) * 1_000_000n
}

function dateOf(nanos: bigint): Date {
	return new Date(Number(nanos / 1_000_000n))
}

function mapValues<T, U>(
	map: Record<string, T> | undefined,
	read: (value: T) => U,
): Record<string, U> {
	return Object.fromEntries(Object.entries(map ?? {}).map(([key, value]) => [key, read(value)]))
}

/** Milliseconds as a span is written: '30d', '12h', '90s', '250ms'. */
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
