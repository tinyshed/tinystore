// The console lines every logger writes: records/console/testdata/console.json,
// which Go's records/console and the Python handler are tested against too, and
// the logger of the console alone.

import { afterEach, describe, expect, test } from 'bun:test'
import { join } from 'node:path'

import { fromEnv } from '../src/config.ts'
import {
	type ConsoleLine,
	type ConsoleTime,
	jsonLine,
	prettyLine,
	redactor,
	secrets,
	words,
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
	redact_secrets?: boolean
	keep_url_passwords?: boolean
	color?: boolean
	time?: ConsoleTime
	hide_stream?: boolean
	json: string
	pretty: string
}

const testdata = join(import.meta.dir, '..', '..', '..', 'records', 'console', 'testdata')
const vectors: { secrets: string[]; lines: Vector[] } = await Bun.file(
	join(testdata, 'console.json'),
).json()

describe('console lines', () => {
	test('secrets are the vectors', () => {
		expect([...secrets]).toEqual(vectors.secrets)
	})

	for (const v of vectors.lines) {
		test(v.name, () => {
			const names = [...(v.redact ?? []), ...(v.redact_secrets === true ? secrets : [])]
			const hides = redactor(names, v.keep_url_passwords === true)
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

describe('redaction', () => {
	test("a key's words are as a person reads them", () => {
		expect(words('DB_PASSWORD')).toEqual(['db', 'password'])
		expect(words('PasswordHash')).toEqual(['password', 'hash'])
		expect(words('APIKey')).toEqual(['api', 'key'])
		expect(words('signing-key')).toEqual(['signing', 'key'])
		expect(words('oauth2Token')).toEqual(['oauth2', 'token'])
	})

	test('secrets hide every spelling of a secret and leave a counter that looks like one', () => {
		const redact = redactor(secrets)
		for (const key of [
			'DB_PASSWORD',
			'bot_token',
			'api_key',
			'apikey',
			'accessKey',
			'signing-key',
		]) {
			expect(redact?.hides(key)).toBe(true)
		}
		for (const key of ['tokens_used', 'passwords', 'secretary']) {
			expect(redact?.hides(key)).toBe(false)
		}
	})

	test('replace changes a value before it is hidden and kept', () => {
		const written: string[] = []
		newLogger(
			undefined,
			'app',
			{
				console: 'json',
				redact: ['password'],
				replace: (key, value) => (key === 'email' ? 'a***@example.com' : value),
			},
			{ write: text => written.push(text) },
		).info('signed in', { email: 'ann@example.com', password: 'hunter2' })
		expect(JSON.parse(written[0] as string)).toMatchObject({
			email: 'a***@example.com',
			password: '[redacted]',
		})
	})
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

	test('says where a line was logged when asked', () => {
		const written: string[] = []
		newLogger(
			undefined,
			'app',
			{ console: 'json', source: true },
			{ write: t => written.push(t) },
		).info('started')
		const source = JSON.parse(written[0] as string).source
		expect(source.file).toEndWith('console.test.ts')
		expect(source.line).toBeGreaterThan(0)
	})
})

describe('the environment', () => {
	afterEach(() => {
		for (const name of ['LOG_LEVEL', 'LOG_FORMAT', 'APP_LOG_LEVEL', 'APP_LOG_FORMAT']) {
			delete process.env[name]
		}
	})

	test('a logger that named its prefix reads its own variables and not the bare ones', () => {
		process.env.LOG_LEVEL = 'debug'
		process.env.APP_LOG_FORMAT = 'json'
		const written: string[] = []
		const log = newLogger(
			undefined,
			'api',
			{ level: 'info', env: fromEnv('APP') },
			{ write: t => written.push(t), isTTY: true },
		)
		log.debug('hidden')
		log.info('shown')
		expect(written).toHaveLength(1)
		expect(JSON.parse(written[0] as string).msg).toBe('shown')
	})

	test('env false reads nothing', () => {
		process.env.LOG_LEVEL = 'error'
		const written: string[] = []
		newLogger(
			undefined,
			'api',
			{ console: 'json', env: false },
			{ write: t => written.push(t) },
		).info('shown')
		expect(written).toHaveLength(1)
	})
})
