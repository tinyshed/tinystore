// jobs through a real tinystore serve, the one test/binary.ts built.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { ConflictError, InvalidError, open, type Store } from '../src/index.ts'

let dir: string
let store: Store

beforeAll(async () => {
	dir = mkdtempSync(join(tmpdir(), 'tinystore-jobs-'))
	store = await open(dir, { private: true })
})

afterAll(async () => {
	await store.close()
	rmSync(dir, { recursive: true, force: true })
})

async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

interface Reminder {
	user: number
	text: string
}

describe('a queue', () => {
	test('a job keyed waits, changes, and is cancelled while it waits', async () => {
		const later = store.jobs.queue<Reminder>('send-later')
		await later.enqueue({ user: 42, text: 'call mom' }, { key: 'chat:42:a', after: '1h' })
		const waiting = await later.get('chat:42:a')
		expect(waiting?.value).toEqual({ user: 42, text: 'call mom' })
		expect(waiting?.state).toBe('waiting')
		expect(waiting?.at.getTime()).toBeGreaterThan(Date.now() + 59 * 60_000)

		await later.update('chat:42:a', { user: 42, text: 'call dad' })
		expect((await later.get('chat:42:a'))?.value.text).toBe('call dad')
		expect(await later.cancel('chat:42:a')).toBe(true)
		expect(await later.cancel('chat:42:a')).toBe(false)
		expect(await later.get('chat:42:a')).toBeUndefined()
		expect(await caught(later.update('chat:42:a', { user: 1, text: 'x' }))).toBeInstanceOf(
			ConflictError,
		)
	})

	test('an enqueue of many is all or none, the refused job named', async () => {
		const q = store.jobs.queue<number>('atomic')
		const err = await caught(
			q.enqueueAll([
				{ value: 1, key: 'a' },
				{ value: 2, repeat: { every: '1m' } },
			]),
		)
		expect(err).toBeInstanceOf(InvalidError)
		expect((err as InvalidError).what).toMatchObject({ call: '1' })
		expect(await q.get('a')).toBeUndefined()
	})

	test('scan pages the jobs under a prefix', async () => {
		const q = store.jobs.queue<number>('scanned')
		await q.enqueueAll(
			Array.from({ length: 12 }, (_, i) => ({
				value: i,
				key: `chat:7:${String(i).padStart(2, '0')}`,
				after: '1h',
			})),
		)
		await q.enqueue(99, { key: 'chat:8:x', after: '1h' })
		const first = await q.scan({ prefix: 'chat:7:', limit: 5 })
		expect(first.items.map(j => j.value)).toEqual([0, 1, 2, 3, 4])
		expect(first.next).toBe('chat:7:04')
		const all = []
		for await (const job of q.all({ prefix: 'chat:7:', limit: 5 })) {
			all.push(job.value)
		}
		expect(all).toEqual(Array.from({ length: 12 }, (_, i) => i))
	})
})

describe('a work loop', () => {
	test('a return acknowledges, a throw retries, and fail and snooze say otherwise', async () => {
		const q = store.jobs.queue<string>('outcomes', { backoff: { first: 1, most: 1 } })
		await q.enqueueAll([
			{ value: 'ok', key: 'ok' },
			{ value: 'flaky', key: 'flaky' },
			{ value: 'broken', key: 'broken' },
			{ value: 'later', key: 'later' },
		])
		const attempts: Record<string, number[]> = {}
		await q.work(
			async job => {
				attempts[job.key] = [...(attempts[job.key] ?? []), job.attempt]
				if (job.value === 'flaky' && job.attempt === 1) {
					throw new Error('try again')
				}
				if (job.value === 'broken') {
					job.fail('bad input')
				}
				if (job.value === 'later') {
					job.snooze({ after: '1h' })
				}
			},
			{ workers: 2, untilIdle: true },
		)
		expect(attempts.flaky).toEqual([1, 2])
		expect(await q.get('ok')).toBeUndefined()
		expect(await q.get('flaky')).toBeUndefined()
		const broken = await q.get('broken')
		expect([broken?.state, broken?.error]).toEqual(['failed', 'bad input'])
		const later = await q.get('later')
		expect([later?.state, later?.attempt]).toEqual(['waiting', 0])
	})

	test('a loop stopped by its signal finishes the jobs in hand and returns', async () => {
		const q = store.jobs.queue<number>('stopping')
		const controller = new AbortController()
		const done: number[] = []
		const loop = q.work(
			async job => {
				done.push(job.value)
				if (done.length === 2) {
					controller.abort()
				}
			},
			{ signal: controller.signal },
		)
		await q.enqueueAll([{ value: 1 }, { value: 2 }])
		await loop
		expect(done).toEqual([1, 2])
		expect(await q.get('1')).toBeUndefined()
	})

	test('a claimed job is settled by its caller', async () => {
		const q = store.jobs.queue<string>('claimed')
		expect(await q.claim()).toBeUndefined()
		await q.enqueue('x', { key: 'k' })
		const job = await q.claim({ lease: '1m' })
		expect([job?.key, job?.value, job?.attempt]).toEqual(['k', 'x', 1])
		expect((await q.get('k'))?.state).toBe('leased')
		await job?.ack()
		expect(await q.get('k')).toBeUndefined()
	})

	test('a retry after nothing runs now, as After(0) does in Go, not after its backoff', async () => {
		const q = store.jobs.queue<string>('again', { backoff: { first: '1h', most: '1h' } })
		await q.enqueue('x', { key: 'k' })
		await (await q.claim({ lease: '1m' }))?.retry('busy', { after: 0 })
		const again = await q.claim()
		expect([again?.key, again?.attempt]).toEqual(['k', 2])
	})

	test('a schedule is a queue of one repeating job under its name', async () => {
		const purge = store.jobs.schedule('purge', { daily: '03:10', zone: 'Europe/Moscow' })
		const job = await purge.get('purge')
		expect(job?.repeat).toContain('10 3 * * *')
		expect(await purge.cancel('purge')).toBe(true)
	})
})
