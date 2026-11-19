// files.getOrCreate as docs/guides/blobs/get-or-create.md has it, through each way a
// store is reached: a miss made once while every other call of the path
// waits for it, a failure every waiting call gets, a put that wins over a
// run, and what a function gives as a stream or a file of this machine.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

import { ConflictError, type Store } from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

function pattern(n: number, seed = 7): Uint8Array<ArrayBuffer> {
	return Uint8Array.from({ length: n }, (_, i) => (i * 31 + seed) & 0xff)
}

const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes)

/** A promise and what resolves it, for a function that waits for the test. */
function gate(): { opened: Promise<void>; open: () => void } {
	let open = () => {}
	const opened = new Promise<void>(resolve => {
		open = resolve
	})
	return { opened, open }
}

for (const way of ways) {
	describe.skipIf(!way.ready)(`getOrCreate through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-fill-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		test('a miss runs the function once and the next call reads its file', async () => {
			const pages = store.files('pages')
			let runs = 0
			const convert = async () => {
				runs++
				return { data: '%PDF-1.7', contentType: 'application/pdf' }
			}
			const page = await pages.getOrCreate('a.pdf', convert, { meta: { doc: '42' } })
			expect(page.contentType).toBe('application/pdf')
			expect(page.meta).toEqual({ doc: '42' })
			expect(text(await page.bytes())).toBe('%PDF-1.7')
			const again = await pages.getOrCreate('a.pdf', convert)
			expect(again.etag).toBe(page.etag)
			expect(runs).toBe(1)

			const typed = await pages.getOrCreate('b.txt', () => ({ data: 'b' }), {
				contentType: 'text/plain',
			})
			expect(typed.contentType).toBe('text/plain')
		})

		// contract: BLOB-21
		test('concurrent calls of one path run the function once and all get its file', async () => {
			const pages = store.files('once')
			const { opened, open } = gate()
			let runs = 0
			const convert = async () => {
				runs++
				await opened
				return { data: 'made once' }
			}
			const calls = Array.from({ length: 6 }, () => pages.getOrCreate('a.pdf', convert))
			await sleep(200)
			open()
			const pages_ = await Promise.all(calls)
			for (const page of pages_) {
				expect(text(await page.bytes())).toBe('made once')
			}
			expect(runs).toBe(1)
		})

		// contract: BLOB-22
		test("a throw is the caller's own error, every waiting call's conflict, and the next call runs again", async () => {
			const pages = store.files('failing')
			const { opened, open } = gate()
			// the second call waits for the first to hold the key, not for a time a slow runner may pass
			const started = gate()
			class Parse extends Error {}
			// each failure is caught as it is made: node fails a test on a rejection no one held
			// yet, and the waiting call's conflict may come before the failing call's own end
			const failing = caught(
				pages.getOrCreate('bad.pdf', async () => {
					started.open()
					await opened
					throw new Parse('the document does not parse')
				}),
			)
			await started.opened
			const waiting = caught(pages.getOrCreate('bad.pdf', () => ({ data: 'never' })))
			await sleep(200)
			open()
			expect(await failing).toBeInstanceOf(Parse)
			const waited = await waiting
			expect(waited).toBeInstanceOf(ConflictError)
			expect((waited as Error).message).toContain('the document does not parse')
			const next = await pages.getOrCreate('bad.pdf', () => ({ data: 'parsed' }))
			expect(text(await next.bytes())).toBe('parsed')
		})

		// contract: BLOB-22
		test("a put during a run wins, and the run's call gets the put's file", async () => {
			const pages = store.files('put')
			const { opened, open } = gate()
			const started = gate()
			const run = pages.getOrCreate('a.pdf', async () => {
				started.open()
				await opened
				return { data: 'made' }
			})
			await started.opened
			await pages.put('a.pdf', 'put')
			open()
			expect(text(await (await run).bytes())).toBe('put')
			expect(text(await (await pages.get('a.pdf'))!.bytes())).toBe('put')
		})

		test('a stream and a file of this machine past 300 KB land and read back', async () => {
			const pages = store.files('large')
			const bytes = pattern(320_000)
			const streamed = await pages.getOrCreate('stream.bin', () => ({
				data: new ReadableStream<Uint8Array>({
					start(controller) {
						for (let at = 0; at < bytes.length; at += 50_000) {
							controller.enqueue(bytes.slice(at, at + 50_000))
						}
						controller.close()
					},
				}),
			}))
			expect(streamed.size).toBe(bytes.length)
			expect(await streamed.bytes()).toEqual(bytes)
			const source = join(dir, 'source.bin')
			writeFileSync(source, bytes)
			const fromFile = await pages.getOrCreate('file.bin', () => ({
				path: source,
				contentType: 'application/pdf',
			}))
			expect(fromFile.etag).toBe(streamed.etag)
			expect(fromFile.contentType).toBe('application/pdf')
			expect(await (await pages.get('file.bin'))!.bytes()).toEqual(bytes)
		})

		test('a signal stops a waiting call, and the run it waited for goes on', async () => {
			const pages = store.files('aborted')
			const { opened, open } = gate()
			const run = pages.getOrCreate('a.pdf', async () => {
				await opened
				return { data: 'made' }
			})
			await sleep(100)
			const controller = new AbortController()
			const waiting = pages.getOrCreate('a.pdf', () => ({ data: 'never' }), {
				signal: controller.signal,
			})
			await sleep(100)
			controller.abort()
			expect(await caught(waiting)).toBeInstanceOf(Error)
			open()
			expect(text(await (await run).bytes())).toBe('made')
		})
	})
}
