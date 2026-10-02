// The application's logs and events in records.db, as Go's records package
// keeps them: one log of the store's whose calls name their streams, read by
// time and by what records hold, followed in the order they were sealed.

import { type Connection, download, type Link } from './connection.ts'
import { InvalidError } from './errors.ts'
import type { Page } from './handles.ts'
import { type Logger, type LoggerOptions, newLogger } from './logger.ts'
import type { Stream } from './session.ts'
import { type Duration, ms } from './time.ts'
import {
	Empty,
	methods,
	RecordsQuery as QueryMessage,
	RecordsBatch,
	RecordsCursor,
	RecordsDamage,
	RecordsDamages,
	RecordsPage,
	RecordsRecord,
	RecordsStream,
} from './wire/messages.ts'

/** slog's levels: -4 debug, 0 info, 4 warn, 8 error, or any other it may name. */
export type Level = 'debug' | 'info' | 'warn' | 'error' | number

const levels = { debug: -4, info: 0, warn: 4, error: 8 } as const

function levelOf(level: Level): number {
	return typeof level === 'number' ? level : levels[level]
}

/**
 * Fields: keys and values, each value written as JSON. An object's keys keep
 * their order; an array of pairs may repeat a key.
 */
export type Fields = Record<string, unknown> | readonly (readonly [string, unknown])[]

/** A record to append: when, where, what, and its fields. */
export interface RecordInput {
	/** its time: a Date, or unix nanoseconds as a bigint; now when absent */
	at?: Date | bigint
	/** the namespace the application names: 'web', 'billing' */
	stream: string
	/** the event: 'log' for a log line */
	name: string
	level?: Level
	/** absent is none, which an empty body is not */
	body?: string | Uint8Array
	/** sixteen bytes, or their 32 hex digits */
	traceId?: Uint8Array | string
	/** eight bytes, or their 16 hex digits */
	spanId?: Uint8Array | string
	/** who produced it */
	context?: Fields
	/** what happened */
	attrs?: Fields
}

/** A record as it was kept: a field's value is the JSON it was written as, spelled as it was given. */
export interface LogRecord {
	/** unix nanoseconds */
	at: bigint
	stream: string
	name: string
	level: number | undefined
	body: string | Uint8Array | undefined
	traceId: Uint8Array | undefined
	spanId: Uint8Array | undefined
	context: [key: string, json: string | Uint8Array][]
	attrs: [key: string, json: string | Uint8Array][]
}

/**
 * The records in a range that meet every condition given.
 *
 * ```ts
 * { since: '1h', minLevel: 'warn' }                 // the last hour's warnings
 * { since: '1h', search: 'connection reset' }       // whose text holds it, case ignored
 * { from: start, to: end, streams: ['api'] }
 * ```
 */
export interface RecordsQuery {
	/** the span before now the range covers: '1h', '15m', or milliseconds; or from and to */
	since?: Duration
	/** the range's start, included; open when absent */
	from?: Date | bigint
	/** the range's end, excluded; open when absent */
	to?: Date | bigint
	/** none or empty is every stream */
	streams?: string[]
	names?: string[]
	/** a record without a level does not match */
	minLevel?: Level
	traceId?: Uint8Array | string
	/** each a record must hold, by key and exact JSON spelling */
	attrs?: Fields
	context?: Fields
	/**
	 * text a record's body or name holds, its case ignored: every record the
	 * rest of the query leaves is read for it, so a search over much text ends
	 * pages early at the budget, and next goes on
	 */
	search?: string
	/** newest first; oldest first when absent */
	newest?: boolean
	/** the records a page holds: 1000, and 10000 at most */
	limit?: number
	/** what one read may open, fetch and decode; each narrows the server's */
	budget?: { blocks?: number; bytes?: number; records?: number }
	/** a page's next, which this page continues from; the rest of the query as it was */
	after?: string | undefined
}

/** Where a follower stands in the sealed segments: kept by the caller between follows. */
export interface Cursor {
	segment: number
	row: number
}

/** A row that no longer reads, which a drop removes. */
export interface Damage {
	stream: string
	/** a sealed segment, dropped whole; or a head row */
	segment: number | undefined
	headRow: number | undefined
	/** the times it held, unix nanoseconds */
	from: bigint
	to: bigint
	/** the invariant its bytes broke */
	reason: string
}

const nsPerMs = 1_000_000n

function nanos(t: Date | bigint): bigint {
	return typeof t === 'bigint' ? t : BigInt(t.getTime()) * nsPerMs
}

function idOf(
	id: Uint8Array | string | undefined,
	bytes: number,
	what: string,
): Uint8Array | undefined {
	if (id === undefined) {
		return undefined
	}
	const raw = typeof id === 'string' ? new Uint8Array(Buffer.from(id, 'hex')) : id
	if (raw.length !== bytes) {
		throw new InvalidError(`a ${what} id of ${raw.length} bytes, not ${bytes}`)
	}
	return raw
}

/** Fields as the wire carries them: one array of keys and JSON texts. */
function fieldsOf(fields: Fields | undefined): string[] | undefined {
	if (fields === undefined) {
		return undefined
	}
	const pairs = Array.isArray(fields) ? fields : Object.entries(fields)
	const flat: string[] = []
	for (const [key, value] of pairs) {
		const json = JSON.stringify(value)
		if (json === undefined) {
			throw new InvalidError(`the field ${key} holds a value JSON cannot write`)
		}
		flat.push(key, json)
	}
	return flat.length === 0 ? undefined : flat
}

const lossy = new TextDecoder()

function textOf(text: string | Uint8Array | undefined): string {
	return text === undefined ? '' : typeof text === 'string' ? text : lossy.decode(text)
}

function pairs(flat: (string | Uint8Array)[] | undefined): [string, string | Uint8Array][] {
	const out: [string, string | Uint8Array][] = []
	for (let i = 0; flat !== undefined && i + 1 < flat.length; i += 2) {
		out.push([textOf(flat[i]), flat[i + 1]!])
	}
	return out
}

function recordOf(r: ReturnType<typeof RecordsRecord.decode>): LogRecord {
	return {
		at: r.at ?? 0n,
		stream: textOf(r.stream),
		name: textOf(r.name),
		level: r.level,
		body: r.body,
		traceId: r.traceId,
		spanId: r.spanId,
		context: pairs(r.context),
		attrs: pairs(r.attrs),
	}
}

/**
 * A page's next: where the range moved, both ends as nanoseconds, so that a
 * query over the last `since` continues the range it started with.
 *
 *     from 1700000000000000000, open end   →   '1700000000000000000:'
 */
function cursorOf(from: bigint | undefined, to: bigint | undefined): string {
	return `${from ?? ''}:${to ?? ''}`
}

function rangeOf(q: RecordsQuery | undefined): {
	from: bigint | undefined
	to: bigint | undefined
} {
	if (q?.after !== undefined) {
		const ends = /^(-?\d*):(-?\d*)$/.exec(q.after)
		if (ends === null) {
			throw new InvalidError(`${JSON.stringify(q.after)} is no page's next`)
		}
		return {
			from: ends[1] === '' ? undefined : BigInt(ends[1]!),
			to: ends[2] === '' ? undefined : BigInt(ends[2]!),
		}
	}
	if (q?.since !== undefined && q.from !== undefined) {
		throw new InvalidError('a range starts since a span before now or from a time, not both')
	}
	const from =
		q?.since !== undefined
			? nanos(new Date(Date.now() - ms(q.since)))
			: q?.from === undefined
				? undefined
				: nanos(q.from)
	return { from, to: q?.to === undefined ? undefined : nanos(q.to) }
}

function queryOf(q: RecordsQuery | undefined): Parameters<typeof QueryMessage.encode>[0] {
	return {
		...rangeOf(q),
		streams: q?.streams !== undefined && q.streams.length > 0 ? q.streams : undefined,
		names: q?.names !== undefined && q.names.length > 0 ? q.names : undefined,
		minLevel: q?.minLevel === undefined ? undefined : levelOf(q.minLevel),
		traceId: idOf(q?.traceId, 16, 'trace'),
		attrs: fieldsOf(q?.attrs),
		context: fieldsOf(q?.context),
		newest: q?.newest === true ? true : undefined,
		limit: q?.limit,
		budgetBlocks: q?.budget?.blocks,
		budgetBytes: q?.budget?.bytes,
		budgetRecords: q?.budget?.records,
		search: q?.search === '' ? undefined : q?.search,
	}
}

export class Records {
	readonly #link: Link
	readonly #loggers: Logger[] = []

	constructor(link: Link) {
		this.#link = link
	}

	/**
	 * A logger of a stream: its calls return at once, and its lines reach the
	 * server every second, as Go's slog handler's and Python's logging.Handler's do.
	 */
	logger(stream: string, options?: LoggerOptions): Logger {
		const logger = newLogger(records => this.append(records), stream, options)
		this.#loggers.push(logger)
		return logger
	}

	/** Hands over what every logger holds, as the store does when it closes. */
	async stop(): Promise<void> {
		await Promise.all(this.#loggers.map(logger => logger.stop()))
	}

	/**
	 * Appends records in one transaction, all or none; a refused one names
	 * itself as `call`, with its stream and name. A read sees them at once.
	 */
	async append(records: RecordInput | readonly RecordInput[]): Promise<void> {
		const now = BigInt(Date.now()) * nsPerMs
		const batch = (Array.isArray(records) ? records : [records as RecordInput]).map(r => ({
			at: r.at === undefined ? now : nanos(r.at),
			stream: r.stream,
			name: r.name,
			level: r.level === undefined ? undefined : levelOf(r.level),
			body: r.body,
			traceId: idOf(r.traceId, 16, 'trace'),
			spanId: idOf(r.spanId, 8, 'span'),
			context: fieldsOf(r.context),
			attrs: fieldsOf(r.attrs),
		}))
		await this.#link.run('write', connection =>
			connection.session.call(methods['records.append'], RecordsBatch.encode({ records: batch })),
		)
	}

	/**
	 * One page of the records a query matches, from one snapshot, in event-time
	 * order; next, while a limit or a budget ended the page early, is passed back
	 * as `after` with the same query. A page never splits a timestamp.
	 */
	async scan(query?: RecordsQuery): Promise<Page<LogRecord, string>> {
		const asked = queryOf(query)
		const got = await this.#link.run('read', connection =>
			download(connection, methods['records.read'], QueryMessage.encode(asked)),
		)
		const page = RecordsPage.decode(got.trailer)
		const items = got.items.map(item => recordOf(RecordsRecord.decode(item)))
		if (page.more !== true) {
			return { items, next: undefined }
		}
		const moved = (end: number | bigint | undefined, sent: number | bigint | undefined) =>
			end !== undefined ? BigInt(end) : sent !== undefined ? BigInt(sent) : undefined
		return { items, next: cursorOf(moved(page.from, asked.from), moved(page.to, asked.to)) }
	}

	/** Every record a query matches, a page at a time. */
	async *all(query?: Omit<RecordsQuery, 'after'>): AsyncGenerator<LogRecord> {
		let page = await this.scan(query)
		yield* page.items
		while (page.next !== undefined) {
			page = await this.scan({ ...query, after: page.next })
			yield* page.items
		}
	}

	/**
	 * The sealed records after a cursor, in the order they were sealed; the
	 * cursor to follow from next, and the segments retention removed before
	 * the cursor reached them. A record reaches a follower once it is sealed.
	 */
	async follow(
		cursor?: Cursor,
		limit?: number,
	): Promise<{ records: LogRecord[]; cursor: Cursor; expired: number }> {
		const got = await this.#link.run('read', connection =>
			download(
				connection,
				methods['records.follow'],
				RecordsCursor.encode({ segment: cursor?.segment, row: cursor?.row, limit }),
			),
		)
		const trailer = RecordsCursor.decode(got.trailer)
		return {
			records: got.items.map(item => recordOf(RecordsRecord.decode(item))),
			cursor: { segment: trailer.segment ?? 0, row: trailer.row ?? 0 },
			expired: trailer.expired ?? 0,
		}
	}

	/**
	 * A writer of another program's output, pino's lines or a child's stdout,
	 * that never waits: each line becomes a record of the stream as the server's
	 * Lines makes it, a stack trace's lines joined and a JSON or logfmt line's
	 * fields kept. What does not fit its buffer is dropped and counted.
	 */
	lines(stream: string, options?: { buffer?: number }): LinesWriter {
		if (stream === '') {
			throw new InvalidError('lines of no stream')
		}
		return new LinesWriter(this.#link, stream, options?.buffer ?? 1 << 20)
	}

	/** The rows the server's records have met that no longer read. */
	async damaged(): Promise<Damage[]> {
		const body = await this.#link.run('read', connection =>
			connection.session.call(methods['records.damaged'], Empty.encode({})),
		)
		return (RecordsDamages.decode(body).damages ?? []).map(d => ({
			stream: textOf(d.stream),
			segment: d.segment,
			headRow: d.headRow,
			from: d.from ?? 0n,
			to: d.to ?? 0n,
			reason: d.reason ?? '',
		}))
	}

	/** Removes a damaged row, a repair an admin connection alone may make. */
	async drop(damage: Damage): Promise<void> {
		await this.#link.run('write', connection =>
			connection.session.call(methods['records.drop'], RecordsDamage.encode(damage)),
		)
	}
}

/** The most one DATA of lines carries. */
const linesChunk = 64 << 10

/**
 * An upload of lines that stays open while it is written: write never waits,
 * holding what the stream's credit does not let go yet up to its buffer and
 * dropping past it. A connection lost drops what it held and the next write
 * uploads on the next connection.
 */
export class LinesWriter {
	readonly #link: Link
	readonly #stream: string
	readonly #most: number
	#waiting: Uint8Array[] = []
	#held = 0
	#pumping: Promise<void> | undefined
	#upload: Stream | undefined
	#ended = false
	/** bytes dropped since the writer began: a full buffer, or a lost connection */
	dropped = 0

	constructor(link: Link, stream: string, most: number) {
		this.#link = link
		this.#stream = stream
		this.#most = most
	}

	/** Takes a piece of output, cut anywhere. It returns at once. */
	write(chunk: string | Uint8Array): boolean {
		if (this.#ended) {
			throw new InvalidError('a write after the lines ended')
		}
		const bytes = typeof chunk === 'string' ? new TextEncoder().encode(chunk) : chunk
		if (bytes.length === 0) {
			return true
		}
		if (this.#held + bytes.length > this.#most) {
			this.dropped += bytes.length
			return false
		}
		for (let at = 0; at < bytes.length; at += linesChunk) {
			this.#waiting.push(bytes.subarray(at, at + linesChunk))
		}
		this.#held += bytes.length
		this.#pumping ??= this.#pump().finally(() => {
			this.#pumping = undefined
		})
		return true
	}

	async #pump(): Promise<void> {
		while (this.#waiting.length > 0) {
			const piece = this.#waiting[0]!
			try {
				const upload = await this.#open()
				await upload.send(piece, false)
			} catch {
				this.#upload = undefined
				this.dropped += this.#held
				this.#waiting = []
				this.#held = 0
				return
			}
			this.#waiting.shift()
			this.#held -= piece.length
		}
	}

	async #open(): Promise<Stream> {
		if (this.#upload !== undefined && !this.#upload.finished) {
			return this.#upload
		}
		const connection: Connection = await this.#link.connection()
		this.#upload = await connection.session.open(
			methods['records.lines'],
			RecordsStream.encode({ stream: this.#stream }),
			false,
		)
		return this.#upload
	}

	/** Hands over what the writer holds, a line cut short included, and ends the upload. */
	async end(): Promise<void> {
		this.#ended = true
		await this.#pumping
		const upload = this.#upload
		if (upload === undefined || upload.finished) {
			return
		}
		await upload.send(new Uint8Array(0), true)
		await upload.next()
	}
}
