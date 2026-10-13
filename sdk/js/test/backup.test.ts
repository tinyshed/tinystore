// A backup of the whole store as plan/api/backup.md has it, through each way
// a store is reached: one zip of every engine that the server writes while
// the store keeps working, which `tinystore restore` takes back into an empty
// directory, and a file of the program's only when it is named.

import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { existsSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import type { Store } from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

const photo = new Uint8Array(3 << 20).map((_, at) => at % 251)
const binary = process.env.TINYSTORE_BIN
const restores = binary !== undefined && binary !== 'unused'

/** Restores a zip into `dir` with `tinystore restore`, saying what it printed when it failed. */
function restore(zip: string, dir: string): void {
	const run = Bun.spawnSync([binary!, 'restore', zip, dir])
	expect(run.exitCode, `${run.stdout}${run.stderr}`).toBe(0)
}

for (const way of ways) {
	describe.skipIf(!way.ready)(`backups through ${way.name}`, () => {
		let dir: string
		let store: Store
		const zips = mkdtempSync(join(tmpdir(), 'tinystore-zips-'))

		beforeAll(async () => {
			dir = mkdtempSync(join(tmpdir(), 'tinystore-backup-'))
			store = await way.open(dir)
			await store.bucket<string>('codes').set('K7Q2', 'kept across the backup')
			// past the 2 MiB a stream takes before its client grants more
			await store.files('photos').put('cat.jpg', photo)
		})

		afterAll(async () => {
			await store.close()
			await way.leave(dir)
			await removed(dir)
			await removed(zips)
		})

		/** Opens the store a backup was restored into, runs `check` on it, and lets it go. */
		async function restored(zip: string, check: (store: Store, home: string) => Promise<void>) {
			const back = mkdtempSync(join(tmpdir(), 'tinystore-restored-'))
			restore(zip, way.home(back))
			const opened = await way.open(back)
			try {
				await check(opened, way.home(back))
			} finally {
				await opened.close()
				await way.leave(back)
				await removed(back)
			}
		}

		test.skipIf(!restores)(
			'a backup is one zip of every engine, which restore takes back into an empty directory',
			async () => {
				const zip = join(zips, 'every.zip')
				const backup = await store.backup(zip)
				expect(backup.files).toBeGreaterThan(0)
				expect(backup.bytes).toBeGreaterThan(0)
				expect(backup.keyLeftOut).toBe(false)
				expect(readdirSync(zips).filter(name => name.endsWith('.part'))).toEqual([])
				expect(backup.bytes).toBeGreaterThan(photo.length)
				await restored(zip, async back => {
					expect(await back.bucket<string>('codes').get('K7Q2')).toBe('kept across the backup')
					const cat = await back.files('photos').get('cat.jpg')
					expect(new Uint8Array(await cat!.bytes())).toEqual(photo)
				})
			},
		)

		test.skipIf(!restores)(
			"a backup keeps a file of the program's only when include names it",
			async () => {
				writeFileSync(join(way.home(dir), 'secret.key'), 'k3y')
				const [named, unnamed] = [join(zips, 'named.zip'), join(zips, 'unnamed.zip')]
				await store.backup(named, { include: ['secret.key'] })
				await store.backup(unnamed)
				await restored(named, async (_, home) => {
					expect(readFileSync(join(home, 'secret.key'), 'utf8')).toBe('k3y')
				})
				await restored(unnamed, async (_, home) => {
					expect(existsSync(join(home, 'secret.key'))).toBe(false)
				})
			},
		)

		test('a backup that fails leaves no zip', async () => {
			const zip = join(zips, 'failed.zip')
			const err = await caught(store.backup(zip, { include: ['missing.txt'] }))
			expect((err as Error).message).toContain('missing.txt')
			expect(readdirSync(zips).filter(name => name.startsWith('failed.zip'))).toEqual([])
		})
	})
}
