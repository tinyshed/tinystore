// once opened with fresh, as docs/guides/kv/once.md has it: a kept answer
// answered at once while one new one is made, and kept when the source is
// down, through the ways a store runs on a test's clock.

import { describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { InvalidError, open, type Store } from '../src/index.ts'
import { caught, until } from './ways.ts'

const library = process.env.TINYSTORE_LIBRARY
const binary = process.env.TINYSTORE_BIN
const clocked = [
	{
		name: 'the core in this process',
		ready: library !== undefined,
		open: (dir: string) =>
			open(dir, { embedded: true, library, clock: new Date('2026-11-02T09:00:00Z') }),
	},
	{
		name: 'a private child',
		ready: binary !== undefined && binary !== 'unused',
		open: (dir: string) => open(dir, { private: true, clock: new Date('2026-11-02T09:00:00Z') }),
	},
]

interface GradeBook {
	user: number
	built: number
}

for (const way of clocked) {
	describe.skipIf(!way.ready)(`once with fresh through ${way.name}`, () => {
		async function opened(): Promise<Store> {
			return way.open(mkdtempSync(join(tmpdir(), 'tinystore-fresh-')))
		}

		// contract: KV-16, KV-17, KV-18
		test('first, within fresh, stale while it builds, and the last good answer while the source is down', async () => {
			await using store = await opened()
			const books = store.once<GradeBook>('cabinet-books', { fresh: '10m', keep: '12h' })
			let built = 0
			let down = false
			const cabinet = async (user: number): Promise<GradeBook> => {
				if (down) {
					throw new Error('the cabinet is down')
				}
				built++
				return { user, built }
			}

			const first = await books.answer('7', () => cabinet(7))
			expect(first).toEqual({ value: { user: 7, built: 1 }, age: '0s', stale: false })

			await store.clock.advance('5m')
			expect(await books.answer('7', () => cabinet(7))).toEqual({
				value: { user: 7, built: 1 },
				age: '5m',
				stale: false,
			})
			expect(built).toBe(1)

			await store.clock.advance('6m')
			const stale = await Promise.all([1, 2, 3].map(() => books.answer('7', () => cabinet(7))))
			for (const answer of stale) {
				expect(answer).toEqual({ value: { user: 7, built: 1 }, age: '11m', stale: true })
			}
			await until(async () => (await books.get('7'))?.built === 2)
			expect(built).toBe(2)
			expect(await books.run('7', () => cabinet(7))).toEqual({ user: 7, built: 2 })

			down = true
			await store.clock.advance('11m')
			expect((await books.answer('7', () => cabinet(7))).value).toEqual({ user: 7, built: 2 })
			await store.clock.advance('1m')
			expect((await books.answer('7', () => cabinet(7))).stale).toBe(true)
			expect(await books.get('7')).toEqual({ user: 7, built: 2 })

			await store.clock.advance('12h')
			expect(((await caught(books.run('7', () => cabinet(7)))) as Error).message).toBe(
				'the cabinet is down',
			)
			expect(await books.get('7')).toBeUndefined()
		})

		// contract: KV-19
		test('a clear forgets a branch and the branches under it', async () => {
			await using store = await opened()
			const books = store.once<number>('books', { fresh: '1m', keep: '1h' })
			await books.under('course-1').run('7', () => 1)
			await books.under('course-1', 'group-2').run('8', () => 2)
			await books.under('course-2').run('7', () => 3)
			await books.under('course-1').clear()
			expect(await books.under('course-1').get('7')).toBeUndefined()
			expect(await books.under('course-1', 'group-2').get('8')).toBeUndefined()
			expect(await books.under('course-2').get('7')).toBe(3)
		})

		test('an answer without fresh says its age and is never stale; a fresh as long as keep is refused', async () => {
			await using store = await opened()
			const charges = store.once<string>('charges', { keep: '1h' })
			await charges.run('req-1', () => 'receipt')
			await store.clock.advance('59m')
			expect(await charges.answer('req-1', () => 'never')).toEqual({
				value: 'receipt',
				age: '59m',
				stale: false,
			})
			const refused = await caught(
				store.once<string>('books', { fresh: '1h', keep: '1h' }).run('7', () => 'x'),
			)
			expect(refused).toBeInstanceOf(InvalidError)
		})
	})
}
