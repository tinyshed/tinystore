// A connection is a transport and the session over it; a link is what gives
// the store its connection, dialling again once the last one ended: the
// directory's sidecar, found through SERVE or started, a private child, or a
// remote server.

import { randomBytes } from 'node:crypto'
import { join, resolve } from 'node:path'

import manifest from '../package.json' with { type: 'json' }
import { currentSignal } from './cancel.ts'
import {
	ClosedError,
	InvalidError,
	OutcomeUnknownError,
	TinystoreError,
	UnavailableError,
} from './errors.ts'
import type { PrivateChild, Runtime, TlsOptions, Transport } from './runtime.ts'
import { LostError, Session, type SessionOptions, type Stream, watch } from './session.ts'
import { Empty, methods, StoreOptions } from './wire/protocol.ts'

/**
 * How far a commit goes before it returns, for the files of a store or of
 * one database:
 *
 *     'full'   synced to the disk: it survives a crash of the operating
 *              system and a power loss. What every file is unless told.
 *     'os'     written to the operating system, synced a little later: it
 *              survives the program's crash, and a power loss may take the
 *              last commits, never the file.
 */
export type Durability = 'full' | 'os'

/** Refuses a durability that is neither word, before anything leaves. */
export function checkDurability(durability: Durability | undefined, of: string): void {
	if (durability !== undefined && durability !== 'full' && durability !== 'os') {
		throw new InvalidError(`${of}: durability ${JSON.stringify(durability)}: it is 'full' or 'os'`)
	}
}

/**
 * Dials as `dial` does, and refuses a store whose files commit another way
 * than this program asked for: a store has one durability, its first
 * opener's, and a program that needs another must not be told it has it.
 */
export function asking(dial: Dialer, dir: string, durability: Durability | undefined): Dialer {
	if (durability === undefined) {
		return dial
	}
	return async () => {
		const connection = await dial()
		const opened = (await connection.session.welcomed).durability
		if (opened !== durability) {
			await connection.close()
			const as = opened === undefined ? 'with none said' : `with durability '${opened}'`
			throw new InvalidError(
				`the store in ${dir} is open ${as}, and this program asked for '${durability}'`,
			)
		}
		return connection
	}
}

/** How long a server may take to answer HELLO. */
const handshakeTime = 5000

export class Connection {
	readonly session: Session
	readonly #transport: Transport
	/** what close waits for once the transport has ended: a private child's exit */
	readonly #closing: () => Promise<unknown>
	/** the handles this connection opened, by what they are: a bucket's name and options */
	readonly handles = new Map<string, Promise<number>>()
	/** the handle each open was answered, once it was: what a call made in place reads without waiting */
	readonly opened = new Map<object, number>()

	constructor(
		session: Session,
		transport: Transport,
		closing: () => Promise<unknown> = () => Promise.resolve(),
	) {
		this.session = session
		this.#transport = transport
		this.#closing = closing
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

	/** Shakes hands over a pipe to the core in this process. */
	static async overPipe(session: Session, transport: Transport): Promise<Connection> {
		const connection = new Connection(session, transport)
		await connection.#handshake()
		return connection
	}

	/**
	 * Shakes hands over a private child's stdin and stdout. Closing waits for
	 * the child's exit, since until then it holds the directory: its LOCK, and
	 * on Windows every file it opened, which a removal fails on.
	 */
	static async overChild(child: PrivateChild, session: Session): Promise<Connection> {
		const connection = new Connection(session, child.transport, () => child.exited)
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

	/** Ends the connection; a private child is waited for until it has exited. */
	async close(): Promise<void> {
		this.session.end(new ClosedError('the store closed'))
		this.#transport.close()
		await this.#closing()
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
	/** a server started for its clients, which a client of a newer release replaces */
	sidecar?: boolean
}

/** How long a client waits for a sidecar to answer, its own or another's, before it gives up. */
const startTime = 15_000

/** The pause before a start that found the directory held starts again, doubling up to a second. */
const heldPause = 100

/**
 * Finds the directory's sidecar through SERVE or starts one, as docs/wire.md
 * "Finding a local server" says: a sidecar found proves itself before the
 * first call, and one started exits with 3 when another holds the directory,
 * whose SERVE the client then waits for. A sidecar of an older release than
 * this SDK's is stopped and replaced with this SDK's binary; another server
 * of an older release, a person's or a program's own, is told of once.
 */
export function sidecar(
	runtime: Runtime,
	dir: string,
	binary: () => string,
	started: Started = {},
	older: (server: string) => boolean = server => olderRelease(server, manifest.version),
): Dialer {
	const absolute = resolve(dir)
	const serve = join(absolute, 'server', 'SERVE')
	const log = join(absolute, 'server', 'serve.log')
	const idling = started.idle === undefined ? [] : ['--idle', `${started.idle}ms`]
	let told = false
	return async () => {
		const found = await reachServe(runtime, serve)
		let stoppedInstance: string | undefined
		if (found !== undefined) {
			const server = (await found.connection.session.welcomed).server
			if (!older(server)) {
				return found.connection
			}
			if (found.sidecar && (await stopped(found.connection))) {
				process.stderr.write(`${replacedSidecar(server, manifest.version, dir)}\n`)
				stoppedInstance = found.instance
			} else {
				if (!told) {
					process.stderr.write(`${olderServer(server, manifest.version, dir)}\n`)
				}
				told = true
				return found.connection
			}
		}
		const flags = [...idling, ...storeFlags(started)]
		const command = [binary(), 'serve', '--dir', absolute, '--local', '--log', log, ...flags]
		return startSidecar(runtime, serve, log, command, stoppedInstance)
	}
}

/** What a server this process starts is started with; one found running keeps its own. */
export interface Started {
	/** how long a sidecar stays once its last connection has gone, in milliseconds */
	idle?: number | undefined
	durability?: Durability | undefined
	/** the file of the store's encryption key, when it is not the store's own */
	encryptionKeyFile?: string | undefined
}

/** The flags of `tinystore serve` that say how its store opens. */
function storeFlags(started: Started): string[] {
	const committing = started.durability === undefined ? [] : ['--durability', started.durability]
	const key = started.encryptionKeyFile
	return key === undefined ? committing : [...committing, '--encryption-key-file', resolve(key)]
}

/**
 * Starts a sidecar and waits for SERVE to name one that answers, its own or
 * another client's that won the start. One that exits with 3 found the
 * directory held, by a winner about to publish or by a sidecar still letting
 * go, and is started again after a pause; any other exit is an error. A
 * sidecar told to stop answers for a moment after it agreed, until SERVE
 * goes, so the one of the instance stopped is passed over.
 */
async function startSidecar(
	runtime: Runtime,
	serve: string,
	log: string,
	command: string[],
	stoppedInstance?: string,
): Promise<Connection> {
	const deadline = Date.now() + startTime
	let held = false
	for (let pause = heldPause; ; pause = Math.min(pause * 2, 1000)) {
		const child = runtime.spawnDetached(command)
		let exited: number | undefined
		child.exited.then(code => {
			exited = code
		})
		let again: number | undefined
		for (let wait = 5; Date.now() < deadline; wait = Math.min(wait * 2, 100)) {
			const reached = await reachServe(runtime, serve)
			if (reached !== undefined && reached.instance !== stoppedInstance) {
				return reached.connection
			}
			await reached?.connection.close()
			if (exited !== undefined && exited !== 3) {
				throw new ClosedError(`the sidecar exited with ${exited}: ${await tail(runtime, log)}`)
			}
			if (exited === 3) {
				held = true
				again ??= Date.now() + pause
				if (Date.now() >= again) {
					break
				}
			}
			await sleep(wait)
		}
		if (Date.now() >= deadline) {
			const why = held
				? 'another process holds the directory but no sidecar answers'
				: 'no sidecar answered'
			throw new ClosedError(`${why} in ${startTime / 1000} s: ${await tail(runtime, log)}`)
		}
	}
}

/**
 * Asks a sidecar to stop as tinystore stop does, closing the connection once
 * it has answered; false when it refuses, and the connection stays open.
 */
async function stopped(connection: Connection): Promise<boolean> {
	try {
		await connection.session.call(methods['server.stop'], Empty.encode({}))
	} catch {
		return false
	}
	await connection.close()
	return true
}

/**
 * Whether a server's release is older than the SDK's own; a build that is no
 * release, a development copy's or a Go pseudo-version, is neither.
 */
export function olderRelease(server: string, own: string): boolean {
	const theirs = release(server)
	const ours = release(own)
	return theirs !== undefined && ours !== undefined && compareReleases(theirs, ours) < 0
}

/** What a program whose SDK replaced a sidecar of an older release is told. */
export function replacedSidecar(server: string, own: string, dir: string): string {
	return (
		`tinystore: the sidecar serving ${dir} was ${server}, older than this SDK's ${own}; it finishes ` +
		"its calls, and this SDK's binary serves the directory from now on"
	)
}

/** What a program whose SDK found a server of an older release that is no sidecar is told. */
export function olderServer(server: string, own: string, dir: string): string {
	return (
		`tinystore: ${dir} is served by ${server}, older than this SDK's ${own}; a person or a program ` +
		'started that server, and it runs until they stop it'
	)
}

interface Release {
	core: number[]
	pre: string[]
}

/**
 * A release's version, its v optional: 0.2.0, v0.2.0-rc.1. A development copy's
 * 0.0.0, a Go pseudo-version and (devel) are none.
 */
function release(version: string): Release | undefined {
	const parts = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.]+))?$/.exec(version)
	if (parts === null) {
		return undefined
	}
	const core = [Number(parts[1]), Number(parts[2]), Number(parts[3])]
	return core.every(n => n === 0)
		? undefined
		: { core, pre: parts[4] === undefined ? [] : parts[4].split('.') }
}

/** Semantic versioning's order: a release after its pre-releases, rc.2 before rc.10. */
function compareReleases(a: Release, b: Release): number {
	for (let i = 0; i < 3; i++) {
		if (a.core[i] !== b.core[i]) {
			return a.core[i]! - b.core[i]!
		}
	}
	if (a.pre.length === 0 || b.pre.length === 0) {
		return b.pre.length - a.pre.length
	}
	for (let i = 0; i < Math.min(a.pre.length, b.pre.length); i++) {
		const x = a.pre[i]!
		const y = b.pre[i]!
		const numbers = /^\d+$/.test(x) && /^\d+$/.test(y)
		if (x !== y) {
			return numbers ? Number(x) - Number(y) : x < y ? -1 : 1
		}
	}
	return a.pre.length - b.pre.length
}

/** The sidecar SERVE names, once its proof checks; undefined on any failure. */
async function reachServe(
	runtime: Runtime,
	serve: string,
): Promise<{ connection: Connection; sidecar: boolean; instance: string } | undefined> {
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
	if (published.protocol !== 2 || endpoint === undefined || secret.length !== 32) {
		return undefined
	}
	try {
		const connection = await Connection.dial(runtime, endpoint, {
			client: runtime.client,
			secret: new Uint8Array(secret),
			challenge: new Uint8Array(randomBytes(16)),
		})
		return { connection, sidecar: published.sidecar === true, instance: published.instance }
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
export function privateChild(
	runtime: Runtime,
	dir: string,
	binary: () => string,
	clock?: Date,
	started: Started = {},
): Dialer {
	// a test's clock starts where the first child's did, and a child started again starts it there
	const clocked = clock === undefined ? [] : ['--clock', clock.toISOString()]
	return async () => {
		let session: Session | undefined
		const argv = [
			binary(),
			'serve',
			'--dir',
			resolve(dir),
			'--stdio',
			...clocked,
			...storeFlags(started),
		]
		const child = runtime.spawnPrivate(argv, {
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

/**
 * The core in this process, through its C ABI: no server and no socket, the
 * frames handed to the library and back. Bun alone, through bun:ffi.
 */
export function embedded(
	runtime: Runtime,
	dir: string,
	library: () => string,
	started: Started = {},
): Dialer {
	const encryptionKeyFile =
		started.encryptionKeyFile === undefined ? undefined : resolve(started.encryptionKeyFile)
	const options = StoreOptions.encode({ durability: started.durability, encryptionKeyFile })
	return async () => {
		const { openPipe } = await import('./runtime/pipe.ts')
		let session: Session | undefined
		const transport = openPipe(library(), resolve(dir), options, {
			frames: (bytes, length) => session?.read(bytes, length) ?? length,
			end: err => session?.end(err),
		})
		session = new Session(bytes => transport.write(bytes), { client: runtime.client })
		return await Connection.overPipe(session, transport)
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
	#dialled: Connection | undefined
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

	/** The connection a call goes on, when one is there to take it without waiting. */
	ready(): Connection | undefined {
		const c = this.#dialled
		return c !== undefined && c.session.ended === undefined && !c.session.goingAway ? c : undefined
	}

	// one dial at a time: every caller that found the same dead connection
	// waits for the one new one
	#redial(dead: Promise<Connection> | undefined): Promise<Connection> {
		if (this.#current !== dead && this.#current !== undefined) {
			return this.connection()
		}
		const dialled = this.#dial()
		this.#current = dialled
		this.#dialled = undefined
		dialled.then(
			connection => {
				if (this.#current === dialled) {
					this.#dialled = connection
				}
			},
			() => {
				if (this.#current === dialled) {
					this.#current = undefined
				}
			},
		)
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
				if (tries < 2 && goesAgain(err, idempotence)) {
					continue
				}
				throw err
			}
		}
	}

	/** Closes the connection and every one to come. */
	async close(): Promise<void> {
		this.#closed = true
		const current = this.#current
		this.#current = undefined
		this.#dialled = undefined
		await current?.then(
			c => c.close(),
			() => {},
		)
	}
}

/**
 * Whether a call that failed goes on another connection: one whose REQUEST
 * had not left, and a read that had. A write that had is OutcomeUnknownError,
 * thrown here; any other failure is the call's own.
 */
export function goesAgain(err: unknown, idempotence: Idempotence): boolean {
	if (!(err instanceof LostError || err instanceof UnavailableError)) {
		return false
	}
	const sent = err instanceof LostError && err.sent
	if (!sent || idempotence === 'read') {
		return true
	}
	throw new OutcomeUnknownError(
		`the connection was lost with the write in flight; read what it wrote before writing again: ${err.message}`,
	)
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
