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
import { type Duration, type DurationText, ms, spelled } from './time.ts'
import { insideStoreTx } from './tx.ts'
import type { Key } from './wire/codec.ts'
import {
	LocksHeld,
	LocksHold,
	LocksLease,
	LocksRenew,
	LocksState,
	LocksStateAsked,
	methods,
} from './wire/protocol.ts'

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
	/** how many of a key's places background holds may take at once, 1 to limit: every place unless said */
	background?: number | undefined
	/** puts a key down for `pause` once `failures` holds of it failed in a row: every ask is then UnavailableError at once */
	breaker?: Breaker | undefined
}

/**
 * A semaphore's breaker. A hold failed when its function threw, unless its
 * permit said `healthy()`, or when it ran past its timeout or maxHold; a
 * success resets the count. Once the pause is over, one hold goes through:
 * its success puts the key up, its failure pauses it again. The store's
 * process keeps it in memory, so a restart of that process puts every key up.
 */
export interface Breaker {
	/** failed holds in a row, 1 or more */
	failures: number
	pause: Duration
}

/** How a semaphore's key stands now: `semaphore.state(key)`. */
export interface KeyState {
	/** false from the failure that put the key down until a hold after the pause succeeds; true without a breaker */
	up: boolean
	downSince?: Date | undefined
	/** the last failed hold's end, kept until a success after it */
	lastFailure?: Date | undefined
	waiting: number
	holding: number
}

export interface HoldOptions {
	/** how long the hold waits in line: 10 s unless said, then InUseError */
	wait?: Duration | undefined
}

/**
 * Which line a semaphore's hold waits in: urgent holds are given a place
 * before background ones, and a background hold waiting never keeps an urgent
 * one from a free place. A background hold is promised no place: it waits
 * while urgent holds keep coming, `wait` at most.
 */
export type Priority = 'urgent' | 'background'

export interface PermitOptions {
	/** urgent unless said */
	priority?: Priority | undefined
	/** how long the hold may keep its place, from its grant: past it the next in line holds it and the hold is LostError */
	timeout?: Duration | undefined
}

export interface SemaphoreHoldOptions extends HoldOptions, PermitOptions {}

/** What tryHold gives: the function's value, or that the key was held. */
export type Tried<T> = { acquired: true; value: T } | { acquired: false }

/** A holding of a lock's key, given to the function that holds it. */
export interface Lease {
	/** grows with every lease the store gives: hand it to whatever must refuse a late holder */
	readonly fence: bigint
	/** aborts when the lease is lost: run out, revoked at maxHold, or its store closed */
	readonly signal: AbortSignal
	/** how long the hold waited for the key, from its call to the grant, the round trip included */
	readonly waited: DurationText
}

/** A holding of a semaphore's key: one of its limit. */
export interface Permit {
	readonly signal: AbortSignal
	/** how long the hold waited for its place, from its call to the grant, the round trip included */
	readonly waited: DurationText
	/** says the guarded service is healthy, so that the breaker does not count the hold a failure though fn then throws: a 404 that is the caller's */
	healthy(): void
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
		// a semaphore's options given a lock, from code the types do not reach, are refused rather than dropped
		const given = options as SemaphoreHoldOptions | undefined
		if (given?.priority !== undefined || given?.timeout !== undefined) {
			throw new InvalidError(`lock ${this.name}: a priority and a timeout are a semaphore's`)
		}
		const asked = { wait: waitOf(options), priority: undefined, timeout: undefined }
		return held(await this.#keys.hold(key, fn, asked, leaseOf), this.#keys.place(key))
	}

	/** Holds key while fn runs if nobody holds it, and says it was held without waiting if somebody does. */
	tryHold<T>(key: Key, fn: (lease: Lease) => T | Promise<T>): Promise<Tried<T>> {
		return this.#keys.hold(key, fn, { wait: 0, priority: undefined, timeout: undefined }, leaseOf)
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
		const { limit, background } = options
		if (!Number.isInteger(limit) || limit < 1 || limit > 1000) {
			throw new InvalidError(`semaphore ${name}: a limit of ${limit}, not 1 to 1000`)
		}
		if (
			background !== undefined &&
			(!Number.isInteger(background) || background < 1 || background > limit)
		) {
			throw new InvalidError(
				`semaphore ${name}: a background of ${background}, not 1 to its limit ${limit}`,
			)
		}
		const breaker = breakerOf(name, options.breaker)
		// a background of the limit is every place, as none is
		const shaped = background === limit ? undefined : background
		keepShape(link, name, [limit, shaped, breaker?.failures, breaker?.pause])
		this.name = name
		this.limit = limit
		this.#keys = new Keys(link, 'semaphore', name, limit, { ...options, breaker })
	}

	/**
	 * Waits for one of key's places, holds it while fn runs and lets it go,
	 * as a lock's hold does; in the background, it waits behind every urgent
	 * hold and takes no more than the semaphore's `background` places:
	 *
	 *     await cabinet.hold('requests', permit => sync(permit.signal), { priority: 'background', timeout: '15s' })
	 */
	async hold<T>(
		key: Key,
		fn: (permit: Permit) => T | Promise<T>,
		options?: SemaphoreHoldOptions,
	): Promise<T> {
		const asked = { wait: waitOf(options), ...termsOf(this.name, options) }
		return held(await this.#keys.hold(key, fn, asked, permitOf), this.#keys.place(key))
	}

	tryHold<T>(
		key: Key,
		fn: (permit: Permit) => T | Promise<T>,
		options?: PermitOptions,
	): Promise<Tried<T>> {
		return this.#keys.hold(key, fn, { wait: 0, ...termsOf(this.name, options) }, permitOf)
	}

	/**
	 * How key stands now: whether what it guards is up, since when it is
	 * down, its last failed hold, and how many wait for it and hold it.
	 *
	 *     const { up, downSince } = await cabinet.state('requests')
	 */
	state(key: Key): Promise<KeyState> {
		return this.#keys.state(key)
	}
}

/** A breaker in milliseconds, checked. */
interface Breaking {
	failures: number
	pause: number
}

function breakerOf(name: string, breaker: Breaker | undefined): Breaking | undefined {
	if (breaker === undefined) {
		return undefined
	}
	const pause = ms(breaker.pause)
	if (!Number.isInteger(breaker.failures) || breaker.failures < 1 || pause === 0) {
		throw new InvalidError(
			`semaphore ${name}: a breaker of ${breaker.failures} failures and a pause of ${spelled(pause)}, not 1 or more and above zero`,
		)
	}
	return { failures: breaker.failures, pause }
}

/** What a semaphore's first open gives: its limit, background, and breaker's failures and pause. */
type Shape = readonly [number, number | undefined, number | undefined, number | undefined]

// The shape each semaphore's first open gave, by the link of the store that
// opened it: one name keeps one in a program, and two stores are two
// programs, each of whose holds the store admits by its own.
const opened = new WeakMap<Link, Map<string, Shape>>()

/** Keeps the shape a semaphore's first open on this store gives: another is refused, naming both. */
function keepShape(link: Link, name: string, given: Shape): void {
	let shapes = opened.get(link)
	if (shapes === undefined) {
		shapes = new Map()
		opened.set(link, shapes)
	}
	const first = shapes.get(name) ?? given
	const refuse = (what: string, before: string, now: string) =>
		new InvalidError(
			`semaphore ${name}: open in this program with a ${what} of ${before}, not ${now}`,
		)
	if (first[0] !== given[0]) {
		throw refuse('limit', String(first[0]), String(given[0]))
	}
	if (first[1] !== given[1]) {
		throw refuse('background', String(first[1] ?? 'none'), String(given[1] ?? 'none'))
	}
	if (first[2] !== given[2] || first[3] !== given[3]) {
		const shown = (shape: Shape) =>
			shape[2] === undefined ? 'none' : `${shape[2]} failures and ${spelled(shape[3] ?? 0)}`
		throw refuse('breaker', shown(first), shown(given))
	}
	shapes.set(name, first)
}

function waitOf(options: HoldOptions | undefined): number {
	return ms(options?.wait ?? defaultWait)
}

/** What a semaphore's hold asks beside its wait. */
function termsOf(name: string, options: PermitOptions | undefined): Omit<Asked, 'wait'> {
	const priority = options?.priority
	if (priority !== undefined && priority !== 'urgent' && priority !== 'background') {
		throw new InvalidError(
			`semaphore ${name}: a priority of ${JSON.stringify(priority)}, not urgent or background`,
		)
	}
	const timeout = options?.timeout === undefined ? undefined : ms(options.timeout)
	if (timeout === 0) {
		throw new InvalidError(`semaphore ${name}: a timeout of zero`)
	}
	return { priority, timeout }
}

/** How a hold asks for its key: how long it waits, in which line, and how long it may keep it. */
interface Asked {
	wait: number
	priority: Priority | undefined
	timeout: number | undefined
}

/** What hold gives: fn's value, a hold told not to wait finding the key held. */
function held<T>(tried: Tried<T>, place: string): T {
	if (!tried.acquired) {
		throw new InUseError(`${place}: held, and the hold would not wait`)
	}
	return tried.value
}

const leaseOf = (holding: Holding): Lease => ({
	fence: holding.fence,
	signal: holding.signal,
	waited: holding.waited,
})
const permitOf = (holding: Holding): Permit => ({
	signal: holding.signal,
	waited: holding.waited,
	healthy: () => holding.healthy(),
})

// The keys the caller holds or waits for, as its async context carries them,
// each with the link of the store it is of.
const holding = new AsyncLocalStorage<readonly (readonly [Link, string])[]>()

/** Runs fn as holding no key: work that outlives the hold its caller is inside, as a build in the background. */
export function withoutHolding<T>(fn: () => T): T {
	return holding.exit(fn)
}

/** What a lock and a semaphore share: asking for a key, and the lease once given. */
class Keys {
	readonly #link: Link
	readonly #kind: 'lock' | 'semaphore'
	readonly #name: string
	readonly #limit: number
	readonly #background: number | undefined
	readonly #breaker: Breaking | undefined
	readonly #ttl: number
	readonly #maxHold: number | undefined

	constructor(
		link: Link,
		kind: 'lock' | 'semaphore',
		name: string,
		limit: number,
		options:
			| (LockOptions & { background?: number | undefined; breaker?: Breaking | undefined })
			| undefined,
	) {
		this.#link = link
		this.#kind = kind
		this.#name = name
		this.#limit = limit
		this.#background = options?.background
		this.#breaker = options?.breaker
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
		terms: Asked,
		viewOf: (holding: Holding) => V,
	): Promise<Tried<T>> {
		const place = this.place(key)
		this.#refuse(place)
		const asked = performance.now()
		const answer = await this.#ask(keyText(key), terms)
		if (answer.acquired !== true) {
			return { acquired: false }
		}
		// rounded up, so that it is never less than the time the ask stood in line
		const waited = spelled(Math.ceil(performance.now() - asked))
		const lease = new Holding(this.#link, place, answer, this.#ttl, waited)
		try {
			const inside = [...(holding.getStore() ?? []), [this.#link, place] as const]
			const value = await holding.run(inside, () => fn(viewOf(lease)))
			lease.check()
			lease.answered(true)
			return { acquired: true, value }
		} finally {
			await lease.end()
		}
	}

	async state(key: Key): Promise<KeyState> {
		const body = LocksStateAsked.encode({ kind: this.#kind, name: this.#name, key: keyText(key) })
		const answer = await this.#link.run('read', connection =>
			connection.session.call(methods['locks.state'], body, currentSignal()),
		)
		const state = LocksState.decode(answer)
		const time = (at: number | undefined) => (at === undefined ? undefined : new Date(at))
		return {
			up: state.up === true,
			downSince: time(state.downSince),
			lastFailure: time(state.lastFailure),
			waiting: state.waiting ?? 0,
			holding: state.holding ?? 0,
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
	// A hold's timeout is a maxHold of its own: the shorter of the two revokes.
	async #ask(key: string, terms: Asked): Promise<ReturnType<typeof LocksHeld.decode>> {
		const { wait, priority, timeout } = terms
		const bounds = [this.#maxHold, timeout].filter(bound => bound !== undefined)
		const body = LocksHold.encode({
			kind: this.#kind,
			name: this.#name,
			key,
			limit: this.#limit,
			ttl: this.#ttl,
			maxHold: bounds.length === 0 ? undefined : Math.min(...bounds),
			wait,
			request: crypto.getRandomValues(new Uint8Array(16)),
			priority: priority === 'background' ? priority : undefined,
			background: this.#background,
			failures: this.#breaker?.failures,
			pause: this.#breaker?.pause,
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
	readonly waited: DurationText
	readonly #link: Link
	readonly #place: string
	readonly #lease: Uint8Array
	readonly #ttl: number
	readonly #lost = new AbortController()
	#succeeded = false
	readonly #renewing: ReturnType<typeof setInterval>
	#runningOut: ReturnType<typeof setTimeout> | undefined
	readonly #revoking: ReturnType<typeof setTimeout> | undefined

	constructor(
		link: Link,
		place: string,
		answer: ReturnType<typeof LocksHeld.decode>,
		ttl: number,
		waited: DurationText,
	) {
		this.waited = waited
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
				: unref(
						setTimeout(() => this.#lose('it was revoked at its maxHold or its timeout'), revokeIn),
					)
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

	/** Keeps whether fn answered, for the release to tell the key's breaker. */
	answered(succeeded: boolean): void {
		this.#succeeded ||= succeeded
	}

	healthy(): void {
		this.#succeeded = true
	}

	/**
	 * Stops renewing and lets the key go, saying whether the hold failed: fn
	 * threw and its permit never said healthy. A lease lost already lets
	 * nothing go, and the store counted it a failure itself if it ran past its
	 * time.
	 */
	async end(): Promise<void> {
		this.#stop()
		const body = LocksLease.encode({
			lease: this.#lease,
			failed: this.#succeeded ? undefined : true,
		})
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
