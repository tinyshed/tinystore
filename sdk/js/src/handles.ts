import type { Connection } from './connection.ts'
import { InvalidError } from './errors.ts'
import type { Key } from './wire/codec.ts'
import { Handle } from './wire/messages.ts'

/**
 * The handle an open answers on a connection, opened once a connection: the
 * open's own bytes name it, so a bucket opened twice with the same options
 * shares its handle. An open that fails is tried again by the next call.
 */
export function handleOn(
	connection: Connection,
	method: number,
	open: Uint8Array,
): Promise<number> {
	const key = `${method}:${Buffer.from(open).toString('base64')}`
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
