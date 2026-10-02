// Configs through a real tinystore serve, the one test/binary.ts built, and
// the variables' names of kv/testdata/config.json, which Go's and Python's
// configs are tested against too.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { envName } from '../src/config.ts'
import { InvalidError, open, type StandardSchemaV1, type Store } from '../src/index.ts'

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
	dbUrl: '',
	origins: ['localhost'],
	limits: { rps: 100, burst: 10, on: false },
}

const vectors: { names: [string, string, string][] } = await Bun.file(
	join(import.meta.dir, '..', '..', '..', 'kv', 'testdata', 'config.json'),
).json()

describe('variables', () => {
	for (const [prefix, path, name] of vectors.names) {
		test(`${prefix} ${path} is ${name}`, () => {
			expect(envName(prefix, path)).toBe(name)
		})
	}
})

describe('a config', () => {
	test('is its defaults, then its file, then the environment, then what update kept', async () => {
		process.env.LAYERS_PORT = '3000'
		process.env.LAYERS_ORIGINS = 'a.com, b.com'
		process.env.LAYERS_LIMITS_ON = 'true'
		const cfg = await store.kv.config('layers', defaults, {
			prefix: 'LAYERS',
			file: { limits: { burst: 20 } },
		})
		expect(cfg.value).toEqual({
			port: 3000,
			dbUrl: '',
			origins: ['a.com', 'b.com'],
			limits: { rps: 100, burst: 20, on: true },
		})

		await cfg.update({ port: 4000, limits: { rps: 50 } })
		expect([cfg.value.port, cfg.value.limits.rps, cfg.value.limits.burst]).toEqual([4000, 50, 20])
		expect(cfg.sources().find(s => s.path === 'port')).toEqual({
			path: 'port',
			value: '4000',
			from: 'kept',
		})
		expect(cfg.sources().find(s => s.path === 'limits.burst')?.from).toBe('file')

		const again = await store.kv.config('layers', defaults, { prefix: 'LAYERS' })
		expect([again.value.port, again.value.limits.rps]).toEqual([4000, 50])
		await cfg.reset('port')
		expect(cfg.value.port).toBe(3000)
		await cfg.reset()
		expect(cfg.value.limits.rps).toBe(100)
	})

	test('a change through one config reaches another of its name at once', async () => {
		const first = await store.kv.config('shared', defaults, { env: false })
		const second = await store.kv.config('shared', defaults, { env: false })
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
		const ports: StandardSchemaV1<unknown, typeof defaults> = {
			'~standard': {
				version: 1,
				vendor: 'test',
				validate: value => {
					const port = (value as { port: number }).port
					return port > 0 && port < 65536
						? { value: value as typeof defaults }
						: { issues: [{ message: 'no port', path: ['port'] }] }
				},
			},
		}
		const cfg = await store.kv.config('checked', defaults, {
			env: false,
			schema: ports,
			secret: ['dbUrl'],
		})
		expect(await caught(cfg.update({ port: 70000 }))).toBeInstanceOf(InvalidError)
		expect(await caught(cfg.update({ dbUrl: 'leaked' }))).toBeInstanceOf(InvalidError)
		expect(cfg.value.port).toBe(8080)
		expect(cfg.sources().find(s => s.path === 'dbUrl')?.value).toBe('***')
	})

	test('a kept value that no longer fits its field is left out and named', async () => {
		const older = await store.kv.config('changed', defaults, { env: false })
		await older.update({ port: 4000 })
		const newer = await store.kv.config('changed', { ...defaults, port: 'eighty' }, { env: false })
		expect(newer.value.port).toBe('eighty')
		expect(newer.sources().find(s => s.path === 'port')?.ignored).toContain('is no string')
	})

	test('a variable that is not its field kind is refused, naming it', async () => {
		process.env.BAD_PORT = 'abc'
		const err = await caught(store.kv.config('bad', defaults, { prefix: 'BAD' }))
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

	test('refuses a rate it cannot read', () => {
		expect(() => store.kv.limiter('bad', { rate: 'fast' })).toThrow(InvalidError)
		expect(() => store.kv.limiter('bad', { rate: '5/10s', burst: 0 })).toThrow(InvalidError)
	})
})
