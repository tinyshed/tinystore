// The application's current state in kv.db, as Go's kv package keeps it:
// buckets of one value type by key, keys in branches, expiry by the store's
// clock, versions that never repeat. A handle opens with its first call.

import { Config, type ConfigLayer, type ConfigValue } from './config.ts'
import type { Connection, Link } from './connection.ts'
import { ConflictError, CorruptError, InvalidError } from './errors.ts'
import { checkName, handleOn, ownerText, type Page, type Settled, settle } from './handles.ts'
import { Limiter, type LimiterOptions, limiterOpen, type Rate } from './limiter.ts'
import { Once, type OnceOptions } from './once.ts'
import { Quota, quotaOpen } from './quota.ts'
import { check, isSchema, type StandardSchemaV1 } from './schema.ts'
import { batchBelongsTo, type Database, databaseBelongsTo, type SqlBatch } from './sql.ts'
import { type Duration, dateOf, ms, type Time, unixMs } from './time.ts'
import { type Key, type Raw, textOf } from './wire/codec.ts'
import { KvBucket, KvCall, KvCalls, KvEntry, KvPage, KvResults, methods } from './wire/messages.ts'

/**
 * How a bucket keeps its values in a row, as Go's codecFor keeps a type's,
 * so that Go, Python and this SDK read each other's buckets:
 *
 *     'json'     JSON text, the default         a Go struct, a Python dataclass
 *     'string'   its UTF-8 bytes                string, str
 *     'bytes'    the bytes                       []byte, bytes
 *     'int'      an integer, a number exactly    int64, int
 *     'bigint'   an integer                      int64, int
 *     'float'    its eight bytes, big-endian     float64, float
 *     'bool'     1 or 0                          bool, bool
 *     'none'     nothing: a bucket of them is a set    struct{}, None
 */
export type Kind = 'json' | 'string' | 'bytes' | 'int' | 'bigint' | 'float' | 'bool' | 'none'

interface KindValue {
	json: unknown
	string: string
	bytes: Uint8Array
	int: number
	bigint: bigint
	float: number
	bool: boolean
	none: null
}

export interface BucketOptions {
	/**
	 * keeps the bucket in a SQL database's file instead of kv.db, so that a
	 * batch of the database writes its keys with its rows: `bucket.withTx(tx)`
	 */
	in?: Database
	/** the expiry a key gets when it is written without its own */
	defaultTtl?: Duration
	/** keeps a key this long from its last read; beside defaultTtl it is refused */
	sliding?: Duration
}

export interface CounterOptions {
	defaultTtl?: Duration
	/**
	 * keeps changes in the server's memory between flushes this far apart, so
	 * a crash may lose them: attempts and rates, not money
	 */
	loseAtMost?: Duration
}

export interface WriteOptions {
	ttl?: Duration
	expireAt?: Time
	/** writes only while the key is live at this version: ConflictError otherwise */
	ifVersion?: string
}

export interface Entry<V> {
	value: V
	/** compared only for equality: give it back to ifVersion */
	version: string
	/** when the key expires; undefined for one that never does */
	expires: Date | undefined
}

export interface Scanned<V> extends Entry<V> {
	/** the key's text; bytes when it is not UTF-8 */
	key: string | Uint8Array
}

/** Encodes a bucket's values into what a row keeps, and back. */
export interface Values<V> {
	encode(value: V): Raw | Promise<Raw>
	decode(raw: Raw): V | Promise<V>
}

export class Kv {
	readonly #link: Link
	readonly #configs = new Set<Config<object>>()

	constructor(link: Link) {
		this.#link = link
	}

	/**
	 * The config name, shaped and typed as its defaults, then each layer over
	 * the one before: a file's values, `fromEnv`, and what update kept over
	 * them all. It resolves once the server's state is read, and follows every
	 * change from then on, whoever makes it, until the store closes.
	 *
	 * ```ts
	 * const cfg = await store.kv.config('app', {
	 *   port: 8080,
	 *   db: { url: secret('DATABASE_URL'), pool: 10 },
	 * }, file, fromEnv('APP'))
	 * cfg.value.port                    // APP_PORT=3000 makes it 3000
	 * await cfg.update({ port: 4000 })  // kept: 4000 after a restart too
	 * ```
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

	/** A limiter of requests by key: `store.kv.limiter('api', { rate: '100/s', burst: 20 })`. */
	limiter(name: string, options: LimiterOptions): Limiter {
		return new Limiter(this.#link, name, limiterOpen(name, options), [])
	}

	/**
	 * A quota of uses by key, its windows by name, each counted from a key's
	 * first use and all of them together or none:
	 * `store.kv.quota('ai', { session: '100/5h', weekly: '300/7d' })`.
	 */
	quota<W extends string>(name: string, windows: Record<W, Rate>): Quota<W> {
		return new Quota(this.#link, name, quotaOpen(name, windows), [])
	}

	/**
	 * The answers a function gives once a key, JSON values of type T kept a
	 * day unless defaultTtl says: `store.kv.once<Receipt>('charges')`.
	 */
	once<T = unknown>(name: string, options?: OnceOptions): Once<T>
	/** Answers each read is checked by, zod's, valibot's or another Standard Schema. */
	once<S extends StandardSchemaV1>(
		name: string,
		schema: S,
		options?: OnceOptions,
	): Once<StandardSchemaV1.InferOutput<S>>
	once(name: string, of?: StandardSchemaV1 | OnceOptions, options?: OnceOptions): Once<unknown> {
		checkName(name, 'once')
		let values: Values<unknown> = jsonValues
		if (isSchema(of)) {
			values = schemaValues(of)
		} else if (of !== undefined) {
			options = of
		}
		const open = KvBucket.encode({
			name,
			once: true,
			defaultTtl: options?.defaultTtl === undefined ? undefined : ms(options.defaultTtl),
		})
		return new Once(this.#link, name, open, values, [])
	}

	/** Stops following configs, as the store does when it closes. */
	stop(): void {
		for (const config of this.#configs) {
			config.stop()
		}
	}

	/** A bucket of JSON values of type T. */
	bucket<T = unknown>(name: string, options?: BucketOptions): Bucket<T>
	/** A bucket of values kept as their kind says. */
	bucket<K extends Kind>(name: string, kind: K, options?: BucketOptions): Bucket<KindValue[K]>
	/** A bucket of JSON values each read is checked by, zod's, valibot's or another Standard Schema. */
	bucket<S extends StandardSchemaV1>(
		name: string,
		schema: S,
		options?: BucketOptions,
	): Bucket<StandardSchemaV1.InferOutput<S>>
	bucket(
		name: string,
		of?: Kind | StandardSchemaV1 | BucketOptions,
		options?: BucketOptions,
	): Bucket<unknown> {
		checkName(name, 'bucket')
		let values: Values<unknown> = jsonValues
		if (typeof of === 'string') {
			values = valuesOf(of)
		} else if (isSchema(of)) {
			values = schemaValues(of)
		} else if (of !== undefined) {
			options = of
		}
		if (options?.defaultTtl !== undefined && options.sliding !== undefined) {
			throw new InvalidError(`bucket ${name}: sliding and defaultTtl are one or the other`)
		}
		if (options?.in !== undefined && !databaseBelongsTo(options.in, this.#link)) {
			throw new InvalidError(`bucket ${name}: its SQL database belongs to another store`)
		}
		const open = KvBucket.encode({
			name,
			defaultTtl: options?.defaultTtl === undefined ? undefined : ms(options.defaultTtl),
			sliding: options?.sliding === undefined ? undefined : ms(options.sliding),
			in: options?.in?.name,
		})
		return new Bucket(this.#link, name, open, values, [], options?.in)
	}

	/** Counters, an int64 a key read as a number, 0 when absent. */
	counters(name: string, options?: CounterOptions): Counters<number>
	/** Counters read as bigints, for values past 2^53. */
	counters(name: string, kind: 'bigint', options?: CounterOptions): Counters<bigint>
	counters(
		name: string,
		of?: 'int' | 'bigint' | CounterOptions,
		options?: CounterOptions,
	): Counters<number | bigint> {
		checkName(name, 'counters')
		const big = of === 'bigint'
		if (typeof of === 'object') {
			options = of
		}
		const open = KvBucket.encode({
			name,
			counters: true,
			defaultTtl: options?.defaultTtl === undefined ? undefined : ms(options.defaultTtl),
			loseAtMost: options?.loseAtMost === undefined ? undefined : ms(options.loseAtMost),
		})
		return new Counters(this.#link, name, open, big, [])
	}

	/**
	 * Runs the calls fn asks for in one transaction, all or none: the calls of
	 * buckets it takes through withTx, whose promises settle once the batch
	 * has. fn returns before anything is sent, since no transaction is held
	 * across the network: a call cannot wait for another's answer inside it.
	 * What fn returns comes back answered, an array's promises each:
	 *
	 *     const [code] = await store.kv.batch(tx => [codes.withTx(tx).take(k), used.withTx(tx).set(k, 1)])
	 */
	batch<const T = void>(fn: (tx: Batch) => T): Promise<Settled<T>> {
		return runCalls(this.#link, 'batch', fn)
	}

	/**
	 * Reads the gets and hases fn asks for from one snapshot, and gives back
	 * what fn returns, answered as batch answers it:
	 *
	 *     const [profile, prefs] = await store.kv.view(tx => [a.withTx(tx).get(id), b.withTx(tx).get(id)])
	 */
	view<const T = void>(fn: (tx: Batch) => T): Promise<Settled<T>> {
		return runCalls(this.#link, 'view', fn)
	}

	/**
	 * Runs fn as one transaction that reads before it decides what to write,
	 * as Go's Tx does, without holding the writer across the network: a read
	 * goes to the server at once, a write waits, and when fn returns the writes
	 * commit in one batch that first checks every key fn read is still as it
	 * read it. When another write changed one meanwhile, fn runs again, five
	 * times at most before ConflictError, so it does nothing else that must
	 * happen once. It gives back what fn returns.
	 *
	 *     const userId = await store.kv.tx(async tx => {
	 *       const userId = await codes.withTx(tx).take(digest(code))
	 *       if (userId === undefined) throw new InvalidCode()
	 *       sessions.withTx(tx).of(userId).set(digest(token), { device })
	 *       return userId
	 *     })
	 */
	async tx<T>(fn: (tx: Tx) => T | Promise<T>): Promise<T> {
		for (let run = 1; ; run++) {
			const tx = new Tx(this.#link)
			const returned = await fn(tx)
			try {
				await tx.commit()
				return returned
			} catch (err) {
				if (run >= txRuns || !tx.stale(err)) {
					throw err
				}
			}
		}
	}
}

/** How many times tx runs its function before a key it read that keeps changing is ConflictError. */
const txRuns = 5

/** The calls a batch or a view gathered, in their order. */
export class Batch {
	readonly kind: 'batch' | 'view'
	readonly calls: Recorded[] = []
	#done = false
	readonly #made = new WeakSet<Promise<unknown>>()

	constructor(kind: 'batch' | 'view') {
		this.kind = kind
	}

	/** Whether the promise is one of this batch's calls', rather than an async function's. */
	made(promise: Promise<unknown>): boolean {
		return this.#made.has(promise)
	}

	record<T>(
		call: Omit<Recorded, 'settle'>,
		settle: (entry: Entry<Raw> & { found: boolean }) => Promise<T>,
	) {
		if (this.#done) {
			throw new InvalidError(
				`a ${this.kind} takes its calls while its function runs, before it is sent`,
			)
		}
		if (this.kind === 'view' && call.method !== 'kv.get' && call.method !== 'kv.has') {
			throw new InvalidError(`${call.method} in a view, which reads`)
		}
		let resolve!: (v: T) => void
		let reject!: (e: unknown) => void
		const settled = new Promise<T>((res, rej) => {
			resolve = res
			reject = rej
		})
		// a caller need not await a call it only wanted done
		settled.catch(() => {})
		this.#made.add(settled)
		this.calls.push({
			...call,
			settle: async entry => {
				try {
					resolve(await settle(entry))
				} catch (err) {
					reject(err)
				}
			},
			fail: reject,
		})
		return settled
	}

	close(): void {
		this.#done = true
	}
}

interface Recorded {
	method: keyof typeof methods
	open: Uint8Array
	call: Parameters<typeof KvCall.encode>[0]
	settle: (entry: Entry<Raw> & { found: boolean }) => Promise<void>
	fail?: (err: unknown) => void
}

/** What a bucket's calls inside a batch, a view or a tx go through. */
interface Recorder {
	record<T>(
		call: Omit<Recorded, 'settle'>,
		settle: (entry: Entry<Raw> & { found: boolean }) => Promise<T>,
	): Promise<T>
}

type Found = Entry<Raw> & { found: boolean }

/** A key a tx read: the call that read it, and what it found, which its commit checks. */
interface Read {
	call: Omit<Recorded, 'settle'>
	version: Uint8Array | undefined
	entry: Found
}

const absent: Found = { found: false, value: null, version: '', expires: undefined }

/**
 * A transaction of store.kv.tx: a read goes to the server at once and is
 * remembered with its version, so that a key read again answers the same; a
 * write waits for the commit, and a key read after it answers what it wrote.
 */
export class Tx implements Recorder {
	readonly #link: Link
	readonly #reads = new Map<string, Promise<Read>>()
	readonly #writes = new Map<string, { call: Omit<Recorded, 'settle'>; entry: Found }>()
	#checks = 0
	#done = false

	constructor(link: Link) {
		this.#link = link
	}

	record<T>(call: Omit<Recorded, 'settle'>, settle: (entry: Found) => Promise<T>): Promise<T> {
		if (this.#done) {
			throw new InvalidError('a tx takes its calls while its function runs, before it commits')
		}
		const at = placeOf(call)
		switch (call.method) {
			case 'kv.get':
			case 'kv.has':
				return this.#read(at, call).then(settle)
			case 'kv.take': {
				const read = this.#read(at, call)
				this.#writes.set(at, { call: { ...call, method: 'kv.delete' }, entry: absent })
				return read.then(settle)
			}
			case 'kv.set': {
				// a set's value is what the bucket encoded, which a read decodes
				const value = (call.call.value ?? null) as Raw
				const entry: Found = { found: true, value, version: '', expires: undefined }
				this.#writes.set(at, { call, entry })
				return settle(entry)
			}
			case 'kv.delete':
				this.#writes.set(at, { call, entry: absent })
				return settle(absent)
		}
		throw new InvalidError(`${call.method} in a tx`)
	}

	#read(at: string, call: Omit<Recorded, 'settle'>): Promise<Found> {
		const written = this.#writes.get(at)
		if (written !== undefined) {
			return Promise.resolve(written.entry)
		}
		let reading = this.#reads.get(at)
		if (reading === undefined) {
			reading = this.#fetch(call)
			// a read fn never awaited still fails the commit, which awaits it
			reading.catch(() => {})
			this.#reads.set(at, reading)
		}
		return reading.then(read => read.entry)
	}

	async #fetch(call: Omit<Recorded, 'settle'>): Promise<Read> {
		const ask = { owners: call.call.owners, key: call.call.key }
		const e = await this.#link.run('read', async connection => {
			const handle = await handleOn(connection, methods['kv.open'], call.open)
			const body = await connection.session.call(
				methods['kv.get'],
				KvCall.encode({ ...ask, handle }),
			)
			return KvEntry.decode(body)
		})
		return {
			call: { method: 'kv.get', open: call.open, call: ask },
			version: e.version,
			entry: entryOfWire(e),
		}
	}

	/**
	 * Sends what fn wrote in one batch, after a check of each key it read: a
	 * key it found at the version it found, a key it found absent still
	 * absent. A tx that only read checks its reads from one snapshot.
	 */
	async commit(): Promise<void> {
		this.#done = true
		const calls: Omit<Recorded, 'settle'>[] = []
		for (const reading of this.#reads.values()) {
			const read = await reading
			const still = read.entry.found ? { ifVersion: read.version } : { ifAbsent: true }
			calls.push({ ...read.call, call: { ...read.call.call, ...still } })
		}
		this.#checks = calls.length
		for (const write of this.#writes.values()) {
			calls.push(write.call)
		}
		if (calls.length === 0) {
			return
		}
		const writes = this.#writes.size > 0
		await this.#link.run(writes ? 'write' : 'read', async connection => {
			const encoded = []
			for (const call of calls) {
				const handle = await handleOn(connection, methods['kv.open'], call.open)
				encoded.push({ method: methods[call.method], ...call.call, handle })
			}
			const method = writes ? methods['kv.batch'] : methods['kv.view']
			await connection.session.call(method, KvCalls.encode({ calls: encoded }))
		})
	}

	/** Whether the commit failed for a key fn read that changed since, which another run reads anew. */
	stale(err: unknown): boolean {
		return err instanceof ConflictError && Number(err.what.call) < this.#checks
	}
}

/**
 * A key's place as one text: its bucket's open and its owners included, an
 * integer its decimal spelling, as the store keeps it.
 */
function placeOf(call: Omit<Recorded, 'settle'>): string {
	const part = (p: Key | undefined) =>
		p instanceof Uint8Array ? `b${Buffer.from(p).toString('base64')}` : `t${String(p ?? '')}`
	const owners = call.call.owners ?? []
	return [Buffer.from(call.open).toString('base64'), ...owners.map(part), part(call.call.key)].join(
		'\u0000',
	)
}

async function runCalls<T>(
	link: Link,
	kind: 'batch' | 'view',
	fn: (tx: Batch) => T,
): Promise<Settled<T>> {
	const tx = new Batch(kind)
	const returned = fn(tx)
	tx.close()
	if (returned instanceof Promise && !tx.made(returned)) {
		throw new InvalidError(
			`a ${kind}'s function returns before anything is sent; it cannot await inside`,
		)
	}
	if (tx.calls.length > 0) {
		await sendCalls(link, kind, tx)
	}
	return settle(returned)
}

async function sendCalls(link: Link, kind: 'batch' | 'view', tx: Batch): Promise<void> {
	try {
		const results = await link.run(kind === 'view' ? 'read' : 'write', async connection => {
			const calls = []
			for (const call of tx.calls) {
				const handle = await handleOn(connection, methods['kv.open'], call.open)
				calls.push({ method: methods[call.method], ...call.call, handle })
			}
			const body = await connection.session.call(methods[`kv.${kind}`], KvCalls.encode({ calls }))
			return KvResults.decode(body).entries ?? []
		})
		await Promise.all(tx.calls.map((call, i) => call.settle(entryOfWire(results[i] ?? {}))))
	} catch (err) {
		for (const call of tx.calls) {
			call.fail?.(err)
		}
		throw err
	}
}

function entryOfWire(e: ReturnType<typeof KvEntry.decode>): Entry<Raw> & { found: boolean } {
	return {
		found: e.found ?? false,
		value: e.value ?? null,
		version: versionText(e.version),
		expires: dateOf(e.expires),
	}
}

/** A version's bytes as text, each byte a code unit, so that any bytes go back as they came. */
function versionText(version: Uint8Array | undefined): string {
	return version === undefined ? '' : String.fromCharCode(...version)
}

function versionBytes(version: string): Uint8Array {
	return Uint8Array.from(version, c => c.charCodeAt(0))
}

function writeFields(options: WriteOptions | undefined) {
	return {
		ttl: options?.ttl === undefined ? undefined : ms(options.ttl),
		expireAt: options?.expireAt === undefined ? undefined : unixMs(options.expireAt),
		ifVersion: options?.ifVersion === undefined ? undefined : versionBytes(options.ifVersion),
	}
}

/**
 * A bucket's values under one branch of owners, the root when there are
 * none: `sessions.of(user.id).get(token)`.
 */
export class Bucket<V> {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #values: Values<V>
	readonly #owners: (string | Uint8Array)[]
	readonly #in: Database | undefined

	constructor(
		link: Link,
		name: string,
		open: Uint8Array,
		values: Values<V>,
		owners: (string | Uint8Array)[],
		inDatabase?: Database,
	) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#values = values
		this.#owners = owners
		this.#in = inDatabase
	}

	/** The branch below this one that the owners name. */
	of(...owners: Key[]): Bucket<V> {
		return new Bucket(
			this.#link,
			this.name,
			this.#open,
			this.#values,
			[...this.#owners, ...owners.map(ownerText)],
			this.#in,
		)
	}

	/**
	 * The bucket's calls inside a batch or a view, whose promises settle with
	 * it, or inside a tx, whose reads answer at once and writes wait for it.
	 * Inside a batch of the SQL database the bucket was opened `in`, its
	 * set, delete and clear commit with the batch's rows or not at all:
	 *
	 *     await db.batch(tx => {
	 *       tx.exec`insert into users (id, email) values (${id}, ${email})`
	 *       sessions.withTx(tx).set(token, { user: id })
	 *     })
	 */
	withTx(tx: Batch | Tx | SqlBatch): BucketTx<V> {
		if (!isSqlBatch(tx)) {
			return new BucketTx(tx, this.#open, this.#values, this.#owners)
		}
		if (
			this.#in === undefined ||
			this.#in.name !== tx.database ||
			!batchBelongsTo(tx, this.#link)
		) {
			const lives = this.#in === undefined ? 'kv.db' : `sql ${this.#in.name}`
			throw new InvalidError(
				`the bucket ${this.name} lives in ${lives}, not in sql ${tx.database}: ` +
					"open it with { in: db } to write it in that database's batches",
			)
		}
		return new BucketTx(sqlWrites(tx), this.#open, this.#values, this.#owners)
	}

	// the bucket's handle, its database opened first when it lives in one, as
	// the server needs
	async #handle(connection: Connection): Promise<number> {
		await this.#in?.handle(connection)
		return handleOn(connection, methods['kv.open'], this.#open)
	}

	async #call(
		method: keyof typeof methods,
		fields: Omit<Parameters<typeof KvCall.encode>[0], 'handle' | 'owners'>,
		idempotence: 'read' | 'write',
	) {
		return this.#link.run(idempotence, async (connection: Connection) => {
			const handle = await this.#handle(connection)
			const body = KvCall.encode({
				handle,
				owners: this.#owners.length > 0 ? this.#owners : undefined,
				...fields,
			})
			return KvEntry.decode(await connection.session.call(methods[method], body))
		})
	}

	/** The key's value, undefined when it holds none. */
	async get(key: Key): Promise<V | undefined> {
		const entry = await this.getEntry(key)
		return entry?.value
	}

	/** The key's value with its version and expiry, undefined when it holds none. */
	async getEntry(key: Key): Promise<Entry<V> | undefined> {
		const e = await this.#call('kv.get', { key }, 'read')
		if (e.found !== true) {
			return undefined
		}
		return {
			value: await this.#values.decode(e.value ?? null),
			version: versionText(e.version),
			expires: dateOf(e.expires),
		}
	}

	async has(key: Key): Promise<boolean> {
		return (await this.#call('kv.has', { key }, 'read')).found === true
	}

	/** Writes the value; a live key keeps its expiry unless the options give one. */
	async set(key: Key, value: V, options?: WriteOptions): Promise<void> {
		await this.setEntry(key, value, options)
	}

	/** Writes the value and gives its new version and expiry. */
	async setEntry(key: Key, value: V, options?: WriteOptions): Promise<Entry<V>> {
		const raw = await this.#values.encode(value)
		const e = await this.#call('kv.set', { key, value: raw, ...writeFields(options) }, 'write')
		return { value, version: versionText(e.version), expires: dateOf(e.expires) }
	}

	/** Writes the value only where no live key is, and says whether it did. */
	async setIfAbsent(
		key: Key,
		value: V,
		options?: Omit<WriteOptions, 'ifVersion'>,
	): Promise<boolean> {
		return (await this.setEntryIfAbsent(key, value, options)).created
	}

	/**
	 * Writes the value only where no live key is: created says whether it did,
	 * and entry is the new key's, or the live one's that was there.
	 */
	async setEntryIfAbsent(
		key: Key,
		value: V,
		options?: Omit<WriteOptions, 'ifVersion'>,
	): Promise<{ created: boolean; entry: Entry<V> }> {
		const raw = await this.#values.encode(value)
		const e = await this.#call(
			'kv.set',
			{ key, value: raw, ifAbsent: true, ...writeFields(options) },
			'write',
		)
		const created = e.found === true
		return {
			created,
			entry: {
				value: created ? value : await this.#values.decode(e.value ?? null),
				version: versionText(e.version),
				expires: dateOf(e.expires),
			},
		}
	}

	async delete(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<void> {
		await this.#call('kv.delete', { key, ...writeFields(options) }, 'write')
	}

	/**
	 * Reads the value and deletes the key in one write: a one-time code read
	 * and burned. A value that no longer decodes has been taken all the same.
	 */
	async take(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<V | undefined> {
		const e = await this.#call('kv.take', { key, ...writeFields(options) }, 'write')
		return e.found === true ? this.#values.decode(e.value ?? null) : undefined
	}

	/** Gives a live key a new expiry, keeping its value and version; false when it holds none. */
	async touch(key: Key, options: WriteOptions): Promise<boolean> {
		return (await this.#call('kv.touch', { key, ...writeFields(options) }, 'write')).found === true
	}

	/** Removes this branch's keys and every branch under it, at once however many. */
	async clear(): Promise<void> {
		await this.#call('kv.clear', {}, 'write')
	}

	/** One page of this branch's own keys, in the byte order of their text. */
	async scan(options?: {
		after?: Key
		limit?: number
	}): Promise<Page<Scanned<V>, string | Uint8Array>> {
		const { items, page } = await this.#link.run('read', async connection => {
			const handle = await this.#handle(connection)
			const stream = await connection.session.open(
				methods['kv.scan'],
				KvCall.encode({
					handle,
					owners: this.#owners.length > 0 ? this.#owners : undefined,
					after: options?.after,
					limit: options?.limit,
				}),
				true,
			)
			const items: ReturnType<typeof KvEntry.decode>[] = []
			for (;;) {
				const event = await stream.next()
				if (event.kind === 'response') {
					continue
				}
				stream.consumed(event.body.length)
				if (event.end) {
					return { items, page: KvPage.decode(event.body) }
				}
				items.push(KvEntry.decode(event.body))
			}
		})
		const decoded: Scanned<V>[] = []
		for (const e of items) {
			decoded.push({
				key: e.key ?? '',
				value: await this.#values.decode(e.value ?? null),
				version: versionText(e.version),
				expires: dateOf(e.expires),
			})
		}
		return { items: decoded, next: page.more === true ? (page.after ?? '') : undefined }
	}

	/**
	 * Walks this branch's own keys a page at a time, holding no snapshot
	 * between pages: a key written during the walk may or may not be met.
	 */
	async *all(options?: { limit?: number }): AsyncGenerator<Scanned<V>> {
		let after: Key | undefined
		for (;;) {
			const page = await this.scan({ ...(after === undefined ? {} : { after }), ...options })
			yield* page.items
			if (page.next === undefined) {
				return
			}
			after = page.next
		}
	}
}

/** A bucket's calls inside a batch or a view. */
export class BucketTx<V> {
	readonly #tx: Recorder
	readonly #open: Uint8Array
	readonly #values: Values<V>
	readonly #owners: (string | Uint8Array)[]

	constructor(tx: Recorder, open: Uint8Array, values: Values<V>, owners: (string | Uint8Array)[]) {
		this.#tx = tx
		this.#open = open
		this.#values = values
		this.#owners = owners
	}

	of(...owners: Key[]): BucketTx<V> {
		return new BucketTx(this.#tx, this.#open, this.#values, [
			...this.#owners,
			...owners.map(ownerText),
		])
	}

	#record<T>(
		method: Recorded['method'],
		fields: Omit<Parameters<typeof KvCall.encode>[0], 'handle' | 'owners'>,
		settle: (e: Entry<Raw> & { found: boolean }) => Promise<T>,
	): Promise<T> {
		const owners = this.#owners.length > 0 ? this.#owners : undefined
		return this.#tx.record({ method, open: this.#open, call: { owners, ...fields } }, settle)
	}

	get(key: Key): Promise<V | undefined> {
		return this.#record('kv.get', { key }, async e =>
			e.found ? this.#values.decode(e.value) : undefined,
		)
	}

	has(key: Key): Promise<boolean> {
		return this.#record('kv.has', { key }, async e => e.found)
	}

	set(key: Key, value: V, options?: WriteOptions): Promise<void> {
		return this.#record(
			'kv.set',
			{ key, value: this.#encodeNow(value), ...writeFields(options) },
			async () => {},
		)
	}

	delete(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<void> {
		return this.#record('kv.delete', { key, ...writeFields(options) }, async () => {})
	}

	/** Removes every key of this branch and of the branches under it. */
	clear(): Promise<void> {
		return this.#record('kv.clear', {}, async () => {})
	}

	take(key: Key): Promise<V | undefined> {
		return this.#record('kv.take', { key }, async e =>
			e.found ? this.#values.decode(e.value) : undefined,
		)
	}

	// a batch is sent once its function returns, so a value is encoded as it is given
	#encodeNow(value: V): Raw {
		const raw = this.#values.encode(value)
		if (raw instanceof Promise) {
			throw new InvalidError('a bucket whose values encode asynchronously takes no batch')
		}
		return raw
	}
}

function isSqlBatch(tx: Batch | Tx | SqlBatch): tx is SqlBatch {
	return typeof (tx as SqlBatch).writeKey === 'function'
}

// a SQL batch as a bucket's calls go through it: set, delete and clear, which
// commit with the batch's rows; a read has no place in a batch of writes
function sqlWrites(tx: SqlBatch): Recorder {
	return {
		record<T>(call: Omit<Recorded, 'settle'>): Promise<T> {
			if (call.method !== 'kv.set' && call.method !== 'kv.delete' && call.method !== 'kv.clear') {
				throw new InvalidError(
					'a SQL batch writes keys with set, delete and clear; read them outside it',
				)
			}
			// a write's promise settles with nothing, so the batch's own promise is its answer
			return tx.writeKey(call.open, { method: methods[call.method], ...call.call }) as Promise<T>
		},
	}
}

/**
 * Counters under one branch: an int64 a key, 0 when absent, which add and
 * max change in one write; a sum past the int64 range is refused.
 */
export class Counters<N extends number | bigint> {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #big: boolean
	readonly #owners: (string | Uint8Array)[]

	constructor(
		link: Link,
		name: string,
		open: Uint8Array,
		big: boolean,
		owners: (string | Uint8Array)[],
	) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#big = big
		this.#owners = owners
	}

	of(...owners: Key[]): Counters<N> {
		return new Counters(this.#link, this.name, this.#open, this.#big, [
			...this.#owners,
			...owners.map(ownerText),
		])
	}

	async #call(method: keyof typeof methods, key: Key | undefined, n?: number | bigint): Promise<N> {
		const e = await this.#link.run(method === 'kv.get' ? 'read' : 'write', async connection => {
			const handle = await handleOn(connection, methods['kv.open'], this.#open)
			const body = KvCall.encode({
				handle,
				owners: this.#owners.length > 0 ? this.#owners : undefined,
				key,
				n,
			})
			return KvEntry.decode(await connection.session.call(methods[method], body))
		})
		const value = typeof e.value === 'bigint' ? e.value : 0n
		return (this.#big ? value : numberOf(value, this.name)) as N
	}

	get(key: Key): Promise<N> {
		return this.#call('kv.get', key)
	}

	/** Adds n, 1 unless given, and gives the new value. */
	add(key: Key, n: number | bigint = 1): Promise<N> {
		return this.#call('kv.add', key, n)
	}

	/** Keeps the larger of the counter and n, and gives it. */
	max(key: Key, n: number | bigint): Promise<N> {
		return this.#call('kv.max', key, n)
	}

	async delete(key: Key): Promise<void> {
		await this.#call('kv.delete', key)
	}

	async clear(): Promise<void> {
		await this.#call('kv.clear', undefined)
	}
}

function numberOf(n: bigint, bucket: string): number {
	if (n > BigInt(Number.MAX_SAFE_INTEGER) || n < BigInt(Number.MIN_SAFE_INTEGER)) {
		throw new CorruptError(
			`${n} does not fit a number exactly: open counters ${bucket} as 'bigint'`,
			{ bucket },
		)
	}
	return Number(n)
}

const utf8 = new TextEncoder()

const jsonValues: Values<unknown> = {
	encode: value => {
		let text: string | undefined
		try {
			text = JSON.stringify(value)
		} catch (err) {
			throw new InvalidError(`a value JSON cannot write: ${(err as Error).message}`)
		}
		if (text === undefined) {
			throw new InvalidError('a value JSON cannot write: undefined, a function or a symbol')
		}
		return utf8.encode(text)
	},
	decode: raw => {
		if (!(raw instanceof Uint8Array)) {
			throw new CorruptError(`a stored value is ${describe(raw)} where JSON was written`)
		}
		const text = textOf(raw)
		try {
			return JSON.parse(text ?? '')
		} catch (err) {
			throw new CorruptError(`a stored value is not JSON any more: ${(err as Error).message}`)
		}
	},
}

function schemaValues<S extends StandardSchemaV1>(schema: S): Values<unknown> {
	return {
		encode: jsonValues.encode,
		decode: async raw => {
			const checked = await check(schema, await jsonValues.decode(raw))
			if ('issues' in checked) {
				throw new CorruptError(
					`a stored value does not meet the bucket's schema: ${checked.issues}`,
				)
			}
			return checked.value
		},
	}
}

function valuesOf(kind: Kind): Values<unknown> {
	switch (kind) {
		case 'json':
			return jsonValues
		case 'string':
			return {
				encode: v => {
					if (typeof v !== 'string') {
						throw new InvalidError(`a ${typeof v} in a bucket of strings`)
					}
					return utf8.encode(v)
				},
				decode: raw => {
					const text = raw === null ? '' : raw instanceof Uint8Array ? textOf(raw) : undefined
					if (text === undefined) {
						throw new CorruptError(`a stored value is ${describe(raw)}, which is no UTF-8 string`)
					}
					return text
				},
			}
		case 'bytes':
			return {
				encode: v => {
					if (!(v instanceof Uint8Array)) {
						throw new InvalidError(`a ${typeof v} in a bucket of bytes`)
					}
					return v
				},
				decode: raw => {
					if (raw !== null && !(raw instanceof Uint8Array)) {
						throw new CorruptError(`a stored value is ${describe(raw)} where bytes were written`)
					}
					return raw ?? new Uint8Array(0)
				},
			}
		case 'int':
			return {
				encode: v => {
					if (typeof v !== 'number' || !Number.isSafeInteger(v)) {
						throw new InvalidError(
							`${String(v)} is not an integer a number holds exactly; open the bucket as 'bigint'`,
						)
					}
					return BigInt(v)
				},
				decode: raw => {
					if (typeof raw !== 'bigint') {
						throw new CorruptError(
							`a stored value is ${describe(raw)} where an integer was written`,
						)
					}
					if (raw > BigInt(Number.MAX_SAFE_INTEGER) || raw < BigInt(Number.MIN_SAFE_INTEGER)) {
						throw new CorruptError(
							`${raw} does not fit a number exactly; open the bucket as 'bigint'`,
						)
					}
					return Number(raw)
				},
			}
		case 'bigint':
			return {
				encode: v => {
					if (typeof v !== 'bigint' && !(typeof v === 'number' && Number.isSafeInteger(v))) {
						throw new InvalidError(`${String(v)} is not an integer`)
					}
					return BigInt(v)
				},
				decode: raw => {
					if (typeof raw !== 'bigint') {
						throw new CorruptError(
							`a stored value is ${describe(raw)} where an integer was written`,
						)
					}
					return raw
				},
			}
		case 'float':
			return {
				encode: v => {
					if (typeof v !== 'number') {
						throw new InvalidError(`a ${typeof v} in a bucket of floats`)
					}
					const bytes = new Uint8Array(8)
					new DataView(bytes.buffer).setFloat64(0, v)
					return bytes
				},
				decode: raw => {
					if (!(raw instanceof Uint8Array) || raw.length !== 8) {
						throw new CorruptError(
							`a stored value is ${describe(raw)} where a float's eight bytes were written`,
						)
					}
					return new DataView(raw.buffer, raw.byteOffset, 8).getFloat64(0)
				},
			}
		case 'bool':
			return {
				encode: v => {
					if (typeof v !== 'boolean') {
						throw new InvalidError(`a ${typeof v} in a bucket of booleans`)
					}
					return v ? 1n : 0n
				},
				decode: raw => {
					if (typeof raw !== 'bigint') {
						throw new CorruptError(`a stored value is ${describe(raw)} where a boolean was written`)
					}
					return raw !== 0n
				},
			}
		case 'none':
			return {
				encode: v => {
					if (v !== null) {
						throw new InvalidError('a bucket of none keeps no value: set null')
					}
					return null
				},
				decode: raw => {
					if (raw !== null) {
						throw new CorruptError(`a stored value is ${describe(raw)} in a bucket that keeps none`)
					}
					return null
				},
			}
	}
}

function describe(raw: Raw): string {
	if (raw === null) {
		return 'nothing'
	}
	return typeof raw === 'bigint' ? 'an integer' : `${raw.length} bytes`
}
