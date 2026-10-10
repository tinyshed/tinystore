import { randomBytes } from 'node:crypto'
import { open as openFile, rename, rm } from 'node:fs/promises'

import { Blobs } from './blobs.ts'
import { Clock } from './clock.ts'
import { Config, type ConfigLayer, type ConfigValue } from './config.ts'
import {
	asking,
	checkDurability,
	type Durability,
	embedded,
	Link,
	privateChild,
	remote,
	sidecar,
} from './connection.ts'
import { ClosedError, InvalidError } from './errors.ts'
import {
	openQueue,
	openSchedule,
	type Queue,
	type QueueOptions,
	type Run,
	type Schedule,
	type ScheduleOptions,
	type ScheduleWhen,
	type Worker,
} from './jobs.ts'
import {
	Bucket,
	type BucketOptions,
	bucketOpen,
	Counters,
	type CountersOptions,
	countersOpen,
	type Values,
	valuesOf,
} from './kv.ts'
import {
	Quota,
	quotaOpen,
	type Rate,
	RateLimit,
	type RateLimitOptions,
	rateLimitOpen,
} from './limits.ts'
import { Metrics } from './metrics.ts'
import { Once, type OnceOptions, onceOpen } from './once.ts'
import { Records } from './records.ts'
import { bunRuntime } from './runtime/bun.ts'
import { nodeRuntime } from './runtime/node.ts'
import type { Runtime, TlsOptions } from './runtime.ts'
import type { StandardSchemaV1 } from './schema.ts'
import { type Database, type DatabaseOptions, openDatabase } from './sql.ts'
import { type Duration, ms, type Time, unixMs } from './time.ts'
import { runTx, type Tx } from './tx.ts'
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
	/**
	 * How far a commit of the store's files goes before it returns: `'full'`
	 * unless given. `'os'` does not wait for the disk, so a write returns in
	 * microseconds, survives this program's crash, and may be lost with the
	 * last others to a power loss. It is the store's, set by whoever opens it
	 * first: a sidecar found running with another is refused. A database may
	 * say its own, `store.database('audit', { durability: 'full' })`.
	 */
	durability?: Durability
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
	readonly blobs: Blobs
	readonly records: Records
	readonly metrics: Metrics
	/** The test's clock of a private store opened with `clock`; on any other, its calls are InvalidError. */
	readonly clock: Clock
	readonly #link: Link
	readonly #configs = new Set<Config<object>>()
	readonly #workers = new Set<Worker>()

	constructor(link: Link) {
		this.#link = link
		this.clock = new Clock(link)
		this.blobs = new Blobs(link)
		this.records = new Records(link)
		this.metrics = new Metrics(link)
	}

	/**
	 * A bucket of values by key, JSON unless `type` says otherwise:
	 *
	 *     const sessions = store.bucket<Session>('sessions', { idle: '30d' })
	 *     const codes = store.bucket<number>('login-codes', { ttl: '15m', type: 'int' })
	 *     const seen = store.bucket('stripe-events', { ttl: '7d' })   // keys alone: a set
	 */
	bucket<T = unknown>(name: string, options?: BucketOptions): Bucket<T> {
		const values = valuesOf(options?.type) as Values<T>
		return new Bucket<T>(this.#link, name, bucketOpen(name, options), values, [])
	}

	/** Numbers by key that only add up: `store.counters('login-attempts', { ttl: '15m' })`. */
	counters(name: string, options?: CountersOptions): Counters {
		return new Counters(this.#link, name, countersOpen(name, options), [])
	}

	/** A smooth rate of requests a key: `store.rateLimit('api', { rate: '100/s', burst: 20 })`. */
	rateLimit(name: string, options: RateLimitOptions): RateLimit {
		return new RateLimit(this.#link, name, rateLimitOpen(name, options), [])
	}

	/**
	 * Uses by key in named windows, each counted from a key's first use, all
	 * of them or none: `store.quota('ai', { daily: '100/1d', weekly: '300/7d' })`.
	 */
	quota<W extends string>(name: string, windows: Record<W, Rate>): Quota<W> {
		return new Quota<W>(this.#link, name, quotaOpen(name, windows), [])
	}

	/** A function run once a key, its JSON answer kept a day unless `keep` says: `store.once<Receipt>('charges')`. */
	once<T = unknown>(name: string, options?: OnceOptions): Once<T> {
		return new Once<T>(this.#link, name, onceOpen(name, options), [])
	}

	/**
	 * A queue of jobs of one type, JSON, run in the order of their time; a
	 * Standard Schema, zod's or valibot's, gives it its type and checks each
	 * value before a handler gets it:
	 *
	 *     const emails = store.queue<Email>('emails', { attempts: 20, backoff: { initial: '5s', max: '30m' } })
	 *     const refreshes = store.queue('refreshes', { schema: Refresh, concurrency: { total: 8, group: 2 } })
	 */
	queue<S extends StandardSchemaV1>(
		name: string,
		options: QueueOptions & { schema: S },
	): Queue<StandardSchemaV1.InferOutput<S>>
	queue<T = unknown>(name: string, options?: QueueOptions): Queue<T>
	queue(name: string, options: QueueOptions & { schema?: StandardSchemaV1 } = {}): Queue<unknown> {
		return openQueue(this.#link, this.#workers, name, options)
	}

	/**
	 * A repeat the code owns: one repeating job under name, and handler run at
	 * each of its times, started at once as a queue's work is. The code's
	 * repeat replaces the one kept each time the program opens it.
	 *
	 *     store.schedule('cleanup', { cron: '10 3 * * *', timeZone: 'Europe/Berlin' }, async run => {
	 *       await purgeDeletedBefore(daysBefore(run.at, 30))
	 *     })
	 */
	schedule(
		name: string,
		when: ScheduleWhen,
		handler: (run: Run) => unknown,
		options?: ScheduleOptions,
	): Schedule {
		return openSchedule(this.#link, this.#workers, name, when, handler, options)
	}

	/**
	 * Runs fn as one transaction over the handles it takes in with
	 * `tx.with(handle)`, and gives back what fn returns. Reads go at once;
	 * writes commit together when fn returns, after a check that nothing it
	 * read has changed, and fn runs again when something did, five times at
	 * most, so it does nothing else that must happen once.
	 *
	 *     await store.tx(async tx => {
	 *       const left = (await tx.with(stock).get(sku)) ?? 0
	 *       if (left < 1) throw new SoldOut()
	 *       await tx.with(stock).set(sku, left - 1)
	 *     })
	 */
	tx<T>(fn: (tx: Tx) => T | Promise<T>): Promise<T> {
		return runTx(this.#link, fn)
	}

	/**
	 * The config name, shaped and typed as its defaults, then each layer over
	 * the one before: a file's values, `fromEnv`, and what update kept over
	 * them all. It resolves once the store's state is read, and follows every
	 * change from then on, whoever makes it, until the store closes.
	 */
	async config<D extends object>(
		name: string,
		defaults: D,
		...layers: ConfigLayer<ConfigValue<D>>[]
	): Promise<Config<ConfigValue<D>>> {
		const config = new Config<ConfigValue<D>>(this.#link, name, defaults)
		await config.lay(layers)
		this.#configs.add(config as unknown as Config<object>)
		try {
			await config.start()
		} catch (err) {
			this.#configs.delete(config as unknown as Config<object>)
			throw err
		}
		return config
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
	 * The application's database name, sql/<name>.db, once its migrations
	 * are applied and checked: a migration that fails, or one changed after
	 * it was applied, is told here, at the start. Without migrations the file
	 * opens as it is, an empty one when there is none.
	 *
	 *     const db = await store.database('app', { migrations: `${import.meta.dir}/migrations` })
	 */
	database(name: string, options?: DatabaseOptions): Promise<Database> {
		return openDatabase(this.#link, this.#workers, name, options)
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
	 * Stops every worker, each once its handlers under way have answered,
	 * ingests the instruments' last values and hands over the loggers' lines,
	 * then closes the connection. A private child is waited for until it has
	 * exited, so that the directory is free once this returns; the directory's
	 * sidecar goes once it has been idle.
	 */
	async close(): Promise<void> {
		await Promise.all([...this.#workers].map(worker => worker.stop()))
		await this.metrics.stop()
		await this.records.stop()
		for (const config of this.#configs) {
			config.stop()
		}
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
	const durability = options.durability
	checkDurability(durability, 'open')
	const dial =
		options.embedded === true
			? embedded(runtime, dir, () => findLibrary(options.library), durability)
			: options.private === true
				? privateChild(runtime, dir, binary, clock, durability)
				: sidecar(runtime, dir, binary, { idle, durability })
	const link = new Link(asking(dial, dir, durability))
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
