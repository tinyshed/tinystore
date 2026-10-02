// A logger's console line, as Go's records.Handler and Python's handler write
// it, byte for byte the lines of records/testdata/console.json: pretty for a
// person at a terminal, one JSON object a line for a collector.
//
//   11:02:11.123 WARN  api  slow request  requestId=7f3a ms=1200
//   {"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request","requestId":"7f3a","ms":1200}

/** How a logger writes its lines as they are logged; without one, pretty on a terminal and JSON otherwise. */
export type ConsoleFormat = 'pretty' | 'json' | 'off'

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
 * trace's first eight digits. A value of several lines, a stack or a
 * traceback, follows the line, indented. A key or a string shows without its
 * quotes when it is one word with no '=', quote or backslash.
 */
export function prettyLine(line: ConsoleLine, color: boolean, utc = false): string {
	const paint = (hue: string, text: string) => (color ? hue + text + reset : text)
	const [label, hue] = prettyLevel(line.level)
	let out = `${paint(dim, clock(line.at, utc))} ${paint(hue, label)}${' '.repeat(Math.max(0, 5 - label.length))}`
	out += ` ${paint(cyan, line.stream)}`
	const message = prettyMessage(line)
	if (message !== '') {
		out += `  ${message}`
	}
	let separator = '  '
	const field = (key: string, text: string) => {
		out += `${separator}${paint(dim, `${bare(key) ? key : JSON.stringify(key)}=`)}${text}`
		separator = ' '
	}
	const below: [string, string][] = []
	for (const fields of [line.context, line.attrs]) {
		for (let i = 0; i < fields.length; i += 2) {
			const [shown, lines] = prettyValue(fields[i + 1] as string)
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
	for (const [key, text] of below) {
		text
			.replace(/[\r\n]+$/, '')
			.split('\n')
			.forEach((part, i) => {
				const shown = part.replace(/\r$/, '')
				out += `\n    ${paint(dim, i === 0 ? `${key}: ${shown}` : shown)}`
			})
	}
	return `${out}\n`
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

function prettyMessage(line: ConsoleLine): string {
	const body = line.msg ?? ''
	if (line.event === undefined) {
		return body
	}
	return body === '' ? line.event : `${line.event}  ${body}`
}

/** How a field's JSON shows, and whether it is text of several lines, which goes under the line. */
function prettyValue(json: string): [string, boolean] {
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

function clock(at: Date, utc: boolean): string {
	const parts = utc
		? [at.getUTCHours(), at.getUTCMinutes(), at.getUTCSeconds()]
		: [at.getHours(), at.getMinutes(), at.getSeconds()]
	const millis = utc ? at.getUTCMilliseconds() : at.getMilliseconds()
	return `${parts.map(n => String(n).padStart(2, '0')).join(':')}.${String(millis).padStart(3, '0')}`
}

/**
 * Says whether a field's key hides its value: the key, or the part of a dotted
 * key after its last dot, is one of the names, the case ignored.
 */
export function redactor(
	names: readonly string[] | undefined,
): ((key: string) => boolean) | undefined {
	if (names === undefined || names.length === 0) {
		return undefined
	}
	const hidden = new Set(names.map(name => name.toLowerCase()))
	return key => {
		const lower = key.toLowerCase()
		return hidden.has(lower) || hidden.has(lower.slice(lower.lastIndexOf('.') + 1))
	}
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
