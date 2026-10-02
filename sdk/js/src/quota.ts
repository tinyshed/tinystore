// A quota of uses by key, as Go's kv.Quota keeps one: several windows, each
// starting at a key's first use after the last ended, checked and counted
// together in one durable write, or not counted at all.

import type { Link } from './connection.ts'
import { InvalidError } from './errors.ts'
import { checkName, handleOn, ownerText } from './handles.ts'
import { type Rate, rateOf } from './limiter.ts'
import { dateOf } from './time.ts'
import type { Key } from './wire/codec.ts'
import { KvAllowance, KvBucket, KvCall, methods } from './wire/messages.ts'

/** One window of a key: what it used of its limit, and when it resets. */
export interface WindowUsage {
	used: number
	limit: number
	left: number
	/** when the window ends and the next use starts another; undefined before it starts */
	resetAt: Date | undefined
}

/** What a quota answers a key: what a limiter answers, and each window by its name. */
export interface QuotaUsage<W extends string = string> {
	ok: boolean
	/** how many more uses would pass now */
	left: number
	/** milliseconds until a refused use would pass; 0 when ok */
	retryAfter: number
	windows: Record<W, WindowUsage>
}

/** A quota: open it with `store.kv.quota(name, { session: '100/5h', weekly: '300/7d' })`. */
export class Quota<W extends string = string> {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #owners: (string | Uint8Array)[]

	/** Quotas come from `store.kv.quota` and their `of`. */
	constructor(link: Link, name: string, open: Uint8Array, owners: (string | Uint8Array)[]) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#owners = owners
	}

	/** The quota of a branch, each key apart from the same key elsewhere. */
	of(...owners: Key[]): Quota<W> {
		return new Quota(this.#link, this.name, this.#open, [...this.#owners, ...owners.map(ownerText)])
	}

	/**
	 * Uses n of key's every window, one unless given, or none when one has no
	 * room for them; more than a window's limit is InvalidError.
	 *
	 *     const { ok, retryAfter, windows } = await ai.allow(user.id)
	 */
	async allow(key: Key, n = 1): Promise<QuotaUsage<W>> {
		return usageOf(await this.#call('kv.allow', key, n, 'write'))
	}

	/** Key's windows without using them: ok says whether one more use would pass now. */
	async get(key: Key): Promise<QuotaUsage<W>> {
		return usageOf(await this.#call('kv.usage', key, 1, 'read'))
	}

	/** Gives n uses back, one unless given, to each of key's windows that has not reset since. */
	async refund(key: Key, n = 1): Promise<void> {
		await this.#call('kv.refund', key, n, 'write')
	}

	/** Forgets key's windows, so that its next use starts each anew. */
	async delete(key: Key): Promise<void> {
		await this.#call('kv.delete', key, 1, 'write')
	}

	async #call(
		method: 'kv.allow' | 'kv.usage' | 'kv.refund' | 'kv.delete',
		key: Key,
		n: number,
		idempotence: 'read' | 'write',
	) {
		return this.#link.run(idempotence, async connection => {
			const handle = await handleOn(connection, methods['kv.open'], this.#open)
			const body = KvCall.encode({
				handle,
				owners: this.#owners.length > 0 ? this.#owners : undefined,
				key,
				n: n === 1 ? undefined : n,
			})
			return await connection.session.call(methods[method], body)
		})
	}
}

function usageOf<W extends string>(body: Uint8Array): QuotaUsage<W> {
	const answer = KvAllowance.decode(body)
	const windows = {} as Record<W, WindowUsage>
	for (const w of answer.windows ?? []) {
		windows[(w.name ?? '') as W] = {
			used: w.used ?? 0,
			limit: w.limit ?? 0,
			left: w.left ?? 0,
			resetAt: dateOf(w.resetAt),
		}
	}
	return {
		ok: answer.ok === true,
		left: answer.left ?? 0,
		retryAfter: answer.retryAfter ?? 0,
		windows,
	}
}

/** A quota's open: its name and windows, checked before anything leaves. */
export function quotaOpen(name: string, windows: Record<string, Rate>): Uint8Array {
	checkName(name, 'quota')
	const spans = Object.entries(windows)
	if (spans.length < 1 || spans.length > 8) {
		throw new InvalidError(`quota ${name}: ${spans.length} windows, not one to eight`)
	}
	return KvBucket.encode({
		name,
		windows: spans.map(([window, rate]) => {
			if (!/^[a-z][a-z0-9_]{0,31}$/.test(window)) {
				throw new InvalidError(
					`quota ${name}: a window name is [a-z][a-z0-9_]{0,31}, not ${window}`,
				)
			}
			const { count, per } = rateOf(rate)
			if (count < 1 || per < 1) {
				throw new InvalidError(`quota ${name}: a window of ${rate}`)
			}
			return { name: window, limit: count, per }
		}),
	})
}
