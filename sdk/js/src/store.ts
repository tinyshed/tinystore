import { Blobs } from './blobs.ts'
import { Link, privateChild, remote, sidecar } from './connection.ts'
import { ClosedError } from './errors.ts'
import { Jobs } from './jobs.ts'
import { Kv } from './kv.ts'
import { Metrics } from './metrics.ts'
import { Records } from './records.ts'
import { bunRuntime } from './runtime/bun.ts'
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
	 * Opens a database of the application's own, sql/<name>.db. The first open
	 * in the server, on an admin connection, applies its migrations; every
	 * later one checks them against what the file applied.
	 */
	sql(name: string, options?: SqlOptions): Promise<Database> {
		return openDatabase(this.#link, name, options)
	}

	/**
	 * Ingests the instruments' last values, then closes the connection; the
	 * directory's sidecar goes once it has been idle.
	 */
	async close(): Promise<void> {
		this.metrics.stop()
		if (this.metrics.instruments > 0) {
			await this.metrics.flush().catch(() => {})
		}
		this.#link.close()
	}

	[Symbol.asyncDispose](): Promise<void> {
		return this.close()
	}
}

/**
 * Opens the store in a directory through its sidecar, found through SERVE or
 * started, or through a private child when asked. It returns once the server
 * has answered, so that a directory that cannot be served fails here.
 */
export async function open(dir: string, options: OpenOptions = {}): Promise<Store> {
	return openWith(bunRuntime, dir, options)
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
	const link = new Link(remote(bunRuntime, url, options.token, options.tls))
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
