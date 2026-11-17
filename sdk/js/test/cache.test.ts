// a set of files as a cache, as docs/guides/blobs/caching.md has it, through
// each way a store is reached: a ceiling kept in the set that evicts or
// refuses, its change, a trim, and a term from a file's last use.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { ConflictError, InvalidError, LimitError, type Store } from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

const bytes = (n: number) => new Uint8Array(n).fill(7)
const day = 86_400_000

for (const way of ways) {
	describe.skipIf(!way.ready)(`a cache of files through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-cache-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		// contract: BLOB-18, BLOB-20
		test('a write past maxSize evicts, and the ceiling is the set’s', async () => {
			const pages = store.files('pages', { maxSize: 300 })
			for (const path of ['a', 'b', 'c', 'd']) {
				await pages.put(path, bytes(100))
			}
			expect(await pages.usage()).toEqual({ count: 3, size: 300, maxSize: 300, evictions: 1 })
			expect((await pages.list()).files.map(file => file.path)).toEqual(['b', 'c', 'd'])
			expect(await caught(store.files('pages', { maxSize: '1KiB' }).head('b'))).toBeInstanceOf(
				ConflictError,
			)
			expect((await store.files('pages').usage()).maxSize).toBe(300)
			expect(await caught(pages.put('big', bytes(301)))).toBeInstanceOf(LimitError)
		})

		// contract: BLOB-20, BLOB-24
		test('setMaxSize changes the ceiling when ifCurrent holds, and trim frees room', async () => {
			const thumbs = store.files('thumbs', { maxSize: '1KiB' })
			for (const path of ['1', '2', '3']) {
				await thumbs.put(path, bytes(100))
			}
			expect(await caught(thumbs.setMaxSize(200, { ifCurrent: '2KiB' }))).toBeInstanceOf(
				ConflictError,
			)
			await thumbs.setMaxSize(200, { ifCurrent: '1KiB' })
			expect(await thumbs.usage()).toEqual({ count: 2, size: 200, maxSize: 200, evictions: 1 })
			await thumbs.setMaxSize(200, { ifCurrent: 200 })
			expect(await thumbs.trim({ to: 100 })).toEqual({ evictions: 1, freed: 100 })
			expect((await thumbs.usage()).maxSize).toBe(200)
			await thumbs.setMaxSize('none')
			expect((await thumbs.usage()).maxSize).toBeUndefined()
		})

		// contract: BLOB-23
		test('whenFull refuse fails a write past the ceiling and removes nothing', async () => {
			const uploads = store.files('uploads', { maxSize: 150, whenFull: 'refuse' })
			await uploads.put('a', bytes(100))
			expect(await caught(uploads.put('b', bytes(100)))).toBeInstanceOf(LimitError)
			await uploads.put('a', bytes(120))
			expect(await uploads.usage()).toEqual({ count: 1, size: 120, maxSize: 150, evictions: 0 })
			expect(await caught(uploads.trim({ to: 0 }))).toBeInstanceOf(InvalidError)
			expect(await caught(store.files('uploads', { whenFull: 'evict' }).head('a'))).toBeInstanceOf(
				ConflictError,
			)
		})

		// contract: BLOB-17
		test('idle gives a file its term from its last use, never beside ttl', async () => {
			const sessions = store.files('sessions', { idle: '30d' })
			const written = Date.now()
			const put = await sessions.put('a.json', '{}')
			expect(Math.abs(put.expires!.getTime() - (written + 30 * day))).toBeLessThan(60_000)
			expect((await sessions.head('a.json'))?.path).toBe('a.json')
			expect(
				await caught(Promise.resolve().then(() => store.files('x', { ttl: '1h', idle: '1h' }))),
			).toBeInstanceOf(InvalidError)
		})
	})
}
