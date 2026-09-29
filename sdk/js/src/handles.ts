import type { Connection } from './connection.ts'
import { InvalidError } from './errors.ts'
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

/** A page of a scan: its items, and where the next begins when more remain. */
export interface Page<T, After> {
	items: T[]
	/** the place the next page begins after; undefined once the scan has ended */
	next: After | undefined
}
