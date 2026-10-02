// Answers a function gives once a key, as Go's kv.Once keeps them: a request
// sent again is answered as the first was rather than done twice. The server
// lets one call of a key run its function at a time, every client of the
// store included, and keeps what it returned.

import { currentSignal } from './cancel.ts'
import type { Link } from './connection.ts'
import { OutcomeUnknownError } from './errors.ts'
import { handleOn, ownerText } from './handles.ts'
import type { Values } from './kv.ts'
import { LostError, type Stream, watch } from './session.ts'
import type { Duration } from './time.ts'
import type { Key } from './wire/codec.ts'
import { KvCall, KvEntry, methods } from './wire/messages.ts'

export interface OnceOptions {
	/** how long an answer is kept: a day when absent */
	defaultTtl?: Duration
}

/** Answers kept once a key: open them with `store.kv.once(name)`. */
export class Once<V> {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #values: Values<V>
	readonly #owners: (string | Uint8Array)[]

	/** Answers come from `store.kv.once` and their `of`. */
	constructor(
		link: Link,
		name: string,
		open: Uint8Array,
		values: Values<V>,
		owners: (string | Uint8Array)[],
	) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#values = values
		this.#owners = owners
	}

	/** The answers of a branch, each key apart from the same key elsewhere. */
	of(...owners: Key[]): Once<V> {
		return new Once(this.#link, this.name, this.#open, this.#values, [
			...this.#owners,
			...owners.map(ownerText),
		])
	}

	/**
	 * The answer kept under key, or fn's, which is kept. A call of a key
	 * another call is running waits for it and gets its answer; a throw of fn
	 * keeps nothing and is thrown, so the next call runs fn again. A
	 * connection lost after fn returned and before its answer was kept is
	 * OutcomeUnknownError.
	 *
	 *     const receipt = await charges.run(requestId, () => pay.charge(order, requestId))
	 */
	async run(key: Key, fn: () => V | Promise<V>): Promise<V> {
		const { stream, kept } = await this.#claim(key)
		if (stream === undefined) {
			return await this.#values.decode(kept ?? null)
		}
		let answer: V
		let body: Uint8Array
		try {
			answer = await fn()
			body = KvEntry.encode({ found: true, value: await this.#values.encode(answer) })
		} catch (err) {
			await end(stream, KvEntry.encode({})).catch(() => {})
			throw err
		}
		await end(stream, body)
		return answer
	}

	/** The answer kept under key, undefined when none is. */
	async get(key: Key): Promise<V | undefined> {
		const e = await this.#call('kv.get', key, 'read')
		return e.found === true ? await this.#values.decode(e.value ?? null) : undefined
	}

	/** Forgets the answer kept under key, so that the next run runs again. */
	async delete(key: Key): Promise<void> {
		await this.#call('kv.delete', key, 'write')
	}

	// asks for key's run: the answer kept under it, or the stream the server
	// handed the run on, which a lost connection asks for again
	async #claim(
		key: Key,
	): Promise<{ stream?: Stream; kept?: ReturnType<typeof KvEntry.decode>['value'] }> {
		return this.#link.run('read', async connection => {
			const handle = await handleOn(connection, methods['kv.open'], this.#open)
			const ask = KvCall.encode({ handle, owners: this.#ownersField(), key })
			const stream = await connection.session.open(methods['kv.run'], ask, false)
			const unwatch = watch(currentSignal(), stream)
			try {
				const first = await stream.next()
				if (first.end) {
					return { kept: KvEntry.decode(first.body).value }
				}
				return { stream }
			} catch (err) {
				stream.cancel()
				throw err
			} finally {
				unwatch()
			}
		})
	}

	async #call(method: 'kv.get' | 'kv.delete', key: Key, idempotence: 'read' | 'write') {
		return this.#link.run(idempotence, async connection => {
			const handle = await handleOn(connection, methods['kv.open'], this.#open)
			const body = KvCall.encode({ handle, owners: this.#ownersField(), key })
			return KvEntry.decode(await connection.session.call(methods[method], body))
		})
	}

	#ownersField(): (string | Uint8Array)[] | undefined {
		return this.#owners.length > 0 ? this.#owners : undefined
	}
}

// sends what a run keeps as the stream's last DATA, and waits for the
// server's, which comes once it is kept
async function end(stream: Stream, body: Uint8Array): Promise<void> {
	try {
		await stream.send(body, true)
		await stream.next()
	} catch (err) {
		if (err instanceof LostError) {
			throw new OutcomeUnknownError(
				`the connection was lost before the answer was kept; the next run may run again: ${err.message}`,
			)
		}
		throw err
	}
}
