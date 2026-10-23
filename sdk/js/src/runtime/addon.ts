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

	// What the session writes while it reads: its frames wait for the read's
	// end, since an answer handed back meanwhile would reach the session in
	// the middle of the bytes it is reading.
	let reading = false
	const waiting: Uint8Array[] = []

	const take = (bytes: Uint8Array | null): void => {
		reading = true
		try {
			let next = bytes
			while (next !== null && pipe !== undefined) {
				events.data(next)
				next = pipe.recv()
			}
		} finally {
			reading = false
		}
		const queued = waiting.shift()
		if (queued !== undefined) {
			send(queued)
		}
	}

	const send = (bytes: Uint8Array): void => {
		if (pipe === undefined) {
			return
		}
		if (reading) {
			waiting.push(bytes)
			return
		}
		take(pipe.send(bytes))
	}

	let pipe: AddonPipe | undefined
	try {
		pipe = addon.open(dir, options, () => {
			if (!reading) {
				take(pipe?.recv() ?? null)
			}
		})
	} catch (err) {
		throw openFailure(dir, (err as Error).message)
	}
	return {
		write: turning(send),
		close() {
			const closing = pipe
			if (closing === undefined) {
				return
			}
			pipe = undefined
			closing.close()
			events.end(new ClosedError('the store closed'))
		},
	}
}
