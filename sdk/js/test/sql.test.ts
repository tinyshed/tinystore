// sql through a real tinystore serve, the one test/binary.ts built.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { ConflictError, type Database, InvalidError, open, type Store } from '../src/index.ts'

let dir: string
let store: Store
let app: Database

async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

interface Note {
	id: number
	author_id: number
	title: string
	done: number
	weight: number | null
	raw: Uint8Array | null
}

beforeAll(async () => {
	dir = mkdtempSync(join(tmpdir(), 'tinystore-sql-'))
	const migrations = join(dir, 'migrations')
	mkdirSync(migrations)
	writeFileSync(
		join(migrations, '001_notes.sql'),
		`create table notes (
			id integer primary key,
			author_id integer not null,
			title text not null unique,
			done integer not null default 0,
			weight real,
			raw blob
		) strict;`,
	)
	store = await open(join(dir, 'data'), { private: true })
	app = await store.sql('app', { migrations })
})

afterAll(async () => {
	await store.close()
	rmSync(dir, { recursive: true, force: true })
})

describe('statements', () => {
	test('an insert, a template, named arguments and every kind of value', async () => {
		const done = await app.exec(
			'insert into notes (author_id, title, weight, raw) values (?, ?, ?, ?)',
			7,
			'milk',
			2.5,
			new Uint8Array([0, 1]),
		)
		expect(done).toEqual({ changes: 1, lastId: 1 })
		await app.exec('insert into notes (author_id, title) values (:author, :title)', {
			author: 7,
			title: 'bread',
		})
		const author = 7
		const mine = await app.all<Note>`select * from notes where author_id = ${author} order by id`
		expect(mine.map(n => n.title)).toEqual(['milk', 'bread'])
		expect(mine[0]?.weight).toBe(2.5)
		expect(mine[0]?.raw).toEqual(new Uint8Array([0, 1]))
		expect(mine[1]?.weight).toBeNull()
		expect(await app.scalar<number>('select count(*) from notes')).toBe(2)
		expect((await app.one<Note>('select * from notes where title = ?', 'milk'))?.id).toBe(1)
		expect(await app.one('select * from notes where title = ?', 'nothing')).toBeUndefined()
	})

	test('a query for one row that answers two is refused', async () => {
		expect(await caught(app.one('select * from notes'))).toBeInstanceOf(InvalidError)
	})

	test('a unique constraint is a conflict, and a statement SQLite refuses is invalid', async () => {
		expect(
			await caught(app.exec("insert into notes (author_id, title) values (1, 'milk')")),
		).toBeInstanceOf(ConflictError)
		expect(await caught(app.all('select nothing from nowhere'))).toBeInstanceOf(InvalidError)
	})

	test('a returning clause answers from the writer; integers past a number come as bigints', async () => {
		const row = await app.execOne<{ id: number }>(
			"insert into notes (author_id, title) values (9, 'eggs') returning id",
		)
		expect(row?.id).toBe(3)
		expect(await app.scalar<bigint>('select 9007199254740993')).toBe(9007199254740993n)
		expect(await app.scalar<bigint>('select ?', 1n << 62n)).toBe(1n << 62n)
	})

	test('each reads rows one at a time', async () => {
		const titles = []
		for await (const note of app.each<Note>('select * from notes order by id')) {
			titles.push(note.title)
		}
		expect(titles).toEqual(['milk', 'bread', 'eggs'])
	})
})

describe('batches', () => {
	test('a batch runs whole or not at all, a failure naming its statement', async () => {
		let inserted: Promise<{ lastId: number }> | undefined
		let count: Promise<number> | undefined
		await app.batch(tx => {
			inserted = tx.exec("insert into notes (author_id, title) values (1, 'tea')")
			count = tx.scalar<number>('select count(*) from notes')
		})
		expect((await inserted)?.lastId).toBe(4)
		expect(await count).toBe(4)

		const err = await caught(
			app.batch(tx => {
				tx.exec("insert into notes (author_id, title) values (1, 'coffee')")
				tx.exec("insert into notes (author_id, title) values (1, 'tea')")
			}),
		)
		expect(err).toBeInstanceOf(ConflictError)
		expect((err as ConflictError).what).toMatchObject({ call: '1' })
		expect(await app.one("select * from notes where title = 'coffee'")).toBeUndefined()
	})

	test('a view reads from one snapshot', async () => {
		let notes: Promise<Note[]> | undefined
		let count: Promise<number> | undefined
		await app.view(tx => {
			notes = tx.all<Note>('select * from notes')
			count = tx.scalar<number>('select count(*) from notes')
		})
		expect((await notes)?.length).toBe(await count)
	})

	test('a batch and a view give back what their function returns, its promises answered', async () => {
		const [jam] = await app.batch(tx => [
			tx.one<Note>("insert into notes (author_id, title) values (1, 'jam') returning *"),
		])
		expect(jam?.title).toBe('jam')
		const [notes, count] = await app.view(tx => [
			tx.all<Note>('select * from notes'),
			tx.scalar<number>('select count(*) from notes'),
		])
		const titles: string[] = notes.map(note => note.title)
		expect(titles).toContain('jam')
		expect(titles.length).toBe(count)
		const awaited = app.view(async tx => [await tx.all('select * from notes')])
		expect(await caught(awaited)).toBeInstanceOf(InvalidError)
	})

	test('a second open checks the migrations it carries against the file', async () => {
		const again = await store
			.sql('app', { migrations: { '001_notes.sql': 'something else' } })
			.catch(e => e)
		expect(again).toBeInstanceOf(InvalidError)
		expect((await store.sql('app')).name).toBe('app')
	})

	test('a database opens without migrations, empty, to try a query', async () => {
		const scratch = await store.sql('scratch')
		expect(await scratch.scalar<number>`select 1`).toBe(1)
		await scratch.exec`create table tries (id integer primary key)`
		await scratch.exec`insert into tries (id) values (${7})`
		expect(await scratch.all`select id from tries`).toEqual([{ id: 7 }])
	})
})

describe('jobs in the database', () => {
	test('a job a batch enqueues commits with the rows or not at all', async () => {
		const index = store.jobs.queue<{ id: number }>('index', { in: app })
		await app.batch(tx => {
			tx.exec("insert into notes (author_id, title) values (1, 'indexed')")
			index.withTx(tx).enqueue({ id: 1 }, { key: 'note:1' })
		})
		const failed = await caught(
			app.batch(tx => {
				index.withTx(tx).enqueue({ id: 2 }, { key: 'note:2' })
				tx.exec("insert into notes (author_id, title) values (1, 'indexed')")
			}),
		)
		expect(failed).toBeInstanceOf(ConflictError)
		expect(await index.get('note:1')).toMatchObject({ value: { id: 1 } })
		expect(await index.get('note:2')).toBeUndefined()
		expect(await app.scalar<number>("select count(*) from notes where title = 'indexed'")).toBe(1)

		// the queue works outside a batch too, and a batch of jobs alone is one
		await index.enqueue({ id: 3 }, { key: 'note:3' })
		await app.batch(tx => {
			index.withTx(tx).enqueue({ id: 4 }, { key: 'note:4' })
		})
		expect(await index.get('note:4')).toMatchObject({ value: { id: 4 } })

		const elsewhere = store.jobs.queue<number>('elsewhere')
		const refused = await caught(
			app.batch(tx => {
				elsewhere.withTx(tx).enqueue(1)
			}),
		)
		expect(refused).toBeInstanceOf(InvalidError)
		const viewed = await caught(
			app.view(tx => {
				index.withTx(tx).enqueue({ id: 5 })
			}),
		)
		expect(viewed).toBeInstanceOf(InvalidError)
	})
})

describe('kv in the database', () => {
	test('a batch rejects buckets and queues from another store with the same database name', async () => {
		const otherDir = mkdtempSync(join(tmpdir(), 'tinystore-sql-other-'))
		const other = await open(otherDir, { private: true })
		try {
			const otherDB = await other.sql('app')
			expect(() => store.kv.bucket('wrong-store', 'string', { in: otherDB })).toThrow(InvalidError)
			expect(() => store.jobs.queue<number>('wrong-store', { in: otherDB })).toThrow(InvalidError)
			const sessions = store.kv.bucket('cross-store', 'string', { in: app })
			const queue = store.jobs.queue<number>('cross-store', { in: app })
			expect(
				await caught(otherDB.batch(tx => sessions.withTx(tx).set('K7Q2', 'user 1'))),
			).toBeInstanceOf(InvalidError)
			expect(await caught(otherDB.batch(tx => queue.withTx(tx).enqueue(1)))).toBeInstanceOf(
				InvalidError,
			)
			expect(
				await other.kv.bucket('cross-store', 'string', { in: otherDB }).get('K7Q2'),
			).toBeUndefined()
		} finally {
			await other.close()
			rmSync(otherDir, { recursive: true, force: true })
		}
	})

	test('a key a batch writes commits with the rows or not at all', async () => {
		const sessions = store.kv.bucket<string>('sessions', { in: app })
		await app.batch(tx => {
			tx.exec("insert into notes (author_id, title) values (1, 'signed in')")
			sessions.withTx(tx).set('K7Q2', 'user 1')
		})
		const failed = await caught(
			app.batch(tx => {
				sessions.withTx(tx).set('P9X4', 'user 2')
				tx.exec("insert into notes (author_id, title) values (1, 'signed in')")
			}),
		)
		expect(failed).toBeInstanceOf(ConflictError)
		expect(await sessions.get('K7Q2')).toBe('user 1')
		expect(await sessions.get('P9X4')).toBeUndefined()

		await app.batch(tx => {
			tx.exec("delete from notes where title = 'signed in'")
			sessions.withTx(tx).delete('K7Q2')
		})
		expect(await sessions.get('K7Q2')).toBeUndefined()
		await sessions.set('Z1A8', 'user 3')
		await app.batch(tx => {
			sessions.withTx(tx).clear()
		})
		expect(await sessions.get('Z1A8')).toBeUndefined()

		const elsewhere = store.kv.bucket<string>('elsewhere')
		expect(await caught(app.batch(tx => elsewhere.withTx(tx).set('K7Q2', 'x')))).toBeInstanceOf(
			InvalidError,
		)
		expect(await caught(app.batch(tx => sessions.withTx(tx).get('K7Q2')))).toBeInstanceOf(
			InvalidError,
		)
		expect(await caught(app.view(tx => sessions.withTx(tx).set('K7Q2', 'x')))).toBeInstanceOf(
			InvalidError,
		)
	})
})
