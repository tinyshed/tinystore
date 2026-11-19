// A store for a test: a directory of its own under the system's temporary
// one, the core in this process, and the directory removed at close.
//
//     import { openTemp } from '@tinyshed/tinystore/testing'
//
//     await using store = await openTemp({ clock: new Date('2026-11-19T09:00:00Z') })
//     await store.clock.advance('1h')

import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

import type { Link } from './connection.ts'
import { InUseError } from './errors.ts'
import { type OpenOptions, openMade, Store } from './store.ts'

/** How long a removal tries again while a file stays held: Windows lets go of a closed file a moment late. */
const letGo = 1000

/** A store in a directory of its own, which its close removes. */
export class TempStore extends Store {
	/** the store's directory, for a test that looks at its files */
	readonly dir: string

	/** Temporary stores come from `openTemp`. */
	constructor(link: Link, dir: string) {
		super(link)
		this.dir = dir
	}

	/**
	 * Closes the store, then removes its directory. A file still held a
	 * second after the close is a handle the test leaked, and fails the
	 * close with InUseError rather than leave the directory behind unsaid.
	 */
	override async close(): Promise<void> {
		await super.close()
		await removed(this.dir)
	}
}

/**
 * Opens a store in a new, empty directory under the system's temporary
 * one, named `tinystore-` and a random part, which the store's close
 * removes (contract STORE-12). It opens embedded, the core in this process,
 * unless `private` asks for a child; every option of `open` passes through,
 * `clock` among them.
 */
export async function openTemp(options: OpenOptions = {}): Promise<TempStore> {
	const dir = await mkdtemp(join(tmpdir(), 'tinystore-'))
	const embedded = options.private !== true
	try {
		return await openMade(dir, { embedded, ...options }, link => new TempStore(link, dir))
	} catch (err) {
		await removed(dir)
		throw err
	}
}

async function removed(dir: string): Promise<void> {
	const deadline = Date.now() + letGo
	for (;;) {
		try {
			// Bun's rm takes no maxRetries, so the loop tries again itself
			await rm(dir, { recursive: true, force: true })
			return
		} catch (err) {
			if (Date.now() > deadline) {
				const why = err instanceof Error ? err.message : String(err)
				throw new InUseError(`openTemp ${dir}: a file stays held after the store closed: ${why}`)
			}
			await sleep(20)
		}
	}
}
