// The core in this process, through its C ABI: the frames a socket would
// carry go to tinystore_send, and those the core owes come back from it, or
// from tinystore_recv once the core wakes us. Bun alone: bun:ffi.

import { dlopen, FFIType, JSCallback, type Pointer, ptr, toArrayBuffer } from 'bun:ffi'

import { ClosedError } from '../errors.ts'
import type { Transport, TransportEvents } from '../runtime.ts'

const symbols = {
	tinystore_open: {
		args: [FFIType.ptr, FFIType.u64, FFIType.function, FFIType.ptr, FFIType.ptr, FFIType.ptr],
		returns: FFIType.u64_fast,
	},
	tinystore_send: {
		args: [FFIType.ptr, FFIType.ptr, FFIType.u64, FFIType.ptr],
		returns: FFIType.u64_fast,
	},
	tinystore_recv: { args: [FFIType.ptr, FFIType.u32, FFIType.ptr], returns: FFIType.u64_fast },
	tinystore_close: { args: [FFIType.ptr], returns: FFIType.void },
	tinystore_free: { args: [FFIType.ptr, FFIType.u64], returns: FFIType.void },
} as const

/** Opens a pipe to the store in a directory, the core loaded from `library`. */
export function openPipe(library: string, dir: string, events: TransportEvents): Transport {
	const lib = dlopen(library, symbols)
	// where a call writes the address of the bytes it hands over
	const out = new BigUint64Array(1)
	const take = (handed: number | bigint): Uint8Array => {
		const length = Number(handed)
		if (length === 0) {
			return new Uint8Array(0)
		}
		const address = Number(out[0]) as Pointer
		const bytes = new Uint8Array(toArrayBuffer(address, 0, length)).slice()
		lib.symbols.tinystore_free(address, length)
		return bytes
	}
	let connection: Pointer | null = null
	// the session reads what arrives on a tick of its own, as a socket's data comes
	const deliver = (bytes: Uint8Array) => {
		if (bytes.length > 0) {
			queueMicrotask(() => events.data(bytes))
		}
	}
	const wake = new JSCallback(
		() => {
			if (connection !== null) {
				deliver(take(lib.symbols.tinystore_recv(connection, 0, ptr(out))))
			}
		},
		{ args: [FFIType.ptr], returns: FFIType.void, threadsafe: true },
	)
	const name = new TextEncoder().encode(dir)
	const opened = new BigUint64Array(1)
	const failed = lib.symbols.tinystore_open(
		ptr(name),
		name.length,
		wake,
		null,
		ptr(opened),
		ptr(out),
	)
	if (Number(failed) > 0) {
		const message = new TextDecoder().decode(take(failed))
		wake.close()
		lib.close()
		throw new ClosedError(`cannot open ${dir}: ${message}`)
	}
	connection = Number(opened[0]) as Pointer
	return {
		write(bytes) {
			if (connection !== null) {
				deliver(take(lib.symbols.tinystore_send(connection, ptr(bytes), bytes.length, ptr(out))))
			}
		},
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
