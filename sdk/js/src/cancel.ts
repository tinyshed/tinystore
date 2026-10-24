// The signal a call runs under, as a Go call carries its context: every call
// made inside withSignal, and inside what it awaits, ends when it aborts.

import { AsyncLocalStorage } from 'node:async_hooks'

import { CancelledError } from './errors.ts'

const carried = new AsyncLocalStorage<AbortSignal>()

/**
 * Runs fn under a signal: every call it makes, a kv get, a statement, a
 * scan, open and connect too, rejects once the signal aborts, its stream
 * cancelled on the wire, with CancelledError. A call given a signal of its
 * own, as work and put take one, keeps that one.
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

/**
 * Runs fn under no caller's signal: a dial that every caller of a store
 * shares stops for none of theirs, only for the store's close.
 */
export function withoutSignal<T>(fn: () => T): T {
	return carried.exit(fn)
}

/** Throws the CancelledError of a call whose signal has aborted, rather than the signal's own reason. */
export function checkSignal(signal: AbortSignal | undefined): void {
	if (signal?.aborted) {
		throw new CancelledError('the call was cancelled')
	}
}

/**
 * What a promise gives, unless the signal aborts first: the caller then
 * stops waiting with the error `cancelled` makes, and the promise goes on
 * for whoever else waits on it.
 */
export function unlessCancelled<T>(
	promise: Promise<T>,
	signal: AbortSignal | undefined,
	cancelled: () => Error = () => new CancelledError('the call was cancelled'),
): Promise<T> {
	if (signal === undefined) {
		return promise
	}
	if (signal.aborted) {
		promise.catch(() => {})
		return Promise.reject(cancelled())
	}
	return new Promise<T>((resolve, reject) => {
		const abort = () => reject(cancelled())
		signal.addEventListener('abort', abort, { once: true })
		promise.then(resolve, reject).finally(() => signal.removeEventListener('abort', abort))
	})
}
