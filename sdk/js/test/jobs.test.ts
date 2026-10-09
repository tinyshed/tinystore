// jobs as plan/api/jobs.md has it over protocol 2, through each way a store
// is reached: ids, answers, steps, progress, cancels, schedules, pages, a
// schema, concurrency and dedupe, the handlers running here while the
// server's loop claims their jobs.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { CancelledError, InvalidError, type StandardSchemaV1, type Store } from '../src/index.ts'
import { caught, removed, until, ways } from './ways.ts'

interface Reminder {
	userId: number
	text: string
}

/** A Standard Schema of reminders, as zod's or valibot's would check them. */
const reminderSchema: StandardSchemaV1<unknown, Reminder> = {
	'~standard': {
		version: 1,
		vendor: 'test',
		validate: value => {
			const reminder = value as Partial<Reminder> | null
			return typeof reminder?.userId === 'number' && typeof reminder.text === 'string'
				? { value: reminder as Reminder }
				: { issues: [{ message: 'a reminder has a userId and a text', path: ['userId'] }] }
		},
	},
}

for (const way of ways) {
	describe.skipIf(!way.ready)(`jobs through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-jobs-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		test('an id adds a job once, set makes it whatever it was, update changes one not started, cancel takes it', async () => {
			const later = store.queue<{ text: string }>('later')
			const id = 'chat:42:draft-1'
			expect(await later.add({ text: 'hi' }, { id, delay: '1h' })).toBe(true)
			expect(await later.add({ text: 'hi again' }, { id, delay: '1h' })).toBe(false)
			const added = await later.get(id)
			expect([added?.state, added?.value, added?.attempt]).toEqual(['scheduled', { text: 'hi' }, 0])

			await later.set(id, { text: 'edited' }, { delay: '2h' })
			expect((await later.get(id))?.value).toEqual({ text: 'edited' })
			expect(await later.update(id, { text: 'edited twice' })).toBe(true)
			const updated = await later.get(id)
			expect([updated?.value, updated?.state]).toEqual([{ text: 'edited twice' }, 'scheduled'])

			expect(await later.cancel(id)).toBe(true)
			expect(await later.get(id)).toBeUndefined()
			expect(await later.update(id, { text: 'too late' })).toBe(false)
			expect(await later.cancel(id)).toBe(false)
		})

		test('a worker runs each job as it falls due, the value first and the run second', async () => {
			const reminders = store.queue<Reminder>('reminders')
			const ran: [number, string | undefined, number][] = []
			const worker = reminders.work(async ({ userId }, run) => {
				ran.push([userId, run.id, run.attempt])
			})
			await reminders.add({ userId: 1, text: 'Call mom' }, { id: 'r1' })
			await reminders.add({ userId: 2, text: 'Drink water' })
			await until(() => ran.length === 2)
			await worker.stop()
			expect(ran).toEqual([
				[1, 'r1', 1],
				[2, undefined, 1],
			])
			expect(await reminders.get('r1')).toBeUndefined()
		})

		test('what a handler returns settles its job: a throw retries, retry counts its run, snooze does not, fail ends it', async () => {
			const pushes = store.queue<string>('pushes', { backoff: { initial: '1h', max: '2h' } })
			for (const id of ['done', 'thrown', 'retry', 'snooze', 'fail', 'unreturned']) {
				await pushes.add(id, { id })
			}
			const ran = await pushes.runDue(async (how, run) => {
				switch (how) {
					case 'thrown':
						throw new Error('the provider is down')
					case 'retry':
						return run.retry('10m')
					case 'snooze':
						return run.snooze('10m')
					case 'fail':
						return run.fail('the provider no longer knows the token')
					case 'unreturned':
						run.snooze('10m')
				}
			})
			expect(ran).toBe(6)

			expect(await pushes.get('done')).toBeUndefined()
			const thrown = await pushes.get('thrown')
			expect([thrown?.state, thrown?.attempt, thrown?.error]).toEqual([
				'scheduled',
				1,
				'the provider is down',
			])
			const retry = await pushes.get('retry')
			expect([retry?.state, retry?.attempt]).toEqual(['scheduled', 1])
			const snooze = await pushes.get('snooze')
			expect([snooze?.state, snooze?.attempt]).toEqual(['scheduled', 0])
			const fail = await pushes.get('fail')
			expect([fail?.state, fail?.error]).toEqual([
				'failed',
				'the provider no longer knows the token',
			])
			const unreturned = await pushes.get('unreturned')
			expect([unreturned?.state, unreturned?.attempt]).toEqual(['scheduled', 1])
			expect(unreturned?.error).toContain('run.snooze(…)')
		})

		test('a step one attempt kept is found by the next, which runs only the steps left', async () => {
			const orders = store.queue<{ order: number }>('orders')
			await orders.add({ order: 7 }, { id: 'order:7' })
			let charged = 0
			const attempts: string[] = []
			const ran = await orders.runDue(async (_, run) => {
				const charge = await run.step('charge', () => {
					charged++
					return { charge: 'ch_1' }
				})
				attempts.push(`${run.attempt}:${charge.charge}`)
				if (run.attempt === 1) {
					return run.retry(new Date(0))
				}
			})
			expect(ran).toBe(2)
			expect(attempts).toEqual(['1:ch_1', '2:ch_1'])
			expect(charged).toBe(1)
		})

		test('what a handler reports shows while its job runs, and a report past 4 KiB is refused', async () => {
			const videos = store.queue<{ video: number }>('videos')
			await videos.add({ video: 3 }, { id: 'video:3' })
			let release!: () => void
			const released = new Promise<void>(resolve => {
				release = resolve
			})
			let refused: unknown
			const worker = videos.work(async (_, run) => {
				try {
					run.setProgress('x'.repeat(5000))
				} catch (err) {
					refused = err
				}
				run.setProgress({ frames: 120 })
				await released
			})
			await until(async () => (await videos.get('video:3'))?.progress !== undefined)
			const running = await videos.get('video:3')
			expect([running?.state, running?.progress]).toEqual(['running', { frames: 120 }])
			expect(refused).toBeInstanceOf(InvalidError)
			release()
			await worker.stop()
			expect(await videos.get('video:3')).toBeUndefined()
		})

		test('a cancel tells a running handler to stop, and what it returns then settles nothing', async () => {
			const exports = store.queue<{ report: number }>('exports')
			await exports.add({ report: 9 }, { id: 'export:9' })
			let reason: unknown
			const worker = exports.work(async (_, run) => {
				await new Promise<void>(resolve => run.signal.addEventListener('abort', () => resolve()))
				reason = run.signal.reason
				return run.fail('it should settle nothing')
			})
			await until(async () => (await exports.get('export:9'))?.state === 'running')
			expect(await exports.cancel('export:9')).toBe(true)
			await until(() => reason !== undefined)
			expect(reason).toBeInstanceOf(CancelledError)
			await worker.stop()
			expect(await exports.get('export:9')).toBeUndefined()
		})

		test('a watch follows its job through its progress to its end, and an id with no job yields nothing', async () => {
			const clips = store.queue<{ clip: number }>('clips')
			await clips.add({ clip: 1 }, { id: 'clip:1' })
			const seen: string[] = []
			const watching = (async () => {
				for await (const job of clips.watch('clip:1')) {
					seen.push(job.progress === undefined ? job.state : `${job.state} ${job.progress}`)
				}
			})()
			await until(() => seen.length > 0)
			let release!: () => void
			const released = new Promise<void>(resolve => {
				release = resolve
			})
			const worker = clips.work(async (_, run) => {
				run.setProgress(0.5)
				await released
			})
			await until(() => seen.at(-1) === 'running 0.5')
			release()
			await watching
			expect([seen[0], seen.at(-1)]).toEqual(['waiting', 'done 0.5'])
			await worker.stop()

			const nothing: unknown[] = []
			for await (const job of clips.watch('clip:never')) {
				nothing.push(job)
			}
			expect(nothing).toEqual([])
		})

		test('a watch of a job a cancel took ends cancelled', async () => {
			const clips = store.queue<{ clip: number }>('clips')
			await clips.add({ clip: 2 }, { id: 'clip:2', delay: '1h' })
			const states: string[] = []
			const watching = (async () => {
				for await (const job of clips.watch('clip:2')) {
					states.push(job.state)
				}
			})()
			await until(() => states.length > 0)
			expect(await clips.cancel('clip:2')).toBe(true)
			await watching
			expect(states).toEqual(['scheduled', 'cancelled'])
		})

		test('a schedule keeps its one job, and a cron needs a time zone', async () => {
			const nightly = store.schedule(
				'nightly',
				{ cron: '10 3 * * *', timeZone: 'Europe/Berlin' },
				async run => run.at,
			)
			await until(async () => (await nightly.get()) !== undefined)
			const job = await nightly.get()
			expect([job?.state, job?.repeat]).toEqual(['scheduled', '10 3 * * * Europe/Berlin'])
			await nightly.stop()
			expect(await nightly.cancel()).toBe(true)

			const user: { timeZone?: string } = {}
			const zoneless = () =>
				store.schedule(
					'digest',
					{ cron: '0 20 * * *', timeZone: user.timeZone as string },
					() => {},
				)
			expect(zoneless).toThrow(InvalidError)
			const later = store.queue('digests')
			const err = await caught(
				later.set('user:42', {}, { cron: '0 20 * * *', timeZone: user.timeZone }),
			)
			expect(err).toBeInstanceOf(InvalidError)
			expect((err as Error).message).toContain('time zone')
		})

		test('a page reads the ids under a prefix, and the failed jobs the last failed first', async () => {
			const chats = store.queue<number>('chats')
			for (const n of [1, 2, 3]) {
				await chats.add(n, { id: `chat:4:${n}`, delay: '1h' })
			}
			await chats.add(42, { id: 'chat:42:1', delay: '1h' })
			const first = await chats.list({ prefix: 'chat:4:', limit: 2 })
			expect(first.jobs.map(job => job.id)).toEqual(['chat:4:1', 'chat:4:2'])
			const second = await chats.list({ prefix: 'chat:4:', limit: 2, after: first.next })
			expect(second.jobs.map(job => job.id)).toEqual(['chat:4:3'])
			expect(second.next).toBeUndefined()
			const all: (string | undefined)[] = []
			for await (const job of chats.all({ prefix: 'chat:' })) {
				all.push(job.id)
			}
			// byte order: '2' is 0x32, before ':' at 0x3a
			expect(all).toEqual(['chat:42:1', 'chat:4:1', 'chat:4:2', 'chat:4:3'])
		})

		test('a schema checks each value before a handler gets it, and one that no longer meets it fails its job, not its queue', async () => {
			const loose = store.queue('checked')
			await loose.add({ userId: 'not a number' }, { id: 'bad' })
			const checked = store.queue('checked', { schema: reminderSchema })
			await checked.add({ userId: 5, text: 'Call mom' }, { id: 'good' })
			const got: string[] = []
			expect(await checked.runDue(async reminder => got.push(reminder.text))).toBe(2)
			expect(got).toEqual(['Call mom'])
			const bad = await loose.get('bad')
			expect(bad?.state).toBe('failed')
			expect(bad?.error).toContain("no longer meets the queue's schema")
		})

		test('a worker runs as many handlers at once as its concurrency says', async () => {
			const refreshes = store.queue<number>('refreshes')
			for (const n of [1, 2, 3, 4]) {
				await refreshes.add(n)
			}
			let running = 0
			let most = 0
			let finished = 0
			const worker = refreshes.work(
				async () => {
					running++
					most = Math.max(most, running)
					await Bun.sleep(50)
					running--
					finished++
				},
				{ concurrency: 2 },
			)
			await until(() => finished === 4)
			await worker.stop()
			expect([finished, most]).toEqual([4, 2])
		})

		test('dedupe keeps a done id taken, so that adding it again adds nothing, and the job its last run', async () => {
			const once = store.queue<string>('welcome', { dedupe: '1h' })
			expect(await once.add('ada', { id: 'welcome:ada' })).toBe(true)
			expect(await once.runDue(() => {})).toBe(1)
			expect(await once.add('ada', { id: 'welcome:ada' })).toBe(false)
			const done = await once.get('welcome:ada')
			expect([done?.state, done?.value]).toEqual(['done', 'ada'])
			const { startedAt, endedAt } = done?.lastRun ?? {}
			expect(endedAt!.getTime()).toBeGreaterThanOrEqual(startedAt!.getTime())
		})

		test('a value JSON cannot write is refused before it leaves', async () => {
			const odd = store.queue<unknown>('odd')
			expect(await caught(odd.add(undefined))).toBeInstanceOf(InvalidError)
			expect(await caught(odd.add({ big: 1n }))).toBeInstanceOf(InvalidError)
		})
	})
}
