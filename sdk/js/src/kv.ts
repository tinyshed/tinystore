// The application's current state in kv.db, as Go's kv package keeps it:
// buckets of one value type by key, keys in branches, expiry by the store's
// clock, versions that never repeat. A handle opens with its first call.

import type { Connection, Link } from './connection.ts'
import { CorruptError, InvalidError } from './errors.ts'
import { checkName, handleOn, type Page } from './handles.ts'
import { check, isSchema, type StandardSchemaV1 } from './schema.ts'
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
interface Values<V> {
	encode(value: V): Raw | Promise<Raw>
	decode(raw: Raw): V | Promise<V>
}

export class Kv {
	readonly #link: Link

	constructor(link: Link) {
		this.#link = link
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
		const open = KvBucket.encode({
			name,
			defaultTtl: options?.defaultTtl === undefined ? undefined : ms(options.defaultTtl),
			sliding: options?.sliding === undefined ? undefined : ms(options.sliding),
		})
		return new Bucket(this.#link, name, open, values, [])
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
	 */
	batch(fn: (tx: Batch) => void): Promise<void> {
		return runCalls(this.#link, 'batch', fn)
	}

	/** Reads the gets and hases fn asks for from one snapshot. */
	view(fn: (tx: Batch) => void): Promise<void> {
		return runCalls(this.#link, 'view', fn)
	}
}

/** The calls a batch or a view gathered, in their order. */
export class Batch {
	readonly kind: 'batch' | 'view'
	readonly calls: Recorded[] = []
	#done = false

	constructor(kind: 'batch' | 'view') {
		this.kind = kind
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

async function runCalls(
	link: Link,
	kind: 'batch' | 'view',
	fn: (tx: Batch) => void,
): Promise<void> {
	const tx = new Batch(kind)
	const returned: unknown = fn(tx)
	tx.close()
	if (returned instanceof Promise) {
		throw new InvalidError(
			`a ${kind}'s function returns before anything is sent; it cannot await inside`,
		)
	}
	if (tx.calls.length === 0) {
		return
	}
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

/** A key's owners, each its text: an integer is its decimal spelling, as Go's Of takes it. */
function ownerText(owner: Key): string | Uint8Array {
	if (typeof owner === 'number' || typeof owner === 'bigint') {
		if (typeof owner === 'number' && !Number.isSafeInteger(owner)) {
			throw new InvalidError(`the owner ${owner} is not an integer; an owner is text or an integer`)
		}
		return String(owner)
	}
	return owner
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

	constructor(
		link: Link,
		name: string,
		open: Uint8Array,
		values: Values<V>,
		owners: (string | Uint8Array)[],
	) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#values = values
		this.#owners = owners
	}

	/** The branch below this one that the owners name. */
	of(...owners: Key[]): Bucket<V> {
		return new Bucket(this.#link, this.name, this.#open, this.#values, [
			...this.#owners,
			...owners.map(ownerText),
		])
	}

	/** The bucket's calls inside a batch or a view, whose promises settle with it. */
	withTx(tx: Batch): BucketTx<V> {
		return new BucketTx(tx, this.#open, this.#values, this.#owners)
	}

	async #call(
		method: keyof typeof methods,
		fields: Omit<Parameters<typeof KvCall.encode>[0], 'handle' | 'owners'>,
		idempotence: 'read' | 'write',
	) {
		return this.#link.run(idempotence, async (connection: Connection) => {
			const handle = await handleOn(connection, methods['kv.open'], this.#open)
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
			const handle = await handleOn(connection, methods['kv.open'], this.#open)
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
	readonly #tx: Batch
	readonly #open: Uint8Array
	readonly #values: Values<V>
	readonly #owners: (string | Uint8Array)[]

	constructor(tx: Batch, open: Uint8Array, values: Values<V>, owners: (string | Uint8Array)[]) {
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
