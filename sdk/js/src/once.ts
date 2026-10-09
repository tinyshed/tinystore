// A function run once a key, its answer kept: a request sent twice is answered
// as the first was rather than done twice. The core lets one run of a key go
// at a time, every client of the store included: it answers the kept answer,
// or hands the run to this client and keeps what it sends back.

import { currentSignal } from './cancel.ts'
import type { Link } from './connection.ts'
import { OutcomeUnknownError, ProtocolError } from './errors.ts'
import { checkName, handleOn } from './handles.ts'
import { callOn, jsonValues, keyText, underField, type Values } from './kv.ts'
import { LostError, type Stream, watch } from './session.ts'
import { type Duration, ms } from './time.ts'
import type { Key } from './wire/codec.ts'
import { KvAnswer, KvCall, KvFound, KvOnceOpen, methods } from './wire/protocol.ts'

export interface OnceOptions {
	/** how long an answer is kept: a day when absent */
	keep?: Duration | undefined
}

/** Answers kept once a key: open them with `store.once(name)`. */
export class Once<T> {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #values: Values<T>
	readonly #under: readonly string[]

	/** Answers come from `store.once` and their `under`. */
	constructor(link: Link, name: string, open: Uint8Array, under: readonly string[]) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#values = jsonValues as Values<T>
		this.#under = under
	}

	/** The answers of a branch, each key apart from the same key elsewhere. */
	under(...owners: Key[]): Once<T> {
		return new Once(this.#link, this.name, this.#open, [...this.#under, ...owners.map(keyText)])
	}

	/**
	 * The answer kept for key, or fn's, which is kept. A run of a key that
	 * another runs, here or in another process, waits for it and gets its
	 * answer; a throw of fn keeps nothing and is thrown, so the next run runs
	 * fn again. A connection lost after fn returned and before its answer was
	 * kept is OutcomeUnknownError.
	 *
	 *     const receipt = await charges.run(requestId, () => pay.charge(order, requestId))
	 */
	async run(key: Key, fn: () => T | Promise<T>): Promise<T> {
		const handed = await this.#claim(key)
		if (!(handed instanceof Run)) {
			return handed.value
		}
		let answer: T
		let kept: Uint8Array
		try {
			answer = await fn()
			kept = KvAnswer.encode({ found: true, value: this.#values.encode(answer) })
		} catch (err) {
			await handed.end(KvAnswer.encode({})).catch(() => {})
			throw err
		}
		await handed.end(kept)
		return answer
	}

	/** The answer kept for key, undefined when none is. */
	async get(key: Key): Promise<T | undefined> {
		const body = await this.#call('kv.once.get', key, 'read')
		const kept = KvAnswer.decode(body)
		return kept.found === true ? this.#values.decode(kept.value ?? null) : undefined
	}

	/** Forgets the answer kept for key, so that its next run runs fn again. */
	async delete(key: Key): Promise<boolean> {
		return KvFound.decode(await this.#call('kv.once.delete', key, 'write')).found === true
	}

	// asks for key's run: the answer kept for it, or the run the core handed
	// over, which a connection lost before the answer asks for again
	async #claim(key: Key): Promise<Run | { value: T }> {
		return this.#link.run('read', async connection => {
			const handle = await handleOn(connection, methods['kv.once.open'], this.#open)
			const call = KvCall.encode({ handle, under: underField(this.#under), key })
			const stream = await connection.session.open(methods['kv.once.run'], call, false)
			const unwatch = watch(currentSignal(), stream)
			try {
				const first = await stream.next()
				const offered = KvAnswer.decode(first.body)
				if (!first.end) {
					return new Run(stream)
				}
				if (offered.found !== true) {
					throw new ProtocolError(`kv once ${this.name}: a run ended without its answer`)
				}
				return { value: this.#values.decode(offered.value ?? null) }
			} catch (err) {
				stream.cancel()
				throw err
			} finally {
				unwatch()
			}
		})
	}

	#call(method: 'kv.once.get' | 'kv.once.delete', key: Key, idempotence: 'read' | 'write') {
		return callOn(this.#link, 'kv.once.open', this.#open, this.#under, method, { key }, idempotence)
	}
}

/** A run the core handed to this client, until it sends what to keep. */
class Run {
	readonly #stream: Stream

	constructor(stream: Stream) {
		this.#stream = stream
	}

	/** Sends the run's answer as the stream's last DATA, and waits for the core's, once it is kept. */
	async end(answer: Uint8Array): Promise<void> {
		try {
			await this.#stream.send(answer, true)
			await this.#stream.next()
		} catch (err) {
			if (err instanceof LostError) {
				throw new OutcomeUnknownError(
					`the connection was lost before the answer was kept; the next run may run again: ${err.message}`,
				)
			}
			throw err
		}
	}
}

/** What a once's open sends, checked before anything leaves. */
export function onceOpen(name: string, options: OnceOptions = {}): Uint8Array {
	checkName(name, 'once')
	return KvOnceOpen.encode({
		name,
		keep: options.keep === undefined ? undefined : ms(options.keep),
	})
}
