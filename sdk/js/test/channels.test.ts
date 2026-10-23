// Channels as docs/guides/channels/README.md has them, through each way a store is
// reached: messages to whoever listens now, in order, a branch and the ones
// below it, a transaction's messages handed over once it commits, and a slow
// handler told what it missed.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

import { type ChannelMessage, ClosedError, type Missed, type Store } from '../src/index.ts'
import { caught, removed, until, ways } from './ways.ts'

for (const way of ways) {
	describe.skipIf(!way.ready)(`channels through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-channels-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		test('a subscriber receives what is published from now on, in order, until it stops', async () => {
			const chat = store.channel<string>('chat')
			await chat.publish('before anyone listened')
			const received: string[] = []
			const sub = await chat.subscribe(m => {
				received.push(m.value)
			})
			for (const text of ['a', 'b', 'c']) {
				await chat.publish(text)
			}
			await until(() => received.length === 3)
			expect(received).toEqual(['a', 'b', 'c'])
			await sub.stop()
			await chat.publish('after the stop')
			await sleep(50)
			expect(received).toHaveLength(3)
		})

		test('a subscription takes its exact branch, and recursive the branches below too', async () => {
			const orders = store.channel<number>('orders')
			const exact: number[] = []
			const below: ChannelMessage<number>[] = []
			const subs = [
				await orders.under('eu').subscribe(m => {
					exact.push(m.value)
				}),
				await orders.under('eu').subscribe(
					m => {
						below.push(m)
					},
					{ recursive: true },
				),
			]
			await orders.publish(1)
			await orders.under('eu').publish(2)
			await orders.under('eu', 'shop-7').publish(3)
			await until(() => below.length === 2)
			await sleep(50)
			expect(exact).toEqual([2])
			expect(below.map(m => m.value)).toEqual([2, 3])
			expect(below[1]!.branch).toEqual(['eu', 'shop-7'])
			for (const sub of subs) {
				await sub.stop()
			}
		})

		test('a loop receives the messages published from its first turn, and break ends it', async () => {
			const ticks = store.channel<number>('ticks')
			const loop = ticks.each()
			const first = loop.next()
			await sleep(50)
			await ticks.publish(1)
			expect((await first).value?.value).toBe(1)
			await ticks.publish(2)
			expect((await loop.next()).value?.value).toBe(2)
			await loop.return(undefined)
		})

		test("a transaction's messages are handed over once it commits, and never when it rolls back", async () => {
			const events = store.channel<string>('events')
			const received: string[] = []
			const sub = await events.subscribe(
				m => {
					received.push(m.value)
				},
				{ recursive: true },
			)
			const orders = store.bucket<string>('orders')
			await caught(
				store.tx(async tx => {
					await tx.with(orders).set('1', 'order')
					await tx.with(events).under('1').publish('created 1')
					throw new Error('the order is refused')
				}),
			)
			await store.tx(async tx => {
				await tx.with(orders).set('2', 'order')
				await tx.with(events).under('2').publish('created 2')
			})
			const app = await store.database('app', {
				migrations: [
					{ name: '0001_notes.sql', sql: 'create table notes (id integer primary key)' },
				],
			})
			await caught(
				app.tx(async tx => {
					await tx.exec`insert into notes (id) values (1)`
					await tx.with(events).publish('rolled back')
					throw new Error('the note is refused')
				}),
			)
			await app.tx(async tx => {
				await tx.exec`insert into notes (id) values (2)`
				await tx.with(events).under('note').publish('noted')
			})
			await until(() => received.length === 2)
			await sleep(50)
			expect(received).toEqual(['created 2', 'noted'])
			await sub.stop()
		})

		test('a handler that throws is told and the next message follows; stop inside it answers at once', async () => {
			const numbers = store.channel<number>('numbers')
			const received: number[] = []
			const failures: unknown[] = []
			const sub = await numbers.subscribe(
				async m => {
					if (m.value === 2) {
						throw new Error('two is refused')
					}
					received.push(m.value)
					if (m.value === 3) {
						await sub.stop()
					}
				},
				{ onError: error => void failures.push(error) },
			)
			for (const value of [1, 2, 3, 4]) {
				await numbers.publish(value)
			}
			await until(() => received.length === 2)
			await sleep(50)
			expect(received).toEqual([1, 3])
			expect((failures[0] as Error).message).toBe('two is refused')
		})

		test('a slow handler falls behind and is told how many it missed', async () => {
			const photos = store.channel<string>('photos')
			const photo = 'p'.repeat(4096)
			let release!: () => void
			const released = new Promise<void>(resolve => {
				release = resolve
			})
			let received = 0
			let missed = 0
			const sub = await photos.subscribe(
				async () => {
					received++
					await released
				},
				{
					onMissed: (told: Missed) => {
						missed += told.count ?? 0
					},
				},
			)
			const published = 3000
			for (let at = 0; at < published; at++) {
				await photos.publish(photo)
			}
			release()
			await until(() => received + missed === published)
			expect(missed).toBeGreaterThan(0)
			expect(received + missed).toBe(published)
			await sub.stop()
		})

		test('names are checked before anything is sent', () => {
			expect(() => store.channel('Chat')).toThrow()
		})
	})
}

for (const way of ways) {
	describe.skipIf(!way.ready)(`a store's close through ${way.name}`, () => {
		test('ends its subscriptions, loops and tails at once, with nothing left for the server to drain', async () => {
			const dir = mkdtempSync(join(tmpdir(), 'tinystore-followers-'))
			const store = await way.open(dir)
			const chat = store.channel<string>('chat')
			const live = store.events('live')
			let subscribed = 0
			let looped = 0
			let tailed = 0
			await chat.subscribe(() => {
				subscribed++
			})
			// what ended each loop, taken at once: a loop ends during the close
			const looping = caught(
				(async () => {
					for await (const _ of chat.each()) {
						looped++
					}
				})(),
			)
			const tailing = caught(
				(async () => {
					for await (const _ of live.tail()) {
						tailed++
					}
				})(),
			)
			await until(async () => {
				await chat.publish('ping')
				await live.append('tick')
				return subscribed > 0 && looped > 0 && tailed > 0
			})

			// a stream the server still ran would keep a private child, or a
			// sidecar, draining it for ten seconds before it let go
			const began = Date.now()
			await store.close()
			await way.leave(dir)
			const took = Date.now() - began
			expect(await looping).toBeInstanceOf(ClosedError)
			expect(await tailing).toBeInstanceOf(ClosedError)
			expect(took).toBeLessThan(5_000)
			await removed(dir)
		}, 30_000)
	})
}
