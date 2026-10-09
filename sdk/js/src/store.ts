import { randomBytes } from 'node:crypto'
import { open as openFile, rename, rm } from 'node:fs/promises'

import { Blobs } from './blobs.ts'
import { Clock } from './clock.ts'
import { embedded, Link, privateChild, remote, sidecar } from './connection.ts'
import { ClosedError, InvalidError } from './errors.ts'
import { Jobs } from './jobs.ts'
import { Kv } from './kv.ts'
import { Metrics } from './metrics.ts'
import { Records } from './records.ts'
import { bunRuntime } from './runtime/bun.ts'
import { nodeRuntime } from './runtime/node.ts'
import type { Runtime, TlsOptions } from './runtime.ts'
import { type Database, openDatabase, type SqlOptions } from './sql.ts'
import { type Duration, ms, type Time, unixMs } from './time.ts'
import { Backup, methods } from './wire/messages.ts'

export interface OpenOptions {
	/**
	 * A server of this process's own, a child on stdin and stdout that lives
	 * and dies with it, rather than the directory's shared sidecar: tests,
	 * scripts, one process alone.
	 */
	private?: boolean
	/**
	 * The core inside this process, loaded from its library, rather than a
	 * server beside it: no process and no socket. Bun alone for now.
	 */
	embedded?: boolean
	/** The core's library for `embedded`: TINYSTORE_LIBRARY when absent. */
	library?: string | undefined
	/** The tinystore binary: TINYSTORE_BIN, this platform's package, or PATH's when absent. */
	binary?: string
	/**
	 * How long a sidecar this process starts stays once its last connection
	 * has gone: 30 s unless given, 0 for ever. One found running keeps its own.
	 */
	idle?: Duration
	/**
	 * Runs a private server on a test's clock, starting at this time, which
	 * `store.clock` moves forward instead of a test waiting: keys expire, jobs
	 * come due and records age at once. Needs `private`.
	 */
	clock?: Time
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

/** What `store.backup` keeps beside the engines' files. */
export interface BackupOptions {
	/** files of the application's, by their paths inside the store's directory */
	files?: readonly string[] | undefined
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
	/** The test's clock of a private store opened with `clock`; on any other, its calls are InvalidError. */
	readonly clock: Clock
	readonly #link: Link

	constructor(link: Link) {
		this.#link = link
		this.clock = new Clock(link)
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
	 * later one checks them against what the file applied. Without migrations
	 * it opens the file as it is, an empty one if there is none, and checks
	 * nothing.
	 */
	sql(name: string, options?: SqlOptions): Promise<Database> {
		return openDatabase(this.#link, name, options)
	}

	/**
	 * Writes a backup of the whole store to a zip at path while the store keeps
	 * working, as `tinystore backup` does: every engine's file, with its size
	 * and checksum, which `tinystore restore` checks. The zip is written beside
	 * path and renamed into place once whole, so a backup that fails leaves no
	 * zip. It needs an admin connection; a remote server sends the zip over it.
	 *
	 * A backup holds no file but the engines' unless `files` names it: a file
	 * of the application's inside the store's directory, such as a key kept
	 * beside the data, `{ files: ['secret.key'] }`.
	 */
	async backup(path: string, options: BackupOptions = {}): Promise<void> {
		const part = `${path}.${randomBytes(4).toString('hex')}.part`
		try {
			await this.#link.run('read', async connection => {
				const file = await openFile(part, 'w')
				try {
					const stream = await connection.session.open(
						methods['server.backup'],
						Backup.encode({ files: options.files === undefined ? undefined : [...options.files] }),
						true,
					)
					await stream.next() // the RESPONSE that heads the zip
					for (;;) {
						const event = await stream.next()
						stream.consumed(event.body.length)
						await file.write(event.body)
						if (event.end) {
							break
						}
					}
					await file.sync()
				} finally {
					await file.close()
				}
			})
			await rename(part, path)
		} catch (err) {
			await rm(part, { force: true })
			throw err
		}
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
	checkDirectory(dir)
	const binary = () => findBinary(runtime, options.binary)
	const idle = options.idle === undefined ? undefined : ms(options.idle)
	if (options.clock !== undefined && options.private !== true) {
		throw new InvalidError(
			"a clock is a private server's: a shared sidecar runs on the system's time",
		)
	}
	const clock = options.clock === undefined ? undefined : new Date(unixMs(options.clock))
	const link = new Link(
		options.embedded === true
			? embedded(runtime, dir, () => findLibrary(options.library))
			: options.private === true
				? privateChild(runtime, dir, binary, clock)
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

// two letters or more before a colon: an address, never a Windows drive
const address = /^([a-z][a-z0-9+.-]+):/i

/**
 * Refuses an address where a directory belongs: open('tcp://…') would make a
 * folder of that name and start a server in it, rather than reach the one meant.
 *
 *     open('tcp://db.internal:7070')       → use connect
 *     open('pipe:tinystore-d761f24b7e59')  → open the directory it serves
 *     open('C:/data')                      → a directory
 */
function checkDirectory(dir: string): void {
	const scheme = address.exec(dir)?.[1]?.toLowerCase()
	if (scheme === 'tcp' || scheme === 'tls') {
		throw new InvalidError(
			`open takes a store's directory, and ${dir} is a server's address: connect('${dir}', { token }) reaches it`,
		)
	}
	if (scheme !== undefined) {
		throw new InvalidError(
			`open takes a store's directory, and ${dir} is an address: open the directory its server serves, and it is found through SERVE`,
		)
	}
}

function findBinary(runtime: Runtime, given: string | undefined): string {
	const found =
		given ?? process.env.TINYSTORE_BIN ?? runtime.packagedBinary() ?? runtime.which('tinystore')
	if (found === undefined || found === '') {
		throw new ClosedError(
			`no tinystore binary: @tinyshed/tinystore-${process.platform}-${process.arch} is not installed; ` +
				'install it, put tinystore on PATH, or set TINYSTORE_BIN',
		)
	}
	return found
}

/** The core's library an embedded store loads. */
function findLibrary(library: string | undefined): string {
	const found = library ?? process.env.TINYSTORE_LIBRARY
	if (found === undefined || found === '') {
		throw new InvalidError(
			'an embedded store needs the core library: pass library or set TINYSTORE_LIBRARY',
		)
	}
	return found
}
