// A logger's buffer against an append that answers when the test lets it, so
// that what a buffer does while a write runs can be watched step by step, and
// what FORCE_COLOR makes of a child's stderr.

import { describe, expect, test } from 'bun:test'
import { fileURLToPath } from 'node:url'

import type { ConsoleLine } from '../src/console.ts'
import { logger, newLogger } from '../src/logger.ts'

/** An append that holds each write until let go, and keeps how many lines each carried. */
function heldAppend() {
	const sizes: number[] = []
	const held: (() => void)[] = []
	return {
		sizes,
		append: (lines: ConsoleLine[]) => {
			sizes.push(lines.length)
			return new Promise<void>(resolve => void held.push(resolve))
		},
		/** answers the oldest write, then lets what follows its answer run */
		async letGo() {
			held.shift()?.()
			await Bun.sleep(0)
		},
	}
}

describe("a logger's buffer", () => {
	test('what filled the buffer while a write ran goes as the write ends, not at the next second', async () => {
		const { sizes, append, letGo } = heldAppend()
		const log = newLogger(append, 'burst', { buffer: 4, console: 'off' })
		log.info('one')
		log.info('two') // half the buffer: the first write takes both
		for (const n of ['three', 'four', 'five', 'six', 'seven']) {
			log.info(n) // four wait, and the fifth finds the buffer full
		}
		expect([sizes, log.dropped]).toEqual([[2], 1])

		await letGo()
		expect(sizes).toEqual([2, 4]) // no line came to ask, and no second passed

		await letGo()
		await log.stop()
		expect(sizes).toEqual([2, 4])
	})

	test('a write that fails drops its lines and lets the next go', async () => {
		const sizes: number[] = []
		const log = newLogger(
			lines => {
				sizes.push(lines.length)
				return Promise.reject(new Error('the server is gone'))
			},
			'failing',
			{ buffer: 4, console: 'off' },
		)
		for (const n of ['one', 'two', 'three']) {
			log.info(n)
		}
		await log.stop()
		expect([sizes, log.dropped]).toEqual([[2, 1], 3])
	})
})

describe('FORCE_COLOR', () => {
	// a child's stderr is a pipe, as an IDE's run console reads it
	async function logged(env: Record<string, string>): Promise<string> {
		const index = fileURLToPath(new URL('../src/index.ts', import.meta.url))
		const child = Bun.spawn(
			[
				process.execPath,
				'-e',
				`import { logger } from ${JSON.stringify(index)}; logger('app').warn('slow request')`,
			],
			{ env: { ...process.env, NO_COLOR: '', TERM: '', ...env }, stdout: 'ignore', stderr: 'pipe' },
		)
		const text = await new Response(child.stderr).text()
		await child.exited
		return text
	}

	test('makes a pipe pretty and coloured, unless NO_COLOR', async () => {
		const forced = await logged({ FORCE_COLOR: '1' })
		expect(forced).toContain('[')
		expect(forced).toContain('slow request')
		expect(forced.startsWith('{')).toBe(false)

		const refused = await logged({ FORCE_COLOR: '1', NO_COLOR: '1' })
		expect(refused).not.toContain('[')
		expect(refused).toContain(' WARN  app  slow request')

		expect((await logged({ FORCE_COLOR: '0' })).startsWith('{')).toBe(true)
	})
})

/** Runs fn with these variables set, and puts back what was there. */
function withEnv(env: Record<string, string>, fn: () => void): void {
	const before = Object.fromEntries(Object.keys(env).map(name => [name, process.env[name]]))
	Object.assign(process.env, env)
	try {
		fn()
	} finally {
		for (const [name, value] of Object.entries(before)) {
			if (value === undefined) {
				delete process.env[name]
			} else {
				process.env[name] = value
			}
		}
	}
}

describe('LOG_LEVEL, LOG_FORMAT and LOG_TIME', () => {
	test('win over the options, and never turn on a console turned off', () => {
		const written: string[] = []
		const events: string[] = []
		const silenced: string[] = []
		withEnv({ LOG_LEVEL: 'WARN', LOG_FORMAT: 'pretty', LOG_TIME: 'off' }, () => {
			const options = { console: 'json', level: 'debug', time: 'full' } as const
			const log = newLogger(undefined, 'api', options, { write: t => written.push(t), utc: true })
			log.info('below the level')
			log.warn('kept', { ms: 1200 })
			newLogger(undefined, 'events', { console: 'off' }, { write: t => events.push(t) }).warn(
				'viewed',
			)
		})
		withEnv({ LOG_FORMAT: 'off' }, () => {
			newLogger(undefined, 'api', {}, { write: t => silenced.push(t) }).warn('w')
		})
		expect(written).toEqual(['WARN  api  kept  ms=1200\n'])
		expect(events).toEqual([])
		expect(silenced).toEqual([])
	})

	test('a value they cannot mean is ignored, and said once', () => {
		const value = `verbose-${Date.now()}`
		const first: string[] = []
		const second: string[] = []
		withEnv({ LOG_LEVEL: value }, () => {
			newLogger(undefined, 'api', { console: 'json' }, { write: t => first.push(t) }).debug('d')
			newLogger(undefined, 'api', { console: 'json' }, { write: t => second.push(t) }).debug('d')
		})
		expect(first).toHaveLength(2)
		expect(JSON.parse(first[0] as string)).toMatchObject({
			level: 'WARN',
			stream: 'tinystore',
			msg: 'LOG_LEVEL is ignored',
			value,
			expected: 'debug, info, warn or error',
		})
		expect(second).toHaveLength(1)
	})
})

test('a logger writes where to says: JSON off a terminal, pretty on one', () => {
	const pipe: string[] = []
	const terminal: string[] = []
	withEnv({ FORCE_COLOR: '', NO_COLOR: '1' }, () => {
		logger('app', { to: { write: t => pipe.push(t) } }).info('started')
		const look = { time: 'off', hideStream: true } as const
		logger('app', { to: { write: t => terminal.push(t), isTTY: true }, ...look }).info('started', {
			port: 3000,
		})
	})
	expect(JSON.parse(pipe[0] as string).msg).toBe('started')
	expect(terminal).toEqual(['INFO  started  port=3000\n'])
})
