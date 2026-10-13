// Configs as plan/api/config.md has them over protocol 2, through each way a
// store is reached: a declaration in layers, each declaring again what came
// before, the rules of one value the core checks, live settings whose change
// every handle sees on one stream, secrets that print as ***, and the names,
// .env files and durations every language reads alike (testdata/config/env.json).

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { envName } from '../src/config.ts'
import { parseDotenv } from '../src/dotenv.ts'
import {
	ConflictError,
	duration,
	env,
	file,
	InvalidError,
	int,
	list,
	live,
	oneOf,
	required,
	Secret,
	type Store,
	secret,
} from '../src/index.ts'
import { ms, spelled } from '../src/time.ts'
import { caught, removed, until, ways } from './ways.ts'

const vectors = JSON.parse(
	readFileSync(join(import.meta.dir, '..', '..', '..', 'testdata', 'config', 'env.json'), 'utf8'),
) as {
	names: [string, string, string][]
	dotenv: { name: string; text: string; want?: Record<string, string>; refused?: number }[]
	spans: [string, number | null][]
}

describe('what every language reads alike', () => {
	test("a setting's variable is its prefix and its path in upper snake case", () => {
		for (const [prefix, path, want] of vectors.names) {
			expect(envName(prefix, path)).toBe(want)
		}
	})

	test('a .env file reads as the core reads one', () => {
		for (const { name, text, want, refused } of vectors.dotenv) {
			if (refused === undefined) {
				expect(parseDotenv(text), name).toEqual(want ?? {})
			} else {
				expect(() => parseDotenv(text), name).toThrow(`line ${refused}:`)
			}
		}
	})

	test('a duration reads and spells back as the core reads one', () => {
		for (const [text, want] of vectors.spans) {
			if (want === null) {
				expect(() => ms(text), text).toThrow(InvalidError)
			} else {
				expect(ms(text), text).toBe(want)
				expect(ms(spelled(want)), text).toBe(want)
			}
		}
		expect(spelled(5_400_000)).toBe('1h30m')
	})

	test('a secret prints as *** wherever it goes, and reveal gives it', () => {
		const url = new Secret('postgres://user:password@host')
		const shown = [
			String(url),
			`${url}`,
			JSON.stringify({ url }),
			Bun.inspect({ url }),
			JSON.stringify({ ...url }),
		]
		for (const text of shown) {
			expect(text).not.toContain('password')
		}
		expect(url.reveal()).toBe('postgres://user:password@host')
	})

	test('a mark that means nothing is refused where it is written', () => {
		// @ts-expect-error a secret is never live
		expect(() => live(secret(String))).toThrow('a secret is never live')
		// @ts-expect-error a secret is required already
		expect(() => required(secret(String))).toThrow('a secret is required already')
		// @ts-expect-error a group is no setting
		expect(() => live({ rps: 1 })).toThrow('a mark goes on a setting, not on a group')
		expect(() => list(live(1) as never)).toThrow("a list's items are neither live nor secret")
	})
})

for (const way of ways) {
	describe.skipIf(!way.ready)(`configs through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-config-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		const declaration = () => ({
			port: int(8080, { min: 1, max: 65535 }),
			level: oneOf('info', ['debug', 'warn', 'error']),
			timeout: duration('30s'),
			origins: ['localhost'],
			db: { url: secret(String), pool: 10 },
			limits: { rps: live(int(100, { min: 0 })), burst: live(10) },
		})

		test('a config is its declaration, then each layer in the order written', async () => {
			const yaml = join(dir, 'app.yaml')
			writeFileSync(yaml, 'port: 8000\nlimits:\n  rps: 50\n')
			const config = await store.config('ordered', [
				declaration(),
				{ level: 'debug' as const },
				file(yaml),
				env('APP', { from: { APP_PORT: '9000', APP_DB_URL: 'postgres://local' } }),
			])
			const port: number = config.value.port
			expect([port, config.value.limits.rps, config.value.level]).toEqual([9000, 50, 'debug'])
			expect(config.value.timeout).toBe('30s')
			const lines = Object.fromEntries(config.explain().map(line => [line.path, line]))
			expect(lines.port).toEqual({
				path: 'port',
				value: '9000',
				from: 'APP_PORT',
				over: `8000 from ${yaml}, 8080 from the declaration`,
			})
			expect(lines.level?.from).toBe('layer 2')
			expect(lines['db.url']?.value).toBe('***')
		})

		test('a rule of one value is the core’s, and a refusal names the layer that gave it', async () => {
			const opening = (from: Record<string, string>) =>
				store.config('rules', [declaration(), env('APP', { from: { APP_DB_URL: 'u', ...from } })])
			const err = await caught(opening({ APP_PORT: '70000', APP_LEVEL: 'verbose' }))
			expect(err).toBeInstanceOf(InvalidError)
			expect((err as Error).message).toContain('APP_PORT: 70000 is above 65535')
			expect((err as Error).message).toContain(
				'APP_LEVEL: "verbose" is not one of info, debug, warn, error',
			)
			const config = await opening({ APP_TIMEOUT: '1m30s', APP_ORIGINS: 'a.com, b.com' })
			expect(config.value.timeout).toBe('1m30s')
			expect(config.value.origins).toEqual(['a.com', 'b.com'])
		})

		test('a required setting is given by a layer, or the config does not open, naming what would', async () => {
			const opening = (from: Record<string, string>) =>
				store.config('required', [
					{ region: required(String), workers: required(int({ min: 1 })), token: secret(String) },
					env('APP', { from }),
				])
			const err = await caught(opening({ APP_REGION: '' }))
			expect(err).toBeInstanceOf(InvalidError)
			expect((err as Error).message).toContain('region is required: set APP_REGION')
			expect((err as Error).message).toContain('token is required: set APP_TOKEN')
			const config = await opening({ APP_REGION: 'eu', APP_WORKERS: '4', APP_TOKEN: 't' })
			const token: Secret = config.value.token
			expect([config.value.region, config.value.workers, token.reveal()]).toEqual(['eu', 4, 't'])
		})

		test('a file absent is refused unless optional, and a key no setting has is named', async () => {
			const missing = join(dir, 'missing.toml')
			const opening = (layers: Parameters<typeof file>[]) =>
				store.config('files', [
					{ port: 8080 },
					...layers.map(([path, options]) => file(path, options)),
				])
			expect(((await caught(opening([[missing]]))) as Error).message).toContain('no such file')
			const config = await opening([[missing, { optional: true }]])
			expect(config.explain().notes).toEqual([`${missing}: absent`])
			const json = join(dir, 'app.jsonc')
			writeFileSync(json, '{\n  // a comment\n  "prot": 1,\n}\n')
			expect(((await caught(opening([[json]]))) as Error).message).toContain(
				'prot: no such setting',
			)
		})

		test('a live setting changes for every handle, and reset gives the layers back', async () => {
			const layers = [
				declaration(),
				env('APP', { from: { APP_DB_URL: 'u', APP_LIMITS_RPS: '1000' } }),
			] as const
			const config = await store.config('live', layers)
			expect(await store.config('live', layers)).toBe(config)
			const other = await store.config('live', [
				{ limits: { rps: live(1), burst: live(1) } },
				env('APP', { from: {} }),
			])
			await config.update({ limits: { rps: 200 } })
			expect(config.value.limits.rps).toBe(200)
			const rps = config.explain().find(line => line.path === 'limits.rps')
			expect(rps?.from.startsWith('a change, version ')).toBe(true)
			expect(rps?.over).toBe('1000 from APP_LIMITS_RPS, 100 from the declaration')
			await until(() => other.value.limits.rps === 200)
			// @ts-expect-error port is not live
			const refused = await caught(config.update({ port: 1 }))
			expect((refused as Error).message).toContain('port is not live')
			await config.reset('limits.rps')
			expect(config.value.limits.rps).toBe(1000)
			await until(() => other.value.limits.rps === 1)
		})

		test('ifVersion refuses a change made since, and two changes of different settings both stay', async () => {
			const first = await store.config('versions', [{ a: live(1), b: live(2) }])
			const second = await store.config('versions', [{ a: live(1), b: live(2), c: 3 }])
			const shown = first.version
			await first.update({ a: 10 })
			await second.update({ b: 20 })
			await until(() => first.value.b === 20 && second.value.a === 10)
			const err = await caught(first.update({ a: 30 }, { ifVersion: shown }))
			expect(err).toBeInstanceOf(ConflictError)
			await first.update({ a: 40 }, { ifVersion: first.version })
			expect(first.value.a).toBe(40)
		})

		test('every watcher of a config is called now, then once for each change, until it stops', async () => {
			const config = await store.config('watched', [{ rps: live(100) }])
			const seen: [number, number | undefined][][] = [[], [], []]
			const stops = seen.map(calls =>
				config.watch((next, previous) => calls.push([next.rps, previous?.rps])),
			)
			await config.update({ rps: 200 })
			for (const calls of seen) {
				expect(calls).toEqual([
					[100, undefined],
					[200, 100],
				])
			}
			stops[0]?.()
			await config.update({ rps: 300 })
			expect(seen[0]).toHaveLength(2)
			expect(seen[1]).toHaveLength(3)
		})

		test('validate refuses a value at open, and a change it refuses keeps nothing', async () => {
			const atMost = (most: number) => (value: { readonly rps: number }) => {
				if (value.rps > most) {
					throw new Error(`rps past ${most}`)
				}
			}
			const refused = await caught(
				store.config('validated', [{ rps: live(5000) }], { validate: atMost(1000) }),
			)
			expect((refused as Error).message).toContain('rps past 1000')
			const config = await store.config('validated', [{ rps: live(100) }], {
				validate: atMost(1000),
			})
			const err = await caught(config.update({ rps: 2000 }))
			expect(err).toBeInstanceOf(InvalidError)
			expect(config.value.rps).toBe(100)
			const schema = {
				'~standard': {
					version: 1 as const,
					vendor: 'test',
					validate: (value: unknown) =>
						(value as { rps: number }).rps === 7 ? { issues: [{ message: 'not 7' }] } : { value },
				},
			}
			const checked = await store.config('schema', [{ rps: live(1) }], { validate: schema })
			expect(((await caught(checked.update({ rps: 7 }))) as Error).message).toContain('not 7')
		})

		test('a list checks each item, and a list of groups fills its items’ defaults', async () => {
			const config = await store.config('lists', [
				{
					hosts: list(String, { default: ['localhost'], max: 2 }),
					upstreams: list({ host: required(String), port: int(80) }),
				},
				env('APP', { from: { APP_UPSTREAMS: '[{"host":"a.com"}]' } }),
			])
			expect(config.value.upstreams).toEqual([{ host: 'a.com', port: 80 }])
			const err = await caught(
				store.config('lists', [
					{ hosts: list(String, { max: 2 }) },
					env('APP', { from: { APP_HOSTS: 'a,b,c' } }),
				]),
			)
			expect((err as Error).message).toContain('APP_HOSTS: 3 items is above 2 items')
		})
	})
}
