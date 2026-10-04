// The console lines every logger writes: records/testdata/console.json, which
// Go's records.Handler and the Python handler are tested against too, and the
// logger of the console alone.

import { describe, expect, test } from 'bun:test'
import { join } from 'node:path'

import {
	type ConsoleLine,
	type ConsoleTime,
	jsonLine,
	prettyLine,
	redactor,
} from '../src/console.ts'
import { encodeFields, logger, newLogger } from '../src/logger.ts'

interface Vector {
	name: string
	at: string
	stream: string
	level?: number
	event?: string
	msg?: string
	context?: [string, unknown][]
	attrs?: [string, unknown][]
	trace_id?: string
	span_id?: string
	redact?: string[]
	color?: boolean
	time?: ConsoleTime
	hide_stream?: boolean
	json: string
	pretty: string
}

const testdata = join(import.meta.dir, '..', '..', '..', 'records', 'testdata')
const vectors: { lines: Vector[] } = await Bun.file(join(testdata, 'console.json')).json()

describe('console lines', () => {
	for (const v of vectors.lines) {
		test(v.name, () => {
			const hides = redactor(v.redact)
			const line: ConsoleLine = {
				at: new Date(Number(BigInt(v.at) / 1_000_000n)),
				stream: v.stream,
				...(v.level === undefined ? {} : { level: v.level }),
				...(v.event === undefined ? {} : { event: v.event }),
				...(v.msg === undefined ? {} : { msg: v.msg }),
				context: encodeFields(v.context ?? [], hides),
				attrs: encodeFields(v.attrs ?? [], hides),
				...(v.trace_id === undefined ? {} : { traceId: v.trace_id }),
				...(v.span_id === undefined ? {} : { spanId: v.span_id }),
			}
			expect(jsonLine(line)).toBe(v.json)
			const look = { color: v.color === true, utc: true, time: v.time, hideStream: v.hide_stream }
			expect(prettyLine(line, look)).toBe(v.pretty)
		})
	}
})

describe('a logger of the console alone', () => {
	test('writes each line as it is logged, a child with its context, redacted', () => {
		const written: string[] = []
		const log = newLogger(
			undefined,
			'app',
			{ console: 'json', redact: ['password'] },
			{ write: text => written.push(text) },
		)
		log.with({ module: 'billing' }).info('charged', { password: 'hunter2', amount: 25 })
		expect(written).toHaveLength(1)
		expect(JSON.parse(written[0] as string)).toMatchObject({
			level: 'INFO',
			stream: 'app',
			msg: 'charged',
			module: 'billing',
			password: '[redacted]',
			amount: 25,
		})
	})

	test('is pretty on a terminal and JSON elsewhere, keeps its level, and off writes nothing', () => {
		const terminal: string[] = []
		const pipe: string[] = []
		const off: string[] = []
		newLogger(
			undefined,
			'app',
			{ level: 'warn' },
			{ write: t => terminal.push(t), isTTY: true, utc: true },
		).info('i')
		newLogger(undefined, 'app', {}, { write: t => terminal.push(t), isTTY: true, utc: true }).warn(
			'w',
		)
		newLogger(undefined, 'app', {}, { write: t => pipe.push(t) }).warn('w')
		newLogger(undefined, 'app', { console: 'off' }, { write: t => off.push(t) }).warn('w')
		expect(terminal).toHaveLength(1)
		expect(terminal[0]).toMatch(/^\d\d:\d\d:\d\d\.\d{3} WARN {2}app {2}w\n$/)
		expect(JSON.parse(pipe[0] as string).level).toBe('WARN')
		expect(off).toEqual([])
	})

	test('a value JSON cannot write costs its own spelling, not the line', () => {
		const written: string[] = []
		const cycle: Record<string, unknown> = {}
		cycle.self = cycle
		newLogger(undefined, 'app', { console: 'json' }, { write: t => written.push(t) }).info('odd', {
			big: 12345678901234567890n,
			cycle,
		})
		expect(written[0]).toContain('"big":12345678901234567890,"cycle":"[object Object]"')
	})

	test('needs no store', () => {
		expect(logger('app', { console: 'off' }).dropped).toBe(0)
	})
})
