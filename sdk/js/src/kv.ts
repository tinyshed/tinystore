// The application's current state in kv.db, as plan/api/kv.md has it: buckets
// of one value type by key, keys in branches, expiry by the store's clock and
// versions that never repeat, with counters beside them. A handle opens with
// its first call on a connection.

import type { Connection, Idempotence, Link } from './connection.ts'
import { CorruptError, InvalidError } from './errors.ts'
import {
	checkName,
	type Home,
	handleOn,
	type Open,
	ownerText,
	type Via,
	viaLink,
} from './handles.ts'
import { type Duration, dateOf, ms, type Time, unixMs } from './time.ts'
import { type Key, type Raw, textOf, type Written } from './wire/codec.ts'
import type { Reader } from './wire/msgpack.ts'
import {
	KvBranch,
	KvBucketOpen,
	KvCall,
	KvCount,
	KvCountersOpen,
	KvEntry,
	KvFound,
	KvList,
	KvPage,
	KvWritten,
	type Method,
	methods,
} from './wire/protocol.ts'

/**
 * How a bucket keeps a value that is not JSON, as the core keeps a Rust type,
 * so that every language reads the others' rows:
 *
 *     'string'   its UTF-8 bytes
 *     'bytes'    the bytes
 *     'int'      an integer a number holds exactly
 *     'bigint'   an integer past what a number holds
 *     'float'    its eight bytes, big-endian, every bit kept
 *     'bool'     1 or 0
 */
export type ValueType = 'string' | 'bytes' | 'int' | 'bigint' | 'float' | 'bool'

export interface BucketOptions {
	/** a key expires this long after it was written, unless its write gives its own */
	ttl?: Duration | undefined
	/** a key expires this long after it was last read or written; not beside ttl */
	idle?: Duration | undefined
	/** how values are kept when they are not JSON */
	type?: ValueType | undefined
	/**
	 * Keeps every value sealed with the store's encryption key, as a password or a
	 * token is kept: the file holds what nobody reads without the key. The
	 * store seals and opens, so this program never holds the key. A key is not
	 * sealed, so a secret is a value and never a key, and a name keeps whether
	 * it is encrypted.
	 */
	encrypted?: boolean | undefined
}

export interface WriteOptions {
	/** the key expires this long from now */
	ttl?: Duration | undefined
	/** the key expires at this time */
	expiresAt?: Time | undefined
	/** writes only while the key is at this version: ConflictError otherwise */
	ifVersion?: string
}

export interface Entry<T> {
	key: string
	value: T
	/** compared only for equality: give it back to ifVersion */
	version: string
	/** undefined for a key that never expires */
	expiresAt: Date | undefined
}

/** A page of a branch's keys, and where the next starts when there is one. */
export interface Page<T> {
	entries: Entry<T>[]
	next: string | undefined
}

export interface CountersOptions {
	/** a counter lasts this long from its first add */
	ttl?: Duration | undefined
	/**
	 * keeps counts in memory and writes them this often, so that an add costs
	 * no commit and a crash loses at most this span: attempts and hits, not money
	 */
	flushEvery?: Duration | undefined
}

/** Turns a bucket's values into what a row keeps, and back. */
export interface Values<T> {
	encode(value: T): Raw
	decode(raw: Raw): T
	/** decode returns the bytes it was given, which must then be a copy of the answer's */
	readonly keeps?: boolean
}

/** What a transaction needs of a handle it takes in. */
export interface Parts {
	readonly link: Link
	readonly name: string
	readonly open: Open
	readonly openMethod: Method
	readonly under: readonly string[]
	/** the database whose file keeps the handle; the store's own when undefined */
	readonly home?: Home | undefined
}

// a handle's parts, kept out of its public shape, for store.tx
const parts = new WeakMap<object, Parts & { values?: Values<unknown> }>()

export function partsOf(handle: object): (Parts & { values?: Values<unknown> }) | undefined {
	return parts.get(handle)
}

/** A bucket's values under one branch of owners, the bucket's own when there are none. */
export class Bucket<T> {
	readonly name: string
	readonly #link: Link
	readonly #open: Open
	readonly #values: Values<T>
	readonly #under: readonly string[]
	readonly #home: Home | undefined
	readonly #via: Via
	/** reads a get's or a take's answer: the value, undefined where the key held none */
	readonly #value: (r: Reader) => T | undefined

	/**
	 * Buckets come from `store.bucket`, `db.bucket` and their `under`; inside
	 * a database's transaction, from `tx.with(bucket)`, whose calls go through
	 * the transaction.
	 */
	constructor(
		link: Link,
		name: string,
		open: Open,
		values: Values<T>,
		under: readonly string[],
		home?: Home,
		via?: Via,
	) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#values = values
		this.#under = under
		this.#home = home
		this.#via = via ?? home?.via(`kv bucket ${name}`) ?? viaLink(link)
		this.#value = r => valueIn(r, values)
		const own = { link, name, open, openMethod: 'kv.bucket.open', under, home } as const
		parts.set(this, { ...own, values: values as Values<unknown> })
	}

	/** The branch the owners name below this one: whose keys these are. */
	under(...owners: Key[]): Bucket<T> {
		const under = [...this.#under, ...owners.map(keyText)]
		return new Bucket(this.#link, this.name, this.#open, this.#values, under, this.#home, this.#via)
	}

	/** The key's value, undefined when it holds none. */
	get(key: Key): Promise<T | undefined> {
		return this.#ask('kv.get', { key }, 'read', this.#value)
	}

	/** The key's value with its version and expiry, undefined when it holds none. */
	entry(key: Key): Promise<Entry<T> | undefined> {
		return this.#ask('kv.get', { key }, 'read', r => {
			const found = KvEntry.read(r)
			return found.found === true ? entryOf(key, found, this.#values) : undefined
		})
	}

	has(key: Key): Promise<boolean> {
		return this.#ask('kv.has', { key }, 'read', found)
	}

	/** Writes the value; a key that exists keeps its expiry unless the options give one. */
	async set(key: Key, value: T, options?: WriteOptions): Promise<void> {
		const raw = this.#values.encode(value)
		await this.#ask('kv.set', { key, value: raw, ...writeFields(options) }, 'write', unread)
	}

	/** Writes the value only where no live key is, and says whether it did. */
	async create(key: Key, value: T, options?: Omit<WriteOptions, 'ifVersion'>): Promise<boolean> {
		const raw = this.#values.encode(value)
		return this.#ask('kv.create', { key, value: raw, ...writeFields(options) }, 'write', written)
	}

	/** Puts the key in a bucket used as a set; false when it was there. */
	async add(key: Key, options?: Omit<WriteOptions, 'ifVersion'>): Promise<boolean> {
		return this.#ask('kv.create', { key, ...writeFields(options) }, 'write', written)
	}

	/**
	 * Reads the value and removes the key in one commit: of two callers taking
	 * a one-time code at once, one gets it.
	 */
	async take(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<T | undefined> {
		return this.#ask('kv.take', { key, ...writeFields(options) }, 'write', this.#value)
	}

	/** Removes the key, and says whether it was there. */
	async delete(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<boolean> {
		return this.#ask('kv.delete', { key, ...writeFields(options) }, 'write', found)
	}

	/**
	 * Gives a live key a new expiry, a span from now or a time, keeping its
	 * value; false when it holds none.
	 */
	async expire(
		key: Key,
		when: Duration | Date,
		options?: Pick<WriteOptions, 'ifVersion'>,
	): Promise<boolean> {
		const expiry = when instanceof Date ? { expiresAt: when } : { ttl: when }
		const fields = { key, ...writeFields({ ...options, ...expiry }) }
		return this.#ask('kv.expire', fields, 'write', found)
	}

	/** Removes this branch's keys and every branch under it, at once however many. */
	async clear(): Promise<void> {
		const under = underField(this.#under)
		await this.#via.call(
			methods['kv.bucket.open'],
			this.#open,
			methods['kv.clear'],
			handle => KvBranch.encode({ handle, under }),
			'write',
		)
	}

	/** One page of this branch's own keys, in the byte order of their text: `"10"` before `"9"`. */
	async list(options?: { limit?: number | undefined; after?: Key | undefined }): Promise<Page<T>> {
		const under = underField(this.#under)
		const body = await this.#via.call(
			methods['kv.bucket.open'],
			this.#open,
			methods['kv.list'],
			handle => KvList.encode({ handle, under, after: options?.after, limit: options?.limit }),
			'read',
		)
		const page = KvPage.decode(body)
		const entries = (page.entries ?? []).map(found => entryOf(found.key ?? '', found, this.#values))
		return { entries, next: page.next === undefined ? undefined : keyText(page.next) }
	}

	/**
	 * Walks this branch's own keys a page at a time, holding nothing between
	 * pages: a key written during the walk may or may not be met. It renews no
	 * idle key.
	 */
	async *all(options?: { limit?: number | undefined }): AsyncGenerator<Entry<T>> {
		let after: string | undefined
		for (;;) {
			const page = await this.list({ ...options, ...(after === undefined ? {} : { after }) })
			yield* page.entries
			if (page.next === undefined) {
				return
			}
			after = page.next
		}
	}

	// a call on one key: its fields written where they leave from, its answer read where it arrived
	#ask<A>(
		method: Method,
		fields: CallFields,
		idempotence: Idempotence,
		read: (r: Reader) => A,
	): Promise<A> {
		// the caller's own fields, made the call's: an object more a call is one too many
		const call = fields as { -readonly [K in keyof Call]: Call[K] }
		call.under = underField(this.#under)
		return this.#via.ask(
			methods['kv.bucket.open'],
			this.#open,
			methods[method],
			(w, handle) => {
				call.handle = handle
				KvCall.write(w, call)
			},
			read,
			idempotence,
		)
	}
}

const [foundField] = KvEntry.fields.found
const [valueField] = KvEntry.fields.value

/**
 * Reads an entry for its value alone, undefined where the key held none: the
 * two fields a get needs, the value decoded from where the answer lies unless
 * its bucket keeps the bytes.
 */
function valueIn<T>(r: Reader, values: Values<T>): T | undefined {
	let found = false
	let raw: Raw = null
	for (let n = r.message(); n > 0; n--) {
		const field = r.field()
		if (field === foundField) {
			found = r.bool()
		} else if (field !== valueField) {
			r.skip()
		} else if (!r.nil()) {
			raw = r.type() !== 'bin' ? r.int64() : values.keeps === true ? r.bytes() : r.bin()
		}
	}
	r.leave()
	return found ? values.decode(raw) : undefined
}

const found = (r: Reader): boolean => KvFound.read(r).found === true
const written = (r: Reader): boolean => KvWritten.read(r).written === true
/** Reads past an answer nobody asked to see. */
const unread = (r: Reader): void => r.skip()

/** Numbers by key that only add up: 0 for one never added to or expired. */
export class Counters {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #under: readonly string[]

	/** Counters come from `store.counters` and their `under`. */
	constructor(link: Link, name: string, open: Uint8Array, under: readonly string[]) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#under = under
		parts.set(this, { link, name, open, openMethod: 'kv.counters.open', under })
	}

	under(...owners: Key[]): Counters {
		const under = [...this.#under, ...owners.map(keyText)]
		return new Counters(this.#link, this.name, this.#open, under)
	}

	/** Adds n, 1 unless given, and gives the new count. */
	async add(key: Key, n: number | bigint = 1): Promise<number> {
		return this.#count('kv.counters.add', { key, n }, 'write')
	}

	async get(key: Key): Promise<number> {
		return this.#count('kv.counters.get', { key }, 'read')
	}

	/** Removes the counter, and says whether it was there. */
	async delete(key: Key): Promise<boolean> {
		const body = await this.#call('kv.counters.delete', { key }, 'write')
		return KvFound.decode(body).found === true
	}

	/** Removes this branch's counters and every branch's under it. */
	async clear(): Promise<void> {
		await this.#link.run('write', async connection => {
			const handle = await handleOn(connection, methods['kv.counters.open'], this.#open)
			const branch = KvBranch.encode({ handle, under: underField(this.#under) })
			await connection.session.call(methods['kv.counters.clear'], branch)
		})
	}

	async #count(method: Method, fields: CallFields, idempotence: Idempotence): Promise<number> {
		const count = KvCount.decode(await this.#call(method, fields, idempotence)).value ?? 0n
		return numberOf(BigInt(count), this.name)
	}

	#call(method: Method, fields: CallFields, idempotence: Idempotence): Promise<Uint8Array> {
		return callOn(
			this.#link,
			'kv.counters.open',
			this.#open,
			this.#under,
			method,
			fields,
			idempotence,
		)
	}
}

/**
 * What a bucket's open sends, checked before anything leaves; `database` is
 * the handle of the database whose file keeps it.
 */
export function bucketOpen(
	name: string,
	options: BucketOptions = {},
	database?: number,
): Uint8Array {
	checkName(name, 'bucket')
	if (options.ttl !== undefined && options.idle !== undefined) {
		throw new InvalidError(`kv bucket ${name}: ttl and idle are one or the other`)
	}
	return KvBucketOpen.encode({
		name,
		ttl: options.ttl === undefined ? undefined : ms(options.ttl),
		idle: options.idle === undefined ? undefined : ms(options.idle),
		database,
		encrypted: options.encrypted === true,
	})
}

export function countersOpen(name: string, options: CountersOptions = {}): Uint8Array {
	checkName(name, 'counters')
	return KvCountersOpen.encode({
		name,
		ttl: options.ttl === undefined ? undefined : ms(options.ttl),
		flushEvery: options.flushEvery === undefined ? undefined : ms(options.flushEvery),
	})
}

type Call = Written<typeof KvCall.fields>
type CallFields = Omit<Call, 'handle' | 'under'>

/** Calls a method of a handle's branch on the link's connection, the handle opened there first. */
export function callOn(
	link: Link,
	openMethod: Method,
	open: Open,
	under: readonly string[],
	method: Method,
	fields: CallFields,
	idempotence: Idempotence,
): Promise<Uint8Array> {
	return link.run(idempotence, async (connection: Connection) => {
		const handle = await handleOn(connection, methods[openMethod], open)
		const call = KvCall.encode({ ...fields, handle, under: underField(under) })
		return connection.session.call(methods[method], call)
	})
}

/** A branch's owners as a call sends them: none at the root. */
export function underField(under: readonly string[]): string[] | undefined {
	return under.length > 0 ? [...under] : undefined
}

export function writeFields(options: WriteOptions | undefined): CallFields {
	return {
		ttl: options?.ttl === undefined ? undefined : ms(options.ttl),
		expiresAt: options?.expiresAt === undefined ? undefined : unixMs(options.expiresAt),
		ifVersion: options?.ifVersion === undefined ? undefined : versionBytes(options.ifVersion),
	}
}

type Found = ReturnType<typeof KvEntry.decode>

export function entryOf<T>(key: Key, found: Found, values: Values<T>): Entry<T> {
	return {
		key: keyText(key),
		value: values.decode(found.value ?? null),
		version: versionText(found.version),
		expiresAt: dateOf(found.expiresAt),
	}
}

/** A version's bytes as text, each byte a code unit, so that any bytes go back as they came. */
export function versionText(version: Uint8Array | undefined): string {
	return version === undefined ? '' : String.fromCharCode(...version)
}

export function versionBytes(version: string): Uint8Array {
	return Uint8Array.from(version, c => c.charCodeAt(0))
}

/** A key or an owner as the server spells it: an integer its decimal text, bytes the text they spell. */
export function keyText(key: Key): string {
	const text = ownerText(key)
	if (typeof text === 'string') {
		return text
	}
	const spelled = textOf(text)
	if (spelled === undefined) {
		throw new InvalidError('a key of bytes that are not UTF-8, which kv does not keep yet')
	}
	return spelled
}

function numberOf(n: bigint, name: string): number {
	if (n > BigInt(Number.MAX_SAFE_INTEGER) || n < BigInt(Number.MIN_SAFE_INTEGER)) {
		throw new CorruptError(`kv counters ${name}: ${n} does not fit a number exactly`)
	}
	return Number(n)
}

const utf8 = new TextEncoder()

/** The first byte of a row of bytes, the kind of value that wrote it, as the core and every SDK write it. */
const kinds = { text: 1, bytes: 2, json: 3, float: 4, u64: 5, f32: 6 } as const
const kindNames: Record<number, string> = {
	1: 'text',
	2: 'bytes',
	3: 'JSON',
	4: 'a float',
	5: 'a u64',
	6: 'an f32',
}

function tagged(kind: number, body: Uint8Array): Uint8Array {
	const row = new Uint8Array(body.length + 1)
	row[0] = kind
	row.set(body, 1)
	return row
}

/** The body of a row that `kind` wrote; a row another kind wrote is corrupt here, never read as this one. */
function bodyOf(raw: Raw, kind: number, wanted: string): Uint8Array {
	if (!(raw instanceof Uint8Array)) {
		throw new CorruptError(`a stored value is ${describe(raw)} where ${wanted} was written`)
	}
	if (raw[0] !== kind) {
		const written =
			raw.length === 0 ? 'no bytes' : (kindNames[raw[0] as number] ?? 'bytes of no kind')
		throw new CorruptError(`${wanted} from a row of ${written}`)
	}
	return raw.subarray(1)
}

/** JSON, what a bucket keeps unless its type says otherwise; a row of nothing, a set's member, reads as null. */
export const jsonValues: Values<unknown> = {
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
		return tagged(kinds.json, utf8.encode(text))
	},
	decode: raw => {
		if (raw === null) {
			return null
		}
		const json = bodyOf(raw, kinds.json, 'JSON')
		try {
			return JSON.parse(textOf(json) ?? '')
		} catch (err) {
			throw new CorruptError(`a stored value is not JSON any more: ${(err as Error).message}`)
		}
	},
}

export function valuesOf(type: ValueType | undefined): Values<unknown> {
	switch (type) {
		case undefined:
			return jsonValues
		case 'string':
			return {
				encode: v => {
					if (typeof v !== 'string') {
						throw new InvalidError(`a ${typeof v} in a bucket of strings`)
					}
					return tagged(kinds.text, utf8.encode(v))
				},
				decode: raw => {
					if (raw === null) {
						return ''
					}
					const text = textOf(bodyOf(raw, kinds.text, 'text'))
					if (text === undefined) {
						throw new CorruptError('a stored value is text that is not UTF-8')
					}
					return text
				},
			}
		case 'bytes':
			return {
				keeps: true,
				encode: v => {
					if (!(v instanceof Uint8Array)) {
						throw new InvalidError(`a ${typeof v} in a bucket of bytes`)
					}
					return tagged(kinds.bytes, v)
				},
				decode: raw => (raw === null ? new Uint8Array(0) : bodyOf(raw, kinds.bytes, 'bytes')),
			}
		case 'int':
			return {
				encode: v => {
					if (typeof v !== 'number' || !Number.isSafeInteger(v)) {
						throw new InvalidError(
							`${String(v)} is not an integer a number holds exactly; open the bucket with type 'bigint'`,
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
							`${raw} does not fit a number exactly; open the bucket with type 'bigint'`,
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
					return tagged(kinds.float, bytes)
				},
				decode: raw => {
					const bits = bodyOf(raw, kinds.float, 'a float')
					if (bits.length !== 8) {
						throw new CorruptError(`a float from ${bits.length} bytes rather than 8`)
					}
					return new DataView(bits.buffer, bits.byteOffset, 8).getFloat64(0)
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
	}
	throw new InvalidError(
		`a bucket's type is string, bytes, int, bigint, float or bool, not ${String(type)}`,
	)
}

function describe(raw: Raw): string {
	if (raw === null) {
		return 'nothing'
	}
	return typeof raw === 'bigint' ? 'an integer' : `${raw.length} bytes`
}
