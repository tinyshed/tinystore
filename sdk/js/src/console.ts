// A logger's console line, as Rust's console and Python's handler write it,
// byte for byte the lines of testdata/console/console.json: pretty for a
// person at a terminal, one JSON object a line for a collector.
//
//   11:02:11.123 WARN  api  slow request  requestId=7f3a ms=1200
//   {"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request","requestId":"7f3a","ms":1200}

/** How a logger writes its lines as they are logged; without one, pretty on a terminal and JSON otherwise. */
export type ConsoleFormat = 'pretty' | 'json' | 'off'

/** How a pretty line shows its time: `11:02:11.123`, `2026-10-02 11:02:11.123 +03:00` or none. */
export type ConsoleTime = 'clock' | 'full' | 'off'

export interface Look {
	color: boolean
	/** a test's, whose lines do not depend on the machine's zone */
	utc?: boolean
	time?: ConsoleTime | undefined
	hideStream?: boolean | undefined
}

/** A line as a console writes it and the store keeps it: its fields already JSON. */
export interface ConsoleLine {
	at: Date
	stream: string
	/** absent for an event */
	level?: number
	/** an event's name; a log line has none */
	event?: string
	msg?: string
	/** key, JSON, key, JSON…, as the wire carries fields */
	context: readonly string[]
	attrs: readonly string[]
	/** 32 hex digits */
	traceId?: string
	/** 16 hex digits */
	spanId?: string
}

/**
 * A line as one JSON object and a newline, its keys in this order: time,
 * level, stream, event, msg, the context's fields, the attributes', the trace
 * and span. The time is UTC to the millisecond; a key repeated stays repeated.
 */
export function jsonLine(line: ConsoleLine): string {
	let out = `{"time":"${line.at.toISOString()}"`
	if (line.level !== undefined) {
		out += `,"level":${JSON.stringify(levelLabel(line.level))}`
	}
	out += `,"stream":${JSON.stringify(line.stream)}`
	if (line.event !== undefined) {
		out += `,"event":${JSON.stringify(line.event)}`
	}
	if (line.msg !== undefined) {
		out += `,"msg":${JSON.stringify(line.msg)}`
	}
	for (const fields of [line.context, line.attrs]) {
		for (let i = 0; i < fields.length; i += 2) {
			out += `,${JSON.stringify(fields[i])}:${fields[i + 1]}`
		}
	}
	if (line.traceId !== undefined) {
		out += `,"trace_id":"${line.traceId}"`
	}
	if (line.spanId !== undefined) {
		out += `,"span_id":"${line.spanId}"`
	}
	return `${out}}\n`
}

// the terminal's own sixteen colours, so that its theme chooses the shades
const reset = '\x1b[0m'
const dim = '\x1b[2m'
const red = '\x1b[31m'
const green = '\x1b[32m'
const yellow = '\x1b[33m'
const blue = '\x1b[34m'
const magenta = '\x1b[35m'
const cyan = '\x1b[36m'

/**
 * A line as a person reads it: the time of day, the level padded to five, the
 * stream, the message, the context's fields and the attributes', and the
 * trace's first eight digits. A message's further lines, and then a value of
 * several lines, a stack or a traceback, follow the line, indented, so that
 * none of them starts where a line does. A key or a string shows without its
 * quotes when it is one word with no '=', quote or backslash. What a program
 * logged shows no control character but a tab: the message, the event's name,
 * a value below the line and a source each show it as its escape.
 */
export function prettyLine(line: ConsoleLine, look: Look): string {
	const paint = (hue: string, text: string) => (look.color ? hue + text + reset : text)
	const [label, hue] = prettyLevel(line.level)
	let out = look.time === 'off' ? '' : `${paint(dim, stamp(line.at, look))} `
	out += paint(hue, label)
	// the level is padded to five; the columns after it are two spaces apart
	let separator = `${' '.repeat(Math.max(0, 5 - label.length))} `
	if (look.hideStream !== true) {
		out += `${separator}${paint(cyan, line.stream)}`
		separator = '  '
	}
	const [message, further] = prettyMessage(line)
	if (message !== '') {
		out += `${separator}${message}`
		separator = '  '
	}
	const field = (key: string, text: string) => {
		out += `${separator}${paint(dim, `${bare(key) ? key : JSON.stringify(key)}=`)}${text}`
		separator = ' '
	}
	const below: [string, string][] = []
	for (const fields of [line.context, line.attrs]) {
		for (let i = 0; i < fields.length; i += 2) {
			const [shown, lines] = prettyValue(fields[i] as string, fields[i + 1] as string)
			if (lines) {
				below.push([fields[i] as string, shown])
			} else {
				field(fields[i] as string, shown)
			}
		}
	}
	if (line.traceId !== undefined) {
		field('trace', line.traceId.slice(0, 8))
	}
	for (const part of further) {
		out += `\n    ${escaped(part)}`
	}
	for (const [key, text] of below) {
		linesOf(text).forEach((part, i) => {
			out += `\n    ${paint(dim, escaped(i === 0 ? `${key}: ${part}` : part))}`
		})
	}
	return `${out}\n`
}

/**
 * The lines of a text of several, without the line ends after the last, and
 * each without the carriage return before its newline: "line one\r\nline
 * two\r\n" is line one, line two.
 */
function linesOf(text: string): string[] {
	const lines = text.replace(/[\r\n]+$/, '').split('\n')
	return lines.map(part => part.replace(/\r$/, ''))
}

/**
 * Text a program logged as a pretty line shows it: each control character but
 * a tab, and DEL, as its JSON escape, so that none reaches the terminal as an
 * escape sequence, a carriage return or a line of its own. The colours the
 * console paints with are its own, never in this text.
 *
 *   "\x1b[2J" → \u001b[2J    "a\rb" → a\rb    "\x7f" → \u007f
 */
function escaped(text: string): string {
	let out = ''
	for (const c of text) {
		const code = c.codePointAt(0) as number
		if ((code < 0x20 && c !== '\t') || code === 0x7f) {
			out += code === 0x7f ? '\\u007f' : JSON.stringify(c).slice(1, -1)
		} else {
			out += c
		}
	}
	return out
}

/** A level as slog spells it: INFO, WARN+2, DEBUG-4. */
export function levelLabel(level: number): string {
	const spell = (base: string, from: number) => {
		const off = level - from
		return off === 0 ? base : `${base}${off > 0 ? '+' : ''}${off}`
	}
	if (level < 0) {
		return spell('DEBUG', -4)
	}
	if (level < 4) {
		return spell('INFO', 0)
	}
	if (level < 8) {
		return spell('WARN', 4)
	}
	return spell('ERROR', 8)
}

function prettyLevel(level: number | undefined): [string, string] {
	if (level === undefined) {
		return ['EVENT', magenta]
	}
	const hue = level < 0 ? blue : level < 4 ? green : level < 8 ? yellow : red
	return [levelLabel(level), hue]
}

/**
 * A log line's message, or an event's name and the message it may carry, as
 * the line shows them; and the further lines of a message of several, which
 * go below the line as a value's do.
 */
function prettyMessage(line: ConsoleLine): [string, string[]] {
	const [first, ...further] = linesOf(line.msg ?? '')
	const body = escaped(first ?? '')
	if (line.event === undefined) {
		return [body, further]
	}
	const event = escaped(line.event)
	return [body === '' ? event : `${event}  ${body}`, further]
}

/** How a field's JSON shows, and whether it is text of several lines, which goes under the line. */
function prettyValue(key: string, json: string): [string, boolean] {
	if (key === sourceKey) {
		const place = shortSource(json)
		if (place !== undefined) {
			return [place, false]
		}
	}
	if (!json.startsWith('"')) {
		return [json, false]
	}
	const text: string = JSON.parse(json)
	if (text.includes('\n')) {
		return [text, true]
	}
	return [bare(text) ? text : json, false]
}

/** One word a person reads without its quotes: no character a space or below, DEL, '"', '=' or '\'. */
function bare(text: string): boolean {
	if (text === '') {
		return false
	}
	for (const c of text) {
		const code = c.codePointAt(0) as number
		if (code <= 0x20 || code === 0x7f || c === '"' || c === '=' || c === '\\') {
			return false
		}
	}
	return true
}

/** The time of day, or with the date and zone in full: 2026-10-02 11:02:11.123 +03:00. */
function stamp(at: Date, look: Look): string {
	const utc = look.utc === true
	const [year, month, day, hours, minutes, seconds, millis] = utc
		? [
				at.getUTCFullYear(),
				at.getUTCMonth() + 1,
				at.getUTCDate(),
				at.getUTCHours(),
				at.getUTCMinutes(),
				at.getUTCSeconds(),
				at.getUTCMilliseconds(),
			]
		: [
				at.getFullYear(),
				at.getMonth() + 1,
				at.getDate(),
				at.getHours(),
				at.getMinutes(),
				at.getSeconds(),
				at.getMilliseconds(),
			]
	const two = (n: number) => String(n).padStart(2, '0')
	const clock = `${two(hours)}:${two(minutes)}:${two(seconds)}.${String(millis).padStart(3, '0')}`
	if (look.time !== 'full') {
		return clock
	}
	const offset = utc ? 0 : -at.getTimezoneOffset()
	const zone = `${offset < 0 ? '-' : '+'}${two(Math.trunc(Math.abs(offset) / 60))}:${two(Math.abs(offset) % 60)}`
	return `${String(year).padStart(4, '0')}-${two(month)}-${two(day)} ${clock} ${zone}`
}

/** The field where a line says where it was logged, as slog's handlers spell it. */
export const sourceKey = 'source'

/**
 * A source as a pretty line shows it, its file's directory and name and its
 * line: {"function":"run","file":"/home/ann/app/server/main.ts","line":42} is
 * server/main.ts:42.
 */
function shortSource(json: string): string | undefined {
	if (!json.startsWith('{')) {
		return undefined
	}
	const source = JSON.parse(json) as { file?: unknown; line?: unknown }
	if (typeof source.file !== 'string' || source.file === '') {
		return undefined
	}
	const parts = source.file.split(/[\\/]+/).filter(part => part !== '')
	const line = typeof source.line === 'number' ? source.line : 0
	return escaped(`${parts.slice(-2).join('/')}:${line}`)
}

/** The names `redact` takes to hide the usual secrets, as Go's console.Secrets and Python's SECRETS. */
export const secrets: readonly string[] = Object.freeze([
	'password',
	'passwd',
	'passphrase',
	'secret',
	'token',
	'credential',
	'credentials',
	'authorization',
	'cookie',
	'api key',
	'private key',
	'secret key',
	'access key',
	'signing key',
	'encryption key',
	'connection string',
	'dsn',
])

/** What a logger hides: the keys its names name, and a URL's password unless it keeps them. */
export interface Redactor {
	hides(key: string): boolean
	urls: boolean
}

/**
 * Hides the values of fields whose keys name a secret: some of the key's
 * words in a row, written together, are a name's. DB_PASSWORD, PasswordHash,
 * api-key and APIKey split into words at '_', '-', '.', spaces and capitals,
 * the case ignored, so 'api key' hides api_key and apiKey, and 'token' hides
 * bot_token and not tokens_used.
 */
export function redactor(
	names: readonly string[] | undefined,
	keepUrlPasswords = false,
): Redactor | undefined {
	const joined = (names ?? []).map(name => words(name).join('')).filter(name => name !== '')
	if (joined.length === 0 && keepUrlPasswords) {
		return undefined
	}
	return {
		hides: key => joined.length > 0 && joined.some(name => joinsTo(words(key), name)),
		urls: !keepUrlPasswords,
	}
}

/**
 * A key's words as a person reads them, in lower case: split at anything but
 * a letter or a digit, and where a capital begins a word, as in passwordHash
 * and APIKey.
 */
export function words(key: string): string[] {
	const out: string[] = []
	const chars = [...key]
	let start = -1
	const upper = (c: string | undefined) => c !== undefined && /\p{Lu}/u.test(c)
	const lower = (c: string | undefined) => c !== undefined && /\p{Ll}/u.test(c)
	const digit = (c: string | undefined) => c !== undefined && /\p{N}/u.test(c)
	for (let i = 0; i < chars.length; i++) {
		const c = chars[i] as string
		if (!/[\p{L}\p{N}]/u.test(c)) {
			if (start >= 0) {
				out.push(chars.slice(start, i).join('').toLowerCase())
				start = -1
			}
			continue
		}
		const before = chars[i - 1]
		const begins =
			upper(c) && (lower(before) || digit(before) || (upper(before) && lower(chars[i + 1])))
		if (start >= 0 && begins) {
			out.push(chars.slice(start, i).join('').toLowerCase())
			start = i
		}
		if (start < 0) {
			start = i
		}
	}
	if (start >= 0) {
		out.push(chars.slice(start).join('').toLowerCase())
	}
	return out
}

function joinsTo(keyWords: string[], name: string): boolean {
	for (let i = 0; i < keyWords.length; i++) {
		let rest = name
		for (const word of keyWords.slice(i)) {
			if (!rest.startsWith(word)) {
				break
			}
			rest = rest.slice(word.length)
			if (rest === '') {
				return true
			}
		}
	}
	return false
}

/**
 * JSON text with the password of each URL inside it hidden, everything else
 * as it was spelled: "postgres://ann:hunter2@db/app" is
 * "postgres://ann:[redacted]@db/app".
 */
export function hideUrlPasswords(text: string): string {
	if (!text.includes('://') && !text.includes('\\')) {
		return text
	}
	return text.replace(/"(?:[^"\\]|\\.)*"/g, raw => {
		const value: string = JSON.parse(raw)
		const hidden = hideUrlTextPasswords(value)
		return hidden === value ? raw : JSON.stringify(hidden)
	})
}

function hideUrlTextPasswords(text: string): string {
	let out = ''
	let written = 0
	for (let from = 0; from < text.length; ) {
		const schemeEnd = text.indexOf('://', from)
		if (schemeEnd < 0) {
			break
		}
		const [password, at, end] = findPassword(text, schemeEnd + 3)
		from = end
		if (password < 0 || !/[A-Za-z][A-Za-z0-9+.-]*$/.test(text.slice(0, schemeEnd))) {
			continue
		}
		out += `${text.slice(written, password)}[redacted]`
		written = at
	}
	return written === 0 ? text : out + text.slice(written)
}

/** Where an authority's password begins and the '@' after it, -1 when it has none, and where it ends. */
function findPassword(text: string, start: number): [number, number, number] {
	let at = -1
	let colon = -1
	let i = start
	while (i < text.length) {
		const c = text[i] as string
		if (c === '/' || c === '?' || c === '#' || c <= ' ') {
			break
		}
		if (c === '@') {
			at = i
		} else if (c === ':' && colon < 0) {
			colon = i
		}
		i++
	}
	const end = Math.min(i, text.length)
	return at < 0 || colon < 0 || colon + 1 >= at ? [-1, -1, end] : [colon + 1, at, end]
}

/** What a hidden value becomes, in the store and on the console. */
export const redacted = '"[redacted]"'

/**
 * A value as JSON, its keys at any depth hidden as hides says; a bigint is its
 * digits, and what JSON cannot write is its text, so that a line is never lost
 * to one field.
 */
export function jsonOf(value: unknown, hides?: (key: string) => boolean): string {
	if (typeof value === 'bigint') {
		return value.toString()
	}
	try {
		const json = JSON.stringify(value, (key, inner) => {
			if (key !== '' && hides?.(key)) {
				return '[redacted]'
			}
			return typeof inner === 'bigint' ? inner.toString() : inner
		})
		if (json !== undefined) {
			return json
		}
	} catch {
		// a cycle, or a toJSON that throws: the value's text below
	}
	return JSON.stringify(String(value))
}
