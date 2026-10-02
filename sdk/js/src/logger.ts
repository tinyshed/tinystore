// The application's own log lines, as Go's slog handler and Python's
// logging.Handler write them: each on the console as it is logged, and as a
// record when the logger is a store's. A call never waits for the server.
// Lines wait in a bounded buffer and go every second, or once half of it
// waits; what does not fit, or a write that fails, is dropped and counted.

import {
	type ConsoleFormat,
	type ConsoleLine,
	jsonLine,
	jsonOf,
	prettyLine,
	redacted,
	redactor,
} from './console.ts'
import { InvalidError } from './errors.ts'
import type { Fields, Level } from './records.ts'
import { currentTrace, type Trace } from './trace.ts'

export interface LoggerOptions {
	/** the lines held between writes; past it a line is dropped and counted: 1024 */
	buffer?: number
	/** the least level a line is kept at; every level when absent */
	level?: Level
	/** how a line is written as it is logged: pretty on a terminal and JSON otherwise when absent */
	console?: ConsoleFormat
	/** fields whose values are hidden, in the store and on the console, at any depth, the case ignored */
	redact?: readonly string[]
	/** the console lines go to stdout rather than stderr */
	stdout?: boolean
}

/** Where a console line goes: a process's stream, or a test's. */
export interface ConsoleOut {
	write(text: string): unknown
	readonly isTTY?: boolean
}

const levels = { debug: -4, info: 0, warn: 4, error: 8 } as const
const flushEvery = 1000

/** What a logger and the loggers its `with` makes share: one buffer, one timer, one count, one console. */
interface Shared {
	/** undefined for a logger of the console alone */
	append: ((lines: ConsoleLine[]) => Promise<void>) | undefined
	stream: string
	most: number
	least: number
	hides: ((key: string) => boolean) | undefined
	print: ((line: ConsoleLine) => void) | undefined
	waiting: ConsoleLine[]
	timer: ReturnType<typeof setInterval> | undefined
	writing: Promise<void> | undefined
	dropped: number
}

/**
 * A logger of the console alone, for a program that wants the logger and not
 * the records: its lines are written as a store's logger writes them, and
 * `store.records.logger(stream)` in its place keeps them too.
 *
 * ```ts
 * const log = logger('app', { redact: ['password'] })
 * const db = log.with({ module: 'db' })   // passed to what needs it
 * ```
 */
export function logger(stream: string, options?: LoggerOptions): Logger {
	return newLogger(undefined, stream, options)
}

/** A logger of one stream, its lines appended through append when it is a store's. */
export function newLogger(
	append: ((lines: ConsoleLine[]) => Promise<void>) | undefined,
	stream: string,
	options: LoggerOptions = {},
	out?: ConsoleOut & { utc?: boolean },
): Logger {
	if (stream === '') {
		throw new InvalidError('a logger of no stream')
	}
	const most = options.buffer ?? 1024
	if (!Number.isInteger(most) || most < 1) {
		throw new InvalidError(`a logger's buffer of ${most} lines`)
	}
	const shared: Shared = {
		append,
		stream,
		most,
		least: options.level === undefined ? Number.NEGATIVE_INFINITY : levelOf(options.level),
		hides: redactor(options.redact),
		print: printer(options, out),
		waiting: [],
		timer: undefined,
		writing: undefined,
		dropped: 0,
	}
	return new Logger(shared, [])
}

/** How a logger prints its lines, or undefined when it prints none. */
function printer(options: LoggerOptions, out?: ConsoleOut & { utc?: boolean }): Shared['print'] {
	if (options.console === 'off') {
		return undefined
	}
	const stream: ConsoleOut = out ?? (options.stdout === true ? process.stdout : process.stderr)
	const terminal = stream.isTTY === true
	const pretty = options.console === 'pretty' || (options.console === undefined && terminal)
	if (!pretty) {
		return line => void stream.write(jsonLine(line))
	}
	const color =
		terminal && out === undefined && !process.env.NO_COLOR && process.env.TERM !== 'dumb'
	return line => void stream.write(prettyLine(line, color, out?.utc === true))
}

/**
 * A logger of one stream. `info`, `warn` and the rest return at once; a line
 * goes to the console as it is logged, and becomes a record named `log`, its
 * message the body, the call's fields its attributes and the fields of `with`
 * its context, who is speaking.
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
	/** key, JSON, key, JSON…, hidden as the logger's redact says */
	readonly #context: string[]

	/** Loggers come from `logger(stream)`, `store.records.logger(stream)` and their `with`. */
	constructor(shared: Shared, context: string[]) {
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
		this.#emit({ event: name }, attrs)
	}

	/** The logger of the same stream, buffer and console, its context these fields besides its own. */
	with(context: Fields): Logger {
		return new Logger(this.#shared, [
			...this.#context,
			...encodeFields(context, this.#shared.hides),
		])
	}

	/** Hands what the buffer holds to the server now, as the timer does every second. */
	async flush(): Promise<void> {
		const shared = this.#shared
		while (shared.writing !== undefined) {
			await shared.writing
		}
		if (shared.waiting.length === 0 || shared.append === undefined) {
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
		this.#emit({ level, msg: message }, attrs)
	}

	#emit(what: Pick<ConsoleLine, 'level' | 'event' | 'msg'>, attrs: Fields | undefined): void {
		const trace = traceOf(currentTrace())
		const line: ConsoleLine = {
			at: new Date(),
			stream: this.#shared.stream,
			...what,
			context: this.#context,
			attrs: attrs === undefined ? [] : encodeFields(attrs, this.#shared.hides),
			...trace,
		}
		this.#shared.print?.(line)
		this.#queue(line)
	}

	#queue(line: ConsoleLine): void {
		const shared = this.#shared
		if (shared.append === undefined) {
			return
		}
		if (shared.waiting.length >= shared.most) {
			shared.dropped++
			return
		}
		shared.waiting.push(line)
		if (shared.timer === undefined) {
			shared.timer = setInterval(() => void this.flush(), flushEvery)
			shared.timer.unref?.()
		}
		if (shared.waiting.length >= shared.most / 2 && shared.writing === undefined) {
			void this.flush()
		}
	}
}

/**
 * Fields as key, JSON pairs, as a console writes them and the store keeps
 * them: an Error's value its stack, which JSON would write as {}, and a field
 * hides names hidden at any depth.
 */
export function encodeFields(
	fields: Fields,
	hides: ((key: string) => boolean) | undefined,
): string[] {
	const pairs = Array.isArray(fields)
		? (fields as readonly (readonly [string, unknown])[])
		: Object.entries(fields as Record<string, unknown>)
	const flat: string[] = []
	for (const [key, value] of pairs) {
		const shown = value instanceof Error ? (value.stack ?? String(value)) : value
		flat.push(key, hides?.(key) ? redacted : jsonOf(shown, hides))
	}
	return flat
}

function levelOf(level: Level): number {
	return typeof level === 'number' ? level : levels[level]
}

/** A trace's ids as hex, or none when they are not a trace's sixteen bytes and a span's eight. */
function traceOf(trace: Trace | undefined): Pick<ConsoleLine, 'traceId' | 'spanId'> {
	if (trace === undefined) {
		return {}
	}
	const hex = (id: Uint8Array | string | undefined) =>
		typeof id === 'string'
			? id.toLowerCase()
			: id === undefined
				? undefined
				: Buffer.from(id).toString('hex')
	const traceId = hex(trace.traceId)
	const spanId = hex(trace.spanId)
	if (traceId === undefined || !/^[0-9a-f]{32}$/.test(traceId)) {
		return {}
	}
	return spanId !== undefined && /^[0-9a-f]{16}$/.test(spanId) ? { traceId, spanId } : { traceId }
}
