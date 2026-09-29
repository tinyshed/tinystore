// blobs through a real tinystore serve, the one test/binary.ts built.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { ConflictError, InvalidError, open, type Store } from '../src/index.ts'

let dir: string
let store: Store

beforeAll(async () => {
	dir = mkdtempSync(join(tmpdir(), 'tinystore-blobs-'))
	store = await open(dir, { private: true })
})

afterAll(async () => {
	await store.close()
	await Bun.sleep(50)
	rmSync(dir, { recursive: true, force: true, maxRetries: 20, retryDelay: 50 })
})

async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

function pattern(n: number): Uint8Array {
	return Uint8Array.from({ length: n }, (_, i) => (i * 31 + 7) & 0xff)
}

describe('objects', () => {
	test('an object comes back byte for byte, small, empty and past a body', async () => {
		const files = store.blobs.bucket('files').of('users', 7)
		for (const size of [0, 1, 16 << 10, 70_000, (3 << 20) + 17]) {
			const bytes = pattern(size)
			const put = await files.put(`f${size}`, bytes, { contentType: 'application/octet-stream' })
			expect(put.size).toBe(size)
			const got = await files.get(`f${size}`)
			expect(got?.etag).toBe(put.etag)
			expect(await got?.bytes()).toEqual(bytes)
		}
		expect(await files.get('nothing')).toBeUndefined()
	})

	test('a Blob gives its type and size, and a stream of unknown length goes whole', async () => {
		const files = store.blobs.bucket('typed')
		const put = await files.put('note.txt', new Blob(['hello'], { type: 'text/plain' }))
		// Bun spells a Blob's text type with its charset, and the object keeps it so
		expect(put.contentType).toStartWith('text/plain')
		expect(put.size).toBe(5)
		expect(await (await files.get('note.txt'))?.text()).toBe('hello')

		const chunks = [pattern(100_000), pattern(3), pattern(70_000)]
		const stream = new ReadableStream<Uint8Array>({
			pull(controller) {
				const next = chunks.shift()
				if (next === undefined) {
					controller.close()
				} else {
					controller.enqueue(next)
				}
			},
		})
		const streamed = await files.put('streamed', stream)
		expect(streamed.size).toBe(170_003)
	})

	test('a stream that disagrees with its size is refused and leaves nothing', async () => {
		const files = store.blobs.bucket('sized')
		expect(await caught(files.put('short', pattern(10), { size: 11 }))).toBeInstanceOf(InvalidError)
		expect(await files.stat('short')).toBeUndefined()
	})

	test('a range reads its bytes and no others', async () => {
		const files = store.blobs.bucket('ranged')
		const bytes = pattern(200_000)
		await files.put('video', bytes)
		const got = await files.get('video', { offset: 1024, length: 4096 })
		expect(await got?.bytes()).toEqual(bytes.subarray(1024, 1024 + 4096))
	})

	test('a reader may stop halfway, and the connection goes on', async () => {
		const files = store.blobs.bucket('stopped')
		await files.put('big', pattern(4 << 20))
		const got = await files.get('big')
		const reader = got!.body.getReader()
		await reader.read()
		await reader.cancel()
		expect((await files.stat('big'))?.size).toBe(4 << 20)
	})
})

describe('conditions and moves', () => {
	test('of two conditional writes one conflicts', async () => {
		const files = store.blobs.bucket('conditional')
		const first = await files.put('doc', 'v1', { ifNoneMatch: true })
		expect(await caught(files.put('doc', 'v2', { ifNoneMatch: true }))).toBeInstanceOf(
			ConflictError,
		)
		await files.put('doc', 'v2', { ifMatch: first.etag })
		expect(await caught(files.put('doc', 'v3', { ifMatch: first.etag }))).toBeInstanceOf(
			ConflictError,
		)
		expect(await (await files.get('doc'))?.text()).toBe('v2')
	})

	test('copy and move share the bytes; scan, usage and clear see the folder', async () => {
		const files = store.blobs.bucket('mail').of('users', 42)
		await files.put('pending/u1', 'message', { meta: { to: 'bob' } })
		const copied = await files.copy('pending/u1', 'archive/u1')
		expect(copied.meta).toEqual({ to: 'bob' })
		const moved = await files.move('pending/u1', 'sent/u1')
		expect(moved.key).toBe('sent/u1')
		expect(await files.stat('pending/u1')).toBeUndefined()

		const keys = []
		for await (const o of files.all()) {
			keys.push(o.key)
		}
		expect(keys).toEqual(['archive/u1', 'sent/u1'])
		expect((await files.scan({ prefix: 'sent/' })).items.map(o => o.key)).toEqual(['sent/u1'])
		expect(await files.usage()).toEqual({ objects: 2, bytes: 14 })
		await files.clear()
		expect(await files.usage()).toEqual({ objects: 0, bytes: 0 })
	})
})
