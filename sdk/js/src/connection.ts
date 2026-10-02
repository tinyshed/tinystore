// A connection is a transport and the session over it; a link is what gives
// the store its connection, dialling again once the last one ended: the
// directory's sidecar, found through SERVE or started, a private child, or a
// remote server.

import { randomBytes } from 'node:crypto'
import { join, resolve } from 'node:path'

import { currentSignal } from './cancel.ts'
import { ClosedError, OutcomeUnknownError, TinystoreError, UnavailableError } from './errors.ts'
import type { PrivateChild, Runtime, TlsOptions, Transport } from './runtime.ts'
import { LostError, Session, type SessionOptions, type Stream, watch } from './session.ts'

/** How long a server may take to answer HELLO. */
const handshakeTime = 5000

export class Connection {
	readonly session: Session
	readonly #transport: Transport
	/** the handles this connection opened, by what they are: a bucket's name and options */
	readonly handles = new Map<string, Promise<number>>()

	constructor(session: Session, transport: Transport) {
		this.session = session
		this.#transport = transport
	}

	/** Connects, shakes hands, and gives up after the handshake's time. */
	static async dial(runtime: Runtime, endpoint: string, options: SessionOptions, tls?: TlsOptions) {
		let session: Session | undefined
		const early: Uint8Array[] = []
		const transport = await runtime
			.connect(
				endpoint,
				{
					data: bytes =>
						session === undefined ? early.push(bytes.slice()) : session.receive(bytes),
					end: err => session?.end(err),
				},
				tls,
			)
			.catch((err: unknown) => {
				// a socket that refuses fails as the platform says it; a caller sees one kind
				if (err instanceof TinystoreError) {
					throw err
				}
				throw new ClosedError(`cannot reach ${endpoint}: ${(err as Error).message}`)
			})
		session = new Session(bytes => transport.write(bytes), options)
		for (const bytes of early) {
			session.receive(bytes)
		}
		const connection = new Connection(session, transport)
		await connection.#handshake()
		return connection
	}

	/** Shakes hands over a private child's stdin and stdout. */
	static async overChild(child: PrivateChild, session: Session): Promise<Connection> {
		const connection = new Connection(session, child.transport)
		await connection.#handshake()
		return connection
	}

	async #handshake(): Promise<void> {
		const timer = setTimeout(
			() => this.session.end(new ClosedError(`no WELCOME within ${handshakeTime / 1000} s`)),
			handshakeTime,
		)
		try {
			await this.session.welcomed
		} catch (err) {
			this.#transport.close()
			throw err
		} finally {
			clearTimeout(timer)
		}
		this.session.onEnd(() => this.#transport.close())
	}

	close(): void {
		this.session.end(new ClosedError('the store closed'))
		this.#transport.close()
	}
}

/** Where a link dials. */
export type Dialer = () => Promise<Connection>

/** SERVE, where a directory's sidecar says it listens. */
interface Published {
	protocol: number
	server: string
	pid: number
	instance: string
	secret: string
	endpoints: string[]
}

/** How long a client waits for another's sidecar, which won the start, before starting one again. */
const winnerTime = 5000

/** The starts a client tries before it gives up on the directory. */
const starts = 3

/**
 * Finds the directory's sidecar through SERVE or starts one, as docs/server.md
 * "SERVE" says: a sidecar found proves itself before the first call, and one
 * started exits with 3 when another holds the directory, whose SERVE the
 * client then waits for.
 */
export function sidecar(
	runtime: Runtime,
	dir: string,
	binary: () => string,
	idle?: number,
): Dialer {
	const absolute = resolve(dir)
	const serve = join(absolute, 'server', 'SERVE')
	const log = join(absolute, 'server', 'serve.log')
	const idling = idle === undefined ? [] : ['--idle', `${idle}ms`]
	return async () => {
		const found = await reachServe(runtime, serve)
		if (found !== undefined) {
			return found
		}
		let why = ''
		for (let tried = 0; tried < starts; tried++) {
			const child = runtime.spawnDetached([
				binary(),
				'serve',
				'--dir',
				absolute,
				'--local',
				'--log',
				log,
				...idling,
			])
			let exited: number | undefined
			child.exited.then(code => {
				exited = code
			})
			const deadline = Date.now() + winnerTime
			for (let pause = 5; Date.now() < deadline; pause = Math.min(pause * 2, 100)) {
				const reached = await reachServe(runtime, serve)
				if (reached !== undefined) {
					return reached
				}
				if (exited !== undefined && exited !== 3) {
					throw new ClosedError(`the sidecar exited with ${exited}: ${await tail(runtime, log)}`)
				}
				await sleep(pause)
			}
			why =
				exited === 3
					? 'another process holds the directory but no sidecar answers'
					: 'no sidecar answered'
		}
		throw new ClosedError(
			`${why} in ${(starts * winnerTime) / 1000} s: ${await tail(runtime, log)}`,
		)
	}
}

/** The sidecar SERVE names, once its proof checks; undefined on any failure. */
async function reachServe(runtime: Runtime, serve: string): Promise<Connection | undefined> {
	let published: Published
	try {
		const text = await runtime.readFile(serve)
		if (text === undefined) {
			return undefined
		}
		published = JSON.parse(new TextDecoder().decode(text))
	} catch {
		return undefined
	}
	const endpoint = published.endpoints?.[0]
	const secret = Buffer.from(published.secret ?? '', 'base64url')
	if (published.protocol !== 1 || endpoint === undefined || secret.length !== 32) {
		return undefined
	}
	try {
		return await Connection.dial(runtime, endpoint, {
			client: runtime.client,
			secret: new Uint8Array(secret),
			challenge: new Uint8Array(randomBytes(16)),
		})
	} catch {
		return undefined
	}
}

function sleep(ms: number): Promise<void> {
	return new Promise(resolve => setTimeout(resolve, ms))
}

async function tail(runtime: Runtime, log: string): Promise<string> {
	const text = await runtime.readFile(log).catch(() => undefined)
	if (text === undefined) {
		return 'it wrote no log'
	}
	const lines = new TextDecoder().decode(text).trimEnd().split('\n')
	return lines.slice(-5).join('\n')
}

/** A private child: tinystore serve --stdio, living and dying with this process. */
export function privateChild(runtime: Runtime, dir: string, binary: () => string): Dialer {
	return async () => {
		let session: Session | undefined
		const child = runtime.spawnPrivate([binary(), 'serve', '--dir', resolve(dir), '--stdio'], {
			data: bytes => session?.receive(bytes),
			end: err => session?.end(err),
		})
		session = new Session(bytes => child.transport.write(bytes), { client: runtime.client })
		session.onEnd(() => {
			child.transport.close()
			// a child told to leave by the end of its stdin drains and exits; one that does not is killed
			const killer = setTimeout(() => child.kill(), 10_000)
			child.exited.finally(() => clearTimeout(killer))
		})
		try {
			return await Connection.overChild(child, session)
		} catch (err) {
			child.kill()
			throw new ClosedError(`the private server did not start: ${(err as Error).message}`)
		}
	}
}

/** A remote server: tls:// checks its certificate before the token leaves; tcp:// does not. */
export function remote(runtime: Runtime, url: string, token: string, tls?: TlsOptions): Dialer {
	return () => Connection.dial(runtime, url, { client: runtime.client, token }, tls)
}

/** What a call may do when its connection is lost: go again, or say its outcome is unknown. */
export type Idempotence = 'read' | 'write'

/**
 * Gives calls a connection, dialling again once the last one ended, and runs
 * a call so that a lost connection costs it what it must: a call whose
 * REQUEST had not left goes again, so does a read that had, and a write that
 * had is OutcomeUnknownError.
 */
export class Link {
	readonly #dial: Dialer
	#current: Promise<Connection> | undefined
	#closed = false

	constructor(dial: Dialer) {
		this.#dial = dial
	}

	/** The connection a call goes on. */
	connection(): Promise<Connection> {
		if (this.#closed) {
			return Promise.reject(new ClosedError('the store is closed'))
		}
		const current = this.#current
		if (current !== undefined) {
			return current.then(
				c => (c.session.ended === undefined && !c.session.goingAway ? c : this.#redial(current)),
				() => this.#redial(current),
			)
		}
		return this.#redial(undefined)
	}

	// one dial at a time: every caller that found the same dead connection
	// waits for the one new one
	#redial(dead: Promise<Connection> | undefined): Promise<Connection> {
		if (this.#current !== dead && this.#current !== undefined) {
			return this.connection()
		}
		const dialled = this.#dial()
		this.#current = dialled
		dialled.catch(() => {
			if (this.#current === dialled) {
				this.#current = undefined
			}
		})
		return dialled
	}

	/**
	 * Runs a call on the link's connection. build makes its REQUEST's body for
	 * that connection, whose handles it may open; run sends it.
	 */
	async run<T>(
		idempotence: Idempotence,
		attempt: (connection: Connection) => Promise<T>,
		signal?: AbortSignal,
	): Promise<T> {
		for (let tries = 0; ; tries++) {
			;(signal ?? currentSignal())?.throwIfAborted()
			const connection = await this.connection()
			try {
				return await attempt(connection)
			} catch (err) {
				if (tries < 2 && (err instanceof LostError || err instanceof UnavailableError)) {
					const sent = err instanceof LostError && err.sent
					if (!sent || idempotence === 'read') {
						continue
					}
					throw new OutcomeUnknownError(
						`the connection was lost with the write in flight; read what it wrote before writing again: ${err.message}`,
					)
				}
				throw err
			}
		}
	}

	/** Closes the connection and every one to come. */
	close(): void {
		this.#closed = true
		const current = this.#current
		this.#current = undefined
		current?.then(
			c => c.close(),
			() => {},
		)
	}
}

/** What a download sent: its header, its items, and the trailer that ended it. */
export interface Downloaded {
	header: Uint8Array
	items: Uint8Array[]
	trailer: Uint8Array
}

/**
 * Runs a download to its end: the RESPONSE that heads it, an item a DATA, and
 * the DATA that ends it, giving each item's credit back as it arrives, since
 * the server bounds a page before it sends it. A download the server answers
 * with a RESPONSE that ends it has no items and that RESPONSE for trailer.
 */
export async function download(
	connection: Connection,
	method: number,
	body: Uint8Array,
	signal?: AbortSignal,
): Promise<Downloaded> {
	signal?.throwIfAborted()
	const stream = await connection.session.open(method, body, true)
	const unwatch = watch(signal, stream)
	try {
		const head = await stream.next()
		if (head.end) {
			return { header: head.body, items: [], trailer: head.body }
		}
		const items: Uint8Array[] = []
		for (;;) {
			const event = await stream.next()
			stream.consumed(event.body.length)
			if (event.end) {
				return { header: head.body, items, trailer: event.body }
			}
			items.push(event.body)
		}
	} finally {
		unwatch()
	}
}

export type { Stream }
