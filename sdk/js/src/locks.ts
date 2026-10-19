// Locks and semaphores, as docs/guides/locks/README.md has them: a key that one holder
// at a time, or `limit`, may hold while a function runs, across every
// program of a store. The store keeps them in its process's memory; a holder
// here renews its lease at a third of its ttl from this program, so that a
// program that froze stops renewing and loses the key.

import { AsyncLocalStorage } from 'node:async_hooks'

import { currentSignal } from './cancel.ts'
import type { Link } from './connection.ts'
import { ClosedError, InUseError, InvalidError, LostError } from './errors.ts'
import { checkName } from './handles.ts'
import { keyText } from './kv.ts'
import { ConnectionLostError } from './session.ts'
import { insideDatabaseTx } from './sql.ts'
import { type Duration, ms } from './time.ts'
import { insideStoreTx } from './tx.ts'
import type { Key } from './wire/codec.ts'
import { LocksHeld, LocksHold, LocksLease, LocksRenew, methods } from './wire/protocol.ts'

/** How long a lease lasts between renewals, and a hold waits, unless said. */
const defaultTtl = 10_000
const defaultWait = 10_000

export interface LockOptions {
	/** how long a lease lasts from its last renewal: 10 s unless said, 1 s to 1 hour */
	ttl?: Duration | undefined
	/** how long a lease lasts from its grant, however it is renewed: past it the next in line holds the key */
	maxHold?: Duration | undefined
}

export interface SemaphoreOptions extends LockOptions {
	/** how many may hold a key at once, 1 to 1000 */
	limit: number
}

export interface HoldOptions {
	/** how long the hold waits in line: 10 s unless said, then InUseError */
	wait?: Duration | undefined
}

/** What tryHold gives: the function's value, or that the key was held. */
export type Tried<T> = { acquired: true; value: T } | { acquired: false }

/** A holding of a lock's key, given to the function that holds it. */
export interface Lease {
	/** grows with every lease the store gives: hand it to whatever must refuse a late holder */
	readonly fence: bigint
	/** aborts when the lease is lost: run out, revoked at maxHold, or its store closed */
	readonly signal: AbortSignal
}

/** A holding of a semaphore's key: one of its limit. */
export interface Permit {
	readonly signal: AbortSignal
}

/** Keys held one holder at a time: open them with `store.lock(name)`. */
export class Lock {
	readonly name: string
	readonly #keys: Keys

	/** A lock comes from `store.lock`. */
	constructor(link: Link, name: string, options: LockOptions | undefined) {
		checkName(name, 'lock')
		this.name = name
		this.#keys = new Keys(link, 'lock', name, 1, options)
	}

	/**
	 * Waits for key, holds it while fn runs and lets it go, giving what fn
	 * gives. A key still held after the wait is InUseError; a lease lost
	 * while fn ran is LostError, though fn succeeded.
	 *
	 *     await invoices.hold(invoice.id, lease => charge(invoice, { fence: lease.fence, signal: lease.signal }))
	 */
	async hold<T>(key: Key, fn: (lease: Lease) => T | Promise<T>, options?: HoldOptions): Promise<T> {
		return held(await this.#keys.hold(key, fn, waitOf(options), leaseOf), this.#keys.place(key))
	}

	/** Holds key while fn runs if nobody holds it, and says it was held without waiting if somebody does. */
	tryHold<T>(key: Key, fn: (lease: Lease) => T | Promise<T>): Promise<Tried<T>> {
		return this.#keys.hold(key, fn, 0, leaseOf)
	}
}

/** Keys held by up to their limit at a time: open them with `store.semaphore(name, { limit })`. */
export class Semaphore {
	readonly name: string
	readonly limit: number
	readonly #keys: Keys

	/** A semaphore comes from `store.semaphore`. */
	constructor(link: Link, name: string, options: SemaphoreOptions) {
		checkName(name, 'semaphore')
		if (!Number.isInteger(options.limit) || options.limit < 1 || options.limit > 1000) {
			throw new InvalidError(`semaphore ${name}: a limit of ${options.limit}, not 1 to 1000`)
		}
		this.name = name
		this.limit = options.limit
		this.#keys = new Keys(link, 'semaphore', name, options.limit, options)
	}

	async hold<T>(
		key: Key,
		fn: (permit: Permit) => T | Promise<T>,
		options?: HoldOptions,
	): Promise<T> {
		return held(await this.#keys.hold(key, fn, waitOf(options), permitOf), this.#keys.place(key))
	}

	tryHold<T>(key: Key, fn: (permit: Permit) => T | Promise<T>): Promise<Tried<T>> {
		return this.#keys.hold(key, fn, 0, permitOf)
	}
}

function waitOf(options: HoldOptions | undefined): number {
	return ms(options?.wait ?? defaultWait)
}

/** What hold gives: fn's value, a hold told not to wait finding the key held. */
function held<T>(tried: Tried<T>, place: string): T {
	if (!tried.acquired) {
		throw new InUseError(`${place}: held, and the hold would not wait`)
	}
	return tried.value
}

const leaseOf = (holding: Holding): Lease => ({ fence: holding.fence, signal: holding.signal })
const permitOf = (holding: Holding): Permit => ({ signal: holding.signal })

// The keys the caller holds or waits for, as its async context carries them,
// each with the link of the store it is of.
const holding = new AsyncLocalStorage<readonly (readonly [Link, string])[]>()

/** What a lock and a semaphore share: asking for a key, and the lease once given. */
class Keys {
	readonly #link: Link
	readonly #kind: 'lock' | 'semaphore'
	readonly #name: string
	readonly #limit: number
	readonly #ttl: number
	readonly #maxHold: number | undefined

	constructor(
		link: Link,
		kind: 'lock' | 'semaphore',
		name: string,
		limit: number,
		options: LockOptions | undefined,
	) {
		this.#link = link
		this.#kind = kind
		this.#name = name
		this.#limit = limit
		this.#ttl = ms(options?.ttl ?? defaultTtl)
		this.#maxHold = options?.maxHold === undefined ? undefined : ms(options.maxHold)
	}

	/** The key as an error names it: `lock invoices: key "42"`. */
	place(key: Key): string {
		return `${this.#kind} ${this.#name}: key ${JSON.stringify(keyText(key))}`
	}

	async hold<T, V>(
		key: Key,
		fn: (view: V) => T | Promise<T>,
		wait: number,
		viewOf: (holding: Holding) => V,
	): Promise<Tried<T>> {
		const place = this.place(key)
		this.#refuse(place)
		const answer = await this.#ask(keyText(key), wait)
		if (answer.acquired !== true) {
			return { acquired: false }
		}
		const lease = new Holding(this.#link, place, answer, this.#ttl)
		try {
			const inside = [...(holding.getStore() ?? []), [this.#link, place] as const]
			const value = await holding.run(inside, () => fn(viewOf(lease)))
			lease.check()
			return { acquired: true, value }
		} finally {
			await lease.end()
		}
	}

	/**
	 * Refuses a hold that would wait for what its caller holds: a database's
	 * writer, which a transaction keeps while the key is waited for, or the
	 * same key.
	 */
	#refuse(place: string): void {
		if (insideStoreTx() || insideDatabaseTx()) {
			throw new InvalidError(
				`${place}: a hold inside a transaction would keep its writer while the key is waited for: hold first`,
			)
		}
		if (holding.getStore()?.some(([link, held]) => link === this.#link && held === place)) {
			throw new InvalidError(`${place}: a hold of a key inside its own hold would wait for itself`)
		}
	}

	// A connection lost on the way asks again with the same request, which the
	// store answers with the lease it gave, or with the ask's place in line.
	async #ask(key: string, wait: number): Promise<ReturnType<typeof LocksHeld.decode>> {
		const body = LocksHold.encode({
			kind: this.#kind,
			name: this.#name,
			key,
			limit: this.#limit,
			ttl: this.#ttl,
			maxHold: this.#maxHold,
			wait,
			request: crypto.getRandomValues(new Uint8Array(16)),
		})
		const answer = await this.#link.run('read', connection =>
			connection.session.call(methods['locks.hold'], body, currentSignal()),
		)
		return LocksHeld.decode(answer)
	}
}

/**
 * A lease this program holds: renewed at a third of its ttl, and counted lost
 * at its own deadline, the time it sent the last renewal that succeeded plus
 * its ttl, less a margin, or at maxHold.
 */
class Holding {
	readonly fence: bigint
	readonly #link: Link
	readonly #place: string
	readonly #lease: Uint8Array
	readonly #ttl: number
	readonly #lost = new AbortController()
	readonly #renewing: ReturnType<typeof setInterval>
	#runningOut: ReturnType<typeof setTimeout> | undefined
	readonly #revoking: ReturnType<typeof setTimeout> | undefined

	constructor(link: Link, place: string, answer: ReturnType<typeof LocksHeld.decode>, ttl: number) {
		this.#link = link
		this.#place = place
		this.#lease = answer.lease ?? new Uint8Array(0)
		this.#ttl = ttl
		this.fence = answer.fence === undefined || answer.fence === '' ? 0n : BigInt(answer.fence)
		this.#renewing = unref(setInterval(() => void this.#renew(), Math.max(1, Math.floor(ttl / 3))))
		this.#runOutAt(performance.now() + ttl)
		const revokeIn = answer.revokeIn
		this.#revoking =
			revokeIn === undefined
				? undefined
				: unref(setTimeout(() => this.#lose('it was revoked at its maxHold'), revokeIn))
	}

	get signal(): AbortSignal {
		return this.#lost.signal
	}

	/** Throws why the lease was lost, if it was. */
	check(): void {
		if (this.#lost.signal.aborted) {
			throw this.#lost.signal.reason
		}
	}

	/** Stops renewing and lets the key go; a lease lost already lets nothing go. */
	async end(): Promise<void> {
		this.#stop()
		const body = LocksLease.encode({ lease: this.#lease })
		await this.#link
			.run('read', connection => connection.session.call(methods['locks.release'], body))
			.catch(() => {})
	}

	async #renew(): Promise<void> {
		const sent = performance.now()
		const body = LocksRenew.encode({ lease: this.#lease, ttl: this.#ttl })
		try {
			await this.#link.run('read', connection =>
				connection.session.call(methods['locks.renew'], body),
			)
			this.#runOutAt(sent + this.#ttl)
		} catch (err) {
			if (err instanceof LostError) {
				this.#lose('it ran out before it was renewed')
			} else if (err instanceof ClosedError && !(err instanceof ConnectionLostError)) {
				this.#lose('its store closed', err)
			}
			// any other failure keeps renewing until the lease's own deadline
		}
	}

	#runOutAt(deadline: number): void {
		if (this.#runningOut !== undefined) {
			clearTimeout(this.#runningOut)
		}
		const margin = Math.min(1000, this.#ttl / 10)
		const left = Math.max(0, deadline - margin - performance.now())
		this.#runningOut = unref(setTimeout(() => this.#lose('it was not renewed in time'), left))
	}

	#lose(why: string, cause?: Error): void {
		this.#stop()
		if (!this.#lost.signal.aborted) {
			this.#lost.abort(cause ?? new LostError(`${this.#place}: the lease was lost: ${why}`))
		}
	}

	#stop(): void {
		clearInterval(this.#renewing)
		clearTimeout(this.#runningOut)
		clearTimeout(this.#revoking)
	}
}

/** A timer that keeps no program running by itself. */
function unref<T extends ReturnType<typeof setTimeout>>(timer: T): T {
	;(timer as { unref?: () => void }).unref?.()
	return timer
}
