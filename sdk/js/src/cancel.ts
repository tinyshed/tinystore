// The signal a call runs under, as a Go call carries its context: every call
// made inside withSignal, and inside what it awaits, ends when it aborts.

import { AsyncLocalStorage } from 'node:async_hooks'

const carried = new AsyncLocalStorage<AbortSignal>()

/**
 * Runs fn under a signal: every call it makes, a kv get, a statement, a
 * scan, rejects once the signal aborts, its stream cancelled on the wire. A
 * call given a signal of its own, as work and put take one, keeps that one.
 *
 * ```ts
 * await withSignal(AbortSignal.timeout(2000), async () => {
 *   const notes = await app.all`select * from notes`
 * })
 * ```
 */
export function withSignal<T>(signal: AbortSignal, fn: () => T): T {
	return carried.run(signal, fn)
}

/** The signal the caller runs under, from withSignal. */
export function currentSignal(): AbortSignal | undefined {
	return carried.getStore()
}
