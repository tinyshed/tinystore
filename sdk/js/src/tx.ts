// A transaction across the wire, which never holds the writer while the host
// awaits: a read goes at once and is kept with what it found; a write waits,
// and when the function returns the writes commit in one kv.tx, after a check
// that every key it read is still as it read it. A check that fails runs the
// function again.

import type { Link } from './connection.ts'
import { ConflictError, InvalidError } from './errors.ts'
import { handleOn } from './handles.ts'
import { commitJobs, type JobOp, type Queue, queueParts, StoreTxQueue } from './jobs.ts'
import {
	type Bucket,
	type Counters,
	type Entry,
	entryOf,
	keyText,
	type Parts,
	partsOf,
	underField,
	type Values,
	type WriteOptions,
	writeFields,
} from './kv.ts'
import { type Duration, ms, unixMs } from './time.ts'
import type { Key, Written } from './wire/codec.ts'
import { KvCall, KvEntry, KvTx, type Method, methods } from './wire/protocol.ts'

/** How many times tx runs its function before a key it read that keeps changing is ConflictError. */
const runs = 5

/**
 * Runs fn as one transaction and gives back what it returns: its writes all
 * commit or none do. When a key fn read changed before the commit, fn runs
 * again, five times at most, so it does nothing else that must happen once.
 */
export async function runTx<T>(link: Link, fn: (tx: Tx) => T | Promise<T>): Promise<T> {
	for (let run = 1; ; run++) {
		const tx = new Tx(link)
		const returned = await fn(tx)
		try {
			await tx.commit()
			return returned
		} catch (err) {
			const changed = err instanceof ConflictError && err.what.check !== undefined
			if (run >= runs || !changed) {
				throw err
			}
		}
	}
}

type Found = ReturnType<typeof KvEntry.decode>
type CallFields = Omit<Written<typeof KvCall.fields>, 'handle' | 'under'>

/** A key the transaction read from the store, which its commit checks. */
interface Read {
	parts: Parts
	key: string
	found: Found
}

/** A write the commit sends, in the order the function made it. */
interface Op {
	method: Method
	parts: Parts
	fields: CallFields
}

const absent: Found = { found: false }

/** The calls of a transaction, each through `tx.with(handle)`. */
export class Tx {
	readonly #link: Link
	readonly #reads = new Map<string, Promise<Read>>()
	/** what the transaction wrote to a key, so that a read after it answers it */
	readonly #written = new Map<string, { found: Found; at: number }>()
	readonly #cleared: { handle: string; under: readonly string[]; at: number }[] = []
	readonly #ops: Op[] = []
	/** a queue's writes: a transaction takes the handles of one file, kv.db's or jobs.db's */
	readonly #jobs: JobOp[] = []
	#file: 'kv.db' | 'jobs.db' | undefined
	#done = false

	constructor(link: Link) {
		this.#link = link
	}

	with<T>(bucket: Bucket<T>): TxBucket<T>
	with(counters: Counters): TxCounters
	with<T>(queue: Queue<T>): StoreTxQueue<T>
	with(
		handle: Bucket<unknown> | Counters | Queue<unknown>,
	): TxBucket<unknown> | TxCounters | StoreTxQueue<unknown> {
		const queue = queueParts(handle)
		if (queue !== undefined) {
			const what = `jobs queue ${queue.name}`
			this.#takes(queue.source.link, queue.source.home?.describe, 'jobs.db', what)
			return new StoreTxQueue(queue, op => {
				this.#open()
				this.#jobs.push(op)
			})
		}
		const parts = partsOf(handle)
		if (parts === undefined) {
			throw new InvalidError('tx.with takes a bucket, counters or a queue')
		}
		this.#takes(parts.link, parts.home?.describe, 'kv.db', `kv ${parts.name}`)
		if (parts.values === undefined) {
			return new TxCounters(this, parts)
		}
		return new TxBucket(this, parts, parts.values)
	}

	/** What the key holds as this transaction sees it: its own writes over what the store held. */
	async read(parts: Parts, key: Key): Promise<Found> {
		this.#open()
		const text = keyText(key)
		const own = this.#own(parts, text)
		if (own !== undefined) {
			return own
		}
		const place = placeOf(parts, text)
		let reading = this.#reads.get(place)
		if (reading === undefined) {
			reading = this.#fetch(parts, text)
			// a read the function never awaited still fails the commit, which awaits it
			reading.catch(() => {})
			this.#reads.set(place, reading)
		}
		return (await reading).found
	}

	/** Keeps a write for the commit; found is what a read of its key answers after it. */
	write(method: Method, parts: Parts, fields: CallFields, found?: Found): void {
		this.#open()
		this.#ops.push({ method, parts, fields })
		if (found !== undefined && fields.key !== undefined) {
			this.#written.set(placeOf(parts, keyText(fields.key)), { found, at: this.#ops.length })
		}
	}

	clear(parts: Parts): void {
		this.write('kv.clear', parts, {})
		this.#cleared.push({ handle: handleOf(parts), under: parts.under, at: this.#ops.length })
	}

	/**
	 * Refuses a handle of another store, one kept in a database's file, and
	 * one of the store's other file: a bucket and a queue of the store are two
	 * files, and no write is atomic across two.
	 */
	#takes(link: Link, home: string | undefined, file: 'kv.db' | 'jobs.db', what: string): void {
		if (link !== this.#link) {
			throw new InvalidError(`${what}: a handle of another store, in a transaction of this one`)
		}
		if (home !== undefined) {
			throw new InvalidError(
				`${home}: ${what}: kept in the database's file, outside this transaction of the store: take it in with db.tx`,
			)
		}
		if (this.#file !== undefined && this.#file !== file) {
			throw new InvalidError(
				`${what}: kept in ${file}, outside this transaction of ${this.#file}: open both from a database to commit them together`,
			)
		}
		this.#file = file
	}

	/** Sends the writes in one kv.tx, after a check of each key read: still at its version, or still absent. */
	async commit(): Promise<void> {
		this.#done = true
		if (this.#jobs.length > 0) {
			return commitJobs(this.#link, this.#jobs)
		}
		const reads = await Promise.all(this.#reads.values())
		if (reads.length === 0 && this.#ops.length === 0) {
			return
		}
		await this.#link.run(this.#ops.length > 0 ? 'write' : 'read', async connection => {
			const handle = (parts: Parts) => handleOn(connection, methods[parts.openMethod], parts.open)
			const checks = []
			for (const read of reads) {
				const version = read.found.found === true ? read.found.version : undefined
				checks.push({
					handle: await handle(read.parts),
					under: underField(read.parts.under),
					key: read.key,
					version,
				})
			}
			const writes = []
			for (const op of this.#ops) {
				const call = {
					...op.fields,
					handle: await handle(op.parts),
					under: underField(op.parts.under),
				}
				writes.push({ method: methods[op.method], call })
			}
			await connection.session.call(methods['kv.tx'], KvTx.encode({ checks, writes }))
		})
	}

	#open(): void {
		if (this.#done) {
			throw new InvalidError(
				'a transaction takes its calls while its function runs, before it commits',
			)
		}
	}

	// the transaction's own word on a key: a write of it, or a clear of a branch above it, whichever came last
	#own(parts: Parts, key: string): Found | undefined {
		const written = this.#written.get(placeOf(parts, key))
		let cleared = 0
		for (const clear of this.#cleared) {
			if (clear.handle === handleOf(parts) && startsWith(parts.under, clear.under)) {
				cleared = Math.max(cleared, clear.at)
			}
		}
		if (written !== undefined && written.at > cleared) {
			return written.found
		}
		return cleared > 0 ? absent : undefined
	}

	async #fetch(parts: Parts, key: string): Promise<Read> {
		const found = await this.#link.run('read', async connection => {
			const handle = await handleOn(connection, methods[parts.openMethod], parts.open)
			const call = KvCall.encode({ handle, under: underField(parts.under), key })
			return KvEntry.decode(await connection.session.call(methods['kv.get'], call))
		})
		return { parts, key, found }
	}
}

/** A bucket's calls inside a transaction: reads answer at once, writes wait for its commit. */
export class TxBucket<T> {
	readonly #tx: Tx
	readonly #parts: Parts
	readonly #values: Values<T>

	constructor(tx: Tx, parts: Parts, values: Values<unknown>) {
		this.#tx = tx
		this.#parts = parts
		this.#values = values as Values<T>
	}

	under(...owners: Key[]): TxBucket<T> {
		const parts = { ...this.#parts, under: [...this.#parts.under, ...owners.map(keyText)] }
		return new TxBucket(this.#tx, parts, this.#values as Values<unknown>)
	}

	async get(key: Key): Promise<T | undefined> {
		return (await this.entry(key))?.value
	}

	async entry(key: Key): Promise<Entry<T> | undefined> {
		const found = await this.#tx.read(this.#parts, key)
		return found.found === true ? entryOf(key, found, this.#values) : undefined
	}

	async has(key: Key): Promise<boolean> {
		return (await this.#tx.read(this.#parts, key)).found === true
	}

	async set(key: Key, value: T, options?: WriteOptions): Promise<void> {
		const raw = this.#values.encode(value)
		this.#tx.write(
			'kv.set',
			this.#parts,
			{ key, value: raw, ...writeFields(options) },
			{ found: true, value: raw },
		)
	}

	/** Writes the value only where no live key is, and says whether it will. */
	async create(key: Key, value: T, options?: Omit<WriteOptions, 'ifVersion'>): Promise<boolean> {
		if ((await this.#tx.read(this.#parts, key)).found === true) {
			return false
		}
		const raw = this.#values.encode(value)
		this.#tx.write(
			'kv.create',
			this.#parts,
			{ key, value: raw, ...writeFields(options) },
			{ found: true, value: raw },
		)
		return true
	}

	async add(key: Key, options?: Omit<WriteOptions, 'ifVersion'>): Promise<boolean> {
		if ((await this.#tx.read(this.#parts, key)).found === true) {
			return false
		}
		this.#tx.write(
			'kv.create',
			this.#parts,
			{ key, ...writeFields(options) },
			{ found: true, value: null },
		)
		return true
	}

	async take(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<T | undefined> {
		const found = await this.#remove('kv.take', key, options)
		return found.found === true ? this.#values.decode(found.value ?? null) : undefined
	}

	async delete(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<boolean> {
		return (await this.#remove('kv.delete', key, options)).found === true
	}

	async expire(
		key: Key,
		when: Duration | Date,
		options?: Pick<WriteOptions, 'ifVersion'>,
	): Promise<boolean> {
		const found = await this.#tx.read(this.#parts, key)
		if (found.found !== true && options?.ifVersion === undefined) {
			return false
		}
		const expiry = when instanceof Date ? { expiresAt: when } : { ttl: when }
		const expiresAt = when instanceof Date ? unixMs(when) : Date.now() + ms(when)
		const fields = { key, ...writeFields({ ...options, ...expiry }) }
		this.#tx.write(
			'kv.expire',
			this.#parts,
			fields,
			found.found === true ? { ...found, expiresAt } : undefined,
		)
		return found.found === true
	}

	/** Removes this branch's keys and every branch's under it, with the commit. */
	async clear(): Promise<void> {
		this.#tx.clear(this.#parts)
	}

	// a take or a delete: what the key held, and its removal, sent when the
	// key was there or when the caller named the version it must be at
	async #remove(
		method: Method,
		key: Key,
		options?: Pick<WriteOptions, 'ifVersion'>,
	): Promise<Found> {
		const found = await this.#tx.read(this.#parts, key)
		if (found.found === true || options?.ifVersion !== undefined) {
			this.#tx.write(method, this.#parts, { key, ...writeFields(options) }, absent)
		}
		return found
	}
}

/** Counters inside a transaction: an add commits with its writes. */
export class TxCounters {
	readonly #tx: Tx
	readonly #parts: Parts

	constructor(tx: Tx, parts: Parts) {
		this.#tx = tx
		this.#parts = parts
	}

	under(...owners: Key[]): TxCounters {
		return new TxCounters(this.#tx, {
			...this.#parts,
			under: [...this.#parts.under, ...owners.map(keyText)],
		})
	}

	/** Adds n, 1 unless given, with the commit; the count is known once it has. */
	async add(key: Key, n: number | bigint = 1): Promise<void> {
		this.#tx.write('kv.counters.add', this.#parts, { key, n })
	}
}

/** A key's place as one text, its handle and owners included, each kept apart. */
function placeOf(parts: Parts, key: string): string {
	return JSON.stringify([handleOf(parts), parts.under, key])
}

/** A handle's place as text: its open's bytes, which a store's own handle has. */
function handleOf(parts: Parts): string {
	if (typeof parts.open === 'function') {
		throw new InvalidError(`kv ${parts.name}: a handle of a database, in a transaction of kv.db`)
	}
	return `${parts.openMethod} ${Buffer.from(parts.open).toString('base64')}`
}

function startsWith(under: readonly string[], branch: readonly string[]): boolean {
	return branch.length <= under.length && branch.every((owner, i) => under[i] === owner)
}
