// May this request pass: a rate limit lets a smooth rate of requests a key
// through, the generic cell rate algorithm with its times in the core's memory;
// a quota counts uses in windows that reset, all of them or none in one
// durable write. Both answer rather than throw, with one Allowance.

import type { Link } from './connection.ts'
import { InvalidError } from './errors.ts'
import { checkName } from './handles.ts'
import { callOn, keyText } from './kv.ts'
import { dateOf, ms, type Unit } from './time.ts'
import type { Key } from './wire/codec.ts'
import { KvAllowance, KvQuotaOpen, KvRateLimitOpen, type Method } from './wire/protocol.ts'

/**
 * How many pass a span: '100/s', '5/10s', '1000/h', '300/7d', a span of one
 * unit leaving out its 1. The type checks it as it is written.
 */
export type Rate = `${number}/${Unit}` | `${number}/${number}${Unit}`

export interface RateLimitOptions {
	/** how many requests a key passes a span */
	rate: Rate
	/** how many may pass at once before the rate holds the next; the rate's count when absent */
	burst?: number | undefined
}

/** One window of a quota as a key stands in it. */
export interface WindowUse {
	used: number
	limit: number
	left: number
	/** when the window resets; undefined for one not started */
	resetsAt: Date | undefined
}

/** What a rate limit or a quota answers a request. */
export interface Allowance<W extends string = never> {
	ok: boolean
	/** how many more would pass now */
	left: number
	/** when a refused request may try again; undefined when it passed */
	retryAt: Date | undefined
	/** a quota's windows by name; none for a rate limit */
	windows: Record<W, WindowUse>
}

/**
 * A rate's count and span in milliseconds: a span of one unit may leave out
 * its 1.
 *
 *     '100/s' → 100 a 1000 ms, '5/10s' → 5 a 10000 ms
 */
export function rateOf(rate: string): { count: number; per: number } {
	const parts = /^(\d+)\/(\d*)([a-z]+)$/.exec(rate)
	if (parts === null) {
		throw new InvalidError(`the rate ${JSON.stringify(rate)}; write it as 100/s, 5/10s or 1000/h`)
	}
	const [, count, span, unit] = parts
	return { count: Number(count), per: ms(`${span === '' ? '1' : span}${unit}`) }
}

/** A rate of requests a key: open it with `store.rateLimit(name, { rate })`. */
export class RateLimit {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #under: readonly string[]

	/** Rate limits come from `store.rateLimit` and their `under`. */
	constructor(link: Link, name: string, open: Uint8Array, under: readonly string[]) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#under = under
	}

	/** The rate limit of a branch, each key apart from the same key elsewhere. */
	under(...owners: Key[]): RateLimit {
		return new RateLimit(this.#link, this.name, this.#open, [
			...this.#under,
			...owners.map(keyText),
		])
	}

	/** Asks for n requests of key, one unless given: all pass or none; more than the burst is InvalidError. */
	async allow(key: Key, n = 1): Promise<Allowance> {
		return allowanceOf(await this.#call('kv.allow', key, n, 'write'))
	}

	/** The answer one request would get, using nothing. */
	async peek(key: Key): Promise<Allowance> {
		return allowanceOf(await this.#call('kv.peek', key, undefined, 'read'))
	}

	/** Forgets key, so that its next request starts afresh. */
	async reset(key: Key): Promise<void> {
		await this.#call('kv.reset', key, undefined, 'write')
	}

	#call(method: Method, key: Key, n: number | undefined, idempotence: 'read' | 'write') {
		const fields = { key, n: n === 1 ? undefined : n }
		return callOn(
			this.#link,
			'kv.rateLimit.open',
			this.#open,
			this.#under,
			method,
			fields,
			idempotence,
		)
	}
}

/** Uses by key in named windows: open it with `store.quota(name, { daily: '100/1d' })`. */
export class Quota<W extends string = string> {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #under: readonly string[]

	/** Quotas come from `store.quota` and their `under`. */
	constructor(link: Link, name: string, open: Uint8Array, under: readonly string[]) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#under = under
	}

	under(...owners: Key[]): Quota<W> {
		return new Quota(this.#link, this.name, this.#open, [...this.#under, ...owners.map(keyText)])
	}

	/**
	 * Uses n of key's every window, one unless given, or none when one has no
	 * room; more than a window's limit is InvalidError.
	 *
	 *     const { ok, retryAt, windows } = await ai.allow(user.id)
	 */
	async allow(key: Key, n = 1): Promise<Allowance<W>> {
		return allowanceOf(await this.#call('kv.allow', key, n, 'write'))
	}

	async peek(key: Key): Promise<Allowance<W>> {
		return allowanceOf(await this.#call('kv.peek', key, undefined, 'read'))
	}

	async reset(key: Key): Promise<void> {
		await this.#call('kv.reset', key, undefined, 'write')
	}

	/** Gives n uses back to every window of key, one unless given. */
	async refund(key: Key, n = 1): Promise<void> {
		await this.#call('kv.refund', key, n, 'write')
	}

	#call(method: Method, key: Key, n: number | undefined, idempotence: 'read' | 'write') {
		const fields = { key, n: n === 1 ? undefined : n }
		return callOn(this.#link, 'kv.quota.open', this.#open, this.#under, method, fields, idempotence)
	}
}

/** A rate limit's open: its name and rate, checked before anything leaves. */
export function rateLimitOpen(name: string, options: RateLimitOptions): Uint8Array {
	checkName(name, 'rate limit')
	const { count, per } = rateOf(options.rate)
	if (count < 1 || per < 1) {
		throw new InvalidError(`kv rate limit ${name}: a rate of ${options.rate}`)
	}
	if (options.burst !== undefined && (!Number.isInteger(options.burst) || options.burst < 1)) {
		throw new InvalidError(`kv rate limit ${name}: a burst of ${options.burst}`)
	}
	return KvRateLimitOpen.encode({ name, rate: count, per, burst: options.burst })
}

/** A quota's open: its windows by name, in the order they were given. */
export function quotaOpen(name: string, windows: Record<string, Rate>): Uint8Array {
	checkName(name, 'quota')
	const named = Object.entries(windows).map(([window, rate]) => {
		const { count, per } = rateOf(rate)
		return { name: window, limit: count, per }
	})
	return KvQuotaOpen.encode({ name, windows: named })
}

function allowanceOf<W extends string>(body: Uint8Array): Allowance<W> {
	const answer = KvAllowance.decode(body)
	const windows = {} as Record<W, WindowUse>
	for (const window of answer.windows ?? []) {
		windows[(window.name ?? '') as W] = {
			used: window.used ?? 0,
			limit: window.limit ?? 0,
			left: window.left ?? 0,
			resetsAt: dateOf(window.resetsAt),
		}
	}
	return {
		ok: answer.ok === true,
		left: answer.left ?? 0,
		retryAt: dateOf(answer.retryAt),
		windows,
	}
}
