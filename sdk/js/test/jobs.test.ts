// jobs through a real tinystore serve, the one test/binary.ts built.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import {
	CancelledError,
	ConflictError,
	InvalidError,
	type Job,
	open,
	type Store,
} from '../src/index.ts'

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

async function until(holds: () => boolean, what: string): Promise<void> {
	const deadline = Date.now() + 5000
	while (!holds()) {
		if (Date.now() > deadline) {
			throw new Error(`waited five seconds for ${what}`)
		}
		await Bun.sleep(10)
	}
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

	test('a group bounds its running jobs, a rate its starts, move sets a time either way, and spread a phase', async () => {
		const refreshes = store.jobs.queue<number>('refreshes', { maxRunningInGroup: 1, rate: '2/m' })
		await refreshes.enqueueAll([
			{ value: 1, key: 'refresh:1', group: 'db:42' },
			{ value: 2, key: 'refresh:2', group: 'db:42' },
			{ value: 3, key: 'refresh:3', group: 'db:7' },
			{ value: 4, key: 'refresh:4' },
		])
		const first = await refreshes.claim()
		const second = await refreshes.claim()
		expect([first?.key, second?.key]).toEqual(['refresh:1', 'refresh:3'])
		expect(await refreshes.claim()).toBeUndefined() // the rate's two starts a minute are taken

		const checks = store.jobs.queue<string>('checks')
		const later = Date.now() + 3_600_000
		await checks.enqueue('backup', { key: 'check:7', at: new Date(later), move: true })
		await checks.enqueue('backup', { key: 'check:7', at: new Date(later + 60_000), move: true })
		expect((await checks.get('check:7'))?.at.getTime()).toBe(later + 60_000)
		await checks.enqueue('probe', { key: 'probe:7', repeat: { every: '30s', spread: true } })
		expect((await checks.get('probe:7'))?.repeat).toBe('@every 30s +6178ms')
		expect(await caught(checks.enqueue('backup', { move: true }))).toBeInstanceOf(InvalidError)
		expect(() => store.jobs.queue('rated', { rate: '0/s' })).toThrow(InvalidError)
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
		await job?.progress({ done: 1 })
		const running = await q.get('k')
		expect([running?.state, running?.progress]).toEqual(['running', { done: 1 }])
		await job?.ack()
		expect(await q.get('k')).toBeUndefined()
	})

	test('a watch follows a job up its queue, through its progress, to a cancel its handler sees', async () => {
		const q = store.jobs.queue<string>('videos', { maxRunning: 1 })
		await q.enqueueAll([
			{ value: 'a', key: 'a' },
			{ value: 'b', key: 'b' },
		])
		const seen: string[] = []
		const watching = (async () => {
			for await (const s of q.watch('b')) {
				seen.push(
					`${s.state} ${s.ahead}${s.progress === undefined ? '' : ` ${JSON.stringify(s.progress)}`}`,
				)
			}
		})()
		await until(() => seen.includes('waiting 1'), 'b waiting behind a')

		const stop = new AbortController()
		let release = () => {}
		const released = new Promise<void>(resolve => {
			release = resolve
		})
		const reasons: unknown[] = []
		const loop = q.work(
			async job => {
				if (job.key === 'a') {
					await released
					return
				}
				job.progress({ done: 1 })
				await new Promise(resolve => job.signal.addEventListener('abort', resolve, { once: true }))
				reasons.push(job.signal.reason)
				throw job.signal.reason
			},
			{ workers: 2, signal: stop.signal },
		)
		await until(() => seen.includes('waiting 0'), 'a running, b next')
		expect((await q.get('b'))?.state).toBe('waiting') // maxRunning holds it though a worker is free
		release()
		await until(() => seen.includes('running 0 {"done":1}'), "b's progress")
		expect(await q.cancel('b')).toBe(true)
		await watching
		expect(seen.at(-1)).toBe('cancelled 0')
		await until(() => reasons.length === 1, 'the handler to see its signal')
		expect(reasons[0]).toBeInstanceOf(CancelledError)
		stop.abort()
		await loop
		expect(await q.get('b')).toBeUndefined()
		const none: unknown[] = []
		for await (const s of q.watch('b')) {
			none.push(s)
		}
		expect(none).toEqual([])
	})

	test('a job keeps its last run: when it began and how long it took', async () => {
		const q = store.jobs.queue<string>('last-run')
		await q.enqueue('x', { key: 'k' })
		const fresh = await q.get('k')
		expect([fresh?.ran, fresh?.took]).toEqual([undefined, undefined])
		const before = Date.now()
		const job = await q.claim()
		await Bun.sleep(20)
		await job?.retry('busy', { after: '1h' })
		const entry = await q.get('k')
		expect(entry?.ran?.getTime()).toBeGreaterThanOrEqual(before - 5)
		expect(entry?.took).toBeGreaterThanOrEqual(20)
		expect(entry?.error).toBe('busy')
	})

	test('a progress JSON cannot write, or past 4 KiB, is refused', async () => {
		const q = store.jobs.queue<string>('reports')
		await q.enqueue('x', { key: 'k' })
		const job = await q.claim()
		expect(() => job?.progress(() => {})).toThrow(InvalidError)
		expect(() => job?.progress('x'.repeat(4096))).toThrow(InvalidError)
		await job?.ack()
	})

	test('a retry after nothing runs now, as After(0) does in Go, not after its backoff', async () => {
		const q = store.jobs.queue<string>('again', { backoff: { first: '1h', most: '1h' } })
		await q.enqueue('x', { key: 'k' })
		await (await q.claim({ lease: '1m' }))?.retry('busy', { after: 0 })
		const again = await q.claim()
		expect([again?.key, again?.attempt]).toEqual(['k', 2])
	})

	test('a step runs once in a run: the attempt after a failure gets its kept answer', async () => {
		const q = store.jobs.queue<{ question: string }>('agent', { backoff: { first: 1, most: 1 } })
		await q.enqueue({ question: 'what is a store?' }, { key: 'run' })
		let searches = 0
		let asks = 0
		const answers: string[] = []
		const handler = async (job: Job<{ question: string }>) => {
			const hits = await job.step('search', async () => {
				searches++
				return ['a file', 'a lock']
			})
			const answer = await job.step('answer', () => {
				asks++
				if (asks === 1) {
					throw new Error('the model is down')
				}
				return hits.join(' and ')
			})
			answers.push(answer)
		}
		// a loop until idle may end before the retry is due, a millisecond on
		await q.work(handler, { untilIdle: true })
		await Bun.sleep(20)
		await q.work(handler, { untilIdle: true })
		expect([searches, asks, answers]).toEqual([1, 2, ['a file and a lock']])

		await q.enqueue({ question: 'claimed' }, { key: 'claimed' })
		const first = await q.claim({ lease: '1m' })
		expect(await first?.step('count', () => 7)).toBe(7)
		await first?.retry('later', { after: 0 })
		const second = await q.claim()
		expect(await second?.step('count', () => 8)).toBe(7)
		await second?.ack()
	})

	test('a schedule is a queue of one repeating job under its name', async () => {
		const purge = store.jobs.schedule('purge', { daily: '03:10', zone: 'Europe/Moscow' })
		const job = await purge.get('purge')
		expect(job?.repeat).toContain('10 3 * * *')
		expect(await purge.cancel('purge')).toBe(true)
	})
})
