// Configs through a real tinystore serve, the one test/binary.ts built, and
// the variables and .env files of kv/testdata/config.json, which Go's and
// Python's configs are tested against too.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { envName, parseDotenv } from '../src/config.ts'
import {
	fromEnv,
	InvalidError,
	open,
	type Rate,
	required,
	type StandardSchemaV1,
	type Store,
	secret,
	validate,
} from '../src/index.ts'

async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

let dir: string
let store: Store

beforeAll(async () => {
	dir = mkdtempSync(join(tmpdir(), 'tinystore-config-'))
	store = await open(dir, { private: true })
})

afterAll(async () => {
	await store.close()
	rmSync(dir, { recursive: true, force: true })
})

const defaults = {
	port: 8080,
	dbUrl: secret('LAYERS_DATABASE_URL', ''),
	origins: ['localhost'],
	limits: { rps: 100, burst: 10, on: false },
}

const vectors: {
	names: [string, string, string][]
	dotenv: { name: string; text: string; want?: Record<string, string>; refused?: number }[]
} = await Bun.file(join(import.meta.dir, '..', '..', '..', 'kv', 'testdata', 'config.json')).json()

describe('variables', () => {
	for (const [prefix, path, name] of vectors.names) {
		test(`${prefix} ${path} is ${name}`, () => {
			expect(envName(prefix, path)).toBe(name)
		})
	}
})

describe('.env files', () => {
	for (const v of vectors.dotenv) {
		test(v.name, () => {
			if (v.refused === undefined) {
				expect(parseDotenv(v.text)).toEqual(v.want ?? {})
			} else {
				expect(() => parseDotenv(v.text)).toThrow(`line ${v.refused}:`)
			}
		})
	}
})

describe('a config', () => {
	test('is its defaults, then each layer in its order, then what update kept', async () => {
		const dotenv = join(dir, 'layers.env')
		writeFileSync(dotenv, 'LAYERS_PORT=3000\nLAYERS_ORIGINS=a.com, b.com\n')
		process.env.LAYERS_LIMITS_ON = 'true'
		process.env.LAYERS_DATABASE_URL = 'postgres://secret'
		const file = { port: 5000, limits: { burst: 20 } }
		const cfg = await store.kv.config('layers', defaults, file, fromEnv('LAYERS', dotenv))
		expect(cfg.value).toEqual({
			port: 3000,
			dbUrl: 'postgres://secret',
			origins: ['a.com', 'b.com'],
			limits: { rps: 100, burst: 20, on: true },
		})

		await cfg.update({ port: 4000, limits: { rps: 50 } })
		expect([cfg.value.port, cfg.value.limits.rps, cfg.value.limits.burst]).toEqual([4000, 50, 20])
		const sources = Object.fromEntries(cfg.sources().map(s => [s.path, s]))
		expect(sources.port).toEqual({ path: 'port', value: '4000', from: 'kept' })
		expect(sources['limits.burst']?.from).toBe('file')
		expect(sources.dbUrl).toEqual({ path: 'dbUrl', value: '***', from: 'env LAYERS_DATABASE_URL' })

		const again = await store.kv.config('layers', defaults, file, fromEnv('LAYERS', dotenv))
		expect([again.value.port, again.value.limits.rps]).toEqual([4000, 50])
		await cfg.reset('port')
		expect(cfg.value.port).toBe(3000)
		await cfg.reset()
		expect(cfg.value.limits.rps).toBe(100)
	})

	test('reads no variable without fromEnv, and a layer after it goes over it', async () => {
		process.env.ORDER_PORT = '3000'
		const none = await store.kv.config('order', { port: 8080 })
		const last = await store.kv.config('order', { port: 8080 }, fromEnv('ORDER'), { port: 9000 })
		expect([none.value.port, last.value.port]).toEqual([8080, 9000])
	})

	test('a change through one config reaches another of its name at once', async () => {
		const first = await store.kv.config('shared', defaults)
		const second = await store.kv.config('shared', defaults)
		const seen: number[] = []
		second.watch(c => seen.push(c.port))
		await first.update({ port: 9090 })
		for (let i = 0; i < 100 && second.value.port !== 9090; i++) {
			await Bun.sleep(10)
		}
		expect(second.value.port).toBe(9090)
		expect(seen).toEqual([8080, 9090])
	})

	test('a change the schema refuses, or one of a secret, keeps nothing', async () => {
		const ports: StandardSchemaV1<unknown, { port: number }> = {
			'~standard': {
				version: 1,
				vendor: 'test',
				validate: value => {
					const port = (value as { port: number }).port
					return port > 0 && port < 65536
						? { value: value as { port: number } }
						: { issues: [{ message: 'no port', path: ['port'] }] }
				},
			},
		}
		const cfg = await store.kv.config('checked', defaults, validate(ports))
		// @ts-expect-error: a schema is no file's values; it checks through validate
		expect(await caught(store.kv.config('checked', defaults, ports))).toBeInstanceOf(InvalidError)
		expect(await caught(cfg.update({ port: 70000 }))).toBeInstanceOf(InvalidError)
		expect(await caught(cfg.update({ dbUrl: 'leaked' }))).toBeInstanceOf(InvalidError)
		expect(cfg.value.port).toBe(8080)
		expect(cfg.sources().find(s => s.path === 'dbUrl')?.value).toBe('***')
	})

	test('a required field is given by a layer, or the config does not open, naming its variable', async () => {
		const settings = {
			url: secret('REQUIRED_DATABASE_URL'),
			region: required(),
			workers: required(Number),
		}
		delete process.env.REQUIRED_DATABASE_URL
		process.env.REQUIRED_REGION = 'eu'
		process.env.REQUIRED_WORKERS = '0'
		const err = await caught(store.kv.config('required', settings, fromEnv('REQUIRED')))
		expect(err).toBeInstanceOf(InvalidError)
		expect(String(err)).toContain('url is required: set REQUIRED_DATABASE_URL')
		expect(String(await caught(store.kv.config('required', settings)))).toContain('url is required')

		process.env.REQUIRED_DATABASE_URL = ''
		expect(await caught(store.kv.config('required', settings, fromEnv('REQUIRED')))).toBeInstanceOf(
			InvalidError,
		)

		process.env.REQUIRED_DATABASE_URL = 'postgres://secret'
		const cfg = await store.kv.config('required', settings, fromEnv('REQUIRED'))
		const value: { url: string; region: string; workers: number } = cfg.value
		expect(value).toEqual({ url: 'postgres://secret', region: 'eu', workers: 0 })
		expect(await caught(cfg.update({ region: '' }))).toBeInstanceOf(InvalidError)
		await cfg.update({ region: 'us' })
		expect(cfg.value.region).toBe('us')
	})

	test('a kept value that no longer fits its field, or names a secret, is left out and named', async () => {
		const older = await store.kv.config('changed', { port: 8080, token: '' })
		await older.update({ port: 4000, token: 'leaked' })
		const newer = await store.kv.config('changed', {
			port: 'eighty',
			token: secret('CHANGED_TOKEN', 'safe'),
		})
		expect([newer.value.port, newer.value.token]).toEqual(['eighty', 'safe'])
		const sources = Object.fromEntries(newer.sources().map(s => [s.path, s]))
		expect(sources.port?.ignored).toContain('is no string')
		expect(sources.token?.ignored).toBe('the field is a secret')
	})

	test('a variable that is not its field kind is refused, naming it', async () => {
		process.env.BAD_PORT = 'abc'
		const err = await caught(store.kv.config('bad', defaults, fromEnv('BAD')))
		expect(err).toBeInstanceOf(InvalidError)
		expect(String(err)).toContain('BAD_PORT')
	})
})

describe('a limiter', () => {
	test('lets its burst through, then says how long to wait, each key and branch apart', async () => {
		const limit = store.kv.limiter('api', { rate: '2/m' })
		const tenant = limit.of('tenant-7')
		expect(await tenant.allow('user-1')).toEqual({ ok: true, left: 1, retryAfter: 0 })
		expect(await tenant.allow('user-1')).toEqual({ ok: true, left: 0, retryAfter: 0 })
		const third = await tenant.allow('user-1')
		expect(third.ok).toBe(false)
		expect(third.retryAfter).toBeGreaterThan(29_000)
		expect((await limit.allow('user-1')).ok).toBe(true)
		expect(await caught(tenant.allow('user-2', 3))).toBeInstanceOf(InvalidError)
	})

	test('refuses a rate it cannot read, its type before its call', () => {
		// @ts-expect-error: 'fast' spells no rate, which the type sees as it is written
		expect(() => store.kv.limiter('bad', { rate: 'fast' })).toThrow(InvalidError)
		expect(() => store.kv.limiter('bad', { rate: '100/5x' as Rate })).toThrow(InvalidError)
		expect(() => store.kv.limiter('bad', { rate: '5/10s', burst: 0 })).toThrow(InvalidError)
	})
})
