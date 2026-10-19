// Search as docs/guides/search/README.md has it, through each way a store is reached:
// documents set by id and found by their words, a vector, or both, kept to
// what a `where` passes, an index read again beside itself, and dropped.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { InvalidError, type SearchFound, type Store, search } from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

interface Product {
	title: string
	category?: string
	price?: number
	tags?: string[]
}

const ids = (found: SearchFound<unknown>) => found.hits.map(hit => hit.id)

for (const way of ways) {
	describe.skipIf(!way.ready)(`search through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-search-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		test('documents set by id are found by any of their words, the best first', async () => {
			const articles = store.index<{ title: string; body: string }>('articles', {
				fields: { title: 2, body: 1 },
			})
			await articles.set('a', { title: 'Sourdough bread', body: 'flour, water and salt' })
			await articles.setMany([
				{ id: 'b', value: { title: 'Rye', body: 'bread from rye flour' } },
				{ id: 'c', value: { title: 'Butter', body: 'milk and cream' } },
			])
			const found = await articles.search('bread salt')
			expect(ids(found)).toEqual(['a', 'b'])
			expect(found.hits[0]?.value.title).toBe('Sourdough bread')
			expect((await articles.search({ text: 'flour', limit: 1, offset: 1 })).hits).toHaveLength(1)
			expect((await articles.search('Sourdouhg')).corrected).toBe('sourdough')
			expect(await articles.get('c')).toEqual({ title: 'Butter', body: 'milk and cream' })
			expect(await articles.delete('c')).toBe(true)
			expect(await articles.get('c')).toBeUndefined()
			expect(await articles.delete('c')).toBe(false)
		})

		test('a vector finds the nearest by its cosine, and with words both are summed or reranked', async () => {
			const shoes = store.index<Product>('shoes', {
				fields: ['title'],
				filters: ['category', 'price', 'tags'],
				vectors: { dimensions: 3 },
			})
			await shoes.set(
				'a',
				{ title: 'red shoes', category: 'shoes', price: 50, tags: ['sale'] },
				{ vector: [1, 0, 0] },
			)
			await shoes.set(
				'b',
				{ title: 'blue shoes', category: 'boots', price: 150 },
				{ vector: new Float32Array([0, 1, 0]) },
			)
			await shoes.set('c', { title: 'red hat' }, { vector: null })
			expect(ids(await shoes.search({ vector: [1, 0, 0] }))).toEqual(['a', 'b'])
			const reranked = await shoes.search({
				text: 'red',
				vector: [0, 1, 0],
				hybrid: { rerank: true },
			})
			expect(ids(reranked)).toEqual(['a', 'c'])
			expect(reranked.hits[1]?.score).toBe(-1)
			expect(
				ids(
					await shoes.search({
						text: 'shoes',
						where: { category: ['shoes', 'boots'], price: { lt: 100 } },
					}),
				),
			).toEqual(['a'])
			expect(ids(await shoes.search({ text: 'red', where: { category: null } }))).toEqual(['c'])
			expect(
				ids(
					await shoes.search({
						text: 'shoes',
						where: search.or({ tags: 'sale' }, { price: { gte: 100 } }),
					}),
				),
			).toEqual(['a', 'b'])
			expect(await caught(shoes.set('d', { title: 'no vector said' }))).toBeInstanceOf(InvalidError)
			expect(
				await caught(
					shoes.search({ text: 'red', where: { category: undefined as unknown as string } }),
				),
			).toBeInstanceOf(InvalidError)
		})

		test('a field added to an index that holds documents is read beside it until it switches in', async () => {
			const notes = store.index<{ title: string; body: string }>('notes', { fields: ['title'] })
			await notes.set('n', { title: 'bread', body: 'salt' })
			const both = store.index<{ title: string; body: string }>('notes', {
				fields: ['title', 'body'],
			})
			await both.ready()
			expect(await both.status()).toEqual({ state: 'ready' })
			expect(ids(await both.search('salt'))).toEqual(['n'])
			const manual = store.index('notes', { fields: ['title'], rebuild: 'manual' })
			expect(await manual.status()).toEqual({ state: 'ready' })
			const refused = store.index('notes', { fields: ['title', 'body', 'tags'], rebuild: 'manual' })
			expect(await caught(refused.status())).toBeInstanceOf(InvalidError)
			const reindexed = await store.reindex('notes', {
				fields: ['title', 'body'],
				rebuild: 'manual',
			})
			await reindexed.ready()
		})

		test("a table's index finds its rows, each hit its row, and follows every write to the table", async () => {
			const db = await store.database('shop', {
				migrations: {
					'0001_articles.sql':
						'create table articles (id integer primary key, title text not null, done integer not null default 0, embedding blob)',
				},
			})
			await db.exec`insert into articles (id, title) values (1, 'Sourdough bread')`
			const articles = db.searchIndex<{ id: number; title: string; done: number }>(
				'articles_search',
				{
					table: 'articles',
					fields: ['title'],
					filters: ['done'],
					vectors: { column: 'embedding', dimensions: 3 },
				},
			)
			await articles.ready()
			expect(ids(await articles.search('bread'))).toEqual(['1'])
			const embedding = new Uint8Array(new Float32Array([1, 0, 0]).buffer)
			await db.exec`insert into articles (id, title, done, embedding) values (2, 'Rye bread', 1, ${embedding})`
			const done = await articles.search({ text: 'bread', where: { done: true } })
			expect(ids(done)).toEqual(['2'])
			expect(done.hits[0]?.value.title).toBe('Rye bread')
			expect(ids(await articles.search({ vector: [1, 0, 0] }))).toEqual(['2'])
			const ragged = db.exec`insert into articles (id, title, embedding) values (3, 'ragged', ${new Uint8Array(5)})`
			expect(await caught(ragged)).toBeInstanceOf(InvalidError)
			expect(await db.scalar<number>`select count(*) from articles where id = 3`).toBe(0)
			expect(await db.dropSearchIndex('articles_search')).toBe(true)
			expect(await caught(articles.search('bread'))).toBeInstanceOf(InvalidError)
		})

		test('an index is listed, and dropped with every document it holds', async () => {
			const posts = store.index<string>('posts')
			await posts.set('p', 'hello')
			const listed = await store.indexes()
			const post = listed.find(index => index.name === 'posts')
			expect(post?.documents).toBe(1)
			expect(post?.opened).toBeInstanceOf(Date)
			expect(await store.dropIndex('posts')).toBe(true)
			expect(await store.dropIndex('posts')).toBe(false)
			expect(await caught(posts.search('hello'))).toBeInstanceOf(InvalidError)
		})
	})
}
