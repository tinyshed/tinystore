// A store's durability: how far a commit of its files goes before it returns,
// said by whoever opens the store first, and by a database for its own file.
// Through each way a store is opened.

import { afterEach, describe, expect, test } from 'bun:test'
import { existsSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { InvalidError, type OpenOptions, open, type Store } from '../src/index.ts'
import { caught, removed, until } from './ways.ts'

const library = process.env.TINYSTORE_LIBRARY
const binary = process.env.TINYSTORE_BIN
const served = binary !== undefined && binary !== 'unused'

/** The ways a program opens a store of its own, each with what it adds to the options. */
const opens: { name: string; ready: boolean; options: OpenOptions; shared: boolean }[] = [
	{
		name: 'the core in this process',
		ready: library !== undefined,
		options: { embedded: true, library },
		shared: true,
	},
	{ name: 'a private child', ready: served, options: { private: true }, shared: false },
	{ name: 'a sidecar', ready: served, options: { idle: 1 }, shared: true },
]

for (const way of opens) {
	describe.skipIf(!way.ready)(`durability through ${way.name}`, () => {
		const stores: Store[] = []
		const dirs: string[] = []

		const opened = async (dir: string, options: OpenOptions): Promise<Store> => {
			const store = await open(dir, { ...way.options, ...options })
			stores.push(store)
			return store
		}

		afterEach(async () => {
			for (const store of stores.splice(0)) {
				await store.close()
			}
			for (const dir of dirs.splice(0)) {
				await until(() => !existsSync(join(dir, 'server', 'SERVE')))
				await removed(dir)
			}
		})

		const directory = (): string => {
			const dir = mkdtempSync(join(tmpdir(), 'tinystore-durability-'))
			dirs.push(dir)
			return dir
		}

		/** SQLite's synchronous on a database's writer: 2 syncs each commit, 1 leaves it to checkpoints. */
		const synchronous = (db: Awaited<ReturnType<Store['database']>>): Promise<number> =>
			db.tx(tx => tx.scalar<number>`pragma synchronous`)

		test("a store opened 'os' gives its files that, and a database may say its own", async () => {
			const store = await opened(directory(), { durability: 'os' })
			const sessions = store.bucket<number>('sessions')
			await sessions.set('a', 1)
			expect(await sessions.get('a')).toBe(1)

			const cache = await store.database('cache')
			const audit = await store.database('audit', { durability: 'full' })
			expect([await synchronous(cache), await synchronous(audit)]).toEqual([1, 2])

			const other = await caught(store.database('audit', { durability: 'os' }))
			expect(other).toBeInstanceOf(InvalidError)
			expect((other as Error).message).toContain('open with durability full, and asked for os')
		})

		test('a store that says nothing syncs every commit', async () => {
			const store = await opened(directory(), {})
			expect(await synchronous(await store.database('app'))).toBe(2)
			expect(await synchronous(await store.database('cache', { durability: 'os' }))).toBe(1)
		})

		test.skipIf(!way.shared)('a store open one way is not opened again another', async () => {
			const dir = directory()
			await opened(dir, { durability: 'os' })
			await opened(dir, { durability: 'os' })
			await opened(dir, {})
			const refused = await caught(opened(dir, { durability: 'full' }))
			expect(refused).toBeInstanceOf(Error)
			expect((refused as Error).message).toMatch(
				/open with durability '?os'?,? and (this program )?asked for '?full'?/,
			)
		})

		test('a word that is neither is refused before anything opens', async () => {
			const dir = directory()
			const refused = await caught(opened(dir, { durability: 'fast' as never }))
			expect(refused).toBeInstanceOf(InvalidError)
			expect(existsSync(join(dir, 'kv.db'))).toBe(false)
		})
	})
}
