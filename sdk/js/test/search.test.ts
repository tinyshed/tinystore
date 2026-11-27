// Search as docs/guides/search/README.md has it, through each way a store is reached:
// documents set by id and found by their words, a vector, or both, kept to
// what a `where` passes, an index read again beside itself, and dropped.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import {
	InvalidError,
	type SearchFound,
	type SearchMatch,
	type Store,
	search,
	sql,
} from '../src/index.ts'
import { caught, removed, until, ways } from './ways.ts'

interface Product {
	title: string
	category?: string
	price?: number
	tags?: string[]
}

const ids = (found: SearchFound<unknown>) => found.hits.map(hit => hit.id)

/** A connection to an SQLite file beside the store, through Bun's SQLite under Bun and Node's under Node. */
async function besides(path: string): Promise<{ exec(sql: string): unknown; close(): unknown }> {
	let beside: { exec(sql: string): unknown; close(): unknown }
	if ('Bun' in globalThis) {
		const { Database } = await import('bun:sqlite')
		beside = new Database(path)
	} else {
		const { DatabaseSync } = await import('node:sqlite')
		beside = new DatabaseSync(path)
	}
	// the store's writer may hold search.db as this writes: wait for it, as the store's own connections do
	beside.exec('pragma busy_timeout = 5000')
	return beside
}

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

		// contract: SRCH-11
		// contract: SRCH-18
		test('a total counts what a search finds under its where, whatever its page', async () => {
			const people = store.index<{ name: string; role: string }>('people_total', {
				fields: ['name'],
				filters: ['role'],
			})
			const roles = ['teacher', 'student', 'student']
			await people.setMany(
				Array.from({ length: 30 }, (_, n) => ({
					id: `p${n}`,
					value: { name: `Ivanov ${n}`, role: roles[n % 3]! },
				})),
			)
			const found = await people.search({
				text: 'ivanov',
				where: { role: 'teacher' },
				limit: 2,
				total: true,
			})
			expect(found.hits).toHaveLength(2)
			expect(found.total).toEqual({ count: 10, atLeast: false })
			expect((await people.search('ivanov')).total).toBeUndefined()
		})

		test('a word typed halfway finds the words it begins with prefix, and nothing without', async () => {
			const courses = store.index<{ name: string }>('courses', { fields: { name: 1 } })
			await courses.setMany([
				{ id: 1, value: { name: 'Cryptographic methods of information security' } },
				{ id: 2, value: { name: 'Cryptanalysis' } },
				{ id: 3, value: { name: 'Cryptography' } },
				{ id: 4, value: { name: 'Foundations of cryptography' } },
			])
			const found = async (text: string, prefix?: boolean) =>
				ids(await courses.search({ text, prefix }))
			expect(await found('crypt')).toEqual([])
			expect(await found('crypt', true)).toEqual(['2', '3', '4', '1'])
			expect(await found('cryptograph', true)).toEqual(['3', '4', '1'])
			expect(await found('crypt ', true)).toEqual([])
		})

		// contract: SRCH-12
		test('a word typed with the other layout on is read as its keys type there, and corrected says so', async () => {
			const courses = store.index<{ name: string }>('catalogue', {
				fields: { name: 1 },
				layouts: ['ru', 'en'],
			})
			await courses.setMany([
				{ id: 1, value: { name: 'Криптографические методы защиты информации' } },
				{ id: 2, value: { name: 'Линейная алгебра' } },
				{ id: 3, value: { name: 'Python для анализа данных' } },
				{ id: 4, value: { name: 'Инженерная графика' } },
			])
			const found = async (text: string) => {
				const result = await courses.search({ text, prefix: true })
				return [ids(result), result.corrected]
			}
			expect(await found('rhbgnj')).toEqual([['1'], 'крипто'])
			expect(await found('kby fku')).toEqual([['2'], 'kby алг'])
			expect(await found('by; uhfa')).toEqual([['4'], 'by; граф'])
			expect(await found('знерщт')).toEqual([['3'], 'python'])
			expect(await found('python')).toEqual([['3'], undefined])
		})

		// contract: SRCH-13
		test("prefix 'every' reads each word as a start, a document holding one for each first", async () => {
			const courses = store.index<{ name: string }>('shortened', { fields: { name: 1 } })
			await courses.setMany([
				{ id: 1, value: { name: 'Линейная алгебра' } },
				{ id: 2, value: { name: 'Алгоритмизация и программирование' } },
				{ id: 3, value: { name: 'Проектирование технологических линий' } },
			])
			const found = async (text: string, prefix: boolean | 'every') =>
				ids(await courses.search({ text, prefix }))
			expect(await found('лин алг', 'every')).toEqual(['1', '3', '2'])
			expect(await found('алг лин', 'every')).toEqual(['1', '3', '2'])
			expect(await found('лин алгебра', 'every')).toEqual(['1', '3'])
			expect(await found('линейная алг', 'every')).toEqual(['1', '2'])
			expect(await found('лин алг', true)).toEqual(['3', '1', '2'])
		})

		// contract: SRCH-14
		test('a hit says where it matched, in UTF-16 units that slice its own strings', async () => {
			type Course = { name: string; short: string; tags?: string[] }
			const catalog = store.index<Course>('catalog', { fields: { name: 2, short: 1, tags: 1 } })
			await catalog.setMany([
				{ id: 1, value: { name: 'Линейная алгебра и аналитическая геометрия', short: 'ЛААГ' } },
				{ id: 2, value: { name: 'Криптографические методы защиты информации', short: 'КМЗИ' } },
				{ id: 3, value: { name: '🦀 Ёлка', short: '', tags: ['ёлки', 'пальмы'] } },
			])
			const sliced = (value: Course, match: SearchMatch) => {
				const field = value[match.field as keyof Course]
				const text = Array.isArray(field) ? field[match.item ?? 0] : field
				return text?.slice(match.start, match.end)
			}
			const shown = async (text: string) => {
				const found = await catalog.search({ text, prefix: true, matches: true })
				return found.hits.flatMap(hit =>
					(hit.matches ?? []).map(match => [
						hit.id,
						match.field,
						sliced(hit.value, match),
						match.word,
					]),
				)
			}
			expect(await shown('алгебре')).toEqual([['1', 'name', 'алгебра', 'алгебре']])
			expect(await shown('крипто')).toEqual([['2', 'name', 'Крипто', 'крипто']])
			expect(await shown('криптографичесике методы')).toEqual([
				['2', 'name', 'Криптографические', 'криптографичесике'],
				['2', 'name', 'методы', 'методы'],
			])
			// the crab before Ёлка is two units of UTF-16, one code point
			const [tree] = (await catalog.search({ text: 'елка', matches: true })).hits
			expect(tree?.matches).toEqual([
				{ field: 'name', start: 3, end: 7, word: 'елка' },
				{ field: 'tags', item: 0, start: 0, end: 4, word: 'елка' },
			])
			expect((await catalog.search('елка')).hits[0]?.matches).toBeUndefined()
		})

		// contract: SRCH-23
		test('a short form finds its phrase and the phrase its short form', async () => {
			const catalog = store.index<{ name: string }>('abbreviated', {
				fields: { name: 4 },
				synonyms: { кмзи: 'криптографические методы защиты информации', ml: 'machine learning' },
			})
			await catalog.set('c-1', { name: 'Криптографические методы защиты информации' })
			await catalog.set('c-2', { name: 'Экзамен по КМЗИ' })
			expect(ids(await catalog.search('кмзи')).sort()).toEqual(['c-1', 'c-2'])
			const [exam] = (
				await catalog.search({ text: 'защиты информации', matches: true })
			).hits.filter(hit => hit.id === 'c-2')
			const at = exam?.matches?.[0]
			expect(exam?.value.name.slice(at?.start, at?.end)).toBe('КМЗИ')
			const refused = await caught(
				store.index('refused', { synonyms: { 'machine learning': 'ml' } }).search('ml'),
			)
			expect(refused).toBeInstanceOf(InvalidError)
		})

		// contract: SRCH-22
		test("a hit's snippet is cut around its first match, its marks in UTF-16 units", async () => {
			const pages = store.index<{ title: string; plain: string }>('pages', {
				fields: { title: 2, plain: 1 },
			})
			const plain =
				'🦀 In the first term we read sets. Then linear algebra took three weeks of the term.'
			await pages.set('p-1', { title: 'Notes', plain })
			const found = await pages.search({ text: 'algebra', snippet: { field: 'plain', around: 12 } })
			const snippet = found.hits[0]?.snippet
			expect(snippet).toEqual({
				text: '…Then linear algebra took three…',
				marks: [{ start: 13, end: 20 }],
			})
			expect(found.hits[0]?.matches).toBeUndefined()
			const whole = await pages.search({ text: 'algebra', snippet: { field: 'plain' } })
			expect(whole.hits[0]?.snippet?.text).toBe(plain)
			expect(whole.hits[0]?.snippet?.marks).toEqual([{ start: 47, end: 54 }])
			const refused = await caught(pages.search({ text: 'algebra', snippet: { field: 'body' } }))
			expect(refused).toBeInstanceOf(InvalidError)
		})

		// contract: SRCH-25, SRCH-26, SRCH-27
		test('one question of several indexes answers a group an index and every hit merged by rank', async () => {
			const courses = store.index<{ title: string }>('box_courses', { fields: { title: 1 } })
			const pages = store.index<{ title: string; plain: string; space_id: number }>('box_pages', {
				fields: { title: 2, plain: 1 },
				filters: ['space_id'],
			})
			const people = store.index<{ name: string }>('box_people', { fields: { name: 1 } })
			await courses.setMany([
				{ id: 'c-1', value: { title: 'Linear algebra' } },
				{ id: 'c-2', value: { title: 'Algebra of logic' } },
			])
			await pages.setMany([
				{
					id: 'p-1',
					value: { title: 'Notes', plain: 'In the term we read linear algebra.', space_id: 7 },
				},
				{ id: 'p-2', value: { title: 'Algebra', plain: 'Another space.', space_id: 8 } },
			])
			await people.set('u-1', { name: 'Ada' })
			const found = await store.search({
				text: 'algebra',
				in: [
					{ index: courses, boost: 2, limit: 1 },
					{
						index: pages,
						where: { space_id: 7 },
						limit: 5,
						snippet: { field: 'plain', around: 12 },
					},
					{ index: people, limit: 5 },
				],
			})
			expect(found.groups.map(group => group.index)).toEqual([
				'box_courses',
				'box_pages',
				'box_people',
			])
			const [first, second, third] = found.groups
			if (first === undefined || 'error' in first || second === undefined || 'error' in second) {
				throw new Error('courses and pages answer')
			}
			expect([first.hits.length, first.more]).toEqual([1, true])
			expect(second.hits.map(hit => hit.id)).toEqual(['p-1'])
			expect(second.hits[0]?.snippet?.text).toBe('…read linear algebra.')
			expect(third !== undefined && !('error' in third) && third.hits).toEqual([])
			expect(found.best.map(best => [best.index, best.hit.id])).toEqual([
				['box_courses', first.hits[0]?.id ?? ''],
				['box_pages', 'p-1'],
			])
			expect(found.best[0]?.hit).toBe(first.hits[0] as never)
			await store.dropIndex('box_people')
			const failed = await store.search({
				text: 'algebra',
				in: [{ index: courses }, { index: people }],
			})
			const gone = failed.groups[1]
			expect(gone !== undefined && 'error' in gone && gone.error.message).toContain(
				'index box_people',
			)
			expect(failed.best.map(best => best.hit.id).sort()).toEqual(['c-1', 'c-2'])
			expect(await caught(store.search({ text: 'algebra', in: [] }))).toBeInstanceOf(InvalidError)
		})

		test('a hit keeps each half of its score, and a floor keeps a far vector out of the sum', async () => {
			const opinions = store.index<Product>('opinions', {
				fields: ['title'],
				vectors: { dimensions: 3 },
			})
			await opinions.set('dota', { title: 'dota is dead' }, { vector: [1, 0, 0] })
			await opinions.set('rain', { title: 'loves the rain' }, { vector: [0, 1, 0] })
			await opinions.set('patch', { title: 'the new dota patch' }, { vector: [0, 0, 1] })
			const far = [-1, -1, -1]
			const summed = await opinions.search({ text: 'dota', vector: far })
			const rain = summed.hits.find(hit => hit.id === 'rain')
			expect([rain?.score, rain?.textScore]).toEqual([0.5, undefined])
			expect(rain?.similarity).toBeCloseTo(-1 / Math.sqrt(3), 6)
			const floored = await opinions.search({ text: 'dota', vector: far, minSimilarity: 0.58 })
			expect(ids(floored)).toEqual(['dota', 'patch'])
			expect(
				floored.hits.every(hit => hit.textScore !== undefined && hit.similarity !== undefined),
			).toBe(true)
			const refused = opinions.search({
				text: 'dota',
				vector: far,
				minSimilarity: 0.58,
				hybrid: { rerank: true },
			})
			expect(await caught(refused)).toBeInstanceOf(InvalidError)
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

		// contract: SRCH-16
		test('a maintenance pass that keeps failing is told by the index status', async () => {
			const drafts = store.index<{ title: string; body: string }>('drafts', { fields: ['title'] })
			await drafts.setMany([
				{ id: 'a', value: { title: 'bread', body: 'salt' } },
				{ id: 'b', value: { title: 'rye', body: 'flour' } },
			])
			// as a full disk would, every pass fails: drafts' rebuild cannot write how far it read
			const beside = await besides(join(way.home(dir), 'search.db'))
			beside.exec(`create trigger full before update of copied_through on _tinystore_search_indexes
				when old.beside_of = (select id from _tinystore_search_indexes where name = 'drafts')
				begin insert into disk_full values (1); end`)
			try {
				// a field added wakes maintenance, whose every pass fails at the rebuild's first step
				const both = store.index<{ title: string; body: string }>('drafts', {
					fields: ['title', 'body'],
				})
				await until(async () => (await both.status()).error !== undefined)
				const status = await both.status()
				expect(status.state).toBe('rebuilding')
				expect(status.error).toContain('no such table: main.disk_full')
				expect(ids(await both.search('rye'))).toEqual(['b'])
			} finally {
				beside.exec('drop trigger full')
				beside.close()
			}
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

		// contract: SRCH-17
		test("a table's index keeps a search to the values a statement of its database gives", async () => {
			const db = await store.database('school', {
				migrations: {
					'0001_people.sql':
						'create table people (id integer primary key, name text not null, role text not null);' +
						'create table members (course integer not null, person integer not null)',
				},
			})
			await db.exec`insert into people (id, name, role) values (1, 'Ivanov', 'student'), (2, 'Ivanov', 'teacher'), (3, 'Ivanov', 'student')`
			await db.exec`insert into members (course, person) values (7, 1), (7, 2), (8, 3)`
			const people = db.searchIndex<{ id: number; name: string }>('people_search', {
				table: 'people',
				fields: ['name'],
				filters: ['role'],
			})
			await people.ready()
			const mine = sql`select person from members where course = ${7}`
			expect(ids(await people.search({ text: 'ivanov', where: { id: mine } }))).toEqual(['1', '2'])
			const roles = sql`select role from people where id = ${3}`
			const both = await people.search({ text: 'ivanov', where: { id: mine, role: roles } })
			expect(ids(both)).toEqual(['1'])
			const notes = store.index<string>('notes_statement')
			expect(await caught(notes.search({ text: 'x', where: { id: mine } }))).toBeInstanceOf(
				InvalidError,
			)
			expect(await db.dropSearchIndex('people_search')).toBe(true)
		})

		// contract: SRCH-19
		test("a table's index keyed by two columns names each hit by their values together", async () => {
			const db = await store.database('tasks', {
				migrations: {
					'0001_tasks.sql':
						'create table tasks (space text not null, n integer not null, title text not null, primary key (space, n))',
				},
			})
			await db.exec`insert into tasks values ('NW', 6, 'login screen'), ('EU', 6, 'login screen')`
			const tasks = db.searchIndex<{ space: string; n: number; title: string }>('tasks_search', {
				table: 'tasks',
				key: ['space', 'n'],
				fields: ['title'],
				filters: ['space'],
			})
			await tasks.ready()
			const found = await tasks.search({ text: 'screen', where: { space: 'NW' } })
			expect(ids(found)).toEqual(['["NW",6]'])
			expect(found.hits[0]?.value).toEqual({ space: 'NW', n: 6, title: 'login screen' })
			expect(JSON.parse(found.hits[0]?.id ?? '')).toEqual(['NW', 6])
			expect(await db.dropSearchIndex('tasks_search')).toBe(true)
		})

		// contract: SRCH-20
		test("a table's index opened with another table answers from the old one until the new one switches in", async () => {
			const db = await store.database('moves', {
				migrations: {
					'0001_posts.sql':
						'create table posts (id integer primary key, title text not null);' +
						'create table posts_v2 (slug text primary key, title text not null)',
				},
			})
			await db.exec`insert into posts values (1, 'rye bread')`
			await db.exec`insert into posts_v2 values ('wheat', 'wheat bread')`
			const old = db.searchIndex<{ title: string }>('posts_search', {
				table: 'posts',
				fields: ['title'],
			})
			await old.ready()
			const moved = db.searchIndex<{ slug: string; title: string }>('posts_search', {
				table: 'posts_v2',
				key: 'slug',
				fields: ['title'],
			})
			await moved.ready()
			expect(ids(await moved.search('bread'))).toEqual(['wheat'])
			expect(await db.dropSearchIndex('posts_search')).toBe(true)
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
