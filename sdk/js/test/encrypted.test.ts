// An encrypted bucket: its values sealed with the store's encryption key, which
// the store makes in its directory or reads from the file its options name.
// Through each way a program opens a store of its own.

import { afterEach, describe, expect, test } from 'bun:test'
import { existsSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { InvalidError, type OpenOptions, open, type Store } from '../src/index.ts'
import { caught, removed, until } from './ways.ts'

const library = process.env.TINYSTORE_LIBRARY
const binary = process.env.TINYSTORE_BIN
const served = binary !== undefined && binary !== 'unused'

const opens: { name: string; ready: boolean; options: OpenOptions }[] = [
	{
		name: 'the core in this process',
		ready: library !== undefined,
		options: { embedded: true, library },
	},
	{ name: 'a private child', ready: served, options: { private: true } },
	{ name: 'a sidecar', ready: served, options: { idle: 1 } },
]

const PASSWORD = 'hunter2-the-password'

/** Whether a file under `dir` holds `text`. LOCK is the store's alone while it is open. */
function shown(dir: string, text: string): boolean {
	return readdirSync(dir, { withFileTypes: true, recursive: true })
		.filter(entry => entry.isFile() && entry.name !== 'LOCK')
		.some(entry => readFileSync(join(entry.parentPath, entry.name)).includes(text))
}

for (const way of opens) {
	describe.skipIf(!way.ready)(`an encrypted bucket through ${way.name}`, () => {
		const stores: Store[] = []
		const dirs: string[] = []

		const opened = async (dir: string, options: OpenOptions = {}): Promise<Store> => {
			const store = await open(dir, { ...way.options, ...options })
			stores.push(store)
			return store
		}

		/** Closes every store and waits until whatever served its directory has let go. */
		const closed = async (): Promise<void> => {
			for (const store of stores.splice(0)) {
				await store.close()
			}
			for (const dir of dirs) {
				await until(() => !existsSync(join(dir, 'server', 'SERVE')))
			}
		}

		afterEach(async () => {
			await closed()
			for (const dir of dirs.splice(0)) {
				await removed(dir)
			}
		})

		const directory = (): string => {
			const dir = mkdtempSync(join(tmpdir(), 'tinystore-encrypted-'))
			dirs.push(dir)
			return dir
		}

		test('its files show no value, and its key is made beside them', async () => {
			const dir = directory()
			const store = await opened(dir)
			await store.bucket<string>('notes').set('n1', 'a-note-kept-plain')
			expect(existsSync(join(dir, 'encryption.key'))).toBe(false)

			const passwords = store.bucket<string>('passwords', { encrypted: true })
			await passwords.set('source-42', PASSWORD)
			expect(await passwords.get('source-42')).toBe(PASSWORD)
			expect(readFileSync(join(dir, 'encryption.key'), 'utf8').trim()).toMatch(/^[0-9a-f]{64}$/)

			const plain = await caught(store.bucket<string>('passwords').get('source-42'))
			expect(plain).toBeInstanceOf(InvalidError)
			expect((plain as Error).message).toContain('the name holds encrypted values, not values')

			await closed()
			expect(shown(dir, 'a-note-kept-plain')).toBe(true)
			expect(shown(dir, PASSWORD)).toBe(false)
			const again = await opened(dir)
			expect(await again.bucket<string>('passwords', { encrypted: true }).take('source-42')).toBe(
				PASSWORD,
			)
		})

		test('its key is the file the options name, which the store only reads', async () => {
			const dir = directory()
			const key = join(directory(), 'tinystore.key')
			const unmade = await opened(dir, { encryptionKeyFile: key })
			const refused = await caught(unmade.bucket<string>('passwords', { encrypted: true }).get('k'))
			expect((refused as Error).message).toContain('tinystore.key')
			expect(existsSync(key)).toBe(false)
			await closed()

			writeFileSync(key, `${'5a'.repeat(32)}\n`)
			const store = await opened(dir, { encryptionKeyFile: key })
			await store.bucket<string>('passwords', { encrypted: true }).set('source-42', PASSWORD)
			expect(existsSync(join(dir, 'encryption.key'))).toBe(false)
			await closed()

			const again = await opened(dir, { encryptionKeyFile: key })
			expect(await again.bucket<string>('passwords', { encrypted: true }).get('source-42')).toBe(
				PASSWORD,
			)
		})
	})
}
