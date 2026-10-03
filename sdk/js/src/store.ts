import { Blobs } from './blobs.ts'
import { Link, privateChild, remote, sidecar } from './connection.ts'
import { ClosedError } from './errors.ts'
import { Jobs } from './jobs.ts'
import { Kv } from './kv.ts'
import { Metrics } from './metrics.ts'
import { Records } from './records.ts'
import { bunRuntime } from './runtime/bun.ts'
import { nodeRuntime } from './runtime/node.ts'
import type { Runtime, TlsOptions } from './runtime.ts'
import { type Database, openDatabase, type SqlOptions } from './sql.ts'
import { type Duration, ms } from './time.ts'

export interface OpenOptions {
	/**
	 * A server of this process's own, a child on stdin and stdout that lives
	 * and dies with it, rather than the directory's shared sidecar: tests,
	 * scripts, one process alone.
	 */
	private?: boolean
	/** The tinystore binary: TINYSTORE_BIN, this platform's package, or PATH's when absent. */
	binary?: string
	/**
	 * How long a sidecar this process starts stays once its last connection
	 * has gone: 30 s unless given, 0 for ever. One found running keeps its own.
	 */
	idle?: Duration
}

/** What a server is, as its WELCOME said it to this store's connection. */
export interface Status {
	/** its version: v0.1.0, or (devel) for one built from a checkout */
	server: string
	protocol: number
	/** the engines it serves */
	engines: string[]
	/** admin may change a schema and drop records; data reads and writes */
	capability: 'admin' | 'data'
}

export interface ConnectOptions {
	/** a line of the server's tokens file, admin or data */
	token: string
	tls?: TlsOptions
}

/**
 * The store in a directory, as another process serves it. Close it once, as
 * a Go program closes its tinystore.Store: `await using store = await open(dir)`.
 */
export class Store implements AsyncDisposable {
	readonly kv: Kv
	readonly jobs: Jobs
	readonly blobs: Blobs
	readonly records: Records
	readonly metrics: Metrics
	readonly #link: Link

	constructor(link: Link) {
		this.#link = link
		this.kv = new Kv(link)
		this.jobs = new Jobs(link)
		this.blobs = new Blobs(link)
		this.records = new Records(link)
		this.metrics = new Metrics(link)
	}

	/**
	 * What the server this store reaches is: its version, the protocol the
	 * connection speaks, the engines it serves and what the connection may do.
	 * A client newer than its server learns here what it may ask for; a call
	 * past it is UnimplementedError, naming the server's version.
	 */
	async status(): Promise<Status> {
		const agreed = await (await this.#link.connection()).session.welcomed
		return {
			server: agreed.server,
			protocol: Number(agreed.protocol),
			engines: [...agreed.engines],
			capability: agreed.capability === 'admin' ? 'admin' : 'data',
		}
	}

	/**
	 * Opens a database of the application's own, sql/<name>.db. The first open
	 * in the server, on an admin connection, applies its migrations; every
	 * later one checks them against what the file applied.
	 */
	sql(name: string, options?: SqlOptions): Promise<Database> {
		return openDatabase(this.#link, name, options)
	}

	/**
	 * Ingests the instruments' last values and hands over the loggers' lines,
	 * then closes the connection. A private child is waited for until it has
	 * exited, so that the directory is free once this returns; the directory's
	 * sidecar goes once it has been idle.
	 */
	async close(): Promise<void> {
		await this.metrics.stop()
		await this.records.stop()
		this.kv.stop()
		await this.#link.close()
	}

	[Symbol.asyncDispose](): Promise<void> {
		return this.close()
	}
}

/** Bun's own runtime under Bun, and Node's under Node. */
const runtime: Runtime = typeof Bun === 'undefined' ? nodeRuntime : bunRuntime

/**
 * Opens the store in a directory through its sidecar, found through SERVE or
 * started, or through a private child when asked. It returns once the server
 * has answered, so that a directory that cannot be served fails here.
 */
export async function open(dir: string, options: OpenOptions = {}): Promise<Store> {
	return openWith(runtime, dir, options)
}

export async function openWith(
	runtime: Runtime,
	dir: string,
	options: OpenOptions = {},
): Promise<Store> {
	const binary = () => findBinary(runtime, options.binary)
	const idle = options.idle === undefined ? undefined : ms(options.idle)
	const link = new Link(
		options.private === true
			? privateChild(runtime, dir, binary)
			: sidecar(runtime, dir, binary, idle),
	)
	await link.connection()
	return new Store(link)
}

/**
 * Connects to a remote server: tls:// checks the server's certificate before
 * the token leaves; tcp:// sends the token in the clear, for a network its
 * operator trusts.
 */
export async function connect(url: string, options: ConnectOptions): Promise<Store> {
	const link = new Link(remote(runtime, url, options.token, options.tls))
	await link.connection()
	return new Store(link)
}

function findBinary(runtime: Runtime, given: string | undefined): string {
	const found =
		given ?? process.env.TINYSTORE_BIN ?? runtime.packagedBinary() ?? runtime.which('tinystore')
	if (found === undefined || found === '') {
		throw new ClosedError(
			'no tinystore binary: this platform has no @tinyshed/tinystore package installed; ' +
				'install it, put tinystore on PATH, or set TINYSTORE_BIN',
		)
	}
	return found
}
