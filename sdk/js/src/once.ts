// A function run once a key, its answer kept: a request sent twice is answered
// as the first was rather than done twice. The core lets one run of a key go
// at a time, every client of the store included: it answers the kept answer,
// or hands the run to this client and keeps what it sends back. Opened with
// `fresh`, a kept answer older than it is answered at once, and the core may
// hand this client the build of the next one, which runs in the background.

import { currentSignal } from './cancel.ts'
import type { Link } from './connection.ts'
import { OutcomeUnknownError, ProtocolError } from './errors.ts'
import { checkName, handleOn } from './handles.ts'
import { callOn, jsonValues, keyText, underField, type Values } from './kv.ts'
import { ConnectionLostError, type Stream, watch } from './session.ts'
import { type Duration, type DurationText, ms, spelled } from './time.ts'
import type { Key } from './wire/codec.ts'
import { KvAnswer, KvBranch, KvCall, KvFound, KvOnceOpen, methods } from './wire/protocol.ts'

export interface OnceOptions {
	/** how long an answer is kept: a day when absent */
	keep?: Duration | undefined
	/**
	 * how long an answer is answered without making a new one, shorter than
	 * keep: past it, the kept answer is answered at once and one new one is
	 * made in the background
	 */
	fresh?: Duration | undefined
}

/** An answer with how old it is: stale once older than the `fresh` its answers opened with. */
export interface OnceAnswer<T> {
	value: T
	age: DurationText
	stale: boolean
}

/**
 * What a once's open sends: `aged` asks for each answer's age, which `answer`
 * reads and `run` does not, so that `run` without `fresh` opens as an older
 * client did and still reaches an older server.
 */
export interface OnceOpens {
	plain: Uint8Array
	aged: Uint8Array
}

/** Answers kept once a key: open them with `store.once(name)`. */
export class Once<T> {
	readonly name: string
	readonly #link: Link
	readonly #opens: OnceOpens
	readonly #values: Values<T>
	readonly #under: readonly string[]

	/** Answers come from `store.once` and their `under`. */
	constructor(link: Link, name: string, opens: OnceOpens, under: readonly string[]) {
		this.#link = link
		this.name = name
		this.#opens = opens
		this.#values = jsonValues as Values<T>
		this.#under = under
	}

	/** The answers of a branch, each key apart from the same key elsewhere. */
	under(...owners: Key[]): Once<T> {
		return new Once(this.#link, this.name, this.#opens, [...this.#under, ...owners.map(keyText)])
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
		return (await this.#ask(key, fn, this.#opens.plain)).value
	}

	/**
	 * run's answer with how old it is. Opened with `fresh`, a kept answer older
	 * than it comes back at once, stale, and fn may make the next one in the
	 * background: its answer replaces the kept one, and its throw, which no
	 * caller sees, keeps it until `keep` ends.
	 *
	 *     const { value, age, stale } = await books.answer(userId, () => cabinet.book(userId))
	 */
	async answer(key: Key, fn: () => T | Promise<T>): Promise<OnceAnswer<T>> {
		return this.#ask(key, fn, this.#opens.aged)
	}

	async #ask(key: Key, fn: () => T | Promise<T>, open: Uint8Array): Promise<OnceAnswer<T>> {
		const handed = await this.#claim(key, open)
		if (!(handed instanceof Run)) {
			return handed
		}
		if (handed.kept !== undefined) {
			background(handed, () => this.#made(fn))
			return handed.kept
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
		return { value: answer, age: '0s', stale: false }
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

	/**
	 * Forgets every answer of this branch and of the branches under it, in one
	 * commit. A run under way meanwhile keeps its answer once it ends.
	 */
	async clear(): Promise<void> {
		await this.#link.run('write', async connection => {
			const handle = await handleOn(connection, methods['kv.once.open'], this.#opens.plain)
			const branch = KvBranch.encode({ handle, under: underField(this.#under) })
			await connection.session.call(methods['kv.clear'], branch)
		})
	}

	// what fn answered as the run's last DATA, or not found when it threw
	async #made(fn: () => T | Promise<T>): Promise<Uint8Array> {
		try {
			return KvAnswer.encode({ found: true, value: this.#values.encode(await fn()) })
		} catch {
			return KvAnswer.encode({})
		}
	}

	// asks for key's run: the answer kept for it, or the run the core handed
	// over, which a connection lost before the answer asks for again
	async #claim(key: Key, open: Uint8Array): Promise<Run | OnceAnswer<T>> {
		return this.#link.run('read', async connection => {
			const handle = await handleOn(connection, methods['kv.once.open'], open)
			const call = KvCall.encode({ handle, under: underField(this.#under), key })
			const stream = await connection.session.open(methods['kv.once.run'], call, false)
			const unwatch = watch(currentSignal(), stream)
			try {
				const first = await stream.next()
				const offered = KvAnswer.decode(first.body)
				if (!first.end) {
					return new Run(stream, offered.refresh === true ? this.#kept(offered) : undefined)
				}
				if (offered.found !== true) {
					throw new ProtocolError(`kv once ${this.name}: a run ended without its answer`)
				}
				return this.#kept(offered)
			} catch (err) {
				stream.cancel()
				throw err
			} finally {
				unwatch()
			}
		})
	}

	#kept(offered: ReturnType<typeof KvAnswer.decode>): OnceAnswer<T> {
		return {
			value: this.#values.decode(offered.value ?? null),
			age: spelled(offered.age ?? 0),
			stale: offered.stale === true,
		}
	}

	#call(method: 'kv.once.get' | 'kv.once.delete', key: Key, idempotence: 'read' | 'write') {
		return callOn(
			this.#link,
			'kv.once.open',
			this.#opens.plain,
			this.#under,
			method,
			{ key },
			idempotence,
		)
	}
}

/**
 * Runs a build the core handed over in the background, its caller answered
 * already: what it makes, or its failure, goes back as the run's last DATA. A
 * connection lost or a store closed meanwhile ends it with nothing kept, and
 * nothing is thrown, since no caller waits for it.
 */
function background(run: Run, made: () => Promise<Uint8Array>): void {
	void made()
		.then(answer => run.end(answer))
		.catch(() => {})
}

/** A run the core handed to this client, until it sends what to keep. */
class Run {
	readonly #stream: Stream
	/** The stale answer the caller gets at once, for a build in the background. */
	readonly kept: OnceAnswer<never> | undefined

	constructor(stream: Stream, kept: OnceAnswer<unknown> | undefined) {
		this.#stream = stream
		this.kept = kept as OnceAnswer<never> | undefined
	}

	/** Sends the run's answer as the stream's last DATA, and waits for the core's, once it is kept. */
	async end(answer: Uint8Array): Promise<void> {
		try {
			await this.#stream.send(answer, true)
			await this.#stream.next()
		} catch (err) {
			if (err instanceof ConnectionLostError) {
				throw new OutcomeUnknownError(
					`the connection was lost before the answer was kept; the next run may run again: ${err.message}`,
				)
			}
			throw err
		}
	}
}

/**
 * What a once's opens send, checked before anything leaves. Only this client,
 * which reads `refresh`, sends `fresh`: the core hands a build in the
 * background only to a handle opened with it, so an older client never reads
 * one as a run of its own. A handle with `fresh` is always opened aged.
 */
export function onceOpen(name: string, options: OnceOptions = {}): OnceOpens {
	checkName(name, 'once')
	const keep = options.keep === undefined ? undefined : ms(options.keep)
	const fresh = options.fresh === undefined ? undefined : ms(options.fresh)
	const aged = KvOnceOpen.encode({ name, keep, fresh, aged: true })
	return { plain: fresh === undefined ? KvOnceOpen.encode({ name, keep }) : aged, aged }
}
