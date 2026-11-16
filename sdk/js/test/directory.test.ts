// directories as docs/guides/blobs/directories.md has them over the protocol,
// through each way a store is reached: a scan, its changes from a cursor, the
// list's answers, a file read from the disk held to its version, a growing
// file read past the size its get gave, and a token that names no path.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { appendFileSync, mkdirSync, mkdtempSync, rmSync, utimesSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { ConflictError, InvalidError, type Store } from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

for (const way of ways) {
	describe.skipIf(!way.ready)(`directories through ${way.name}`, () => {
		let dir: string
		let folder: string
		let store: Store

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-directory-'))
			folder = join(dir, 'movies')
			mkdirSync(folder, { recursive: true })
			store = await way.open(dir)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
		})

		/** The directory `movies` on the test's folder: by its path, or over a token the server's own. */
		const movies = (options?: { growing?: boolean }) =>
			store.directory('movies', way.token === true ? options : { ...options, path: folder })

		function write(path: string, text: string, seconds: number): void {
			const file = join(folder, path)
			mkdirSync(join(file, '..'), { recursive: true })
			writeFileSync(file, text)
			utimesSync(file, seconds, seconds)
		}

		test('a scan lists the folder, its changes follow a cursor, and a read comes from the disk', async () => {
			write('Dune (2021)/Dune.mkv', 'spice must flow', 1_700_000_000)
			write('Heat.mkv', 'heat', 1_700_000_000)
			const directory = movies()
			expect(await directory.usage()).toEqual({ count: 0, size: 0 })
			const scan = await directory.scan()
			expect(scan).toEqual({ added: 2, changed: 0, removed: 0, skipped: 0, count: 2, size: 19 })

			const first = await directory.changes()
			expect(first.items.map(item => [item.path, item.change])).toEqual([
				['Dune (2021)/Dune.mkv', 'added'],
				['Heat.mkv', 'added'],
			])
			const film = (await directory.get('Dune (2021)/Dune.mkv'))!
			expect(film.etag).toStartWith('"d-f-')
			expect(film.contentType).toBe('application/octet-stream')
			expect(await film.text()).toBe('spice must flow')
			expect(await film.slice(6, 10).text()).toBe('must')
			expect((await directory.folder('Dune (2021)').list()).files.map(file => file.path)).toEqual([
				'Dune.mkv',
			])
			expect(await directory.head('Heat.mkv')).toMatchObject({ size: 4 })

			// a file past one message is read from the disk as it is read, held to its get's version
			write('Big.mkv', 'x'.repeat(3 << 20), 1_700_000_000)
			await directory.scan()
			const big = (await directory.get('Big.mkv'))!
			write('Big.mkv', 'y'.repeat(3 << 20), 1_700_000_200)
			expect(await caught(big.text())).toBeInstanceOf(ConflictError)
			rmSync(join(folder, 'Big.mkv'))
			await directory.scan()
			const before = await directory.changes({ after: first.next })

			rmSync(join(folder, 'Heat.mkv'))
			write('Dune (2021)/Dune.mkv', 'spice must flow, longer', 1_700_000_100)
			expect(await directory.scan()).toMatchObject({ added: 0, changed: 1, removed: 1, count: 1 })
			const since = await directory.changes({ after: before.next })
			expect(since.items.map(item => [item.path, item.change, item.info === undefined])).toEqual([
				['Dune (2021)/Dune.mkv', 'changed', false],
				['Heat.mkv', 'removed', true],
			])
			const after = await directory.changes({ after: since.next })
			expect(after).toEqual({ items: [], next: since.next })
			expect(await directory.get('nothing.mkv')).toBeUndefined()

			await directory.forget()
			expect(await directory.usage()).toEqual({ count: 0, size: 0 })
			rmSync(join(folder, 'Dune (2021)'), { recursive: true })
		})

		test("a growing directory's read goes past the size its get gave", async () => {
			write('rec.ts', '0123', 1_700_000_000)
			const live = movies({ growing: true })
			await live.scan()
			const recording = (await live.get('rec.ts'))!
			expect(recording.size).toBe(4)
			appendFileSync(join(folder, 'rec.ts'), '4567')
			expect(await recording.text()).toBe('01234567')
			rmSync(join(folder, 'rec.ts'))
		})

		test.if(way.token === true)('a connection that came with a token names no path', async () => {
			const named = store.directory('elsewhere', { path: folder })
			expect(await caught(named.usage())).toBeInstanceOf(InvalidError)
			expect(await caught(store.directory('photos').usage())).toBeInstanceOf(InvalidError)
		})
	})
}
