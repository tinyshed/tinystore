// The core in this process, through its C ABI: the frames a socket would
// carry go to tinystore_send, and those the core owes are written into bytes
// of ours, by that call or by tinystore_recv once the core wakes us. No
// memory of the core's comes back: it reads our frames where they are and
// writes its own where we say. Bun alone: bun:ffi.

import { dlopen, FFIType, JSCallback, type Pointer, ptr } from 'bun:ffi'

import { ClosedError } from '../errors.ts'
import type { Transport } from '../runtime.ts'
import { bodyLength, headerSize } from '../wire/frame.ts'
import { openFailure, turning } from './shared.ts'

const symbols = {
	tinystore_open: {
		args: [
			FFIType.ptr,
			FFIType.u64,
			FFIType.ptr,
			FFIType.u64,
			FFIType.function,
			FFIType.ptr,
			FFIType.ptr,
			FFIType.ptr,
			FFIType.u64,
		],
		returns: FFIType.u64_fast,
	},
	tinystore_send: {
		args: [FFIType.ptr, FFIType.ptr, FFIType.u64, FFIType.ptr, FFIType.u64],
		returns: FFIType.u64_fast,
	},
	tinystore_recv: {
		args: [FFIType.ptr, FFIType.u32, FFIType.ptr, FFIType.u64],
		returns: FFIType.u64_fast,
	},
	tinystore_close: { args: [FFIType.ptr], returns: FFIType.void },
} as const

/** What the pipe tells the session reading it. */
export interface PipeEvents {
	/**
	 * Reads the frames whole in the first `length` bytes and says how many
	 * bytes they were. The bytes are the pipe's, written over by its next
	 * read: nothing of them may be kept.
	 */
	frames(bytes: Uint8Array, length: number): number
	/** Reads one whole frame in bytes of its own, which the session may keep. */
	frame(bytes: Uint8Array): void
	/** the store closed; called once */
	end(err: Error): void
}

/** The room for what the core owes: a frame longer than it is written into bytes of its own. */
const room = 1 << 16

/**
 * Opens a pipe to the store in a directory, the core loaded from `library`.
 * `options` is the wire's store.Options, how this process opens the store.
 */
export function openPipe(
	library: string,
	dir: string,
	options: Uint8Array,
	events: PipeEvents,
): Transport {
	const lib = dlopen(library, symbols)
	const name = new TextEncoder().encode(dir)
	const opened = new BigUint64Array(1)
	const failure = new Uint8Array(1024)
	let connection: Pointer | null = null

	// where the core writes what it owes: whole frames are read from the
	// front, and a frame's first part is held there for its rest
	const inbox = new Uint8Array(room)
	let held = 0
	// A frame longer than the inbox, in bytes as long as it is, and how much
	// of it came: the core writes its rest there, and the session keeps them,
	// so that a file's mebibyte is written once here rather than copied out
	// of an inbox grown for it (issue #10).
	let large: Uint8Array | undefined
	let filledLarge = 0

	// where the core writes next, and how many bytes it may
	const into = (): [Uint8Array, number, number] =>
		large === undefined
			? [inbox, held, inbox.length - held]
			: [large, filledLarge, large.length - filledLarge]

	// reads what the core wrote where `into` said, and says whether it filled
	// that room, which means more may wait
	const read = (written: number): boolean => {
		if (large !== undefined) {
			filledLarge += written
			if (filledLarge < large.length) {
				return false
			}
			const whole = large
			large = undefined
			events.frame(whole)
			return true
		}
		const length = held + written
		const filled = length === inbox.length
		const taken = written === 0 ? 0 : events.frames(inbox, length)
		inbox.copyWithin(0, taken, length)
		held = length - taken
		const frameLength = held < headerSize ? 0 : headerSize + bodyLength(inbox)
		if (frameLength > inbox.length) {
			large = new Uint8Array(frameLength)
			large.set(inbox.subarray(0, held))
			filledLarge = held
			held = 0
		}
		return filled
	}

	// What the session writes while it reads: its frames wait for the read's
	// end, since a call of the core made meanwhile would write over the bytes
	// being read.
	let reading = false
	const waiting: Uint8Array[] = []

	// reads what a call of the core wrote, and on while the core fills the
	// room, since no wake comes for what waits; then sends what was written
	// meanwhile
	const readOn = (written: number | undefined): void => {
		reading = true
		try {
			let filled = written === undefined || read(written)
			while (filled && connection !== null) {
				const [bytes, at, free] = into()
				filled = read(Number(lib.symbols.tinystore_recv(connection, 0, ptr(bytes, at), free)))
			}
		} finally {
			reading = false
		}
		const next = waiting.shift()
		if (next !== undefined) {
			send(next)
		}
	}

	const send = (bytes: Uint8Array): void => {
		if (connection === null) {
			return
		}
		if (reading) {
			waiting.push(bytes)
			return
		}
		const [owed, at, free] = into()
		const written = lib.symbols.tinystore_send(
			connection,
			ptr(bytes),
			bytes.length,
			ptr(owed, at),
			free,
		)
		readOn(Number(written))
	}

	const wake = new JSCallback(() => readOn(undefined), {
		args: [FFIType.ptr],
		returns: FFIType.void,
		threadsafe: true,
	})
	const failed = Number(
		lib.symbols.tinystore_open(
			ptr(name),
			name.length,
			ptr(options),
			options.length,
			wake,
			null,
			ptr(opened),
			ptr(failure),
			failure.length,
		),
	)
	if (failed > 0) {
		const message = new TextDecoder().decode(failure.subarray(0, failed))
		wake.close()
		lib.close()
		throw openFailure(dir, message)
	}
	connection = Number(opened[0]) as Pointer
	// Bun's loop with no timer armed spins while it waits for a threadsafe
	// callback from another thread: a write waiting for its sync burned a
	// whole processor (research release-2026-10-23). A timer armed, however
	// far off, lets the loop sleep until the core wakes it; it also keeps the
	// process alive while the store is open, as a sidecar's socket does.
	const sleeps = setInterval(() => {}, 2 ** 30)
	return {
		write: turning(send),
		close() {
			if (connection === null) {
				return
			}
			const closing = connection
			connection = null
			clearInterval(sleeps)
			lib.symbols.tinystore_close(closing)
			wake.close()
			events.end(new ClosedError('the store closed'))
		},
	}
}
