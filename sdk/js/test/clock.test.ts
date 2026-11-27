// A test's clock: a private store runs on it, and a test moves it forward
// instead of waiting.

import { expect, test } from 'bun:test'
import { existsSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { InvalidError, open } from '../src/index.ts'
import { openTemp } from '../src/testing.ts'
import { until, ways } from './ways.ts'

const inProcess = ways[0]!
const library = process.env.TINYSTORE_LIBRARY

async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

test('a private store runs on the clock it is given, which moves only forward', async () => {
	const dir = mkdtempSync(join(tmpdir(), 'tinystore-clock-'))
	await using store = await open(dir, { private: true, clock: new Date('2026-10-03T09:00:00Z') })
	expect((await store.clock.now()).toISOString()).toBe('2026-10-03T09:00:00.000Z')

	const codes = store.bucket<number>('codes')
	await codes.set('K7Q2', 42, { ttl: '15m' })
	const reminders = store.queue<{ userId: number }>('reminders')
	await reminders.add({ userId: 42 }, { delay: '1h' })
	const ran: number[] = []
	const remind = async ({ userId }: { userId: number }) => {
		ran.push(userId)
	}
	expect(await reminders.runDue(remind)).toBe(0) // nothing is due yet
	expect(ran).toEqual([])

	expect((await store.clock.advance('1h')).toISOString()).toBe('2026-10-03T10:00:00.000Z')
	expect(await codes.get('K7Q2')).toBeUndefined() // expired 45 minutes ago, on the store's clock
	expect(await reminders.runDue(remind)).toBe(1)
	expect(ran).toEqual([42])

	expect(await caught(store.clock.set(new Date('2026-10-03T09:30:00Z')))).toBeInstanceOf(
		InvalidError,
	)
	const set = await store.clock.set(new Date('2026-10-04T00:00:00Z'))
	expect(set.toISOString()).toBe('2026-10-04T00:00:00.000Z')
})

test('a running worker runs at once a job the moved clock made due', async () => {
	const dir = mkdtempSync(join(tmpdir(), 'tinystore-clock-'))
	await using store = await open(dir, { private: true, clock: new Date('2026-10-03T09:00:00Z') })
	const reminders = store.queue<{ userId: number }>('reminders')
	await reminders.add({ userId: 1 })
	await reminders.add({ userId: 42 }, { delay: '1h' })
	const ran: number[] = []
	const worker = reminders.work(async ({ userId }) => {
		ran.push(userId)
	})
	await until(() => ran.length === 1) // the worker runs, and sleeps until the later job
	await store.clock.advance('1h')
	await until(() => ran.length === 2)
	expect(ran).toEqual([1, 42])
	await worker.stop()
})

test("a clock is a private or an embedded store's, and a store on the system's time refuses to move one", async () => {
	const shared = open(mkdtempSync(join(tmpdir(), 'tinystore-clock-')), { clock: new Date() })
	expect(await caught(shared)).toBeInstanceOf(InvalidError)
	const both = open(mkdtempSync(join(tmpdir(), 'tinystore-clock-')), {
		private: true,
		embedded: true,
	})
	expect(await caught(both)).toBeInstanceOf(InvalidError)
	await using plain = await open(mkdtempSync(join(tmpdir(), 'tinystore-clock-')), { private: true })
	expect(await caught(plain.clock.advance('1h'))).toBeInstanceOf(InvalidError)
})

// contract: STORE-12
test.skipIf(!inProcess.ready)(
	'a temporary store runs in this process on the clock it is given, in a directory its close removes',
	async () => {
		const store = await openTemp({ library, clock: new Date('2026-11-19T09:00:00Z') })
		expect(store.dir).toContain('tinystore-')
		expect(existsSync(join(store.dir, 'LOCK'))).toBe(true)
		const codes = store.bucket<number>('codes')
		await codes.set('K7Q2', 42, { ttl: '15m' })
		expect((await store.clock.advance('1h')).toISOString()).toBe('2026-11-19T10:00:00.000Z')
		expect(await codes.get('K7Q2')).toBeUndefined() // expired on the store's clock, in this process
		await store.close()
		expect(existsSync(store.dir)).toBe(false)
	},
)

test.skipIf(process.env.TINYSTORE_BIN === undefined)(
	'a temporary store asked to be private runs in a child, and goes with it',
	async () => {
		let dir: string
		{
			await using store = await openTemp({ private: true, clock: new Date('2026-11-19T09:00:00Z') })
			dir = store.dir
			expect((await store.clock.now()).toISOString()).toBe('2026-11-19T09:00:00.000Z')
		}
		expect(existsSync(dir)).toBe(false)
	},
)

// contract: SQL-25
test.skipIf(process.env.TINYSTORE_BIN === undefined)(
	"a table's delete and a mirror's gone mark with the store's clock, in the unit their column names",
	async () => {
		const at = Date.parse('2026-11-27T09:00:00.750Z')
		await using store = await openTemp({ private: true, clock: new Date(at) })
		const db = await store.database('uni', {
			migrations: [
				{
					name: '0001_works.sql',
					sql: `create table works (id text primary key, grade text, gone_at integer);
						create table tasks (id integer primary key, title text, deleted_at integer)`,
				},
			],
		})
		const works = db.mirror<{ id: string; grade: string }>('works', {
			key: 'id',
			gone: { column: 'gone_at', unit: 's' },
		})
		await works.replace({}, [
			{ id: 'a', grade: '4' },
			{ id: 'b', grade: '5' },
		])
		await works.replace({}, [{ id: 'a', grade: '4' }])
		const seconds = Math.floor(at / 1000)
		expect(await db.scalar<number>`select gone_at from works where id = 'b'`).toBe(seconds)

		const tasks = db.table<{ id: number; title: string }>('tasks', {
			deleted: { column: 'deleted_at', unit: 's' },
		})
		await tasks.insert([
			{ id: 1, title: 'draft' },
			{ id: 2, title: 'plan' },
		])
		await store.clock.advance('1d')
		await tasks.where({ id: 1 }).delete()
		const day = 86_400
		expect(await db.scalar<number>`select deleted_at from tasks where id = 1`).toBe(seconds + day)
		await db.table('tasks', { deleted: 'deleted_at' }).where({ id: 2 }).delete()
		expect(await db.scalar<number>`select deleted_at from tasks where id = 2`).toBe(at + day * 1000)
		// a purge asked in seconds finds the mark kept in seconds, and not the one in milliseconds
		const purged = await tasks
			.deletedOnly()
			.where('deleted_at <= ?', seconds + day)
			.purge()
		expect(purged.changes).toBe(1)
		const minutes = { column: 'deleted_at', unit: 'min' as 's' }
		expect(() => db.table('tasks', { deleted: minutes })).toThrow(InvalidError)
	},
)
