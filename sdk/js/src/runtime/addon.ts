// The core in this process under Node, through its addon, tinystore.node:
// the frames a socket would carry go to the addon's send, which hands back
// what is ready at once, and the core's wake tells the loop to read the rest.
// Node alone: Bun loads the C library through bun:ffi instead.

import { createRequire } from 'node:module'

import { ClosedError } from '../errors.ts'
import type { Transport } from '../runtime.ts'
import { openFailure, turning } from './shared.ts'

/** What tinystore.node exports: crates/ffi built with `node`. */
interface Addon {
	open(dir: string, options: Uint8Array, wake: () => void): AddonPipe
}

interface AddonPipe {
	send(frames: Uint8Array): Uint8Array | null
	recv(): Uint8Array | null
	close(): void
}

/** What the pipe tells the session reading it. */
export interface AddonEvents {
	/** bytes the core wrote: any part of a frame, or several */
	data(bytes: Uint8Array): void
	/** the store closed; called once */
	end(err: Error): void
}

/**
 * Opens a pipe to the store in a directory, the core loaded from the addon
 * at `path`. `options` is the wire's store.Options, how this process opens
 * the store.
 */
export function openAddon(
	path: string,
	dir: string,
	options: Uint8Array,
	events: AddonEvents,
): Transport {
	const addon = createRequire(import.meta.url)(path) as Addon
	const reader = new PipeReader(events)
	try {
		reader.pipe = addon.open(dir, options, () => reader.woken())
	} catch (err) {
		throw openFailure(dir, (err as Error).message)
	}
	return {
		write: turning(bytes => reader.send(bytes)),
		close() {
			const closing = reader.pipe
			if (closing === undefined) {
				return
			}
			reader.pipe = undefined
			closing.close()
			events.end(new ClosedError('the store closed'))
		},
	}
}

/** The addon's pipe as the session reads and writes it. */
class PipeReader {
	pipe: AddonPipe | undefined
	readonly #events: AddonEvents
	// What the session writes while it reads: its frames wait for the read's
	// end, since an answer handed back meanwhile would reach the session in
	// the middle of the bytes it is reading.
	#reading = false
	readonly #waiting: Uint8Array[] = []

	constructor(events: AddonEvents) {
		this.#events = events
	}

	/** The core's wake: what it has ready, unless a read under way, which reads until nothing is, takes it. */
	woken(): void {
		if (!this.#reading) {
			this.#take(this.pipe?.recv() ?? null)
		}
	}

	send(bytes: Uint8Array): void {
		if (this.pipe === undefined) {
			return
		}
		if (this.#reading) {
			this.#waiting.push(bytes)
			return
		}
		this.#take(this.pipe.send(bytes))
	}

	#take(bytes: Uint8Array | null): void {
		this.#reading = true
		try {
			let next = bytes
			while (next !== null && this.pipe !== undefined) {
				this.#events.data(next)
				next = this.pipe.recv()
			}
		} finally {
			this.#reading = false
		}
		const queued = this.#waiting.shift()
		if (queued !== undefined) {
			this.send(queued)
		}
	}
}
