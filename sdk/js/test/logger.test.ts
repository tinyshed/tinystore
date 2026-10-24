// A logger's buffer against an append that answers when the test lets it, so
// that what a buffer does while a write runs can be watched step by step, and
// what FORCE_COLOR makes of a child's stderr.

import { describe, expect, test } from 'bun:test'
import { spawnSync } from 'node:child_process'
import { setTimeout as sleep } from 'node:timers/promises'

import { env } from '../src/config.ts'
import type { ConsoleLine } from '../src/console.ts'
import { InvalidError } from '../src/errors.ts'
import { logger, newLogger, newStoreLogger } from '../src/logger.ts'
import type { StoreLoggerOptions } from '../src/records.ts'

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
			await sleep(0)
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

	test('a buffer of more text than a request holds goes as several writes, none of them refused', async () => {
		const sizes: number[] = []
		const log = newLogger(
			async lines => {
				sizes.push(lines.length)
			},
			'long',
			{ buffer: 64, console: 'off' },
		)
		const stack = 'x'.repeat(100 * 1024) // a stack trace's worth of text, as a failure logs it
		for (let n = 0; n < 31; n++) {
			log.error('failed', { stack })
		}
		await log.stop()
		// five lines of 100 KiB and their framing fill a write of 512 KiB
		expect([sizes, log.dropped]).toEqual([[5, 5, 5, 5, 5, 5, 1], 0])
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
		// a URL, since Node's loader takes no Windows path
		const index = new URL('../src/index.ts', import.meta.url).href
		const child = spawnSync(
			process.execPath,
			['-e', `import { logger } from ${JSON.stringify(index)}; logger('app').warn('slow request')`],
			{ env: { ...process.env, NO_COLOR: '', TERM: '', ...env }, encoding: 'utf8' },
		)
		return child.stderr
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
			expected: 'trace, debug, info, warn or error',
		})
		expect(second).toHaveLength(1)
	})
})

/** A store's logger over an append of its own: the messages it printed, and those it kept. */
async function printedAndKept(
	options: Omit<StoreLoggerOptions, 'keep'>,
	log: (logger: ReturnType<typeof newStoreLogger>[0]) => void,
): Promise<{ printed: string[]; kept: string[] }> {
	const printed: string[] = []
	const kept: string[] = []
	const to = { write: (text: string) => void printed.push(JSON.parse(text).msg) }
	const append = async (lines: ConsoleLine[]) =>
		void kept.push(...lines.map(line => line.msg ?? ''))
	const [made] = newStoreLogger(append, 'api', { console: 'json', to, ...options })
	log(made)
	await made.stop()
	return { printed, kept }
}

const fourLevels = (log: ReturnType<typeof newStoreLogger>[0]) => {
	log.debug('debug')
	log.info('info')
	log.warn('warn')
	log.error('error')
}

describe('LOG_CONSOLE_LEVEL', () => {
	// contract: LOG-05
	test("wins over level's console part, as LOG_LEVEL wins over its store part", async () => {
		const from = { LOG_CONSOLE_LEVEL: 'error', LOG_LEVEL: 'debug' }
		const options = {
			level: { store: 'warn', console: 'info' },
			env: env('', { from }),
		} as const
		expect(await printedAndKept(options, fourLevels)).toEqual({
			printed: ['error'],
			kept: ['debug', 'info', 'warn', 'error'],
		})
	})

	// contract: LOG-05
	test('LOG_LEVEL leaves a console part the code gave as it is', async () => {
		const options = {
			level: { console: 'warn' },
			env: env('', { from: { LOG_LEVEL: 'debug' } }),
		} as const
		expect(await printedAndKept(options, fourLevels)).toEqual({
			printed: ['warn', 'error'],
			kept: ['debug', 'info', 'warn', 'error'],
		})
	})

	// contract: LOG-05
	test('a prefix reads APP_LOG_CONSOLE_LEVEL, and a value it cannot mean is ignored and said once', async () => {
		const prefixed = {
			env: env('APP', { from: { LOG_CONSOLE_LEVEL: 'error', APP_LOG_CONSOLE_LEVEL: 'warn' } }),
		}
		expect((await printedAndKept(prefixed, fourLevels)).printed).toEqual(['warn', 'error'])

		const value = `quiet-${Date.now()}`
		const said = { env: env('', { from: { LOG_CONSOLE_LEVEL: value } }) }
		const first = await printedAndKept(said, fourLevels)
		expect(first.printed).toEqual([
			'LOG_CONSOLE_LEVEL is ignored',
			'debug',
			'info',
			'warn',
			'error',
		])
		expect((await printedAndKept(said, fourLevels)).printed).toEqual([
			'debug',
			'info',
			'warn',
			'error',
		])
	})

	// contract: LOG-05
	test('sets what a logger of the console alone prints', () => {
		const printed: string[] = []
		const to = { write: (text: string) => void printed.push(JSON.parse(text).msg) }
		const from = { LOG_CONSOLE_LEVEL: 'warn', LOG_LEVEL: 'debug' }
		fourLevels(logger('app', { console: 'json', to, env: env('', { from }) }))
		expect(printed).toEqual(['warn', 'error'])
	})
})

describe("a store's logger's level in two parts", () => {
	// contract: LOG-08
	test('one level sets both what is kept and what is printed', async () => {
		expect(await printedAndKept({ level: 'warn', env: false }, fourLevels)).toEqual({
			printed: ['warn', 'error'],
			kept: ['warn', 'error'],
		})
	})

	// contract: LOG-08
	test('a console part alone keeps every level and prints from its own', async () => {
		expect(await printedAndKept({ level: { console: 'warn' }, env: false }, fourLevels)).toEqual({
			printed: ['warn', 'error'],
			kept: ['debug', 'info', 'warn', 'error'],
		})
	})

	// contract: LOG-08
	test('prints from its console part and keeps from its store part, the two apart', async () => {
		const quieter = { level: { store: 'info', console: 'warn' }, env: false } as const
		expect(await printedAndKept(quieter, fourLevels)).toEqual({
			printed: ['warn', 'error'],
			kept: ['info', 'warn', 'error'],
		})
		const louder = { level: { store: 'info', console: 'debug' }, env: false } as const
		expect(await printedAndKept(louder, fourLevels)).toEqual({
			printed: ['debug', 'info', 'warn', 'error'],
			kept: ['info', 'warn', 'error'],
		})
	})

	test('refuses a part that is neither store nor console', () => {
		const level = { default: 'info' } as unknown as { store: 'info' }
		expect(() => newStoreLogger(async () => {}, 'api', { level, env: false })).toThrow(InvalidError)
	})

	// contract: LOG-08
	test('prints and keeps an event, which has no level', async () => {
		const options = { level: { store: 'error', console: 'error' }, env: false } as const
		const printed: string[] = []
		const kept: string[] = []
		const to = { write: (text: string) => void printed.push(JSON.parse(text).event) }
		const append = async (lines: ConsoleLine[]) =>
			void kept.push(...lines.map(line => line.event ?? ''))
		const [log] = newStoreLogger(append, 'api', { console: 'json', to, ...options })
		log.event('signup', { plan: 'pro' })
		await log.stop()
		expect([printed, kept]).toEqual([['signup'], ['signup']])
	})

	test('follows the config log only while its level has no console part', async () => {
		const printed: string[] = []
		const to = { write: (text: string) => void printed.push(JSON.parse(text).msg) }
		const append = async () => {}
		const made = (level: StoreLoggerOptions['level']) =>
			newStoreLogger(append, 'api', { console: 'json', to, level, env: false })
		const [following, followingConfig] = made('error')
		const [storePart, storePartConfig] = made({ store: 'error' })
		const [own, ownConfig] = made({ store: 'error', console: 'warn' })
		for (const config of [followingConfig, storePartConfig, ownConfig]) {
			config.take('debug')
		}
		following.debug('followed')
		storePart.debug('a store part alone followed')
		own.debug('own console part')
		own.warn('warned')
		await Promise.all([following.stop(), storePart.stop(), own.stop()])
		expect(printed).toEqual(['followed', 'a store part alone followed', 'warned'])
	})

	test('encodes no line that no destination it has takes', async () => {
		let replaced = 0
		const replace = (_: string, value: unknown) => {
			replaced++
			return value
		}
		const options = {
			console: 'off',
			level: { store: 'warn', console: 'debug' },
			replace,
			env: false,
		} as const
		const [log] = newStoreLogger(async () => {}, 'api', options)
		log.debug('to no one', { user: 42 })
		await log.stop()
		expect(replaced).toBe(0)
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
