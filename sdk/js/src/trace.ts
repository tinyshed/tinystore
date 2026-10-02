// The trace a call runs in, as Go's records.WithTrace carries one in a
// context: a logger's lines and appended records of no trace of their own
// take it.

import { AsyncLocalStorage } from 'node:async_hooks'

/** A trace's 16 bytes and the span's 8, as bytes or hex. */
export interface Trace {
	traceId: Uint8Array | string
	spanId?: Uint8Array | string
}

const carried = new AsyncLocalStorage<Trace>()

/**
 * Runs fn in a trace: a logger's lines, and records appended without a trace
 * of their own, inside it and in what it awaits, take its trace and span.
 *
 * ```ts
 * await withTrace({ traceId, spanId }, async () => {
 *   log.info('charged')   // a record of that trace and span
 * })
 * ```
 */
export function withTrace<T>(trace: Trace, fn: () => T): T {
	return carried.run(trace, fn)
}

/** The trace the caller runs in, from withTrace. */
export function currentTrace(): Trace | undefined {
	return carried.getStore()
}
