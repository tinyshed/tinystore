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
	type Database,
	InvalidError,
	LimitError,
	type Store,
	sql,
} from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

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

		test('a key already held is a conflict, and the error names the database', async () => {
			await app.exec`insert into notes (id, author_id, title) values (${'k1'}, 1, 'a')`
			const taken = await caught(
				app.exec`insert into notes (id, author_id, title) values (${'k1'}, 1, 'b')`,
			)
			expect(taken).toBeInstanceOf(ConflictError)
			expect((taken as Error).message).toContain('sql app')
			const missing = await caught(app.all`select * from no_such_table`)
			expect((missing as Error).message).toContain('no_such_table')
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

		test('a query past its bound is a limit', async () => {
			const error = await caught(app.all`select zeroblob(70 * 1024 * 1024)`)
			expect(error).toBeInstanceOf(LimitError)
		})
	})
}
