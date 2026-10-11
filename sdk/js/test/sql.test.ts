// sql as plan/api/sqldb.md has it over protocol 2, through each way a store
// is reached: migrations checked at every open, the verbs that say what comes
// back, writes, batches and transactions, and the values between JavaScript
// and SQLite.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import {
	ConflictError,
	type Constraint,
	type Database,
	InvalidError,
	LimitError,
	limits,
	type SqlTx,
	type Store,
	sql,
} from '../src/index.ts'
import { caught, removed, until, ways } from './ways.ts'

const NOTES = `create table notes (
	id         text primary key,
	author_id  integer not null,
	title      text not null,
	done       integer not null default 0 check (done in (0, 1)),
	tags       text not null default '[]' check (json_valid(tags)),
	created_at integer not null default 0,
	raw        blob
) strict`

interface Note {
	id: string
	author_id: number
	title: string
	done: number
	tags: string
	created_at: number
	raw: Uint8Array | null
}

for (const way of ways) {
	describe.skipIf(!way.ready)(`sql through ${way.name}`, () => {
		let dir: string
		let store: Store
		let app: Database

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-sql-'))
			const migrations = join(dir, 'migrations')
			mkdirSync(migrations)
			writeFileSync(join(migrations, '0001_notes.sql'), NOTES)
			store = await way.open(dir)
			app = await store.database('app', { migrations })
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		test('a write returns once it is durable, and the verbs say what comes back', async () => {
			const written =
				await app.exec`insert into notes (id, author_id, title) values (${'n1'}, ${42}, ${'Buy milk'})`
			expect(written.changes).toBe(1)
			await app.exec(
				'insert into notes (id, author_id, title) values (?, ?, ?)',
				'n2',
				42,
				'Call mom',
			)

			const mine = await app.all<Note>`select * from notes where author_id = ${42} order by id`
			expect(mine.map(note => note.title)).toEqual(['Buy milk', 'Call mom'])
			expect(await app.one<Note>`select * from notes where id = ${'none'}`).toBeUndefined()
			expect(await app.scalar<number>`select count(*) from notes`).toBe(2)
			const named = await app.one<Note>(
				'select * from notes where id = :id and author_id = :author',
				{
					id: 'n2',
					author: 42,
				},
			)
			expect(named?.title).toBe('Call mom')

			const done = await app.one<Note>`update notes set done = 1 where id = ${'n1'} returning *`
			expect(done?.done).toBe(1)
			const two = await caught(app.one`update notes set done = 1 returning id`)
			expect(two).toBeInstanceOf(InvalidError)
			expect(await app.scalar<number>`select count(*) from notes where done = 1`).toBe(1)
		})

		test('a key already held is a conflict, and the error names the database and the constraint', async () => {
			await app.exec`insert into notes (id, author_id, title) values (${'k1'}, 1, 'a')`
			const taken = await caught(
				app.exec`insert into notes (id, author_id, title) values (${'k1'}, 1, 'b')`,
			)
			expect(taken).toBeInstanceOf(ConflictError)
			expect((taken as Error).message).toContain('sql app')
			const key: Constraint = {
				kind: 'primaryKey',
				table: 'notes',
				columns: ['id'],
				name: undefined,
			}
			expect((taken as ConflictError).constraint).toEqual(key)
			const checked = await caught(
				app.tx(
					tx => tx.exec`insert into notes (id, author_id, title, done) values ('k2', 1, 'b', 7)`,
				),
			)
			expect(checked).toBeInstanceOf(InvalidError)
			expect((checked as InvalidError).constraint).toEqual({
				kind: 'check',
				table: undefined,
				columns: [],
				name: 'done in (0, 1)',
			})
			const missing = await caught(app.all`select * from no_such_table`)
			expect((missing as Error).message).toContain('no_such_table')
			expect((missing as InvalidError).constraint).toBeUndefined()
		})

		test('a batch writes all of its statements or none', async () => {
			const done = await app.batch([
				sql`insert into notes (id, author_id, title) values (${'b1'}, 7, 'a')`,
				sql('insert into notes (id, author_id, title) values (?, ?, ?)', 'b2', 7, 'b'),
			])
			expect(done.map(each => each.changes)).toEqual([1, 1])
			const refused = await caught(
				app.batch([
					sql`insert into notes (id, author_id, title) values (${'b3'}, 7, 'c')`,
					sql`insert into notes (id, author_id, title) values (${'b1'}, 7, 'taken')`,
				]),
			)
			expect(refused).toBeInstanceOf(ConflictError)
			expect(await app.scalar<number>`select count(*) from notes where author_id = 7`).toBe(2)
		})

		test('a statement changes nothing of the connection it runs on', async () => {
			const before = await app.scalar<number>`select count(*) from notes`
			for (const text of [
				"attach database 'other.db' as other",
				'commit',
				'pragma query_only = off',
			]) {
				expect(await caught(app.all(sql(text)))).toBeInstanceOf(InvalidError)
				expect(await caught(app.exec(sql(text)))).toBeInstanceOf(InvalidError)
			}
			const kept = sql`insert into notes (id, author_id, title) values (${'g1'}, 11, 'before the commit')`
			expect(await caught(app.batch([kept, sql`commit`]))).toBeInstanceOf(InvalidError)
			expect(await app.scalar<number>`select count(*) from notes`).toBe(before)
			expect((await app.all`pragma table_info(notes)`).length).toBeGreaterThan(0)
		})

		test('a transaction sees its own writes, commits what it returns and rolls back a throw', async () => {
			const titles = await app.tx(async tx => {
				await tx.exec`insert into notes (id, author_id, title) values (${'t1'}, 9, 'a')`
				const taken = await caught(
					tx.exec`insert into notes (id, author_id, title) values (${'t1'}, 9, 'b')`,
				)
				expect(taken).toBeInstanceOf(ConflictError)
				return tx.all<{ title: string }>`select title from notes where author_id = 9`
			})
			expect(titles).toEqual([{ title: 'a' }])
			expect(await app.scalar<number>`select count(*) from notes where author_id = 9`).toBe(1)

			const refused = await caught(
				app.tx(async tx => {
					await tx.exec`delete from notes where author_id = 9`
					throw new Error('sold out')
				}),
			)
			expect((refused as Error).message).toBe('sold out')
			expect(await app.scalar<number>`select count(*) from notes where author_id = 9`).toBe(1)
		})

		test('a value goes as SQLite keeps it, and one it would change is refused before it leaves', async () => {
			const raw = new Uint8Array([0, 255, 7])
			const at = new Date('2026-10-10T12:00:00.123Z')
			await app.exec`insert into notes (id, author_id, title, done, tags, created_at, raw)
				values (${'v1'}, ${2n ** 60n}, ${'x'}, ${true}, ${sql.json(['home'])}, ${at}, ${raw})`
			const kept = await app.one<
				Omit<Note, 'author_id'> & { author_id: bigint }
			>`select * from notes where id = ${'v1'}`
			expect(kept?.author_id).toBe(2n ** 60n)
			expect(kept?.done).toBe(1)
			expect(kept?.tags).toBe('["home"]')
			expect(kept?.created_at).toBe(at.getTime())
			expect(kept?.raw).toEqual(raw)

			for (const refused of [undefined, ['a'], { a: 1 }, 2 ** 60, Number.NaN]) {
				const error = await caught(app.all`select ${refused}`)
				expect(error).toBeInstanceOf(InvalidError)
			}
			expect(await caught(app.all('select ?, ?', 1))).toBeInstanceOf(InvalidError)
		})

		test('a database opens without migrations as it is, and refuses a migration changed after it was applied', async () => {
			const scratch = await store.database('scratch')
			expect(await scratch.scalar<number>`select 1`).toBe(1)
			const edited = await caught(
				store.database('app', { migrations: { '0001_notes.sql': `${NOTES};` } }),
			)
			expect(edited).toBeInstanceOf(InvalidError)
			expect((edited as Error).message).toContain('0001_notes.sql changed after it was applied')
		})

		test('each reads a query past one message a part at a time', async () => {
			// a batch is one message, so three of a thousand rows each
			for (let batch = 0; batch < 3; batch++) {
				const rows = Array.from(
					{ length: 1000 },
					(_, n) =>
						sql`insert into notes (id, author_id, title) values (${`e${batch}-${n}`}, 3, ${'x'.repeat(1000)})`,
				)
				await app.batch(rows)
			}
			let count = 0
			for await (const note of app.each<Note>`select * from notes where author_id = 3 order by id`) {
				expect(note.title.length).toBe(1000)
				count++
			}
			expect(count).toBe(3000)
			expect((await app.all`select id from notes where author_id = 3`).length).toBe(3000)
		})

		test('a statement gives by its columns what it gives by its rows', async () => {
			await app.exec`create table samples (ts integer not null, cpu real, hits integer, host text, raw blob) strict`
			await app.batch([
				sql`insert into samples values (${1000}, ${0.5}, ${7}, ${'a'}, ${new Uint8Array([1])})`,
				sql`insert into samples values (${2000}, ${null}, ${null}, ${null}, ${null})`,
				sql`insert into samples values (${3000}, ${2.5}, ${9}, ${'c'}, ${null})`,
			])
			interface Sample {
				ts: number
				cpu: number | null
				hits: number | null
				host: string | null
				raw: Uint8Array | null
				either: number
			}

			const read = sql`select ts, cpu, hits, host, raw, coalesce(cpu, 0) as either from samples order by ts`
			const columns = await app.columns<Sample>(read)
			expect<unknown>(columns).toEqual({
				ts: [1000, 2000, 3000],
				cpu: [0.5, null, 2.5],
				hits: [7, null, 9],
				host: ['a', null, 'c'],
				raw: [new Uint8Array([1]), null, null],
				// an INTEGER beside REALs
				either: [0.5, 0, 2.5],
			})
			const rows = await app.all<Sample>(read)
			for (const [name, column] of Object.entries(columns)) {
				expect(Array.isArray(column)).toBe(true)
				expect<unknown>(column).toEqual(rows.map(row => row[name as keyof Sample]))
			}

			const written =
				await app.columns`insert into samples (ts, cpu) values (${4000}, ${4.5}) returning ts, cpu`
			expect<unknown>(written).toEqual({ ts: [4000], cpu: [4.5] })
			const twice = await caught(app.columns`select ts, ts from samples`)
			expect(twice).toBeInstanceOf(InvalidError)
			expect((twice as Error).message).toContain('two columns named ts')
		})

		test('a column of no value is an array as any other, and an integer past 2^53 a bigint', async () => {
			const none = await app.columns`select ts, cpu, host, cpu + 1 as more from samples where 0`
			expect<unknown>(none).toEqual({ ts: [], cpu: [], host: [], more: [] })
			const nulls =
				await app.columns`select cpu, hits, host, cpu + 1 as more from samples where ts = 2000`
			expect<unknown>(nulls).toEqual({ cpu: [null], hits: [null], host: [null], more: [null] })

			const edges = await app.columns`
				select 9007199254740991 as most, -9007199254740991 as least, -9007199254740992 as below`
			expect<unknown>(edges).toEqual({
				most: [Number.MAX_SAFE_INTEGER],
				least: [Number.MIN_SAFE_INTEGER],
				below: [-9007199254740992n],
			})
			const late = sql`select case ts when 1000 then null when 3000 then 9007199254740993 else ts end as late
				from samples where ts <= 3000 order by ts`
			expect<unknown>(await app.columns(late)).toEqual({ late: [null, 2000, 9007199254740993n] })
			expect((await app.all(late)).map(row => row.late)).toEqual([null, 2000, 9007199254740993n])
		})

		test('columns past one message come in parts, and are whole', async () => {
			const rows = 150_000
			const columns = await app.columns<{
				i: number
				half: number
				even: number | null
				name: string
			}>`with recursive n(i) as (select 1 union all select i + 1 from n where i < ${rows})
				select i, i / 2.0 as half, iif(i % 2, null, i) as even, 'r' || i as name from n`
			expect(Object.values(columns).map(column => column.length)).toEqual([rows, rows, rows, rows])
			for (const row of [0, 1, 77, 65_535, 65_536, 149_998, 149_999]) {
				expect(columns.i[row]).toBe(row + 1)
				expect(columns.half[row]).toBe((row + 1) / 2)
				expect(columns.even[row]).toBe(row % 2 === 0 ? null : row + 1)
				expect(columns.name[row]).toBe(`r${row + 1}`)
			}
			// no row was left unfilled between two parts
			expect(columns.half.findIndex(half => half === undefined)).toBe(-1)
		})

		test('a query past its bound is a limit, which names the bound', async () => {
			const error = await caught(app.all`select zeroblob(70 * 1024 * 1024)`)
			expect(error).toBeInstanceOf(LimitError)
			const limit = error as LimitError
			expect([limit.limit, limit.bound]).toEqual([limits.rowBytes, 64 * 1024 * 1024])
			expect(limit.wanted).toBeGreaterThan(64 * 1024 * 1024)
		})

		interface Typed {
			id: string
			author_id: number
			title: string
			done: boolean
			tags: string[]
			created_at: Date
			raw: Uint8Array | null
		}
		const types = { done: 'bool', tags: 'json', created_at: 'time' } as const

		test('a table inserts rows and gives them back in their types', async () => {
			const notes = app.table<Typed>('notes', { types })
			const at = new Date(1_791_547_200_123)
			const note = await notes.insert({
				id: 'tb1',
				author_id: 5,
				title: 'Buy milk',
				done: false,
				tags: ['home'],
				created_at: at,
			})
			expect(note).toEqual({
				id: 'tb1',
				author_id: 5,
				title: 'Buy milk',
				done: false,
				tags: ['home'],
				created_at: at,
				raw: null,
			})
			const two = await notes.insert([
				{ id: 'tb2', author_id: 5, title: 'Call mom', done: true, tags: [], created_at: at },
				{ id: 'tb3', author_id: 5, title: 'Read', done: false, tags: ['books'], created_at: at },
			])
			expect(two.map(row => row.done)).toEqual([true, false])
			expect(await notes.where({ author_id: 5, done: false }).count()).toBe(2)
			const titles = await notes
				.where({ author_id: 5 })
				.orderBy('id', 'desc')
				.select<{ title: string }>('title')
				.all()
			expect(titles).toEqual([{ title: 'Read' }, { title: 'Call mom' }, { title: 'Buy milk' }])
			const taken = await caught(
				notes.insert({ id: 'tb1', author_id: 5, title: 'again', created_at: at }),
			)
			expect(taken).toBeInstanceOf(ConflictError)
			let seen = 0
			for await (const row of notes.where({ author_id: 5 }).where(sql.has('tags', 'home')).each()) {
				expect(row.tags).toEqual(['home'])
				seen++
			}
			expect(seen).toBe(1)
		})

		test('an update or a delete changes what its condition says, and refuses every row', async () => {
			const notes = app.table<Typed>('notes', { types })
			await notes.insert({ id: 'ud1', author_id: 6, title: 'a', created_at: new Date() })
			await notes.insert({ id: 'ud2', author_id: 6, title: 'b', created_at: new Date() })
			const updated = await notes
				.where({ id: 'ud1' })
				.update({ done: true, title: sql`title || '!'` })
			expect(updated.changes).toBe(1)
			expect(await notes.where({ id: 'ud1' }).one()).toMatchObject({ done: true, title: 'a!' })

			expect(await caught(notes.delete())).toBeInstanceOf(InvalidError)
			expect(await caught(notes.where({}).delete())).toBeInstanceOf(InvalidError)
			expect(await caught(notes.where(sql.and()).update({ title: 'x' }))).toBeInstanceOf(
				InvalidError,
			)
			expect(() => notes.where({ author_id: undefined })).toThrow(InvalidError)
			expect(
				await caught(notes.where({ id: 'ud1' }).update({ title: undefined as unknown as string })),
			).toBeInstanceOf(InvalidError)
			expect((await notes.where({ author_id: 6, done: true }).delete()).changes).toBe(1)
			expect(await notes.where({ author_id: 6 }).count()).toBe(1)
		})

		test('an upsert sets only what it names, on the row its owner holds', async () => {
			const notes = app.table<Typed>('notes', { types })
			const options = { conflict: 'id', update: ['title'] as const, where: { author_id: 1 } }
			const made = await notes.upsert(
				{ id: 'up1', author_id: 1, title: 'mine', created_at: new Date() },
				options,
			)
			expect(made?.title).toBe('mine')
			const changed = await notes.upsert(
				{ id: 'up1', author_id: 1, title: 'still mine', done: true },
				options,
			)
			expect([changed?.title, changed?.done]).toEqual(['still mine', false])
			const stolen = await notes.upsert(
				{ id: 'up1', author_id: 2, title: 'stolen' },
				{ ...options, where: { author_id: 2 } },
			)
			expect(stolen).toBeUndefined()
			expect((await notes.where({ id: 'up1' }).one())?.title).toBe('still mine')
		})

		test('a page reads after the last row of the one before, and only its own query', async () => {
			const notes = app.table<Typed>('notes', { types })
			for (let n = 0; n < 5; n++) {
				await notes.insert({
					id: `pg${n}`,
					author_id: 8,
					title: `${n}`,
					created_at: new Date(1000 + Math.floor(n / 2)),
				})
			}
			const query = notes
				.where({ author_id: 8 })
				.orderBy('created_at', 'desc')
				.orderBy('id', 'desc')
			const first = await query.list({ limit: 2 })
			const second = await query.list({ limit: 2, after: first.next })
			const third = await query.list({ limit: 2, after: second.next })
			expect([first, second, third].flatMap(page => page.rows.map(row => row.id))).toEqual([
				'pg4',
				'pg3',
				'pg2',
				'pg1',
				'pg0',
			])
			expect(third.next).toBeUndefined()
			const other = notes
				.where({ author_id: 9 })
				.orderBy('created_at', 'desc')
				.orderBy('id', 'desc')
			expect(await caught(other.list({ limit: 2, after: first.next }))).toBeInstanceOf(InvalidError)
		})

		test('a join reads its first table, a query in a piece is a subquery, and a plan explains it', async () => {
			const library = await store.database('library', {
				migrations: {
					'0001_books.sql': `create table authors (id integer primary key, name text not null, active integer not null) strict;
						create table books (id integer primary key, author_id integer not null, title text not null) strict;`,
				},
			})
			await library.batch([
				sql`insert into authors (id, name, active) values (1, 'Le Guin', 1), (2, 'Lem', 0)`,
				sql`insert into books (id, author_id, title) values (1, 1, 'The Dispossessed'), (2, 2, 'Solaris')`,
			])
			const books = await library
				.table<{ id: number; title: string }>('books as b')
				.join('authors as a', 'a.id = b.author_id')
				.where({ 'a.active': 1 })
				.all()
			expect(books).toEqual([{ id: 1, author_id: 1, title: 'The Dispossessed' } as never])
			const written = library.table('books as b').select('1').where('b.author_id = a.id')
			const writers = await library
				.table<{ name: string }>('authors as a')
				.where(sql`exists ${written}`)
				.orderBy('name')
				.all()
			expect(writers.map(writer => writer.name)).toEqual(['Le Guin', 'Lem'])
			const plan = await library.table('books').where({ author_id: 1 }).explain()
			expect(plan.length).toBeGreaterThan(0)
		})

		test('a table inside a transaction writes with it, and rolls back with it', async () => {
			const refused = await caught(
				app.tx(async tx => {
					await tx
						.table<Typed>('notes', { types })
						.insert({ id: 'tx1', author_id: 4, title: 'a', created_at: new Date() })
					expect(await tx.table('notes').where({ id: 'tx1' }).count()).toBe(1)
					throw new Error('changed my mind')
				}),
			)
			expect((refused as Error).message).toBe('changed my mind')
			expect(await app.table('notes').where({ id: 'tx1' }).count()).toBe(0)
		})

		test('an included query gives each row its rows, every value as it is kept', async () => {
			const db = await store.database('included', {
				migrations: {
					'0001_included.sql': `create table authors (id integer primary key, name text not null) strict;
						create table posts (id integer primary key, author_id integer not null, title text not null,
							score real, big integer, raw blob, done integer not null default 0) strict;`,
				},
			})
			const odd = `it's, (odd)\u0000 "text"`
			await db.exec`insert into authors (id, name) values (1, 'ann'), (2, 'bob'), (3, 'eve')`
			await db.exec`insert into posts (id, author_id, title, score, big, raw, done) values
				(1, 1, 'first', ${0.1 + 0.2}, ${2n ** 60n + 1n}, ${new Uint8Array([0, 255, 39])}, 1),
				(2, 1, ${odd}, 1e308, null, null, 0),
				(3, 1, 'third', null, -5, x'', 0),
				(4, 2, 'only', 2.5, 7, null, 1)`

			interface Post {
				id: number
				title: string
				score: number | null
				big: number | bigint | null
				raw: Uint8Array | null
				done: boolean
			}
			const latest = db
				.table('posts as p', { types: { done: 'bool' } })
				.select<Post>('p.id, p.title, p.score, p.big, p.raw, p.done')
				.where('p.author_id = a.id')
				.orderBy('p.id', 'desc')
				.limit(2)
			const authors = await db
				.table<{ id: number; name: string }>('authors as a')
				.include('posts', latest)
				.orderBy('a.id')
				.all()
			expect(authors.map(author => [author.name, author.posts.map(post => post.id)])).toEqual([
				['ann', [3, 2]],
				['bob', [4]],
				['eve', []],
			])
			const [third, second] = authors[0]?.posts ?? []
			expect(second).toEqual({ id: 2, title: odd, score: 1e308, big: null, raw: null, done: false })
			expect(third).toEqual({
				id: 3,
				title: 'third',
				score: null,
				big: -5,
				raw: new Uint8Array(0),
				done: false,
			})

			const ann = await db
				.table('authors as a')
				.select<{ name: string }>('a.name')
				.include('posts', latest.limit(3))
				.where({ 'a.id': 1 })
				.one()
			expect(ann?.posts[2]).toEqual({
				id: 1,
				title: 'first',
				score: 0.1 + 0.2,
				big: 2n ** 60n + 1n,
				raw: new Uint8Array([0, 255, 39]),
				done: true,
			})
			const inTx = await db.tx(tx =>
				tx.table('authors as a').include('posts', latest).where({ 'a.id': 2 }).one(),
			)
			expect(inTx?.posts.map(post => post.title)).toEqual(['only'])

			const posts = db.table('posts as p')
			const authorsTable = db.table('authors as a')
			expect(() => authorsTable.include('posts', posts.limit(1))).toThrow(InvalidError)
			expect(() => authorsTable.include('posts', posts.select('p.id'))).toThrow(InvalidError)
			expect(() => authorsTable.include('posts', posts.select('count(*)').limit(1))).toThrow(
				InvalidError,
			)
		})

		test('a bucket and a queue opened from the database commit with its rows, or roll back with them', async () => {
			const sessions = app.bucket<{ user: string }>('sessions')
			const emails = app.queue<{ note: string }>('emails')
			await sessions.set('t-0', { user: 'ann' })
			expect(await sessions.get('t-0')).toEqual({ user: 'ann' })

			const insert = (tx: SqlTx) =>
				tx.exec`insert into notes (id, author_id, title) values ('q1', 5, 'a')`
			const refused = await caught(
				app.tx(async tx => {
					await insert(tx)
					await tx.with(sessions).set('t-1', { user: 'bob' })
					expect(await tx.with(emails).add({ note: 'q1' }, { id: 'q1' })).toBe(true)
					expect(await tx.with(sessions).get('t-1')).toEqual({ user: 'bob' })
					expect((await tx.with(emails).get('q1'))?.state).toBe('waiting')
					throw new Error('declined')
				}),
			)
			expect((refused as Error).message).toBe('declined')
			expect(await sessions.has('t-1')).toBe(false)
			expect(await emails.get('q1')).toBeUndefined()
			expect(await app.scalar<number>`select count(*) from notes where id = 'q1'`).toBe(0)

			await app.tx(async tx => {
				await insert(tx)
				await tx.with(sessions).set('t-1', { user: 'bob' })
				await tx.with(emails).add({ note: 'q1' }, { id: 'q1' })
			})
			expect(await sessions.get('t-1')).toEqual({ user: 'bob' })
			expect((await emails.get('q1'))?.value).toEqual({ note: 'q1' })
			expect(await store.bucket('sessions').has('t-1')).toBe(false)
		})

		test('a transaction refuses a handle kept elsewhere, or one made after it began', async () => {
			const kept = await caught(app.tx(tx => tx.with(store.bucket<number>('sessions')).set('k', 1)))
			expect(kept).toBeInstanceOf(InvalidError)
			expect((kept as Error).message).toContain(
				'kept in the store, outside this transaction of sql app',
			)
			const late = await caught(app.tx(tx => tx.with(app.bucket<number>('late')).set('k', 1)))
			expect(late).toBeInstanceOf(InvalidError)
			expect((late as Error).message).toContain('make it before db.tx')
			const inDatabase = app.bucket<number>('sessions')
			expect(await caught(store.tx(tx => tx.with(inDatabase).set('k', 1)))).toBeInstanceOf(
				InvalidError,
			)
		})

		test('a call around a transaction from inside it is refused, and one beside it waits its turn', async () => {
			const emails = app.queue<{ note: string }>('around')
			const sessions = app.bucket<{ user: string }>('around')
			const notes = app.table('notes')
			const around = [
				() => app.exec`delete from notes where id = 'none'`,
				() => app.all`select 1`,
				() => app.batch([sql`delete from notes where id = 'none'`]),
				() => app.tx(async () => {}),
				() => notes.count(),
				() => emails.add({ note: 'around' }),
				() => sessions.get('k'),
			]
			let release = () => {}
			const gate = new Promise<void>(resolve => {
				release = resolve
			})
			const held = app.tx(async tx => {
				for (const call of around) {
					const refused = await caught(call())
					expect(refused).toBeInstanceOf(InvalidError)
					expect((refused as Error).message).toContain('make it through the transaction')
				}
				await tx.with(emails).add({ note: 'inside' }, { id: 'inside' })
				await gate
			})
			// a call of another flow is no call from inside: it waits for the writer, and is made
			const beside = emails.add({ note: 'beside' }, { id: 'beside' })
			release()
			await held
			expect(await beside).toBe(true)
			expect((await emails.get('inside'))?.state).toBe('waiting')
			for (const call of around) {
				await call()
			}
		})

		test('a worker runs a job its transaction added, once the transaction commits', async () => {
			const sent = app.queue<{ note: string }>('sent')
			const seen: string[] = []
			const worker = sent.work(async email => {
				seen.push(email.note)
			})
			await app.tx(async tx => {
				await tx.with(sent).add({ note: 'w1' })
			})
			await until(() => seen.length === 1)
			await worker.stop()
			expect(seen).toEqual(['w1'])
		})
	})
}
