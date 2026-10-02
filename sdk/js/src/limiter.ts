// A limiter of requests by key, as Go's kv.Limiter keeps one: the generic
// cell rate algorithm, one time a key, which the server holds in memory and
// writes every second, so a crash lets at most one burst more through.

import type { Link } from './connection.ts'
import { InvalidError } from './errors.ts'
import { checkName, handleOn, ownerText } from './handles.ts'
import { ms, type Unit } from './time.ts'
import type { Key } from './wire/codec.ts'
import { KvAllowance, KvBucket, KvCall, methods } from './wire/messages.ts'

/**
 * How many pass a span: '100/s', '5/10s', '1000/h', '300/7d', a span of one
 * unit leaving out its 1. The type checks it as it is written.
 */
export type Rate = `${number}/${Unit}` | `${number}/${number}${Unit}`

export interface LimiterOptions {
	/** how many requests pass a span, each key on its own */
	rate: Rate
	/** how many may pass at once before the rate holds the next; the rate's count when absent */
	burst?: number
}

/** What a limiter answers a request. */
export interface Allowance {
	ok: boolean
	/** how many more requests would pass now */
	left: number
	/** milliseconds until the request would pass; 0 when ok */
	retryAfter: number
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

/** A limiter: open it with `store.kv.limiter(name, { rate })`. */
export class Limiter {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #owners: (string | Uint8Array)[]

	/** Limiters come from `store.kv.limiter` and their `of`. */
	constructor(link: Link, name: string, open: Uint8Array, owners: (string | Uint8Array)[]) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#owners = owners
	}

	/** The limiter of a branch, each key apart from the same key elsewhere. */
	of(...owners: Key[]): Limiter {
		return new Limiter(this.#link, this.name, this.#open, [
			...this.#owners,
			...owners.map(ownerText),
		])
	}

	/** Asks for n requests of key, one unless given: all pass or none does; past the burst is InvalidError. */
	async allow(key: Key, n = 1): Promise<Allowance> {
		const answer = await this.#link.run('write', async connection => {
			const handle = await handleOn(connection, methods['kv.open'], this.#open)
			const body = KvCall.encode({
				handle,
				owners: this.#owners.length > 0 ? this.#owners : undefined,
				key,
				n: n === 1 ? undefined : n,
			})
			return KvAllowance.decode(await connection.session.call(methods['kv.allow'], body))
		})
		return { ok: answer.ok === true, left: answer.left ?? 0, retryAfter: answer.retryAfter ?? 0 }
	}
}

/** A limiter's open: its name and rate, checked before anything leaves. */
export function limiterOpen(name: string, options: LimiterOptions): Uint8Array {
	checkName(name, 'limiter')
	const { count, per } = rateOf(options.rate)
	if (count < 1 || per < 1) {
		throw new InvalidError(`limiter ${name}: a rate of ${options.rate}`)
	}
	if (options.burst !== undefined && (!Number.isInteger(options.burst) || options.burst < 1)) {
		throw new InvalidError(`limiter ${name}: a burst of ${options.burst}`)
	}
	return KvBucket.encode({ name, rate: count, per, burst: options.burst })
}
