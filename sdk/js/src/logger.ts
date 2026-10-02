// The application's own log lines as records, as Go's slog handler and
// Python's logging.Handler keep them: a call never waits for the server. Lines
// wait in a bounded buffer and go every second, or once half of it waits; what
// does not fit, or a write that fails, is dropped and counted.

import { InvalidError } from './errors.ts'
import type { Fields, Level, RecordInput } from './records.ts'
import { currentTrace } from './trace.ts'

export interface LoggerOptions {
	/** the lines held between writes; past it a line is dropped and counted: 1024 */
	buffer?: number
	/** the least level a line is kept at; every level when absent */
	level?: Level
}

const levels = { debug: -4, info: 0, warn: 4, error: 8 } as const
const flushEvery = 1000

/** What a logger and the loggers its `with` makes share: one buffer, one timer, one count. */
interface Shared {
	append: (records: RecordInput[]) => Promise<void>
	stream: string
	most: number
	least: number
	waiting: RecordInput[]
	timer: ReturnType<typeof setInterval> | undefined
	writing: Promise<void> | undefined
	dropped: number
}

/** A logger of one stream, its lines appended through append. */
export function newLogger(
	append: (records: RecordInput[]) => Promise<void>,
	stream: string,
	options: LoggerOptions = {},
): Logger {
	if (stream === '') {
		throw new InvalidError('a logger of no stream')
	}
	const most = options.buffer ?? 1024
	if (!Number.isInteger(most) || most < 1) {
		throw new InvalidError(`a logger's buffer of ${most} lines`)
	}
	const least = options.level === undefined ? Number.NEGATIVE_INFINITY : levelOf(options.level)
	const shared = {
		append,
		stream,
		most,
		least,
		waiting: [],
		timer: undefined,
		writing: undefined,
		dropped: 0,
	}
	return new Logger(shared, [])
}

/**
 * A logger of one stream. `info`, `warn` and the rest return at once; a line
 * becomes a record named `log`, its message the body, the call's fields its
 * attributes and the fields of `with` its context, who is speaking.
 *
 * ```ts
 * const log = store.records.logger('api')
 * log.info('server started', { port: 3000 })
 * log.with({ requestId }).warn('slow request', { ms: 1200 })
 * log.event('user.created', { userId: 42 })   // a record named for the event
 * ```
 */
export class Logger {
	readonly #shared: Shared
	readonly #context: [string, unknown][]

	/** Loggers come from `store.records.logger(stream)` and their `with`. */
	constructor(shared: Shared, context: [string, unknown][]) {
		this.#shared = shared
		this.#context = context
	}

	/** The lines dropped since the logger began: a full buffer, or a write that failed. */
	get dropped(): number {
		return this.#shared.dropped
	}

	debug(message: string, attrs?: Fields): void {
		this.#log(levels.debug, message, attrs)
	}

	info(message: string, attrs?: Fields): void {
		this.#log(levels.info, message, attrs)
	}

	warn(message: string, attrs?: Fields): void {
		this.#log(levels.warn, message, attrs)
	}

	error(message: string, attrs?: Fields): void {
		this.#log(levels.error, message, attrs)
	}

	/** An event of the stream: a record named for it, with no level and no body. */
	event(name: string, attrs?: Fields): void {
		if (name === '') {
			throw new InvalidError('an event of no name')
		}
		this.#queue({ stream: this.#shared.stream, name, ...this.#fields(attrs) })
	}

	/** The logger of the same stream and buffer, its context these fields besides its own. */
	with(context: Fields): Logger {
		return new Logger(this.#shared, [...this.#context, ...pairsOf(context)])
	}

	/** Hands what the buffer holds to the server now, as the timer does every second. */
	async flush(): Promise<void> {
		const shared = this.#shared
		while (shared.writing !== undefined) {
			await shared.writing
		}
		if (shared.waiting.length === 0) {
			return
		}
		const batch = shared.waiting.splice(0)
		shared.writing = shared
			.append(batch)
			.catch(() => {
				shared.dropped += batch.length
			})
			.finally(() => {
				shared.writing = undefined
			})
		await shared.writing
	}

	/** Hands over what the buffer holds and stops the timer; a later line starts it again. */
	async stop(): Promise<void> {
		await this.flush()
		if (this.#shared.timer !== undefined) {
			clearInterval(this.#shared.timer)
			this.#shared.timer = undefined
		}
	}

	#log(level: number, message: string, attrs: Fields | undefined): void {
		if (level < this.#shared.least) {
			return
		}
		this.#queue({
			stream: this.#shared.stream,
			name: 'log',
			level,
			body: message,
			...this.#fields(attrs),
		})
	}

	#fields(attrs: Fields | undefined): Pick<RecordInput, 'context' | 'attrs'> {
		const fields: Pick<RecordInput, 'context' | 'attrs'> = {}
		if (this.#context.length > 0) {
			fields.context = this.#context
		}
		if (attrs !== undefined) {
			fields.attrs = pairsOf(attrs)
		}
		return fields
	}

	#queue(record: RecordInput): void {
		const shared = this.#shared
		if (shared.waiting.length >= shared.most) {
			shared.dropped++
			return
		}
		const carried = record.traceId === undefined ? currentTrace() : undefined
		shared.waiting.push(
			carried === undefined
				? { at: new Date(), ...record }
				: { at: new Date(), ...record, traceId: carried.traceId, spanId: carried.spanId },
		)
		if (shared.timer === undefined) {
			shared.timer = setInterval(() => void this.flush(), flushEvery)
			shared.timer.unref?.()
		}
		if (shared.waiting.length >= shared.most / 2 && shared.writing === undefined) {
			void this.flush()
		}
	}
}

function levelOf(level: Level): number {
	return typeof level === 'number' ? level : levels[level]
}

/** Fields as pairs, an Error's value its stack, which JSON would write as {}. */
function pairsOf(fields: Fields): [string, unknown][] {
	const pairs = Array.isArray(fields)
		? (fields as readonly (readonly [string, unknown])[])
		: Object.entries(fields as Record<string, unknown>)
	return pairs.map(([key, value]) => [
		key,
		value instanceof Error ? (value.stack ?? String(value)) : value,
	])
}
