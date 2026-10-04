// The application's own log lines, as Go's slog handler and Python's
// logging.Handler write them: each on the console as it is logged, and as a
// record when the logger is a store's. A call never waits for the server.
// Lines wait in a bounded buffer and go every second, or once half of it
// waits; what does not fit, or a write that fails, is dropped and counted,
// and said in TinyStore's own lines, since no call is waiting to be told.

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { DropNotice, FailureLog, type Say } from './background.ts'
import { FromEnv, parseDotenv } from './config.ts'
import {
	type ConsoleFormat,
	type ConsoleLine,
	type ConsoleTime,
	hideUrlPasswords,
	jsonLine,
	jsonOf,
	type Look,
	prettyLine,
	type Redactor,
	redacted,
	redactor,
	sourceKey,
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
	/** how a pretty line shows its time: 'clock' when absent */
	time?: ConsoleTime
	/** leaves the stream out of a pretty line; the record keeps it */
	hideStream?: boolean
	/** where the console lines go: process.stderr when absent */
	to?: ConsoleOut
	/**
	 * secrets whose fields are hidden, in the store and on the console, at any
	 * depth: a key hides its value when some of its words in a row, written
	 * together, are a name's, so 'api key' hides api_key and apiKey; `secrets`
	 * holds the usual ones
	 */
	redact?: readonly string[]
	/** leaves a URL's password in a value as it is, which is otherwise hidden */
	keepUrlPasswords?: boolean
	/** changes each field's value before it is hidden, kept and shown */
	replace?: (key: string, value: unknown) => unknown
	/**
	 * where LOG_LEVEL, LOG_FORMAT and LOG_TIME are read, which win over the
	 * options: `fromEnv('APP')` reads APP_LOG_LEVEL and the rest, and false
	 * reads nothing; the bare names when absent
	 */
	env?: FromEnv | false
	/** adds where each line was logged: source={"function":…,"file":…,"line":…} */
	source?: boolean
}

/** Where console lines go: process.stdout, a file's stream, or any other writer. */
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
	redact: Redactor | undefined
	replace: ((key: string, value: unknown) => unknown) | undefined
	source: boolean
	print: ((line: ConsoleLine) => void) | undefined
	waiting: ConsoleLine[]
	timer: ReturnType<typeof setInterval> | undefined
	writing: Promise<void> | undefined
	dropped: number
	/** undefined for a logger of the console alone, which writes nothing */
	failures: FailureLog | undefined
	drops: DropNotice | undefined
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
	const [read, ignored] = fromEnvironment(options)
	const shared: Shared = {
		append,
		stream,
		most,
		least: read.level === undefined ? Number.NEGATIVE_INFINITY : levelOf(read.level),
		redact: redactor(read.redact, read.keepUrlPasswords),
		replace: read.replace,
		source: read.source === true,
		print: printer(read, out),
		waiting: [],
		timer: undefined,
		writing: undefined,
		dropped: 0,
		failures: append === undefined ? undefined : new FailureLog(`records flush ${stream}`, sayOwn),
		drops: append === undefined ? undefined : new DropNotice(stream, most, sayOwn),
	}
	sayIgnored(shared.print, ignored)
	return new Logger(shared, [])
}

type Unread = [name: string, value: string, expected: string]

/**
 * LOG_LEVEL, LOG_FORMAT and LOG_TIME over the options, after the prefix env
 * names, as Go and Python read them; a console turned off stays off.
 */
function fromEnvironment(options: LoggerOptions): [LoggerOptions, Unread[]] {
	const read = { ...options }
	const ignored: Unread[] = []
	const env = options.env ?? new FromEnv('', [])
	if (env === false) {
		return [read, ignored]
	}
	const variables = environment(env)
	const name = (variable: string) => (env.prefix === '' ? variable : `${env.prefix}_${variable}`)
	const level = variables(name('LOG_LEVEL'))
	if (level !== undefined) {
		const known = level.toLowerCase() === 'warning' ? 'warn' : level.toLowerCase()
		if (Object.hasOwn(levels, known)) {
			read.level = known as keyof typeof levels
		} else {
			ignored.push([name('LOG_LEVEL'), level, 'debug, info, warn or error'])
		}
	}
	const format = variables(name('LOG_FORMAT'))
	if (format !== undefined) {
		const known = format.toLowerCase()
		if (known !== 'pretty' && known !== 'json' && known !== 'off') {
			ignored.push([name('LOG_FORMAT'), format, 'pretty, json or off'])
		} else if (read.console !== 'off') {
			read.console = known
		}
	}
	const time = variables(name('LOG_TIME'))
	if (time !== undefined) {
		const known = time.toLowerCase()
		if (known === 'clock' || known === 'full' || known === 'off') {
			read.time = known
		} else {
			ignored.push([name('LOG_TIME'), time, 'clock, full or off'])
		}
	}
	return [read, ignored]
}

/** The variables of fromEnv's files, the process's over them, a missing file skipped; an empty one is none. */
function environment(env: FromEnv): (name: string) => string | undefined {
	const fromFiles: Record<string, string> = {}
	for (const file of env.files) {
		let text: string
		try {
			text = readFileSync(file, 'utf8')
		} catch {
			continue
		}
		Object.assign(fromFiles, parseDotenv(text))
	}
	return name => {
		const text = (process.env[name] ?? fromFiles[name])?.trim()
		return text === '' ? undefined : text
	}
}

/** The values said to be ignored, so that a program making many loggers says each once. */
const saidIgnored = new Set<string>()

function sayIgnored(print: Shared['print'], ignored: Unread[]): void {
	for (const [name, value, expected] of ignored) {
		if (print === undefined || saidIgnored.has(`${name}=${value}`)) {
			continue
		}
		saidIgnored.add(`${name}=${value}`)
		print({
			at: new Date(),
			stream: 'tinystore',
			level: levels.warn,
			msg: `${name} is ignored`,
			context: [],
			attrs: ['value', JSON.stringify(value), 'expected', JSON.stringify(expected)],
		})
	}
}

let own: Logger | undefined

/** TinyStore's own lines, on the console of a logger of stream tinystore made when first needed. */
export const sayOwn: Say = (level, message, fields) => {
	own ??= logger('tinystore')
	own[level](message, fields)
}

function forceColor(): boolean {
	const force = process.env.FORCE_COLOR
	return force !== undefined && force !== '' && force !== '0'
}

/** How a logger prints its lines, or undefined when it prints none. */
function printer(options: LoggerOptions, out?: ConsoleOut & { utc?: boolean }): Shared['print'] {
	if (options.console === 'off') {
		return undefined
	}
	const stream: ConsoleOut = out ?? options.to ?? process.stderr
	// FORCE_COLOR other than 0 says a person reads a pipe, as an IDE's run console is one
	const forced = out === undefined && forceColor()
	const terminal = stream.isTTY === true
	const pretty =
		options.console === 'pretty' || (options.console === undefined && (terminal || forced))
	if (!pretty) {
		return line => void stream.write(jsonLine(line))
	}
	const color =
		(forced || (terminal && out === undefined)) &&
		!process.env.NO_COLOR &&
		process.env.TERM !== 'dumb'
	const look: Look = {
		color,
		utc: out?.utc === true,
		time: options.time,
		hideStream: options.hideStream,
	}
	return line => void stream.write(prettyLine(line, look))
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

	/**
	 * The lines dropped since the logger began: a full buffer, or a write that
	 * failed. Each is also said on stderr, at most once in ten minutes.
	 */
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
			...encodeFields(context, this.#shared.redact, this.#shared.replace),
		])
	}

	/** Hands what the buffer holds to the server now, as the timer does every second. */
	async flush(): Promise<void> {
		const shared = this.#shared
		while (shared.writing !== undefined) {
			await shared.writing
		}
		shared.drops?.sayIfDue()
		if (shared.waiting.length === 0 || shared.append === undefined) {
			return
		}
		shared.writing = writeWaiting(shared, shared.append).finally(() => {
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
		const shared = this.#shared
		const encoded = attrs === undefined ? [] : encodeFields(attrs, shared.redact, shared.replace)
		const line: ConsoleLine = {
			at: new Date(),
			stream: shared.stream,
			...what,
			context: this.#context,
			attrs: shared.source ? [...encoded, sourceKey, sourceOfCaller()] : encoded,
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
			shared.drops?.dropped()
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
 * Writes what the buffer holds, and again while half of it filled during the
 * write before. A full buffer drops a line before the line could ask for a
 * write, so a burst that filled it while a write ran would otherwise wait for
 * the timer's next second, every line until then dropped.
 */
async function writeWaiting(
	shared: Shared,
	append: (lines: ConsoleLine[]) => Promise<void>,
): Promise<void> {
	let batch = shared.waiting.splice(0)
	while (batch.length > 0) {
		try {
			await append(batch)
			shared.failures?.succeeded()
		} catch (err) {
			shared.dropped += batch.length
			shared.failures?.failed(err)
		}
		batch = shared.waiting.length >= shared.most / 2 ? shared.waiting.splice(0) : []
	}
}

/**
 * Fields as key, JSON pairs, as a console writes them and the store keeps
 * them: an Error's value its stack, which JSON would write as {}, a secret's
 * value hidden at any depth, and a URL's password unless kept.
 */
export function encodeFields(
	fields: Fields,
	redact: Redactor | undefined,
	replace?: (key: string, value: unknown) => unknown,
): string[] {
	const pairs = Array.isArray(fields)
		? (fields as readonly (readonly [string, unknown])[])
		: Object.entries(fields as Record<string, unknown>)
	const flat: string[] = []
	for (const [key, given] of pairs) {
		const value = replace === undefined ? given : replace(key, given)
		const shown = value instanceof Error ? (value.stack ?? String(value)) : value
		if (redact?.hides(key)) {
			flat.push(key, redacted)
			continue
		}
		const json = jsonOf(shown, redact?.hides)
		flat.push(key, redact?.urls === true ? hideUrlPasswords(json) : json)
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

/** This file, whose frames a line's source skips. */
const here = pathOf(import.meta.url)

/**
 * Where a line was logged, as slog's handlers spell a source: the first frame
 * of the stack outside this file.
 */
function sourceOfCaller(): string {
	for (const frame of (new Error().stack ?? '').split('\n').slice(1)) {
		const found = /at (?:(.+?) \()?(.+):(\d+):\d+\)?$/.exec(frame.trim())
		if (found === null || pathOf(found[2] as string) === here) {
			continue
		}
		return JSON.stringify({
			function: found[1] ?? '',
			file: pathOf(found[2] as string),
			line: Number(found[3]),
		})
	}
	return '{}'
}

/** A frame's file as a path, which Node writes as a file: URL. */
function pathOf(file: string): string {
	return file.startsWith('file://') ? fileURLToPath(file) : file
}
