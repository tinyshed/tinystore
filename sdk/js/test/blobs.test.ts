// files as plan/api/blobs.md has them over protocol 2, through each way a
// store is reached: writes in one call and in pieces, whole reads checked,
// folders, conditions, copies, expiry, and uploads that appear at their
// commit and leave nothing otherwise.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import {
	existsSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	statSync,
	writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import {
	ConflictError,
	CorruptError,
	InvalidError,
	LimitError,
	NotFoundError,
	type Size,
	type Store,
} from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

function pattern(n: number, seed = 7): Uint8Array<ArrayBuffer> {
	return Uint8Array.from({ length: n }, (_, i) => (i * 31 + seed) & 0xff)
}

function joined(chunks: Uint8Array[]): Uint8Array<ArrayBuffer> {
	const whole = new Uint8Array(chunks.reduce((n, chunk) => n + chunk.length, 0))
	let at = 0
	for (const chunk of chunks) {
		whole.set(chunk, at)
		at += chunk.length
	}
	return whole
}

/** A term's end as the server set it, within a minute of what the test expects. */
function about(expires: Date | undefined, from: number, term: number): boolean {
	return expires !== undefined && Math.abs(expires.getTime() - (from + term)) < 60_000
}

const hour = 3_600_000

for (const way of ways) {
	describe.skipIf(!way.ready)(`files through ${way.name}`, () => {
		let dir: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-files-'))
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		test('a file comes back byte for byte, empty, inline, in one call and in pieces', async () => {
			const sizes = store.files('sizes')
			for (const size of [0, 1, 16 << 10, 70_000, (3 << 20) + 17]) {
				const bytes = pattern(size)
				const put = await sizes.put(`f${size}`, bytes)
				expect(put.size).toBe(size)
				expect(put.etag).toBe(`"${createHash('sha256').update(bytes).digest('hex')}"`)
				expect(put.contentType).toBe('application/octet-stream')
				expect(await sizes.head(`f${size}`)).toEqual(put)
				const file = await sizes.get(`f${size}`)
				expect(file?.etag).toBe(put.etag)
				expect(await file?.bytes()).toEqual(bytes)
			}
			expect(await sizes.get('nothing')).toBeUndefined()
			expect(await sizes.head('nothing')).toBeUndefined()
		})

		test('a Blob gives its type, and a stream of unknown length goes in pieces', async () => {
			const bodies = store.files('bodies')
			const note = await bodies.put('note.txt', new Blob(['hello'], { type: 'text/plain' }))
			// Bun spells a Blob's text type with its charset, and the file keeps it so
			expect(note.contentType).toStartWith('text/plain')
			expect(await (await bodies.get('note.txt'))?.text()).toBe('hello')

			const chunks = [pattern(100_000), pattern(3), pattern(3 << 20, 11)]
			async function* iterated() {
				yield* chunks
			}
			const put = await bodies.put('iterated.bin', iterated(), {
				contentType: 'application/x-test',
			})
			expect(put.size).toBe(joined(chunks).length)
			const iteratedFile = await bodies.get('iterated.bin')
			expect(iteratedFile?.contentType).toBe('application/x-test')
			expect(await iteratedFile?.bytes()).toEqual(joined(chunks))

			const left = [...chunks]
			const stream = new ReadableStream<Uint8Array>({
				pull(controller) {
					const next = left.shift()
					if (next === undefined) {
						controller.close()
						return
					}
					controller.enqueue(next)
				},
			})
			await bodies.put('streamed.bin', stream)
			const read: Uint8Array[] = []
			for await (const piece of (await bodies.get('streamed.bin'))!.stream()) {
				read.push(piece)
			}
			expect(joined(read)).toEqual(joined(chunks))

			await bodies.put('data.json', JSON.stringify({ a: 1 }), { meta: { name: 'Data.json' } })
			const data = await bodies.get('data.json')
			expect(data?.meta).toEqual({ name: 'Data.json' })
			expect(await data?.json()).toEqual({ a: 1 })
			expect(await caught(data!.text())).toBeInstanceOf(InvalidError)
		})

		test('a file read after a replace and a delete is the one get found', async () => {
			const pinned = store.files('pinned')
			const first = pattern(3 << 20)
			await pinned.put('a.bin', first)
			const file = await pinned.get('a.bin')
			await pinned.put('a.bin', pattern(5))
			await pinned.delete('a.bin')
			expect(await file?.bytes()).toEqual(first)
		})

		test('a folder is whole segments, listed, counted and cleared', async () => {
			const media = store.files('media')
			await media.folder('users', 42).put('photos/1.jpg', 'one')
			expect(await (await media.get('users/42/photos/1.jpg'))?.text()).toBe('one')
			await media.put('users/42/photos/10.jpg', 'ten')
			await media.put('users/420/photos/1.jpg', 'other')
			const user = media.folder('users', 42)

			const page = await user.list({ prefix: 'photos/1' })
			expect(page.files.map(file => file.path)).toEqual(['photos/1.jpg', 'photos/10.jpg'])
			expect(page.next).toBeUndefined()
			const first = await user.list({ limit: 1 })
			expect(first.files.map(file => file.path)).toEqual(['photos/1.jpg'])
			const rest = await user.list({ after: first.next })
			expect(rest.files.map(file => file.path)).toEqual(['photos/10.jpg'])
			const all: string[] = []
			for await (const file of media.all()) {
				all.push(file.path)
			}
			expect(all).toEqual([
				'users/42/photos/1.jpg',
				'users/42/photos/10.jpg',
				'users/420/photos/1.jpg',
			])

			expect(await user.usage()).toEqual({ count: 2, size: 6 })
			await user.clear()
			expect(await user.usage()).toEqual({ count: 0, size: 0 })
			expect(await media.head('users/420/photos/1.jpg')).toBeDefined()
			expect(await caught(media.folder('users', '').put('a', 'x'))).toBeInstanceOf(InvalidError)
			expect(await caught(media.folder('users/42').put('a', 'x'))).toBeInstanceOf(InvalidError)
			expect(await caught(media.put('../a', 'x'))).toBeInstanceOf(InvalidError)
		})

		test('ifMatch and create write only as they say', async () => {
			const docs = store.files('conditions')
			const first = await docs.put('notes.md', 'v1')
			await docs.put('notes.md', 'v2', { ifMatch: first.etag })
			expect(await caught(docs.put('notes.md', 'v3', { ifMatch: first.etag }))).toBeInstanceOf(
				ConflictError,
			)
			expect(await (await docs.get('notes.md'))?.text()).toBe('v2')
			expect(await docs.create('new.md', 'a')).toBe(true)
			expect(await docs.create('new.md', 'b')).toBe(false)
			expect(await (await docs.get('new.md'))?.text()).toBe('a')
		})

		test('a copy shares the bytes, and a copy or a rename replaces a file only by its ETag', async () => {
			const docs = store.files('moves')
			const source = await docs.put('a.md', 'alpha')
			const copy = await docs.copy('a.md', 'b.md')
			expect([copy.path, copy.etag]).toEqual(['b.md', source.etag])
			expect(await caught(docs.copy('a.md', 'b.md'))).toBeInstanceOf(ConflictError)
			const target = await docs.put('c.md', 'gamma')
			const renamed = await docs.rename('a.md', 'c.md', { ifMatch: target.etag })
			expect(renamed.etag).toBe(source.etag)
			expect(await docs.head('a.md')).toBeUndefined()
			expect(await caught(docs.rename('a.md', 'd.md'))).toBeInstanceOf(NotFoundError)
			await docs.delete('b.md')
			await docs.delete('b.md')
			expect(await docs.head('b.md')).toBeUndefined()
		})

		test('a file expires by its own term or its files', async () => {
			const reports = store.files('reports', { ttl: '24h' })
			const now = Date.now()
			expect(about((await reports.put('a.csv', 'a')).expires, now, 24 * hour)).toBe(true)
			expect(
				about((await reports.put('b.csv', 'b', { ttl: '7d' })).expires, now, 7 * 24 * hour),
			).toBe(true)
			expect(await reports.expire('a.csv', '1h')).toBe(true)
			expect(about((await reports.head('a.csv'))?.expires, now, hour)).toBe(true)
			expect(await reports.expire('none.csv', '1h')).toBe(false)
			expect((await store.files('kept').put('a', 'a')).expires).toBeUndefined()
		})

		test('an upload appears at its commit, and one left without it leaves nothing', async () => {
			const uploads = store.files('uploads')
			const line = `${'x'.repeat(100_000)}\n`
			{
				await using upload = await uploads.upload('2026-10/report.csv', { contentType: 'text/csv' })
				for (let i = 0; i < 40; i++) {
					await upload.write(line)
				}
				expect(await uploads.head('2026-10/report.csv')).toBeUndefined()
				const info = await upload.commit()
				expect([info.size, info.contentType]).toEqual([40 * line.length, 'text/csv'])
			}
			expect((await uploads.head('2026-10/report.csv'))?.size).toBe(40 * line.length)
			{
				await using upload = await uploads.upload('left.bin')
				await upload.write(pattern(3 << 20))
			}
			expect(await uploads.head('left.bin')).toBeUndefined()

			const sized = await uploads.upload('sized.bin', { size: 10 })
			await sized.write(pattern(11))
			expect(await caught(sized.commit())).toBeInstanceOf(InvalidError)
			expect(await uploads.head('sized.bin')).toBeUndefined()
		})

		test('a file past maxFileSize is refused and leaves nothing', async () => {
			const small = store.files('small', { maxFileSize: '64KiB' })
			expect(await caught(small.put('big', pattern(65_537)))).toBeInstanceOf(LimitError)
			async function* growing() {
				yield pattern(40_000)
				yield pattern(40_000)
			}
			expect(await caught(small.put('big', growing()))).toBeInstanceOf(LimitError)
			expect(await small.head('big')).toBeUndefined()
			expect(() => store.files('wrong', { maxFileSize: '2GB' as Size })).toThrow(InvalidError)
		})

		test('a whole read of a changed byte fails CorruptError before its end', async () => {
			const checked = store.files('checked')
			const bytes = pattern((2 << 20) + 5, 3)
			await checked.put('a.bin', bytes)
			const root = existsSync(join(dir, 'store', 'blobs')) ? join(dir, 'store') : dir
			const stored = readdirSync(join(root, 'blobs', 'objects'), {
				recursive: true,
				withFileTypes: true,
			})
				.filter(entry => entry.isFile())
				.map(entry => join(entry.parentPath, entry.name))
				.filter(path => statSync(path).size === bytes.length)
			expect(stored).toHaveLength(1)
			const changed = readFileSync(stored[0]!)
			changed[4096]! ^= 0x5a
			writeFileSync(stored[0]!, changed)

			const read: Uint8Array[] = []
			const failed = await caught(
				(async () => {
					for await (const piece of (await checked.get('a.bin'))!.stream()) {
						read.push(piece)
					}
				})(),
			)
			expect(failed).toBeInstanceOf(CorruptError)
			expect(joined(read).length).toBeLessThan(bytes.length)
		})
	})
}
