// A test's clock: a private store runs on it, and a test moves it forward
// instead of waiting.

import { expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { InvalidError, open } from '../src/index.ts'
import { until } from './ways.ts'

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

test("a clock is a private store's, and a store on the system's time refuses to move one", async () => {
	const shared = open(mkdtempSync(join(tmpdir(), 'tinystore-clock-')), { clock: new Date() })
	expect(await caught(shared)).toBeInstanceOf(InvalidError)
	await using plain = await open(mkdtempSync(join(tmpdir(), 'tinystore-clock-')), { private: true })
	expect(await caught(plain.clock.advance('1h'))).toBeInstanceOf(InvalidError)
})
