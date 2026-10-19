// Locks and semaphores as docs/guides/locks/README.md has them, through each way a
// store is reached: a key held by one holder at a time while a function
// runs, a line of holds behind it, a lease this program renews, and one lost
// at its maxHold failing its hold though the function succeeded.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

import { InUseError, InvalidError, LostError, type Store } from '../src/index.ts'
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

		test('names and limits are checked before anything is sent', () => {
			expect(() => store.lock('Invoices')).toThrow(InvalidError)
			expect(() => store.semaphore('uploads', { limit: 0 })).toThrow(InvalidError)
		})
	})
}
