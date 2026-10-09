// A backup of the whole store, written by the server serving it, which
// tinystore restore takes back into an empty directory.

import { expect, test } from 'bun:test'
import { existsSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { open } from '../src/index.ts'

test('a backup is one zip of every engine, which restore takes back into an empty directory', async () => {
	const dir = mkdtempSync(join(tmpdir(), 'tinystore-backup-'))
	const zip = join(mkdtempSync(join(tmpdir(), 'tinystore-zip-')), 'backup.zip')
	{
		await using store = await open(dir, { private: true })
		await store.bucket<string>('codes').set('K7Q2', 'kept across the backup')
		await store.backup(zip)
	}
	expect(existsSync(zip)).toBe(true)
	expect(readdirSync(join(zip, '..')).filter(name => name.endsWith('.part'))).toEqual([])

	const restored = join(mkdtempSync(join(tmpdir(), 'tinystore-restored-')), 'data')
	const restore = Bun.spawnSync([process.env.TINYSTORE_BIN ?? '', 'restore', zip, restored])
	expect(restore.exitCode).toBe(0)
	await using back = await open(restored, { private: true })
	expect(await back.bucket<string>('codes').get('K7Q2')).toBe('kept across the backup')
})

test("a backup keeps a file of the application's only when files names it", async () => {
	const dir = mkdtempSync(join(tmpdir(), 'tinystore-backup-'))
	const zip = join(mkdtempSync(join(tmpdir(), 'tinystore-zip-')), 'backup.zip')
	{
		await using store = await open(dir, { private: true })
		writeFileSync(join(dir, 'secret.key'), 'k3y')
		await store.backup(zip, { files: ['secret.key'] })
	}
	const restored = join(mkdtempSync(join(tmpdir(), 'tinystore-restored-')), 'data')
	const restore = Bun.spawnSync([process.env.TINYSTORE_BIN ?? '', 'restore', zip, restored])
	expect(restore.exitCode).toBe(0)
	expect(readFileSync(join(restored, 'secret.key'), 'utf8')).toBe('k3y')
})
