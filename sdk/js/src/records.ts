// Streams of events, as docs/guides/records/README.md has them, over the protocol: a
// stream opened by name, its events appended whole and read back by time and
// by what they hold, a page at a time, and every stream read together. A
// store's logger keeps its lines in a stream through the same append; a tail
// follows what is written from now on, and a writer of lines turns another
// program's output into events.

import { type Connection, type Link, pieceOf } from './connection.ts'
import { type ConsoleLine, jsonOf } from './console.ts'
import { ClosedError, InvalidError } from './errors.ts'
import { refuseUnknown } from './fields.ts'
import { checkName, type Home, handleOn, type Open, type Via, viaLink } from './handles.ts'
import { parseJson } from './json.ts'
import {
	encodeFields,
	type LogConfig,
	type Logger,
	type LoggerOptions,
	newStoreLogger,
} from './logger.ts'
import type { Stream } from './session.ts'
import { BoundStatement, reachOf, Sql } from './sql.ts'
import { type Calendar, type Duration, type DurationText, keepSpelled, ms } from './time.ts'
import { currentTrace, type Trace } from './trace.ts'
import {
	Empty,
	Handle,
	methods,
	RecordsAggregateAsked,
	RecordsAppend,
	RecordsAppended,
	RecordsBatch,
	RecordsCount,
	RecordsDamage,
	RecordsDamages,
	RecordsDropped,
	type RecordsEvent,
	RecordsEvents,
	RecordsFieldNamesAsked,
	RecordsFieldsAsked,
	RecordsLargestAsked,
	RecordsNames,
	type RecordsNewEvent,
	RecordsOpen,
	RecordsPage,
	RecordsPiece,
	RecordsPollAsked,
	RecordsQuery,
	RecordsRows,
	RecordsSessions,
	RecordsSessionsAsked,
	RecordsStreamFields,
	RecordsStreams,
	RecordsTailBatch,
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
	/** what its append named it by: its stream holds one event of an id */
	id?: string
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
	/**
	 * what names it in its stream, a webhook's delivery or a job's key, 1 to 128 bytes:
	 * an append of an id the stream holds writes nothing, and the first event of an id stands
	 */
	id?: string
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
	/**
	 * each key, in fields or in context, with this value by the JSON it was
	 * written as; one of a list, `[7, 12]`; none of them with `{ not }`, the
	 * key absent too; or a text that starts with `{ prefix }`. A list or an
	 * object as the value itself goes inside a list: `[['a', 'b']]`. A
	 * statement of a database, `` db.statement`select id from teachers` ``,
	 * is the list of the values its one column gives, read once as the query
	 * starts, on its database: not in one moment with the events, and again
	 * for a page asked after.
	 */
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

/** A query of every event a delete takes out: one with a `where`, and no `search`. */
export type Deleted = Omit<Counted, 'search' | 'where'> & { where: Record<string, unknown> }

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
	/** the local date a calendar's row begins on, '2026-11-19': a week's Monday, a month's first */
	date?: string
}

export interface Aggregate extends Counted {
	/** the span of a row: the range is one without it */
	every?: Duration
	/** rows of a calendar in timeZone, in place of every: 'day' begins each at a local midnight */
	calendar?: Calendar
	/** the IANA zone a calendar's rows are in: 'Europe/Moscow', 'UTC' */
	timeZone?: string
	/** keys whose every value has rows of its own */
	by?: readonly string[]
	/** fields whose numbers a row sums */
	stats?: readonly string[]
	/** fields whose different values a row counts */
	distinct?: readonly string[]
	/** a row for every span of the range the stream keeps, an empty one counting 0; it needs since or from */
	fill?: boolean
}

/**
 * What `sessions` cuts a read into: the events of one value of each key of
 * `by`, each within `gap` of the one before.
 */
export interface SessionsQuery extends Counted {
	/** the keys whose values a session shares: a field's, else a context's, or stream, name, level or message */
	by: string | readonly string[]
	/** the pause that ends a session: an event later than this after the one before begins the next */
	gap: Duration
	/** the longest a session lasts: an event later than this after its first begins the next */
	max?: Duration
	/** each value's first sessions of the range alone, its later events passed over */
	first?: number
	/** the first events each session keeps, 1000 at most; none unless asked */
	events?: number
}

/** A session: the events of one value of each key of `by`, each within `gap` of the one before. */
export interface Session {
	by: Record<string, unknown>
	/** its first event's time */
	from: Date
	/** its last event's time */
	to: Date
	/** every event of it, however few it keeps */
	count: number
	/** its first events, as many as `events` asked */
	events: Event[]
	/** whether it may go on outside what was read: its first event within gap of the range's start, or its last of its end */
	partial: boolean
}

export interface StreamInfo {
	name: string
	keep: DurationText
	/** its oldest event, or where its keep begins while it holds older */
	retainedFrom: Date
	newest: Date
}

/** What a stream's events carry, each by name. */
export interface StreamFields {
	eventNames: NameCount[]
	fields: FieldInfo[]
	context: FieldInfo[]
}

/** A name a stream's events have, and how many have it. */
export interface NameCount {
	name: string
	count: number
}

/** A key of a stream's events' fields or contexts. */
export interface FieldInfo {
	name: string
	/** the kinds of value it holds, in the order `ValueKind` lists them */
	kinds: ValueKind[]
	/** the events that carry it: one that writes it twice, once */
	count: number
}

/** What a value is, as its JSON is spelled: `float` has a fraction or an exponent, `integer` is whole. */
export type ValueKind = 'float' | 'integer' | 'text' | 'bool' | 'object' | 'list' | 'null'

export interface PollBatch {
	events: Event[]
	/** where the next poll begins, kept once these events are safe */
	cursor: string
	/** the events keep removed before the cursor reached them */
	expired: number
}

/** What a tail finds: a query's conditions, and no range, order or limit. */
export type TailQuery = Pick<
	Query,
	'streams' | 'names' | 'minLevel' | 'traceId' | 'where' | 'search'
>

/** A tail's batch: the events written since the last, and how many it missed. */
export interface TailBatch {
	events: Event[]
	/** the events that left the store's hold before the tail read them */
	missed: number
}

/** A head row or a segment that no longer reads, as `damaged` lists it and `drop` takes it. */
export interface Damage {
	/** a head row holds events not packed yet, a segment packed ones */
	part: 'head row' | 'segment'
	id: number
	stream: string
	/** its events' oldest and newest times */
	first: Date
	last: Date
	/** what broke */
	error: string
}

type Method = keyof typeof methods

const newline = 0x0a
/** What a writer of lines holds for the server at most: past it, lines are dropped and counted. */
const linesHeld = 1 << 20
/** The bytes one event holds: a longer line is dropped whole, as the store drops one. */
const eventRoom = 256 << 10
const encoder = new TextEncoder()

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
	/**
	 * Calls `method` with what `body` writes, given the stream's handle and,
	 * for each of `databases`, its handle on the connection the call goes on.
	 */
	protected abstract ask(
		method: Method,
		body: (handle: number | undefined, databases: readonly number[]) => Uint8Array,
		databases?: readonly Home[],
	): Promise<Uint8Array>

	protected abstract opened(
		method: Method,
		body: (handle: number | undefined) => Uint8Array,
	): Promise<Stream>

	/** Keeps a stream that follows the store for its close to cancel; it returns what lets it go. */
	protected abstract follow(stream: Stream): () => void

	/**
	 * One page: `{ events, next }`, its `limit` of events that match, fewer
	 * only on the last, which has no `next`.
	 */
	async list(query?: Query): Promise<Page> {
		const asked = queryOf(query)
		const answer = await this.ask(
			'records.list',
			(handle, databases) => RecordsQuery.encode(queryOn(asked, handle, databases)),
			databasesOf(asked),
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

	/**
	 * Deletes every event the query finds, of those appended before the
	 * call, and what the store keeps only for them: a context of their own,
	 * their keys and counts, the bytes they lay in. Answers how many it
	 * deleted. It cannot be undone, and run again it deletes what is left:
	 * no poll's cursor moves, and an event's id stays held until the
	 * stream's keep passes its time. A backup taken before it, and another
	 * node's store, keep the events.
	 *
	 *     await store.events().delete({ streams: ['activity'], where: { userId: 97516 } })
	 */
	async delete(query: Deleted): Promise<number> {
		const asked = queryOf(query)
		const answer = await this.ask(
			'records.delete',
			(handle, databases) => RecordsQuery.encode(queryOn(asked, handle, databases)),
			databasesOf(asked),
		)
		return RecordsCount.decode(answer).count ?? 0
	}

	/** How many events the query finds. */
	async count(query?: Counted): Promise<number> {
		const asked = queryOf(query)
		const answer = await this.ask(
			'records.count',
			(handle, databases) => RecordsQuery.encode(queryOn(asked, handle, databases)),
			databasesOf(asked),
		)
		return RecordsCount.decode(answer).count ?? 0
	}

	/**
	 * A key's most frequent values and how many events hold each: ten unless
	 * `limit` asks for more, at most 1000. The key is a field's or a
	 * context's, or `stream`, `name`, `level` or `message`.
	 */
	async top(key: string, query?: Counted & { limit?: number }): Promise<Top[]> {
		const asked = queryOf(query)
		const answer = await this.ask(
			'records.top',
			(handle, databases) =>
				RecordsTopAsked.encode({ query: queryOn(asked, handle, databases), key }),
			databasesOf(asked),
		)
		return (RecordsTops.decode(answer).values ?? []).map(top => ({
			value: parseJson(top.value ?? 'null'),
			count: top.count ?? 0,
		}))
	}

	/**
	 * The events whose value at `key` is a number, the largest first: ten
	 * unless `limit` asks for more, at most 1000. The key is a field's, else
	 * a context's; an event without a number there is passed over, and
	 * events of one number come in the order a list gives them.
	 */
	async largest(key: string, query?: Counted & { limit?: number }): Promise<Event[]> {
		const asked = queryOf(query)
		const answer = await this.ask(
			'records.largest',
			(handle, databases) =>
				RecordsLargestAsked.encode({ query: queryOn(asked, handle, databases), key }),
			databasesOf(asked),
		)
		return (RecordsEvents.decode(answer).events ?? []).map(eventOf)
	}

	/** A row for each span of `every` and each value of the keys of `by`, with its count. */
	async aggregate(query: Aggregate = {}): Promise<Row[]> {
		const { every, calendar, timeZone, by, stats, distinct, fill, ...counted } = query
		const asked = queryOf(counted)
		const answer = await this.ask(
			'records.aggregate',
			(handle, databases) =>
				RecordsAggregateAsked.encode({
					query: queryOn(asked, handle, databases),
					every: every === undefined ? undefined : ms(every),
					calendar,
					timeZone,
					by: by === undefined ? undefined : [...by],
					stats: stats === undefined ? undefined : [...stats],
					distinct: distinct === undefined ? undefined : [...distinct],
					fill,
				}),
			databasesOf(asked),
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
			...(row.date === undefined ? {} : { date: row.date }),
		}))
	}

	/**
	 * The events the query finds cut into sessions by a pause, in the order of
	 * their first events: the answer whole, not paged, and `LimitError` past
	 * 10,000 sessions held, open ones among them, or 64 MiB of what they keep.
	 *
	 *     await events.sessions({ since: '30d', by: 'userId', gap: '30m', max: '4h', first: 1, events: 50 })
	 */
	async sessions(query: SessionsQuery): Promise<Session[]> {
		const { by, gap, max, first, events, ...counted } = query
		const asked = queryOf(counted)
		const answer = await this.ask(
			'records.sessions',
			(handle, databases) =>
				RecordsSessionsAsked.encode({
					query: queryOn(asked, handle, databases),
					by: typeof by === 'string' ? [by] : [...by],
					gap: ms(gap),
					max: max === undefined ? undefined : ms(max),
					first,
					events,
				}),
			databasesOf(asked),
		)
		return (RecordsSessions.decode(answer).sessions ?? []).map(session => ({
			by: mapValues(session.by, json => parseJson(json)),
			from: dateOf(session.from ?? 0n),
			to: dateOf(session.to ?? 0n),
			count: session.count ?? 0,
			events: (session.events ?? []).map(eventOf),
			partial: session.partial ?? false,
		}))
	}

	/** The keys the events hold, in fields and in context, sorted; `prefix` narrows them. */
	async fieldNames(
		query?: Omit<Query, 'newest' | 'after'> & { prefix?: string },
	): Promise<string[]> {
		const { prefix, ...rest } = query ?? {}
		const asked = queryOf(rest)
		const answer = await this.ask(
			'records.fieldNames',
			(handle, databases) =>
				RecordsFieldNamesAsked.encode({ query: queryOn(asked, handle, databases), prefix }),
			databasesOf(asked),
		)
		return RecordsNames.decode(answer).names ?? []
	}

	/**
	 * The events written from now on that the query finds, a batch as they
	 * are written: `{ events, missed }`. A reader slower than the writers
	 * skips what left the store's hold, and the batch after says how many.
	 * It ends when its loop is left, and when the server's store closes.
	 */
	async *tail(query?: TailQuery): AsyncGenerator<TailBatch> {
		const asked = queryOf(query)
		if (asked.bound.length > 0) {
			throw new InvalidError(
				"a tail's where takes no statement: a statement is read once, as a query starts, and a tail lives on",
			)
		}
		const stream = await this.opened('records.tail', handle =>
			RecordsQuery.encode(queryOn(asked, handle, [])),
		)
		const unfollow = this.follow(stream)
		try {
			for (;;) {
				const event = await stream.next()
				stream.consumed(event.body.length)
				if (event.end) {
					return
				}
				const batch = RecordsTailBatch.decode(event.body)
				yield { events: (batch.events ?? []).map(eventOf), missed: batch.missed ?? 0 }
			}
		} finally {
			stream.cancel()
			unfollow()
		}
	}
}

/** Opens a stream of events: lazily, at its first call on a connection. */
export function openEvents(link: Link, stream: string, options?: EventsOptions): Events {
	return new Events(link, stream, eventsOpen(stream, options))
}

/** A stream's open, through the database of `database`, a handle on the connection, when given. */
export function eventsOpen(stream: string, options?: EventsOptions, database?: number): Uint8Array {
	checkName(stream, 'stream')
	const keep = options?.keep === undefined ? undefined : ms(options.keep)
	return RecordsOpen.encode({ stream, keep, database })
}

/** What a stream's appends in a database's transaction and batch need of it. */
interface EventsParts {
	stream: string
	open: Open
	home: Home | undefined
}

let eventsPartsOf: (events: Events) => EventsParts

/** A stream of events: written with `append`, which waits for the file, and read back. */
export class Events extends Reads {
	readonly stream: string
	readonly #link: Link
	readonly #via: Via
	readonly #open: Open
	readonly #home: Home | undefined

	/** Events come from `store.events(stream)`, or `db.events(stream)` through a database. */
	constructor(link: Link, stream: string, open: Open, home?: Home) {
		super()
		this.stream = stream
		this.#link = link
		this.#via = home === undefined ? viaLink(link) : home.via(`events ${stream}`)
		this.#open = open
		this.#home = home
	}

	static {
		eventsPartsOf = events => ({ stream: events.stream, open: events.#open, home: events.#home })
	}

	/**
	 * An event's append for a batch of the database the stream was opened
	 * from, `db.events`: it is kept with the batch's rows and reaches the
	 * stream once they commit, before the batch returns, or never.
	 *
	 *     await db.batch([sql`update roles set role = ${role} where user_id = ${id}`, audit.appending('role.changed', { userId: id, role })])
	 */
	appending(name: string, fields?: Fields, options?: AppendOptions): Appending {
		const event = newEventOf(oneEvent(name, fields, options), 0)
		return new Appending({ ...eventsPartsOf(this), event })
	}

	/**
	 * Writes an event, or a list of them whole or not at all: it returns once
	 * they are written, and a read finds them at once. An event whose id the
	 * stream holds, or an earlier event of the list has, is written as
	 * nothing: one event answers whether it was written, a list how many of
	 * its events were. A refused event names its place in the list and its name.
	 */
	append(name: string, fields?: Fields, options?: AppendOptions): Promise<boolean>
	append(events: readonly NewEvent[]): Promise<number>
	async append(
		first: string | readonly NewEvent[],
		fields?: Fields,
		options?: AppendOptions,
	): Promise<boolean | number> {
		const events = typeof first === 'string' ? [oneEvent(first, fields, options)] : first
		const encoded = events.map(newEventOf)
		const answer = await this.#call(
			'records.append',
			handle => RecordsAppend.encode({ handle, events: encoded }),
			'write',
		)
		const written = events.length - (RecordsAppended.decode(answer).held ?? 0)
		return typeof first === 'string' ? written === 1 : written
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

	/**
	 * A writer of another program's output into this stream: each line it is
	 * given becomes an event at the time it arrived, a stack trace's or a JSON
	 * value's lines one, and a write never waits.
	 *
	 *     const out = store.events('worker').lines()
	 *     child.stdout.on('data', chunk => out.write(chunk))
	 *     child.on('close', () => out.end())
	 */
	lines(): Lines {
		return new Lines(async () => {
			const connection = await this.#link.connection()
			const handle = await handleOn(connection, methods['records.open'], this.#open)
			const body = Handle.encode({ handle })
			const stream = await streamOf(connection, methods['records.lines'], body, false)
			return { stream, piece: pieceOf(connection) }
		})
	}

	protected ask(
		method: Method,
		body: (handle: number | undefined, databases: readonly number[]) => Uint8Array,
		databases: readonly Home[] = [],
	): Promise<Uint8Array> {
		if (databases.length === 0) {
			return this.#call(method, handle => body(handle, []), 'read')
		}
		refuseStatements(this.#link, databases)
		if (this.#home !== undefined) {
			reachOf(this.#home)?.refuseAround(`events ${this.stream}`)
		}
		// the databases' handles are the connection's the call goes on, opened before it
		return this.#link.run('read', async connection => {
			const opened = await Promise.all(databases.map(database => database.handle(connection)))
			const handle = await handleOn(connection, methods['records.open'], this.#open)
			return connection.session.call(methods[method], body(handle, opened))
		})
	}

	protected opened(
		method: Method,
		body: (handle: number | undefined) => Uint8Array,
	): Promise<Stream> {
		return this.#link.run('read', async connection => {
			const handle = await handleOn(connection, methods['records.open'], this.#open)
			return streamOf(connection, methods[method], body(handle), true)
		})
	}

	protected follow(stream: Stream): () => void {
		return this.#link.follow(stream)
	}

	#call(
		method: Method,
		body: (handle: number) => Uint8Array,
		idempotence: 'read' | 'write',
	): Promise<Uint8Array> {
		return this.#via.call(methods['records.open'], this.#open, methods[method], body, idempotence)
	}
}

/** The parts of a stream, when `handle` is one. */
export function eventsParts(handle: unknown): EventsParts | undefined {
	return handle instanceof Events ? eventsPartsOf(handle) : undefined
}

/**
 * A stream's appends inside a transaction of the database it was opened
 * from, `tx.with(audit)`: each is kept with the transaction's rows and
 * reaches the stream once it commits, before `db.tx` returns. A read goes
 * through the stream itself, and finds what committed.
 */
export class TxEvents {
	readonly stream: string
	readonly #open: Open
	readonly #via: Via

	/** TxEvents come from `tx.with(events)`. */
	constructor(parts: EventsParts, via: Via) {
		this.stream = parts.stream
		this.#open = parts.open
		this.#via = via
	}

	/**
	 * Appends an event once the transaction commits, and says whether it
	 * will be written: false when its stream, or this transaction, holds its id.
	 */
	async append(name: string, fields?: Fields, options?: AppendOptions): Promise<boolean> {
		const event = newEventOf(oneEvent(name, fields, options), 0)
		const answer = await this.#via.call(
			methods['records.open'],
			this.#open,
			methods['records.append'],
			handle => RecordsAppend.encode({ handle, events: [event] }),
			'write',
		)
		return (RecordsAppended.decode(answer).held ?? 0) === 0
	}
}

interface AppendingParts extends EventsParts {
	event: Parameters<typeof RecordsNewEvent.encode>[0]
}

let appendingPartsOf: (appending: Appending) => AppendingParts

/** An event's append for a database's batch, from `events.appending`. */
export class Appending {
	readonly #parts: AppendingParts

	/** Comes from `events.appending`. */
	constructor(parts: AppendingParts) {
		this.#parts = parts
	}

	static {
		appendingPartsOf = appending => appending.#parts
	}
}

/**
 * What a batch of `home` sends of an append, on the connection it goes on:
 * the append's call, its stream's handle opened there first. A stream opened
 * from the store or another database is refused, since its event would not
 * commit with the rows.
 */
export function appendingIn(
	appending: Appending,
	home: Home,
): (connection: Connection) => Promise<{ method: number; body: Uint8Array }> {
	const { stream, open, home: kept, event } = appendingPartsOf(appending)
	if (kept !== home) {
		const where = kept === undefined ? 'records.db' : kept.label
		throw new InvalidError(
			`${home.label}: events ${stream}: kept in ${where}, outside this batch of ${home.label}`,
		)
	}
	return async connection => {
		const handle = await handleOn(connection, methods['records.open'], open)
		return {
			method: methods['records.append'],
			body: RecordsAppend.encode({ handle, events: [event] }),
		}
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

	/** Every stream with events, by name, with what it keeps, where that begins and its newest event's time. */
	async streams(): Promise<StreamInfo[]> {
		const answer = await this.ask('records.streams', () => Empty.encode({}))
		return (RecordsStreams.decode(answer).streams ?? []).map(stream => ({
			name: stream.name ?? '',
			keep: keepSpelled(stream.keep ?? 0),
			retainedFrom: dateOf(stream.retainedFrom ?? 0n),
			newest: dateOf(stream.newest ?? 0n),
		}))
	}

	/**
	 * What the events of `stream` carry, none of it read from a packed event: each name with how
	 * many events have it, and each key of their fields and contexts with the kinds of value it
	 * holds and how many events carry it. A stream with no events carries nothing.
	 */
	async fields(stream: string): Promise<StreamFields> {
		const answer = await this.ask('records.fields', () => RecordsFieldsAsked.encode({ stream }))
		const carried = RecordsStreamFields.decode(answer)
		const info = (field: (typeof carried.fields & {})[number]): FieldInfo => ({
			name: field.name ?? '',
			kinds: (field.kinds ?? []) as ValueKind[],
			count: field.count ?? 0,
		})
		return {
			eventNames: (carried.eventNames ?? []).map(name => ({
				name: name.name ?? '',
				count: name.count ?? 0,
			})),
			fields: (carried.fields ?? []).map(info),
			context: (carried.context ?? []).map(info),
		}
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
		body: (handle: number | undefined, databases: readonly number[]) => Uint8Array,
		databases: readonly Home[] = [],
	): Promise<Uint8Array> {
		refuseStatements(this.#link, databases)
		return this.#link.run('read', async connection => {
			const opened = await Promise.all(databases.map(database => database.handle(connection)))
			return connection.session.call(methods[method], body(undefined, opened))
		})
	}

	/**
	 * What reads met that no longer reads, which `drop` removes: each head
	 * row and segment with its stream and its events' times. What the store
	 * has not met since it opened is not listed.
	 */
	async damaged(): Promise<Damage[]> {
		const answer = await this.ask('records.damaged', () => Empty.encode({}))
		return (RecordsDamages.decode(answer).damages ?? []).map(damage => ({
			part: damage.part === 'segment' ? 'segment' : 'head row',
			id: damage.id ?? 0,
			stream: damage.stream ?? '',
			first: dateOf(damage.first ?? 0n),
			last: dateOf(damage.last ?? 0n),
			error: damage.error ?? '',
		}))
	}

	/**
	 * Removes a head row or a segment that `damaged` listed, so that the
	 * reads over it work again; what still reads is refused.
	 *
	 *     for (const damage of await store.events().damaged()) await store.events().drop(damage)
	 */
	async drop(damage: Damage): Promise<void> {
		const body = RecordsDamage.encode({
			...damage,
			first: nanosOf(damage.first),
			last: nanosOf(damage.last),
		})
		await this.#link.run('write', connection =>
			connection.session.call(methods['records.drop'], body),
		)
	}

	protected opened(
		method: Method,
		body: (handle: number | undefined) => Uint8Array,
	): Promise<Stream> {
		return this.#link.run('read', connection =>
			streamOf(connection, methods[method], body(undefined), true),
		)
	}

	protected follow(stream: Stream): () => void {
		return this.#link.follow(stream)
	}
}

/** A writer's stream, and the bytes a piece of it carries. */
interface Opened {
	stream: Stream
	piece: number
}

/**
 * A writer of another program's output into a stream, as `events.lines()`
 * begins it: each line becomes an event, and a write never waits. It sends
 * whole lines, so that what it has no room for, `dropped` counts as lines.
 */
export class Lines {
	/** whole lines not sent yet */
	readonly #queued: Uint8Array[] = []
	#queuedBytes = 0
	/** a line not ended yet */
	#partial = new Uint8Array(0)
	/** the line being written is longer than an event holds: the rest of it goes too */
	#discarding = false
	#dropped = 0
	#ended = false
	#failure: Error | undefined
	#wake: (() => void) | undefined
	readonly #sent: Promise<void>

	/** Lines come from `events.lines()`. */
	constructor(open: () => Promise<Opened>) {
		this.#sent = this.#send(open).catch((err: unknown) => {
			this.#failure = err instanceof Error ? err : new Error(String(err))
			for (const bytes of this.#queued.splice(0)) {
				this.#dropped += linesIn(bytes)
			}
			this.#queuedBytes = 0
		})
	}

	/** The lines dropped: past what the writer holds, and, once it ended, by the store. */
	get dropped(): number {
		return this.#dropped
	}

	/** Takes a piece of output, cut anywhere; it never waits. */
	write(chunk: Uint8Array | string): void {
		if (this.#ended) {
			throw new ClosedError('lines written after their end')
		}
		let bytes = typeof chunk === 'string' ? encoder.encode(chunk) : chunk
		if (this.#discarding) {
			const end = bytes.indexOf(newline)
			if (end < 0) {
				return
			}
			this.#discarding = false
			bytes = bytes.subarray(end + 1)
		}
		const joined = concat(this.#partial, bytes)
		const cut = joined.lastIndexOf(newline) + 1
		this.#partial = joined.slice(cut)
		if (this.#partial.length > eventRoom) {
			this.#partial = new Uint8Array(0)
			this.#discarding = true
			this.#dropped++
		}
		this.#queue(joined.subarray(0, cut))
	}

	/**
	 * Ends the writer: what it holds goes, a last line without its newline
	 * too, and it returns once the store wrote them.
	 */
	async end(): Promise<void> {
		if (!this.#ended) {
			this.#ended = true
			this.#queue(this.#partial)
			this.#partial = new Uint8Array(0)
			this.#wake?.()
		}
		await this.#sent
		if (this.#failure !== undefined) {
			throw this.#failure
		}
	}

	/** Holds bytes for the server, or drops and counts their lines when it holds no room. */
	#queue(bytes: Uint8Array): void {
		if (bytes.length === 0) {
			return
		}
		if (this.#failure !== undefined || this.#queuedBytes + bytes.length > linesHeld) {
			this.#dropped += linesIn(bytes)
			return
		}
		this.#queued.push(bytes)
		this.#queuedBytes += bytes.length
		this.#wake?.()
	}

	/** Sends what is held as it comes, in pieces within the stream's credit, then the end. */
	async #send(open: () => Promise<Opened>): Promise<void> {
		const { stream, piece } = await open()
		for (;;) {
			const bytes = this.#queued.shift()
			if (bytes === undefined) {
				if (this.#ended) {
					break
				}
				await new Promise<void>(resolve => {
					this.#wake = resolve
				})
				this.#wake = undefined
				continue
			}
			this.#queuedBytes -= bytes.length
			for (let at = 0; at < bytes.length; at += piece) {
				await stream.send(RecordsPiece.encode({ bytes: bytes.subarray(at, at + piece) }), false)
			}
		}
		await stream.send(new Uint8Array(0), true)
		for (;;) {
			const event = await stream.next()
			if (event.end) {
				this.#dropped += RecordsDropped.decode(event.body).dropped ?? 0
				return
			}
		}
	}
}

/** A stream of the method, open once its RESPONSE came; `end` leaves the client nothing to send. */
async function streamOf(
	connection: Connection,
	method: number,
	body: Uint8Array,
	end: boolean,
): Promise<Stream> {
	const stream = await connection.session.open(method, body, end)
	await stream.next()
	return stream
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
	if (a.length === 0) {
		return b
	}
	const joined = new Uint8Array(a.length + b.length)
	joined.set(a)
	joined.set(b, a.length)
	return joined
}

/** The lines bytes hold: one a newline ends, and one more past the last. */
function linesIn(bytes: Uint8Array): number {
	let lines = bytes[bytes.length - 1] === newline ? 0 : 1
	for (const byte of bytes) {
		if (byte === newline) {
			lines++
		}
	}
	return lines
}

/** What a store's logger is told: a logger's options, how long its stream keeps, and the levels it keeps and prints. */
export interface StoreLoggerOptions extends Omit<LoggerOptions, 'level'> {
	/** how long the stream keeps its events, as store.events takes it */
	keep?: Duration
	/**
	 * the least level kept and printed, every level when absent, or each
	 * apart: `{ store: 'info', console: 'warn' }` keeps every line from info
	 * and prints warnings and errors. LOG_LEVEL wins over the level kept, and
	 * LOG_CONSOLE_LEVEL over the level printed
	 */
	level?: Level | Levels
}

/**
 * The least levels a store's logger keeps and prints, each part left out as
 * the option is: no store keeps every level, and no console prints what is
 * kept, following the config `log`'s changes of it.
 *
 * ```ts
 * { level: 'info' }                               // keeps info+, prints info+
 * { level: { store: 'info', console: 'warn' } }   // keeps info+, prints warn+
 * { level: { console: 'warn' } }                  // keeps every level, prints warn+
 * ```
 */
export interface Levels {
	/** the least level kept; LOG_LEVEL and the config `log` win over it */
	store?: Level
	/** the least level printed; LOG_CONSOLE_LEVEL wins over it */
	console?: Level
}

/**
 * A logger of `stream` that keeps its lines there through an append of their
 * own, and what it declares its config `log` with.
 */
export function storeLogger(
	link: Link,
	stream: string,
	options: StoreLoggerOptions = {},
): [Logger, LogConfig] {
	const { keep, ...told } = options
	const events = openEvents(link, stream, keep === undefined ? undefined : { keep })
	return newStoreLogger(lines => events.appendLines(lines), stream, told)
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
	if (event.id !== undefined && (typeof event.id !== 'string' || event.id === '')) {
		throw new InvalidError(`${named}: an id is a string of 1 to 128 bytes`)
	}
	return {
		name: event.name,
		id: event.id,
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

/** The fields of a query, which a call takes on top of its own. */
const queryFields: readonly string[] = [
	'since',
	'from',
	'to',
	'streams',
	'names',
	'minLevel',
	'traceId',
	'where',
	'search',
	'newest',
	'limit',
	'after',
]

/**
 * A query as the wire carries it; a condition given as undefined reads
 * nothing it was not asked, and a field no query has is refused by name,
 * since one left out would read every event.
 */
function queryOf(query: Query | undefined): Asked {
	const q = query ?? {}
	refuseUnknown('records query', q, queryFields)
	for (const [key, value] of Object.entries(q)) {
		if (value === undefined) {
			throw new InvalidError(`a query's ${key} given as undefined: leave it out to read without it`)
		}
	}
	const bound: BoundStatement[] = []
	const { where, conditions } = whereOf(q.where, bound)
	const asked: WireQuery = {
		since: q.since === undefined ? undefined : ms(q.since),
		from: q.from === undefined ? undefined : nanosOf(q.from),
		to: q.to === undefined ? undefined : nanosOf(q.to),
		streams: q.streams === undefined ? undefined : [...q.streams],
		names: q.names === undefined ? undefined : [...q.names],
		minLevel: q.minLevel === undefined ? undefined : levelNumber(q.minLevel, 'minLevel'),
		traceId: q.traceId === undefined ? undefined : idBytes(q.traceId, 16, 'traceId'),
		where,
		conditions,
		search: q.search,
		newest: q.newest,
		limit: q.limit,
		after: q.after,
	}
	return { asked, bound }
}

type WireQuery = Parameters<typeof RecordsQuery.encode>[0]

/**
 * Refuses a statement of another store's database: its database's handle
 * would be opened on this store's connection, and read this store's file of
 * the same name.
 */
function refuseStatements(link: Link, databases: readonly Home[]): void {
	for (const database of databases) {
		if (reachOf(database)?.link !== link) {
			throw new InvalidError(
				`${database.label}: a statement of another store's database, in a query of this one`,
			)
		}
	}
}

/** The database of each statement a query names, in their order. */
function databasesOf({ bound }: Asked): readonly Home[] {
	return bound.map(statement => statement.database)
}

/**
 * A query as the wire carries it, but for its handle and its statements'
 * databases, which are handles of the connection it goes on: `bound` are
 * the statements its conditions name by their place.
 */
interface Asked {
	asked: WireQuery
	bound: BoundStatement[]
}

/** A query with its handle and its statements, each with its database's handle on the connection. */
function queryOn(
	{ asked, bound }: Asked,
	handle: number | undefined,
	databases: readonly number[],
): WireQuery {
	if (bound.length === 0) {
		return { ...asked, handle }
	}
	const statements = bound.map((statement, at) => ({
		database: databases[at] ?? 0,
		text: statement.text,
		values: [...statement.values],
	}))
	return { ...asked, handle, statements }
}

type WireCondition = { values?: string[]; not?: boolean; prefix?: string; statement?: number }

/**
 * A where as the wire carries it: a plain value as its JSON in `where`, which
 * every server reads, and a list, `{ not }`, `{ prefix }` or a statement in
 * `conditions`, which a server older than them refuses rather than matching
 * nothing. A statement goes into `bound`, which the condition names by its
 * place.
 */
function whereOf(
	given: Record<string, unknown> | undefined,
	bound: BoundStatement[],
): {
	where?: Record<string, string> | undefined
	conditions?: Record<string, WireCondition> | undefined
} {
	const where: Record<string, string> = {}
	const conditions: Record<string, WireCondition> = {}
	for (const [key, value] of Object.entries(given ?? {})) {
		const named = `a query's where ${JSON.stringify(key)}`
		if (value === undefined) {
			throw new InvalidError(`${named} given as undefined`)
		}
		if (Array.isArray(value)) {
			conditions[key] = { values: value.map(item => jsonOf(item)) }
		} else if (value instanceof BoundStatement) {
			conditions[key] = { statement: bound.push(value) - 1 }
		} else if (value instanceof Sql) {
			throw new InvalidError(
				`${named}: a stream belongs to no database, so its statement says whose it is: db.statement\`…\``,
			)
		} else if (value !== null && typeof value === 'object' && !(value instanceof Date)) {
			conditions[key] = conditionOf(named, value as Record<string, unknown>, bound)
		} else {
			where[key] = jsonOf(value)
		}
	}
	return {
		where: Object.keys(where).length === 0 ? undefined : where,
		conditions: Object.keys(conditions).length === 0 ? undefined : conditions,
	}
}

function conditionOf(
	named: string,
	condition: Record<string, unknown>,
	bound: BoundStatement[],
): WireCondition {
	const keys = Object.keys(condition)
	if (keys.length === 1 && keys[0] === 'not' && condition.not instanceof BoundStatement) {
		return { statement: bound.push(condition.not) - 1, not: true }
	}
	if (keys.length === 1 && keys[0] === 'not') {
		const not = condition.not
		return { values: (Array.isArray(not) ? not : [not]).map(item => jsonOf(item)), not: true }
	}
	if (keys.length === 1 && keys[0] === 'prefix' && typeof condition.prefix === 'string') {
		return { prefix: condition.prefix }
	}
	throw new InvalidError(
		`${named}: a value, a list, { not } or { prefix }; an object as the value itself goes in a list, [{ … }]`,
	)
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
	if (read.id !== undefined) {
		event.id = read.id
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
