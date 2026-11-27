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

		// contract: WIRE-13
		test('a request and an answer past one body go in several frames, each one message', async () => {
			const courses = store.index<{ name: string }>('catalogue', { fields: { name: 1 } })
			// 1000 documents of 5 KB: 5 MB in pieces of the 2 MiB body, set in one write
			const long = Array.from({ length: 1000 }, (_, i) => ({
				id: i,
				value: { name: `course ${i} `.repeat(500) },
			}))
			await courses.setMany(long)
			const found = await courses.search({ text: 'course', limit: 1000 })
			expect(found.hits.length).toBe(1000)
			const answered = found.hits.reduce((bytes, hit) => bytes + hit.value.name.length, 0)
			expect(answered).toBeGreaterThan(5 << 20)

			// an append of 3.5 MB passes the body and is written whole; one of
			// 5 MB reaches the store, whose own bound of an append refuses it
			const imports = store.events('imports')
			const lines = (count: number) =>
				Array.from({ length: count }, (_, i) => ({
					name: 'imported',
					fields: { line: i, text: 'x'.repeat(5000) },
				}))
			expect(await imports.append(lines(700))).toBe(700)
			const past = facts(await caught(imports.append(lines(1000))))
			expect({ limit: past.limit, bound: past.bound }).toEqual({
				limit: limits.appendBytes.name,
				bound: limits.appendBytes.bound,
			})
		})

		// contract: ERR-05, WIRE-14
		test('a request past the max message fails before it is sent, naming its call and the bound', async () => {
			const courses = store.index<{ name: string }>('catalogue', { fields: { name: 1 } })
			const huge = Array.from({ length: 1000 }, (_, i) => ({
				id: 5000 + i,
				value: { name: 'x'.repeat(70_000) },
			}))
			const refused = await caught(courses.setMany(huge))
			const { limit, wanted, bound } = facts(refused)
			expect({ limit, bound }).toEqual({
				limit: limits.requestBytes.name,
				bound: limits.requestBytes.bound,
			})
			expect(wanted).toBeGreaterThan(limits.requestBytes.bound)
			const said = `search.setMany: a request of ${wanted} bytes, past the ${bound} a message holds`
			expect((refused as Error).message).toBe(said)
			expect(await courses.get(5000)).toBeUndefined()
		})
	})
}
