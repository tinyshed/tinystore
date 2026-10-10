// The core in this process, through its C ABI: the frames a socket would
// carry go to tinystore_send, and those the core owes are written into bytes
// of ours, by that call or by tinystore_recv once the core wakes us. No
// memory of the core's comes back: it reads our frames where they are and
// writes its own where we say. Bun alone: bun:ffi.

import { dlopen, FFIType, JSCallback, type Pointer, ptr } from 'bun:ffi'

import { ClosedError } from '../errors.ts'
import type { Transport } from '../runtime.ts'

const symbols = {
	tinystore_open: {
		args: [
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
	/** the store closed; called once */
	end(err: Error): void
}

/** The room for what the core owes, to begin with and once a large frame has passed. */
const room = 1 << 16
/** Room past this is given back once nothing is held in it. */
const kept = 1 << 20

/** Opens a pipe to the store in a directory, the core loaded from `library`. */
export function openPipe(library: string, dir: string, events: PipeEvents): Transport {
	const lib = dlopen(library, symbols)
	const name = new TextEncoder().encode(dir)
	const opened = new BigUint64Array(1)
	const failure = new Uint8Array(1024)
	let connection: Pointer | null = null

	// where the core writes what it owes: whole frames are read from the
	// front, and a frame's first part is held there for its rest
	let inbox = new Uint8Array(room)
	let held = 0

	// reads the frames among the `written` bytes behind what is held, and
	// says whether the core filled the room, which means more may wait
	const read = (written: number): boolean => {
		const length = held + written
		const filled = length === inbox.length
		const taken = written === 0 ? 0 : events.frames(inbox, length)
		inbox.copyWithin(0, taken, length)
		held = length - taken
		if (held === inbox.length) {
			// a frame longer than the room: twice the room, until it fits
			const grown = new Uint8Array(inbox.length * 2)
			grown.set(inbox)
			inbox = grown
		} else if (held === 0 && inbox.length > kept) {
			inbox = new Uint8Array(room)
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
				const free = inbox.length - held
				filled = read(Number(lib.symbols.tinystore_recv(connection, 0, ptr(inbox, held), free)))
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
		const free = inbox.length - held
		const written = lib.symbols.tinystore_send(
			connection,
			ptr(bytes),
			bytes.length,
			ptr(inbox, held),
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
		throw new ClosedError(`cannot open ${dir}: ${message}`)
	}
	connection = Number(opened[0]) as Pointer
	return {
		write: send,
		close() {
			if (connection === null) {
				return
			}
			const closing = connection
			connection = null
			lib.symbols.tinystore_close(closing)
			wake.close()
			events.end(new ClosedError('the store closed'))
		},
	}
}
