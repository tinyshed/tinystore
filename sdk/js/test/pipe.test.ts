// The SDK over the core in this process: the same calls as through a
// sidecar, the frames handed to the Rust library through bun:ffi. Runs when
// TINYSTORE_LIBRARY names the library `cargo build -p tinystore-ffi` made.

import { afterEach, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { open, type Store } from '../src/index.ts'

const library = process.env.TINYSTORE_LIBRARY
const opened: Store[] = []
const dirs: string[] = []

async function embeddedStore(): Promise<Store> {
	const dir = mkdtempSync(join(tmpdir(), 'tinystore-pipe-'))
	dirs.push(dir)
	const store = await open(dir, { embedded: true, library })
	opened.push(store)
	return store
}

afterEach(async () => {
	for (const store of opened.splice(0)) {
		await store.close()
	}
	for (const dir of dirs.splice(0)) {
		rmSync(dir, { recursive: true, force: true })
	}
})

test.skipIf(!library)('a value set through the core in this process reads back', async () => {
	const store = await embeddedStore()
	const sessions = store.bucket<{ user: number; device: string }>('sessions')
	await sessions.set('token', { user: 42, device: 'phone' })
	expect(await sessions.get('token')).toEqual({ user: 42, device: 'phone' })
	expect(await sessions.get('other')).toBeUndefined()
})

test.skipIf(!library)('many writes at once share the core and all land', async () => {
	const store = await embeddedStore()
	const counts = store.bucket<number>('counts')
	await Promise.all(Array.from({ length: 200 }, (_, n) => counts.set(`k${n}`, n)))
	const read = await Promise.all(Array.from({ length: 200 }, (_, n) => counts.get(`k${n}`)))
	expect(read).toEqual(Array.from({ length: 200 }, (_, n) => n))
})

test.skipIf(!library)('an answer longer than the room it is read into comes whole', async () => {
	const store = await embeddedStore()
	const blobs = store.bucket<Uint8Array>('pictures', { type: 'bytes' })
	// 300 KiB, past the 64 KiB the pipe reads into at first: the frame comes in
	// parts, the room grown for it
	const picture = Uint8Array.from({ length: 300_000 }, (_, n) => n % 251)
	await blobs.set('one', picture)
	const [read, again] = await Promise.all([blobs.get('one'), blobs.get('one')])
	expect(read).toEqual(picture)
	expect(again).toEqual(picture)
	// and a small answer after it is read as before
	await blobs.set('two', Uint8Array.of(1, 2, 3))
	expect(await blobs.get('two')).toEqual(Uint8Array.of(1, 2, 3))
})

test.skipIf(!library)('take reads a key once', async () => {
	const store = await embeddedStore()
	const codes = store.bucket<number>('login-codes', { ttl: '15m' })
	await codes.set('481-205', 7)
	expect(await codes.take('481-205')).toBe(7)
	expect(await codes.take('481-205')).toBeUndefined()
})
