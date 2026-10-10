import type { Connection, Idempotence, Link } from './connection.ts'
import { InvalidError } from './errors.ts'
import type { Key } from './wire/codec.ts'
import { Handle } from './wire/protocol.ts'

/**
 * What opens a handle: its open's bytes, or, for a bucket or a queue kept in
 * a database's file, what makes them once the database is open on the
 * connection, since the open names the database by its handle there.
 */
export type Open = Uint8Array | ((connection: Connection) => Promise<Uint8Array>)

/** A database a bucket or a queue is kept in: its handle on a connection, and its name as errors say it. */
export interface Home {
	readonly describe: string
	handle(connection: Connection): Promise<number>
	/**
	 * The opener of a handle kept in the database's file: the one made first
	 * for the same open, so that the database's transaction opens each once,
	 * before it holds the writer, and knows the handles it opened.
	 */
	keep(
		method: number,
		bytes: Uint8Array,
		make: (connection: Connection) => Promise<Uint8Array>,
	): Open
	/**
	 * How a handle kept in the database's file reaches the server outside a
	 * transaction: on the link's connection, and refused from inside the
	 * database's own transaction, where it would wait for the writer the
	 * transaction holds.
	 */
	via(what: string): Via
}

/**
 * How a handle's calls reach the server: on the link's connection, the handle
 * opened there first, or inside a database's transaction, on its stream.
 */
export interface Via {
	call(
		openMethod: number,
		open: Open,
		method: number,
		body: (handle: number) => Uint8Array,
		idempotence: Idempotence,
	): Promise<Uint8Array>
}

/** Calls on whichever connection the link has, dialled again as it says. */
export function viaLink(link: Link): Via {
	return {
		call: (openMethod, open, method, body, idempotence) =>
			link.run(idempotence, async connection => {
				const handle = await handleOn(connection, openMethod, open)
				return connection.session.call(method, body(handle))
			}),
	}
}

// the base64 of an open's bytes, made once: a database's open holds its migrations
const openKeys = new WeakMap<Uint8Array, string>()

// what an opener of a database's file opened, by connection
const opened = new WeakMap<object, WeakMap<Connection, Promise<number>>>()

/**
 * The handle an open answers on a connection, opened once a connection: the
 * open's own bytes name it, so a bucket opened twice with the same options
 * shares its handle. An open that fails is tried again by the next call.
 */
export function handleOn(connection: Connection, method: number, open: Open): Promise<number> {
	if (typeof open === 'function') {
		let byConnection = opened.get(open)
		if (byConnection === undefined) {
			byConnection = new WeakMap()
			opened.set(open, byConnection)
		}
		let opening = byConnection.get(connection)
		if (opening === undefined) {
			const kept = byConnection
			opening = open(connection).then(bytes => handleOn(connection, method, bytes))
			kept.set(connection, opening)
			opening.catch(() => kept.delete(connection))
		}
		return opening
	}
	let encoded = openKeys.get(open)
	if (encoded === undefined) {
		encoded = Buffer.from(open).toString('base64')
		openKeys.set(open, encoded)
	}
	const key = `${method}:${encoded}`
	let opening = connection.handles.get(key)
	if (opening === undefined) {
		opening = connection.session.call(method, open).then(body => Handle.decode(body).handle ?? 0)
		connection.handles.set(key, opening)
		opening.catch(() => connection.handles.delete(key))
	}
	return opening
}

/** A bucket's or a queue's name, as every engine takes it: [a-z0-9][a-z0-9_-]{0,63}. */
export function checkName(name: string, of: string): void {
	if (!/^[a-z0-9][a-z0-9_-]{0,63}$/.test(name)) {
		throw new InvalidError(
			`the ${of} name ${JSON.stringify(name)}: a name is [a-z0-9][a-z0-9_-]{0,63}`,
		)
	}
}

/**
 * What a batch's or a view's function returned, once the batch has settled: a
 * promise its answer, an array or a tuple each element's as Promise.all gives
 * them, and anything else itself.
 */
export type Settled<T> =
	T extends PromiseLike<infer V>
		? V
		: T extends readonly unknown[]
			? { -readonly [K in keyof T]: Awaited<T[K]> }
			: T

/** Answers what a batch's function returned, once the batch's calls have settled. */
export async function settle<T>(returned: T): Promise<Settled<T>> {
	return (Array.isArray(returned) ? await Promise.all(returned) : await returned) as Settled<T>
}

/** A page of a scan: its items, and where the next begins when more remain. */
export interface Page<T, After> {
	items: T[]
	/** the place the next page begins after; undefined once the scan has ended */
	next: After | undefined
}

/** A key's owners, each its text: an integer is its decimal spelling, as Go's Of takes it. */
export function ownerText(owner: Key): string | Uint8Array {
	if (typeof owner === 'number' || typeof owner === 'bigint') {
		if (typeof owner === 'number' && !Number.isSafeInteger(owner)) {
			throw new InvalidError(`the owner ${owner} is not an integer; an owner is text or an integer`)
		}
		return String(owner)
	}
	return owner
}
