// Locks and semaphores as docs/guides/locks/README.md has them, through each way a
// store is reached: a key held by one holder at a time while a function
// runs, a line of holds behind it, a lease this program renews, and one lost
// at its maxHold failing its hold though the function succeeded.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

import {
	InUseError,
	InvalidError,
	LostError,
	ms,
	type Store,
	UnavailableError,
} from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

/** A promise the test resolves itself. */
function gate(): { opened: Promise<void>; open: () => void } {
	let open!: () => void
	const opened = new Promise<void>(resolve => {
		open = resolve
	})
	return { opened, open }
}

for (const way of ways) {
	describe.skipIf(!way.ready)(`locks through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-locks-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		test('a key is held by one holder at a time, the next in line given it when it is let go', async () => {
			const invoices = store.lock('invoices')
			const holding = gate()
			const letGo = gate()
			const first = invoices.hold('42', async lease => {
				holding.open()
				await letGo.opened
				return lease.fence
			})
			await holding.opened
			expect(await invoices.tryHold('42', () => 'ran')).toEqual({ acquired: false })
			expect(await invoices.tryHold('43', () => 'another key')).toEqual({
				acquired: true,
				value: 'another key',
			})
			const second = invoices.hold('42', lease => lease.fence)
			await sleep(50)
			letGo.open()
			const [before, after] = await Promise.all([first, second])
			expect(typeof before).toBe('bigint')
			expect(after > before).toBe(true)
		})

		test('a key still held past the wait is InUseError, and a throw lets the key go', async () => {
			const reports = store.lock('reports')
			const holding = gate()
			const letGo = gate()
			const first = reports.hold('sales', async () => {
				holding.open()
				await letGo.opened
			})
			await holding.opened
			const waited = await caught(reports.hold('sales', () => 'ran', { wait: 200 }))
			expect(waited).toBeInstanceOf(InUseError)
			letGo.open()
			await first
			const thrown = await caught(
				reports.hold('sales', () => {
					throw new Error('the holder failed')
				}),
			)
			expect((thrown as Error).message).toBe('the holder failed')
			expect(await reports.tryHold('sales', () => 'free')).toEqual({
				acquired: true,
				value: 'free',
			})
		})

		test('a hold inside its own hold of the key, or inside a transaction, is InvalidError', async () => {
			const invoices = store.lock('nested')
			const nested = await caught(invoices.hold('42', () => invoices.hold('42', () => 'inner')))
			expect(nested).toBeInstanceOf(InvalidError)
			expect(await invoices.hold('42', () => invoices.hold('43', () => 'another key'))).toBe(
				'another key',
			)
			const stock = store.bucket<number>('stock')
			const inside = await caught(
				store.tx(async tx => {
					await tx.with(stock).set('sku', 1)
					return invoices.hold('42', () => 'inside')
				}),
			)
			expect(inside).toBeInstanceOf(InvalidError)
			expect(await stock.get('sku')).toBeUndefined()
		})

		test('a lease renewed by this program outlives its ttl', async () => {
			const jobs = store.lock('long', { ttl: '1s' })
			const holding = gate()
			const letGo = gate()
			const held = jobs.hold('nightly', async lease => {
				holding.open()
				await letGo.opened
				return lease.signal.aborted
			})
			await holding.opened
			await sleep(1600)
			expect(await jobs.tryHold('nightly', () => 'ran')).toEqual({ acquired: false })
			letGo.open()
			expect(await held).toBe(false)
		})

		test('maxHold revokes a lease, gives the key on and fails the hold LostError', async () => {
			const exports = store.lock('exports', { maxHold: '300ms' })
			const holding = gate()
			const hung = caught(
				exports.hold('daily', async lease => {
					holding.open()
					await new Promise<void>(resolve =>
						lease.signal.addEventListener('abort', () => resolve()),
					)
					return 'done after its lease was lost'
				}),
			)
			await holding.opened
			const next = await exports.hold('daily', () => 'the next in line', { wait: '5s' })
			expect(next).toBe('the next in line')
			const lost = await hung
			expect(lost).toBeInstanceOf(LostError)
		})

		// contract: LOCK-08
		test('a holding says how long it waited, at least as long as it stood in line', async () => {
			const moodle = store.semaphore('moodle', { limit: 1 })
			const holding = gate()
			const letGo = gate()
			const first = moodle.hold('requests', async permit => {
				holding.open()
				await letGo.opened
				return permit.waited
			})
			await holding.opened
			const second = moodle.hold('requests', permit => ms(permit.waited))
			const inLine = performance.now()
			await sleep(50)
			const stood = performance.now() - inLine
			letGo.open()
			expect(ms(await first)).toBeLessThan(10_000)
			expect(await second).toBeGreaterThanOrEqual(Math.floor(stood))
			const lease = await store.lock('invoices').hold('waited', lease => lease.waited)
			expect(typeof lease).toBe('string')
		})

		test('a semaphore admits its limit a key, its permit without a fence', async () => {
			const uploads = store.semaphore('uploads', { limit: 2 })
			const holding = [gate(), gate()]
			const letGo = gate()
			const holders = holding.map(each =>
				uploads.hold('ann', async permit => {
					each.open()
					await letGo.opened
					return 'fence' in permit
				}),
			)
			await Promise.all(holding.map(each => each.opened))
			expect(await uploads.tryHold('ann', () => 'ran')).toEqual({ acquired: false })
			expect(await uploads.tryHold('bob', () => 'a limit a key')).toEqual({
				acquired: true,
				value: 'a limit a key',
			})
			letGo.open()
			expect(await Promise.all(holders)).toEqual([false, false])
		})

		// contract: LOCK-09
		test('an urgent hold takes the first place freed before the background holds that came first', async () => {
			const cabinet = store.semaphore('cabinet', { limit: 2 })
			const served: string[] = []
			const letGo = new Map<string, () => void>()
			const hold = (name: string, priority: 'urgent' | 'background') =>
				cabinet.hold(
					'requests',
					async () => {
						served.push(name)
						const done = gate()
						letGo.set(name, done.open)
						await done.opened
					},
					{ priority, wait: '60s' },
				)
			const holds = [hold('b1', 'background'), hold('b2', 'background')]
			await sleep(50)
			holds.push(hold('b3', 'background'), hold('b4', 'background'))
			await sleep(50)
			holds.push(hold('urgent', 'urgent'))
			await sleep(50)
			expect(served).toEqual(['b1', 'b2'])
			letGo.get('b1')?.()
			while (served.length < 3) await sleep(5)
			expect(served).toEqual(['b1', 'b2', 'urgent'])
			for (const name of ['b2', 'urgent']) {
				const before = served.length
				letGo.get(name)?.()
				while (served.length === before) await sleep(5)
			}
			letGo.get('b3')?.()
			letGo.get('b4')?.()
			await Promise.all(holds)
			expect(served).toEqual(['b1', 'b2', 'urgent', 'b3', 'b4'])
		})

		// contract: LOCK-10
		test('background holds take no more than their places, and an urgent hold finds the rest at once', async () => {
			const lane = store.semaphore('lane', { limit: 2, background: 1 })
			const holding = gate()
			const letGo = gate()
			const first = lane.hold(
				'requests',
				async () => {
					holding.open()
					await letGo.opened
				},
				{ priority: 'background' },
			)
			await holding.opened
			const waiting = lane.hold('requests', () => 'the background in line', {
				priority: 'background',
				wait: '60s',
			})
			expect(await lane.tryHold('requests', () => 'urgent', { priority: 'background' })).toEqual({
				acquired: false,
			})
			expect(await lane.tryHold('requests', () => 'urgent')).toEqual({
				acquired: true,
				value: 'urgent',
			})
			letGo.open()
			await first
			expect(await waiting).toBe('the background in line')
			expect(() => store.semaphore('lane', { limit: 2 })).toThrow(
				'semaphore lane: open in this program with a background of 1, not none',
			)
			expect(() => store.semaphore('wide', { limit: 2, background: 3 })).toThrow(InvalidError)
			// a background of the limit is every place, as none is
			store.semaphore('full', { limit: 2, background: 2 })
			store.semaphore('full', { limit: 2 })
			const lock = store.lock('one')
			const untyped = { priority: 'background', timeout: '1s' } as never
			expect(await caught(lock.hold('k', () => 'held', untyped))).toBeInstanceOf(InvalidError)
		})

		// contract: LOCK-11
		test("a hold's timeout counts from its grant, gives the place on and fails the hold LostError", async () => {
			const lane = store.semaphore('timed', { limit: 1 })
			const holding = gate()
			const letGo = gate()
			const first = lane.hold('requests', async () => {
				holding.open()
				await letGo.opened
			})
			await holding.opened
			const granted = gate()
			const hung = caught(
				lane.hold(
					'requests',
					async permit => {
						granted.open()
						await new Promise<void>(resolve =>
							permit.signal.addEventListener('abort', () => resolve()),
						)
					},
					{ wait: '60s', timeout: '300ms' },
				),
			)
			await sleep(400)
			letGo.open()
			await first
			await granted.opened
			const next = await lane.hold('requests', () => 'the next in line', { wait: '5s' })
			expect(next).toBe('the next in line')
			expect(await hung).toBeInstanceOf(LostError)
		})

		// contract: LOCK-12, LOCK-13, LOCK-14
		test('failed holds in a row put the key down, and one hold after the pause puts it up', async () => {
			const flaky = store.semaphore('flaky', { limit: 1, breaker: { failures: 2, pause: '300ms' } })
			const badGateway = () => {
				throw new Error('the cabinet answered 502')
			}
			expect((await flaky.state('requests')).up).toBe(true)
			for (const _ of [1, 2]) {
				expect(await caught(flaky.hold('requests', badGateway))).toBeInstanceOf(Error)
			}
			const refused = await caught(flaky.hold('requests', () => 'answered'))
			expect(refused).toBeInstanceOf(UnavailableError)
			expect((refused as Error).message).toContain('after 2 failed holds in a row')
			const down = await flaky.state('requests')
			expect(down.up).toBe(false)
			expect(down.downSince).toBeInstanceOf(Date)
			expect(down.lastFailure?.getTime()).toBe(down.downSince?.getTime())
			await sleep(350)
			expect(await flaky.hold('requests', () => 'answered')).toBe('answered')
			const up = await flaky.state('requests')
			expect([up.up, up.downSince, up.waiting, up.holding]).toEqual([true, undefined, 0, 0])

			for (const _ of [1, 2]) {
				const notFound = await caught(
					flaky.hold('requests', permit => {
						permit.healthy()
						throw new Error('no such student')
					}),
				)
				expect((notFound as Error).message).toBe('no such student')
			}
			expect((await flaky.state('requests')).up).toBe(true)
			expect(() => store.semaphore('flaky', { limit: 1 })).toThrow(
				'with a breaker of 2 failures and 300ms, not none',
			)
		})

		test('names and limits are checked before anything is sent', () => {
			expect(() => store.lock('Invoices')).toThrow(InvalidError)
			expect(() => store.semaphore('uploads', { limit: 0 })).toThrow(InvalidError)
		})

		// contract: LOCK-07
		test('a semaphore opened again with another limit is refused at the open, with the same limit it is not', () => {
			store.semaphore('gpu', { limit: 1 })
			expect(() => store.semaphore('gpu', { limit: 1 })).not.toThrow()
			const wider = () => store.semaphore('gpu', { limit: 3 })
			expect(wider).toThrow(InvalidError)
			expect(wider).toThrow('semaphore gpu: open in this program with a limit of 1, not 3')
			expect(() => store.semaphore('tpu', { limit: 3 })).not.toThrow()
		})
	})
}

const embedded = ways.find(way => way.name === 'the core in this process')!

// contract: LOCK-07
describe.skipIf(!embedded.ready)('a semaphore of two stores', () => {
	test('two stores on one directory hold one name each by its own limit, as two programs do', async () => {
		const dir = mkdtempSync(join(tmpdir(), 'tinystore-locks-'))
		const first = await embedded.open(dir)
		const second = await embedded.open(dir)
		try {
			const narrow = first.semaphore('gpu', { limit: 1 })
			const wide = second.semaphore('gpu', { limit: 2 })
			const held = gate()
			const letGo = gate()
			const holder = narrow.hold('device-0', async () => {
				held.open()
				await letGo.opened
			})
			await held.opened
			expect(await wide.tryHold('device-0', () => 'beside the other program')).toEqual({
				acquired: true,
				value: 'beside the other program',
			})
			letGo.open()
			await holder
		} finally {
			await first.close()
			await second.close()
			await embedded.leave(dir)
			await removed(dir)
		}
	})
})
