// A store that is a node: the core in this process copying the files of
// another node, reading them here and passing their writes to their home.

import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { open, type Store } from '../src/index.ts'
import { removed, until } from './ways.ts'

const library = process.env.TINYSTORE_LIBRARY

describe.skipIf(library === undefined)('a node in this process', () => {
	const stores: Store[] = []
	const dirs: string[] = []

	afterEach(async () => {
		for (const store of stores.splice(0).reverse()) {
			await store.close()
		}
		for (const dir of dirs.splice(0)) {
			await removed(dir)
		}
	})

	const node = async (serve: string | undefined, from: string[]): Promise<Store> => {
		const dir = mkdtempSync(join(tmpdir(), 'tinystore-node-'))
		dirs.push(dir)
		const store = await open(dir, { embedded: true, library, node: { serve, from } })
		stores.push(store)
		return store
	}

	test("a copy of a copy reads its home's writes and passes its own to the home", async () => {
		const port = 20000 + Math.floor(Math.random() * 20000)
		const home = await node(`127.0.0.1:${port}`, [])
		await home.bucket<string>('sessions').set('seed', 'from the home')
		const between = await node(`127.0.0.1:${port + 1}`, [`127.0.0.1:${port}`])
		const far = await node(undefined, [`127.0.0.1:${port + 1}`])

		const sessions = far.bucket<string>('sessions')
		expect(await sessions.get('seed')).toBe('from the home')
		await sessions.set('far', 'written two links away')
		expect(await sessions.get('far')).toBe('written two links away')
		expect(await home.bucket<string>('sessions').get('far')).toBe('written two links away')
		expect(await far.counters('views').add('page', 2)).toBe(2)
		expect(await home.counters('views').get('page')).toBe(2)

		await home.bucket<string>('sessions').set('later', 'after the copies')
		await until(async () => (await sessions.get('later')) !== undefined)
		expect(await between.bucket<string>('sessions').get('later')).toBe('after the copies')
		expect(await sessions.get('later')).toBe('after the copies')
	})
})
