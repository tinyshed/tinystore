// A connection's protocol, without its bytes' way there and back: frames in,
// frames out, streams, credit both ways, PING, GOAWAY and cancelling, as
// docs/wire.md says them. The transport hands it what it read and takes what
// it writes; nothing here waits on a socket, so the same code serves a Unix
// socket, a Windows pipe, a child's stdio and TLS.

import { createHmac, timingSafeEqual } from 'node:crypto'

import { currentSignal } from './cancel.ts'
import {
	CancelledError,
	ClosedError,
	errorOf,
	LimitError,
	ProtocolError,
	type TinystoreError,
	UnavailableError,
} from './errors.ts'
import type { Read } from './wire/codec.ts'
import {
	checkHeader,
	credit,
	Flag,
	frame,
	granted,
	headerSize,
	Kind,
	kindName,
	parseHeader,
	putHeader,
} from './wire/frame.ts'
import { Reader, Writer } from './wire/msgpack.ts'
import { Failure, GoAway, Hello, Welcome } from './wire/protocol.ts'

export const protocol = 2

/** What this client lets the server send on a stream before it grants more: a body at least. */
export const downloadWindow = 2 << 20

/** The largest body a WELCOME may come in, before a body is agreed. */
const welcomeMost = 1 << 20

/** A frame past this leaves by itself, so that what a turn's calls are written into stays small. */
const largeFrame = 1 << 15

export type Agreed = Required<Omit<Read<typeof Welcome.fields>, 'proof'>>

export interface SessionOptions {
	/** a name and version, for the server's logs */
	client: string
	/** a remote connection's token */
	token?: string | undefined
	/** SERVE's secret, whose proof a local server must give before any call */
	secret?: Uint8Array | undefined
	/** sixteen random bytes the proof answers; given with the secret */
	challenge?: Uint8Array | undefined
}

/**
 * The connection was lost with a stream on it. Sent says whether its REQUEST
 * had left: one that had not may go on another connection whatever it was,
 * and one that had is a read to send again or a write whose outcome is
 * unknown.
 */
export class LostError extends ClosedError {
	readonly sent: boolean

	constructor(message: string, sent: boolean) {
		super(message)
		this.sent = sent
	}
}

/** A frame of a stream's answer: the RESPONSE, a DATA, or the error that ends it. */
export interface Event {
	kind: 'response' | 'data'
	body: Uint8Array
	end: boolean
}

type Waiter<T> = { resolve: (v: T) => void; reject: (e: Error) => void }

/**
 * One stream: a client's REQUEST from its frame to the server's final frame,
 * the one span in which its number is in use.
 */
export class Stream {
	readonly id: number
	readonly method: number
	/** the REQUEST has left, so the server may have acted on it */
	sent = false
	/** the server's final frame came, or the connection ended */
	finished = false
	#events: Event[] = []
	#waiting: Waiter<Event> | undefined
	#failure: Error | undefined
	#session: Session
	#uploadCredit: number
	#creditWaiters: Waiter<void>[] = []
	#granting = 0
	#cancelled = false

	constructor(session: Session, id: number, method: number, uploadCredit: number) {
		this.#session = session
		this.id = id
		this.method = method
		this.#uploadCredit = uploadCredit
	}

	/** The next frame of the answer, or its error. */
	next(): Promise<Event> {
		const event = this.#events.shift()
		if (event !== undefined) {
			return Promise.resolve(event)
		}
		if (this.#failure !== undefined) {
			return Promise.reject(this.#failure)
		}
		return new Promise((resolve, reject) => {
			this.#waiting = { resolve, reject }
		})
	}

	/**
	 * Sends a body of an upload as DATA once the stream's credit lets it go;
	 * end says it is the last. Only an empty body ending the upload may be
	 * empty.
	 */
	async send(body: Uint8Array, end: boolean): Promise<void> {
		while (!this.finished && this.#uploadCredit < body.length) {
			await new Promise<void>((resolve, reject) => this.#creditWaiters.push({ resolve, reject }))
		}
		if (this.finished) {
			throw this.#failure ?? new ClosedError('the stream ended before its upload did')
		}
		this.#uploadCredit -= body.length
		this.#session.enqueue(this, frame(Kind.data, end ? Flag.end : 0, 0, this.id, body), body.length)
	}

	/**
	 * Gives back what a DATA the server sent took of the stream's credit, once
	 * the caller has done with it; a grant goes once half the window has.
	 */
	consumed(n: number): void {
		if (this.finished) {
			return
		}
		this.#granting += n
		if (this.#granting >= downloadWindow / 2) {
			this.#session.post(credit(this.id, this.#granting))
			this.#granting = 0
		}
	}

	/**
	 * Asks the server to stop. A stream whose REQUEST has not left never
	 * leaves; one that has keeps its number until the server's final frame,
	 * which is dropped.
	 */
	cancel(): void {
		if (this.finished || this.#cancelled) {
			return
		}
		this.#cancelled = true
		this.#session.cancel(this)
		this.fail(new CancelledError('the call was cancelled'))
	}

	get cancelled(): boolean {
		return this.#cancelled
	}

	deliver(event: Event): void {
		if (this.#cancelled) {
			return
		}
		const waiting = this.#waiting
		if (waiting !== undefined) {
			this.#waiting = undefined
			waiting.resolve(event)
			return
		}
		this.#events.push(event)
	}

	/** Ends what waits on the stream with err; events already arrived are read first. */
	fail(err: Error): void {
		this.#failure ??= err
		const waiting = this.#waiting
		this.#waiting = undefined
		waiting?.reject(err)
		for (const w of this.#creditWaiters.splice(0)) {
			w.reject(err)
		}
	}

	grant(n: number): void {
		this.#uploadCredit += n
		for (const w of this.#creditWaiters.splice(0)) {
			w.resolve()
		}
	}

	finish(): void {
		this.finished = true
		for (const w of this.#creditWaiters.splice(0)) {
			w.resolve()
		}
		this.#unwatch?.()
		this.#unwatch = undefined
	}

	/** Cancels the stream when the signal aborts, until the stream finishes. */
	watchSignal(signal: AbortSignal): void {
		this.#unwatch = watch(signal, this)
	}

	#unwatch: (() => void) | undefined
}

/**
 * A call that left the moment it was made, a REQUEST the RESPONSE answers:
 * its answer is read where it arrives and given to whoever asked, with no
 * stream's state between. Nothing cancels it.
 */
class Direct {
	readonly read: (r: Reader) => unknown
	readonly resolve: (value: never) => void
	readonly reject: (err: Error) => void

	constructor(
		read: (r: Reader) => unknown,
		resolve: (value: never) => void,
		reject: (err: Error) => void,
	) {
		this.read = read
		this.resolve = resolve
		this.reject = reject
	}
}

/** A frame waiting for the connection's credit, in the order it was asked for. */
interface Queued {
	stream: Stream
	bytes: Uint8Array
	counted: number
	request: boolean
}

/**
 * The protocol of one connection. It writes through send, which the
 * transport takes whole, and reads what receive is given, frames split
 * anywhere.
 */
export class Session {
	readonly #send: (bytes: Uint8Array) => void
	readonly #options: SessionOptions
	/** resolves with what the WELCOME agreed, or rejects with why there is none */
	readonly welcomed: Promise<Agreed>
	#welcome: Waiter<Agreed>
	#agreed: Agreed | undefined

	#streams = new Map<number, Stream | Direct>()
	#next = 0
	#waitingToOpen: Waiter<void>[] = []
	#queue: Queued[] = []
	#credit = 0

	// what leaves in this turn's one write: every frame asked for is written
	// here, a call's body straight from its fields
	#out = new Writer(1 << 12)
	#flushing = false
	readonly #leave = () => this.#flush()
	readonly #reader = new Reader(new Uint8Array(0))

	#rest: Uint8Array | undefined
	#goingAway = false
	#ended: Error | undefined
	readonly #endListeners: ((err: Error) => void)[] = []

	constructor(send: (bytes: Uint8Array) => void, options: SessionOptions) {
		this.#send = send
		this.#options = options
		let welcome!: Waiter<Agreed>
		this.welcomed = new Promise<Agreed>((resolve, reject) => {
			welcome = { resolve, reject }
		})
		this.#welcome = welcome
		// a caller that never awaits the handshake still hears of its end
		this.welcomed.catch(() => {})
		this.post(
			frame(
				Kind.hello,
				0,
				0,
				0,
				Hello.encode({
					protocol,
					client: options.client,
					token: options.token,
					streamCredit: downloadWindow,
					challenge: options.challenge,
				}),
			),
		)
	}

	get agreed(): Agreed | undefined {
		return this.#agreed
	}

	/** The server said GOAWAY: no stream opens here any more. */
	get goingAway(): boolean {
		return this.#goingAway
	}

	/** Why the connection ended, once it has. */
	get ended(): Error | undefined {
		return this.#ended
	}

	/** Streams in use, their final frames not yet come. */
	get streams(): number {
		return this.#streams.size
	}

	onEnd(listener: (err: Error) => void): void {
		if (this.#ended !== undefined) {
			listener(this.#ended)
			return
		}
		this.#endListeners.push(listener)
	}

	/**
	 * Opens a stream with its REQUEST. End says the REQUEST is the client's
	 * whole side; an upload's DATA follows through the stream otherwise. It
	 * waits while the streams in flight are all in use.
	 */
	async open(method: number, body: Uint8Array, end: boolean): Promise<Stream> {
		const signal = currentSignal()
		signal?.throwIfAborted()
		const agreed = await this.welcomed
		if (body.length > agreed.maxBody) {
			throw new LimitError(
				`a request of ${body.length} bytes, past the ${agreed.maxBody} a body holds`,
			)
		}
		while (this.#streams.size >= agreed.inFlight && this.#usable() === undefined) {
			await new Promise<void>((resolve, reject) => this.#waitingToOpen.push({ resolve, reject }))
		}
		const refused = this.#usable()
		if (refused !== undefined) {
			throw refused
		}
		const stream = new Stream(this, this.#nextID(), method, agreed.streamCredit)
		this.#streams.set(stream.id, stream)
		this.#enqueue({
			stream,
			bytes: frame(Kind.request, end ? Flag.end : 0, method, stream.id, body),
			counted: body.length,
			request: true,
		})
		if (signal !== undefined) {
			stream.watchSignal(signal)
		}
		return stream
	}

	/** Why no stream may open now: the connection ended or is going away. */
	#usable(): Error | undefined {
		if (this.#ended !== undefined) {
			return new LostError(`the connection ended: ${this.#ended.message}`, false)
		}
		if (this.#goingAway) {
			return new UnavailableError('the server is closing this connection')
		}
		return undefined
	}

	/**
	 * Makes a call in place when nothing makes it wait: `write` writes its
	 * body into the bytes that leave next, and `read` reads its answer where
	 * it arrived, for `resolve`. It says false, having sent nothing, when the
	 * call must wait or may be cancelled: before the WELCOME, under a signal,
	 * with every stream in use, or past the connection's credit. Then `call`
	 * makes it.
	 *
	 * A call an event loop makes costs it a promise and nothing more this way.
	 * Through a stream it cost five, and a copy of its body three times.
	 */
	direct<T>(
		method: number,
		write: (w: Writer) => void,
		read: (r: Reader) => T,
		resolve: (value: T) => void,
		reject: (err: Error) => void,
	): boolean {
		const agreed = this.#agreed
		if (
			agreed === undefined ||
			this.#ended !== undefined ||
			this.#goingAway ||
			this.#queue.length > 0 ||
			this.#streams.size >= agreed.inFlight ||
			currentSignal() !== undefined
		) {
			return false
		}
		const out = this.#out
		const start = out.reserve(headerSize)
		let length: number
		try {
			write(out)
			length = out.length - start - headerSize
		} catch (err) {
			out.truncate(start)
			throw err
		}
		if (length > agreed.maxBody || length > this.#credit) {
			out.truncate(start)
			return false
		}
		const id = this.#nextID()
		putHeader(out.buffer, start, Kind.request, Flag.end, method, id, length)
		this.#credit -= length
		this.#streams.set(id, new Direct(read, resolve as (value: never) => void, reject))
		this.#soon()
		return true
	}

	/** A call: one REQUEST, and the RESPONSE that answers it. */
	async call(method: number, body: Uint8Array, signal?: AbortSignal): Promise<Uint8Array> {
		signal?.throwIfAborted()
		const stream = await this.open(method, body, true)
		const unwatch = watch(signal, stream)
		try {
			const event = await stream.next()
			if (event.kind !== 'response' || !event.end) {
				this.fault(
					new ProtocolError(`a ${event.kind} without END answering a call on stream ${stream.id}`),
				)
			}
			return event.body
		} finally {
			unwatch()
		}
	}

	// a stream's number: from 1, never 0 and never one in use, so that a frame
	// for a stream that has ended never reaches the next one
	#nextID(): number {
		for (;;) {
			this.#next = this.#next === 0xffff_ffff ? 1 : this.#next + 1
			if (!this.#streams.has(this.#next)) {
				return this.#next
			}
		}
	}

	enqueue(stream: Stream, bytes: Uint8Array, counted: number): void {
		this.#enqueue({ stream, bytes, counted, request: false })
	}

	#enqueue(q: Queued): void {
		this.#queue.push(q)
		this.#drain()
	}

	// frames that count against the connection's credit leave in the order
	// they were asked for, as far as the credit goes
	#drain(): void {
		while (this.#queue.length > 0) {
			const q = this.#queue[0]!
			if (q.counted > this.#credit) {
				return
			}
			this.#queue.shift()
			this.#credit -= q.counted
			if (q.request) {
				q.stream.sent = true
			}
			this.post(q.bytes)
		}
	}

	/** Writes a frame that does not wait for credit: a CREDIT, a CANCEL, a PONG. */
	post(bytes: Uint8Array): void {
		if (this.#ended !== undefined) {
			return
		}
		if (bytes.length > largeFrame) {
			this.#flush()
			this.#send(bytes)
			return
		}
		this.#out.raw(bytes)
		this.#soon()
	}

	#soon(): void {
		if (!this.#flushing) {
			this.#flushing = true
			queueMicrotask(this.#leave)
		}
	}

	// the frames asked for in one turn leave in one write
	#flush(): void {
		this.#flushing = false
		if (this.#out.length === 0 || this.#ended !== undefined) {
			return
		}
		const bytes = this.#out.bytes()
		this.#out.truncate(0)
		this.#send(bytes)
	}

	cancel(stream: Stream): void {
		if (!stream.sent) {
			this.#queue = this.#queue.filter(q => q.stream !== stream)
			this.#release(stream)
			return
		}
		this.post(frame(Kind.cancel, 0, 0, stream.id, new Uint8Array(0)))
	}

	// takes a stream out of use, dropping what of it has not left: its number
	// may be named again at once
	#release(stream: Stream): void {
		stream.finish()
		if (this.#streams.get(stream.id) === stream) {
			this.#streams.delete(stream.id)
		}
		this.#queue = this.#queue.filter(q => q.stream !== stream || q.request)
		this.#waitingToOpen.shift()?.resolve()
	}

	/** Takes bytes the transport read: any part of a frame, or several. */
	receive(chunk: Uint8Array): void {
		if (this.#ended !== undefined) {
			return
		}
		let bytes = chunk
		if (this.#rest !== undefined) {
			bytes = new Uint8Array(this.#rest.length + chunk.length)
			bytes.set(this.#rest)
			bytes.set(chunk, this.#rest.length)
			this.#rest = undefined
		}
		const at = this.read(bytes, bytes.length)
		if (at < bytes.length && this.#ended === undefined) {
			this.#rest = bytes.slice(at)
		}
	}

	/**
	 * Takes the frames whole in the first `length` bytes and says how many
	 * bytes they were: the rest is a frame's first part, which whoever owns
	 * the bytes keeps for its end. Nothing of the bytes is kept here, so their
	 * owner may write them over once this returns.
	 */
	read(bytes: Uint8Array, length: number): number {
		if (this.#ended !== undefined) {
			return length
		}
		let at = 0
		try {
			while (length - at >= headerSize) {
				const h = parseHeader(bytes, at)
				checkHeader(h, this.#agreed?.maxBody ?? welcomeMost)
				const stop = at + headerSize + h.length
				if (stop > length) {
					break
				}
				this.#take(h, bytes, at + headerSize, stop)
				at = stop
			}
		} catch (err) {
			this.fault(err instanceof Error ? err : new ProtocolError(String(err)))
			return length
		}
		return at
	}

	// takes the frame whose body is the bytes from `from` to `to`
	#take(h: ReturnType<typeof parseHeader>, bytes: Uint8Array, from: number, to: number): void {
		if (this.#agreed !== undefined && (h.kind === Kind.response || h.kind === Kind.data)) {
			this.#answer(h, bytes, from, to)
			return
		}
		const body = bytes.slice(from, to)
		if (this.#agreed === undefined) {
			this.#handshake(h.kind, body)
			return
		}
		switch (h.kind) {
			case Kind.credit:
				this.#credited(h.stream, granted(body))
				return
			case Kind.ping:
				this.post(frame(Kind.pong, 0, 0, 0, body))
				return
			case Kind.pong:
				return
			case Kind.goaway:
				this.#goAway(body)
				return
		}
		throw new ProtocolError(`a ${kindName(h.kind)} from the server`)
	}

	#handshake(kind: number, body: Uint8Array): void {
		if (kind === Kind.goaway) {
			const away = GoAway.decode(body)
			this.end(
				errorOf(away.code ?? 'unavailable', away.message ?? 'the server refused the connection'),
			)
			return
		}
		if (kind !== Kind.welcome) {
			throw new ProtocolError(`a ${kindName(kind)} where the WELCOME belongs`)
		}
		const welcome = Welcome.decode(body)
		if (welcome.protocol !== protocol) {
			throw new ProtocolError(`the server speaks protocol ${welcome.protocol}, not ${protocol}`)
		}
		const { secret, challenge } = this.#options
		if (
			secret !== undefined &&
			challenge !== undefined &&
			!proves(secret, challenge, welcome.proof)
		) {
			throw new ProtocolError(
				'the server cannot prove it read SERVE: another process holds its endpoint',
			)
		}
		const agreed: Agreed = {
			protocol: welcome.protocol,
			server: welcome.server ?? '',
			instance: welcome.instance ?? new Uint8Array(0),
			capability: welcome.capability ?? 'data',
			maxBody: welcome.maxBody ?? 0,
			inFlight: welcome.inFlight ?? 1,
			connectionCredit: welcome.connectionCredit ?? 0,
			streamCredit: welcome.streamCredit ?? 0,
			engines: welcome.engines ?? [],
			now: welcome.now ?? 0,
		}
		this.#agreed = agreed
		this.#credit = agreed.connectionCredit
		this.#welcome.resolve(agreed)
	}

	#answer(h: ReturnType<typeof parseHeader>, bytes: Uint8Array, from: number, to: number): void {
		const { kind, flags, stream: id } = h
		const stream = this.#streams.get(id)
		if (stream === undefined) {
			throw new ProtocolError(`a ${kindName(kind)} on stream ${id}, which is not in use`)
		}
		if (stream instanceof Direct) {
			this.#answerDirect(stream, h, bytes, from, to)
			return
		}
		// a body of its own, since events outlive the read's buffer
		const body = bytes.slice(from, to)
		const end = (flags & Flag.end) !== 0
		if (end) {
			this.#release(stream)
		}
		if ((flags & Flag.error) !== 0) {
			stream.fail(failureOf(body))
			return
		}
		stream.deliver({ kind: kind === Kind.response ? 'response' : 'data', body, end })
		if (stream.cancelled && kind === Kind.data) {
			stream.consumed(body.length)
		}
	}

	// reads a call's answer where it arrived: what `read` returns holds none of these bytes
	#answerDirect(
		call: Direct,
		h: ReturnType<typeof parseHeader>,
		bytes: Uint8Array,
		from: number,
		to: number,
	): void {
		if (h.kind !== Kind.response || (h.flags & Flag.end) === 0) {
			throw new ProtocolError(
				`a ${kindName(h.kind)} without END answering a call on stream ${h.stream}`,
			)
		}
		this.#streams.delete(h.stream)
		this.#waitingToOpen.shift()?.resolve()
		if ((h.flags & Flag.error) !== 0) {
			call.reject(failureOf(bytes.slice(from, to)))
			return
		}
		const reader = this.#reader
		reader.reset(bytes, from, to)
		let value: unknown
		try {
			value = call.read(reader)
			reader.end()
		} catch (err) {
			call.reject(err instanceof Error ? err : new ProtocolError(String(err)))
			return
		}
		call.resolve(value as never)
	}

	#credited(id: number, n: number): void {
		if (id === 0) {
			this.#credit += n
			this.#drain()
			return
		}
		const stream = this.#streams.get(id)
		if (stream instanceof Stream) {
			stream.grant(n)
		}
	}

	// after a GOAWAY no stream opens; a REQUEST still waiting for credit goes
	// nowhere and may be sent on another connection
	#goAway(body: Uint8Array): void {
		const away = GoAway.decode(body)
		this.#goingAway = true
		const unsent = new UnavailableError(away.message ?? 'the server is closing this connection')
		for (const q of this.#queue.filter(q => q.request)) {
			q.stream.fail(unsent)
			this.#release(q.stream)
		}
		for (const w of this.#waitingToOpen.splice(0)) {
			w.reject(unsent)
		}
	}

	/** A frame broke the protocol: the connection ends, and whoever runs it closes it. */
	fault(err: Error): void {
		this.end(err instanceof ProtocolError ? err : new ProtocolError(err.message))
	}

	/**
	 * Ends the session: the transport closed, or a frame broke the protocol.
	 * Every stream still open learns whether its REQUEST had left.
	 */
	end(err: Error): void {
		if (this.#ended !== undefined) {
			return
		}
		this.#ended = err
		this.#welcome.reject(err)
		for (const stream of this.#streams.values()) {
			if (stream instanceof Direct) {
				stream.reject(new LostError(`the connection ended: ${err.message}`, true))
				continue
			}
			stream.fail(new LostError(`the connection ended: ${err.message}`, stream.sent))
			stream.finish()
		}
		this.#streams.clear()
		this.#queue = []
		for (const w of this.#waitingToOpen.splice(0)) {
			w.reject(new LostError(`the connection ended: ${err.message}`, false))
		}
		for (const listener of this.#endListeners.splice(0)) {
			listener(err)
		}
	}
}

/** Cancels a stream when a signal aborts; returns what stops watching. */
export function watch(signal: AbortSignal | undefined, stream: Stream): () => void {
	if (signal === undefined) {
		return () => {}
	}
	const cancel = () => stream.cancel()
	if (signal.aborted) {
		cancel()
		return () => {}
	}
	signal.addEventListener('abort', cancel, { once: true })
	return () => signal.removeEventListener('abort', cancel)
}

/** The error a final frame with ERROR carries. */
export function failureOf(body: Uint8Array): TinystoreError {
	const failure = Failure.decode(body)
	return errorOf(failure.code ?? 'internal', failure.message ?? '', failure.what ?? {})
}

/** A WELCOME's proof is the HMAC-SHA256 of the challenge keyed with SERVE's secret. */
export function proves(
	secret: Uint8Array,
	challenge: Uint8Array,
	proof: Uint8Array | undefined,
): boolean {
	if (proof === undefined || proof.length !== 32) {
		return false
	}
	return timingSafeEqual(createHmac('sha256', secret).update(challenge).digest(), proof)
}
