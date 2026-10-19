// Channels, as docs/guides/channels/README.md has them: messages a program publishes
// on a named channel, which every subscriber listening at that moment
// receives, in memory and at most once. A subscription's handler is called
// one message at a time; a slow one lets the store's log run ahead and is
// told what it missed, and a connection lost is subscribed again and told
// that messages may have been lost meanwhile.

import { AsyncLocalStorage } from 'node:async_hooks'

import type { Link } from './connection.ts'
import { ClosedError, InvalidError, UnavailableError } from './errors.ts'
import { checkName } from './handles.ts'
import { keyText } from './kv.ts'
import { sayOwn } from './logger.ts'
import { ConnectionLostError, Stream } from './session.ts'
import type { Key } from './wire/codec.ts'
import { ChannelsBatch, ChannelsPublish, ChannelsSubscribe, methods } from './wire/protocol.ts'

/** A message as a subscriber receives it: what was published, and the branch it was published under. */
export interface Message<T> {
	value: T
	branch: string[]
}

/**
 * Messages a subscriber may have lost: it fell behind its branch's log, and
 * `count` says how many; or its connection was lost and made again, and how
 * many is unknown.
 */
export interface Missed {
	reason: 'behind' | 'reconnect'
	count?: number | undefined
}

export interface SubscribeOptions {
	/** take every branch below this one too */
	recursive?: boolean | undefined
	/** told what was missed, in line with the messages; the store's log says it when absent */
	onMissed?: ((missed: Missed) => unknown) | undefined
	/** told what the handler threw, with the message; the store's log says it when absent */
	onError?: ((error: unknown, message: Message<unknown>) => unknown) | undefined
}

export interface EachOptions {
	recursive?: boolean | undefined
	onMissed?: ((missed: Missed) => unknown) | undefined
}

/** How long a subscription without onMissed or onError waits between two lines of the store's log. */
const quiet = 60_000

/** A channel of messages of one type, or one of its branches: open it with `store.channel(name)`. */
export class Channel<T> {
	readonly name: string
	readonly branch: readonly string[]
	readonly #link: Link

	/** A channel comes from `store.channel` and its `under`. */
	constructor(link: Link, name: string, branch: readonly string[] = []) {
		if (branch.length === 0) {
			checkName(name, 'channel')
		}
		this.#link = link
		this.name = name
		this.branch = branch
	}

	/** The branch of owners under this one: whose messages these are. */
	under(...owners: Key[]): Channel<T> {
		return new Channel(this.#link, this.name, [...this.branch, ...owners.map(keyText)])
	}

	/**
	 * Hands value to every subscriber of this branch, and of the branches above
	 * it listened to recursive, that listens now. It answers once the store
	 * has the message, not once anyone handled it.
	 */
	async publish(value: T): Promise<void> {
		const body = publishBody(this.name, this.branch, value)
		await this.#link.run('write', connection =>
			connection.session.call(methods['channels.publish'], body),
		)
	}

	/**
	 * Calls handler with each message published from now on, one at a time
	 * and in order: it answers once the store has the subscription, so that a
	 * message published after it arrives.
	 *
	 *     const sub = await chat.under(roomId).subscribe(m => show(m.value), { onMissed: () => reload() })
	 *     await sub.stop()
	 */
	async subscribe(
		handler: (message: Message<T>) => unknown,
		options?: SubscribeOptions,
	): Promise<Subscription> {
		const subscription = new Subscription(
			this.#link,
			this.#asked(options?.recursive),
			this.#shown(),
		)
		await subscription.start(handler as (message: Message<unknown>) => unknown, options)
		return subscription
	}

	/**
	 * The messages published from now on, as a loop: it listens from its first
	 * turn, and break ends it. A throw in the loop's body ends it too.
	 *
	 *     for await (const m of chat.under(roomId).each()) await save(m.value)
	 */
	async *each(options?: EachOptions): AsyncGenerator<Message<T>> {
		const told = new Told(this.#shown(), options?.onMissed, undefined)
		for (let first = true; ; first = false) {
			let stream: Stream
			try {
				stream = await subscribed(this.#link, this.#asked(options?.recursive))
			} catch (err) {
				if (!first && again(err)) {
					continue
				}
				throw err
			}
			if (!first) {
				await told.missed({ reason: 'reconnect' })
			}
			try {
				for (;;) {
					const event = await stream.next()
					if (event.end) {
						return
					}
					const batch = ChannelsBatch.decode(event.body)
					if ((batch.missed ?? 0) > 0) {
						await told.missed({ reason: 'behind', count: batch.missed })
					}
					for (const message of batch.messages ?? []) {
						yield messageOf<T>(message)
					}
					stream.consumed(event.body.length)
				}
			} catch (err) {
				if (!again(err)) {
					throw err
				}
			} finally {
				stream.cancel()
			}
		}
	}

	#asked(recursive: boolean | undefined): Uint8Array {
		return ChannelsSubscribe.encode({
			name: this.name,
			under: [...this.branch],
			recursive: recursive === true,
		})
	}

	#shown(): string {
		return shown(this.name, this.branch)
	}
}

/**
 * A subscription whose handler is called with each message: `stop` ends
 * it, and the store's close does.
 */
export class Subscription {
	readonly #link: Link
	readonly #asked: Uint8Array
	readonly #shown: string
	#stream: Stream | undefined
	#stopped = false
	#done: Promise<void> = Promise.resolve()

	/** A subscription comes from `channel.subscribe`. */
	constructor(link: Link, asked: Uint8Array, shown: string) {
		this.#link = link
		this.#asked = asked
		this.#shown = shown
	}

	/**
	 * No call starts after it, and it answers once the running call ended;
	 * called from inside the subscription's own handler, it answers at once.
	 */
	async stop(): Promise<void> {
		this.#stopped = true
		this.#stream?.cancel()
		if (inside.getStore() === this) {
			return
		}
		await this.#done
	}

	/** Registers the subscription, then runs its handler in the background. */
	async start(
		handler: (message: Message<unknown>) => unknown,
		options: SubscribeOptions | undefined,
	) {
		this.#stream = await subscribed(this.#link, this.#asked)
		const told = new Told(this.#shown, options?.onMissed, options?.onError)
		this.#done = this.#run(handler, told)
	}

	async #run(handler: (message: Message<unknown>) => unknown, told: Told): Promise<void> {
		for (;;) {
			const stream = this.#stream
			if (stream === undefined || this.#stopped) {
				return
			}
			try {
				await this.#deliver(stream, handler, told)
				return
			} catch (err) {
				if (this.#stopped || !again(err)) {
					return told.ended(err, this.#stopped)
				}
			}
			const made = await subscribed(this.#link, this.#asked).catch((err: unknown) => err)
			if (!(made instanceof Stream)) {
				return told.ended(made, this.#stopped)
			}
			this.#stream = made
			await told.missed({ reason: 'reconnect' })
		}
	}

	/** Reads the stream's batches and calls the handler, one message at a time. */
	async #deliver(stream: Stream, handler: (message: Message<unknown>) => unknown, told: Told) {
		for (;;) {
			const event = await stream.next()
			if (event.end) {
				return
			}
			const batch = ChannelsBatch.decode(event.body)
			if ((batch.missed ?? 0) > 0 && !this.#stopped) {
				await told.missed({ reason: 'behind', count: batch.missed })
			}
			for (const each of batch.messages ?? []) {
				if (this.#stopped) {
					return
				}
				const message = messageOf<unknown>(each)
				try {
					await inside.run(this, () => handler(message))
				} catch (err) {
					await told.failed(err, message)
				}
			}
			stream.consumed(event.body.length)
		}
	}
}

// The subscription whose handler the caller runs inside, so that its stop
// answers at once there rather than wait for itself.
const inside = new AsyncLocalStorage<Subscription>()

/** Opens a subscription's stream and waits for the RESPONSE that says the store has it. */
function subscribed(link: Link, asked: Uint8Array): Promise<Stream> {
	return link.run('read', async connection => {
		const stream = await connection.session.open(methods['channels.subscribe'], asked, true)
		try {
			await stream.next()
			return stream
		} catch (err) {
			stream.cancel()
			throw err
		}
	})
}

/** Whether a subscription ended by its connection subscribes again on another. */
function again(err: unknown): boolean {
	return err instanceof ConnectionLostError || err instanceof UnavailableError
}

function messageOf<T>(message: {
	under?: string[] | undefined
	value?: string | undefined
}): Message<T> {
	return { value: JSON.parse(message.value ?? 'null') as T, branch: message.under ?? [] }
}

/** What tells a subscription's missed messages and its handler's throws: its own, or the store's log once a minute. */
class Told {
	readonly #shown: string
	readonly #onMissed: SubscribeOptions['onMissed']
	readonly #onError: SubscribeOptions['onError']
	#said = Number.NEGATIVE_INFINITY

	constructor(
		shown: string,
		onMissed: SubscribeOptions['onMissed'],
		onError: SubscribeOptions['onError'],
	) {
		this.#shown = shown
		this.#onMissed = onMissed
		this.#onError = onError
	}

	async missed(missed: Missed): Promise<void> {
		if (this.#onMissed !== undefined) {
			await this.#onMissed(missed)
		} else if (this.#speaks()) {
			sayOwn('warn', 'a subscriber may have missed messages', { channel: this.#shown, ...missed })
		}
	}

	async failed(error: unknown, message: Message<unknown>): Promise<void> {
		if (this.#onError !== undefined) {
			await this.#onError(error, message)
		} else if (this.#speaks()) {
			sayOwn('warn', "a subscriber's handler threw", { channel: this.#shown, error })
		}
	}

	/** Says why the subscription ended, unless it was stopped or its store closed. */
	ended(err: unknown, stopped: boolean): void {
		if (!stopped && !(err instanceof ClosedError && !(err instanceof ConnectionLostError))) {
			sayOwn('warn', 'a subscription ended', { channel: this.#shown, error: err })
		}
	}

	#speaks(): boolean {
		const now = Date.now()
		if (now - this.#said < quiet) {
			return false
		}
		this.#said = now
		return true
	}
}

/** A channel's branch as an error names it: `channel chat: under "room-7"`. */
function shown(name: string, branch: readonly string[]): string {
	if (branch.length === 0) {
		return `channel ${name}`
	}
	return `channel ${name}: under ${branch.map(owner => JSON.stringify(owner)).join(' ')}`
}

/** A publish's body: its channel, its branch and its value as JSON. */
export function publishBody(name: string, branch: readonly string[], value: unknown): Uint8Array {
	const json = JSON.stringify(value)
	if (json === undefined) {
		throw new InvalidError(`${shown(name, branch)}: a message that does not write as JSON`)
	}
	return ChannelsPublish.encode({ name, under: [...branch], value: json })
}

/** A channel inside a transaction: its messages are handed over once the transaction commits. */
export interface TxChannel<T> {
	under(...owners: Key[]): TxChannel<T>
	publish(value: T): Promise<void>
}

/** A channel's name and branch, when handle is a channel. */
export function channelParts(
	handle: unknown,
): { name: string; branch: readonly string[] } | undefined {
	return handle instanceof Channel ? { name: handle.name, branch: handle.branch } : undefined
}

/** A channel inside a transaction, its publishes sent by send. */
export function txChannel<T>(
	name: string,
	branch: readonly string[],
	send: (body: Uint8Array) => Promise<void>,
): TxChannel<T> {
	return {
		under: (...owners) => txChannel(name, [...branch, ...owners.map(keyText)], send),
		publish: value => send(publishBody(name, branch, value)),
	}
}
