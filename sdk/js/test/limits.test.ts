// The bounds a call meets, through each way a store is reached: `limits` is
// the table of testdata/wire/limits.json, and a LimitError names the bound,
// what the call wanted and the bound, whether the store or the SDK refused it.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

import { LimitError, limits, type Store } from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

const facts = (error: unknown) => {
	expect(error).toBeInstanceOf(LimitError)
	const { limit, wanted, bound } = error as LimitError
	return { limit, wanted, bound }
}

test('limits is every bound of testdata/wire/limits.json', () => {
	const path = resolve(import.meta.dirname, '../../../testdata/wire/limits.json')
	const table = JSON.parse(readFileSync(path, 'utf8')) as Record<string, number>
	const exported = Object.fromEntries(Object.values(limits).map(limit => [limit.name, limit.bound]))
	expect(exported).toEqual(table)
})

for (const way of ways) {
	describe.skipIf(!way.ready)(`limits through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-limits-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		// contract: ERR-05
		test('a call past a bound names it, what it wanted and the bound', async () => {
			const courses = store.index<{ name: string }>('courses', { fields: { name: 1 } })
			const many = Array.from({ length: 1001 }, (_, i) => ({
				id: i,
				value: { name: `course ${i}` },
			}))
			const counted = await caught(courses.setMany(many))
			expect((counted as Error).message).toBe(
				'index courses: a set_many of 1001 documents, past 1000: limit',
			)
			expect(facts(counted)).toEqual({ limit: 'set_many documents', wanted: 1001, bound: 1000 })
			const hits = await caught(courses.search({ text: 'course', limit: 2000 }))
			expect(facts(hits)).toEqual({ limit: 'search hits', wanted: 2000, bound: 1000 })
		})

		// contract: ERR-05
		test('a request past one body fails before it is sent, naming its call and the body', async () => {
			const courses = store.index<{ name: string }>('catalogue', { fields: { name: 1 } })
			const long = Array.from({ length: 300 }, (_, i) => ({
				id: i,
				value: { name: `course ${i} `.repeat(1000) },
			}))
			const refused = await caught(courses.setMany(long))
			const { limit, wanted, bound } = facts(refused)
			expect({ limit, bound }).toEqual({
				limit: limits.requestBytes.name,
				bound: limits.requestBytes.bound,
			})
			expect(wanted).toBeGreaterThan(limits.requestBytes.bound)
			const said = `search.setMany: a request of ${wanted} bytes, past the ${bound} a body holds`
			expect((refused as Error).message).toBe(said)
			expect(await courses.get(0)).toBeUndefined()
		})
	})
}
