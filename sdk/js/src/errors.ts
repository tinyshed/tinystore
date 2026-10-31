/**
 * What failed, as the server's error or the SDK's own says it. Each code is a
 * class of its own, so a caller catches `ConflictError` as a Go program asks
 * `errors.Is(err, tinystore.ErrConflict)`.
 */
export type Code =
	| 'invalid'
	| 'not_found'
	| 'limit'
	| 'closed'
	| 'in_use'
	| 'conflict'
	| 'corrupt'
	| 'too_old'
	| 'too_new'
	| 'suspended'
	| 'outcome_unknown'
	| 'committed_locally'
	| 'lost'
	| 'permission'
	| 'unimplemented'
	| 'cancelled'
	| 'unavailable'
	| 'internal'
	| 'protocol'
	| 'unauthenticated'

/**
 * The item a failure is about, by the names the server gives: a bucket and a
 * key, a queue and a key, a series' labels, a table and a constraint. A value
 * whose bytes are not UTF-8, as a key of bytes may be, comes as its bytes.
 */
export type What = Record<string, string | Uint8Array>

/**
 * A constraint a database's write broke, as SQLite names it: a unique or a
 * primary key already held is a ConflictError, any other an InvalidError.
 *
 *     if (err instanceof ConflictError && err.constraint?.columns.includes('email')) return 'that email is taken'
 */
export interface Constraint {
	kind: 'unique' | 'primaryKey' | 'foreignKey' | 'check' | 'notNull'
	/** the table, when SQLite names it */
	table: string | undefined
	/** the columns, when SQLite names them */
	columns: string[]
	/** a check's name, or its text when it has none; the index of a unique key on an expression */
	name: string | undefined
}

const constraintKinds: readonly string[] = [
	'unique',
	'primaryKey',
	'foreignKey',
	'check',
	'notNull',
]

export class TinystoreError extends Error {
	readonly code: Code
	readonly what: What
	/** the constraint a database's write broke; undefined for any other failure */
	readonly constraint: Constraint | undefined

	constructor(code: Code, message: string, what: What = {}) {
		super(message)
		this.name = new.target.name
		this.code = code
		this.what = what
		this.constraint = constraintOf(what)
	}
}

/** The constraint the server named in what, by its constraint, table, columns and name. */
function constraintOf(what: What): Constraint | undefined {
	const text = (name: string) => {
		const value = what[name]
		return typeof value === 'string' ? value : undefined
	}
	const kind = text('constraint')
	if (kind === undefined || !constraintKinds.includes(kind)) {
		return undefined
	}
	const columns = text('columns')
	return {
		kind: kind as Constraint['kind'],
		table: text('table'),
		columns: columns === undefined ? [] : columns.split(', '),
		name: text('name'),
	}
}

/** The request cannot be done as asked. */
export class InvalidError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('invalid', message, what)
	}
}

/**
 * A bound: memory, size or count. Later, or smaller, it may go through. The
 * error names the bound, what the call would have taken of it, and the
 * bound: `set_many documents`, 1001, 1000. `limits` holds the bounds a call
 * meets by those names.
 */
export class LimitError extends TinystoreError {
	readonly limit: string | undefined
	readonly wanted: number | undefined
	readonly bound: number | undefined

	constructor(message: string, what?: What) {
		super('limit', message, what)
		this.limit = typeof what?.limit === 'string' ? what.limit : undefined
		this.wanted = typeof what?.wanted === 'string' ? Number(what.wanted) : undefined
		this.bound = typeof what?.bound === 'string' ? Number(what.bound) : undefined
	}
}

/** A bound one call meets: its name, as a LimitError's `limit` says it, and the most it takes. */
export interface Limit {
	readonly name: string
	readonly bound: number
}

const limit = (name: string, bound: number): Limit => Object.freeze({ name, bound })

/**
 * The bounds one call meets, the same in every SDK and in Rust's
 * `tinystore::limits`. A program cuts its batches to them before it calls,
 * and tells which bound a LimitError met without reading its text:
 *
 * ```ts
 * for (let at = 0; at < courses.length; at += limits.setManyDocuments.bound) { … }
 * if (err instanceof LimitError && err.limit === limits.requestBytes.name) { … }
 * ```
 *
 * A bound the store's options set, its memory (`store memory`) or its
 * `maxSeries` (`series in a store`), is named the same way and is not here.
 */
export const limits = Object.freeze({
	/** A bucket's value, as it is stored. */
	valueBytes: limit('value bytes', 1 << 20),
	/** A live setting's change, as JSON. */
	settingBytes: limit('setting bytes', 16 << 10),
	/** Every change a config keeps, together. */
	configBytes: limit('config bytes', 256 << 10),
	/** The callers that wait for one lock or semaphore at once. */
	lockWaiters: limit('lock waiters', 10_000),
	/** The subscriptions to the store's channels at once. */
	subscriptions: limit('subscriptions', 10_000),
	/** A job's payload, its result or a step's answer, as JSON. */
	jobBytes: limit('job bytes', 1 << 20),
	/** A run's progress, as JSON. */
	progressBytes: limit('progress bytes', 4 << 10),
	/** The values one call reads from a database: past them, read a page at a time. */
	rowsBytes: limit("bytes of a call's rows", 64 << 20),
	/** One event, its name, fields and text together. */
	eventBytes: limit('event bytes', 256 << 10),
	/** The events of one append together. */
	appendBytes: limit('append bytes', 4 << 20),
	/** The rows one `aggregate` answers. */
	aggregateRows: limit('aggregate rows', 10_000),
	/** The different values one `top` or `distinct` holds, as JSON. */
	countBytes: limit('count bytes', 64 << 20),
	/** The samples of one `ingest`. */
	ingestSamples: limit('ingest samples', 10_000),
	/** The bytes of one `ingest`'s samples, each with its labels. */
	ingestBytes: limit('ingest bytes', 4 << 20),
	/** The samples of one exporter's text. */
	textSamples: limit('text samples', 100_000),
	/** One exporter's text, and the bytes of its samples. */
	textBytes: limit('text bytes', 32 << 20),
	/** The series one query finds. */
	querySeries: limit('series', 1000),
	/** What one query reads from the file. */
	queryBytes: limit('bytes read', 16 << 20),
	/** The samples one query decodes. */
	querySamples: limit('samples decoded', 1 << 20),
	/** The samples or buckets one query answers. */
	queryAnswer: limit('answer', 100_000),
	/** The documents of one `setMany`. */
	setManyDocuments: limit('set_many documents', 1000),
	/** The documents of one `setMany` as JSON; one request body, `requestBytes`, bounds it first. */
	setManyBytes: limit('set_many bytes', 16 << 20),
	/** One document as JSON, or a table's row as its index reads it. */
	documentBytes: limit('document bytes', 1 << 20),
	/** The hits a search reaches, its offset and its limit together. */
	searchHits: limit('search hits', 1000),
	/** A search's text, in bytes. */
	searchText: limit('search text', 1 << 10),
	/** The conditions of one search's filter. */
	filterConditions: limit('filter conditions', 8),
	/** The values the lists of one search's conditions hold together. */
	filterValues: limit('filter values', 100),
	/** A text a filter keeps or asks for, in bytes. */
	filterText: limit('filter text', 256),
	/** A vector's dimensions. */
	vectorDimensions: limit('vector dimensions', 4096),
	/** One request, its message as the SDK sends it: a larger one fails before it is sent. */
	requestBytes: limit('request bytes', 2 << 20),
	/** One answer, its message as the store sends it. */
	answerBytes: limit('answer bytes', 2 << 20),
})

/** What the call needs is not there: a file to copy or rename. */
export class NotFoundError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('not_found', message, what)
	}
}

/** The store, the handle or the connection closed. */
export class ClosedError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('closed', message, what)
	}
}

/** A name is taken. */
export class InUseError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('in_use', message, what)
	}
}

/** A condition or a version no longer holds: read again, then decide. */
export class ConflictError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('conflict', message, what)
	}
}

/** Stored bytes no longer read. */
export class CorruptError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('corrupt', message, what)
	}
}

/** A time before its engine's window. */
export class TooOldError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('too_old', message, what)
	}
}

/** A time past its engine's window, ahead of the store's clock. */
export class TooNewError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('too_new', message, what)
	}
}

/** A metrics series in quarantine until its repair. */
export class SuspendedError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('suspended', message, what)
	}
}

/**
 * A write whose commit may or may not have happened: the connection was lost
 * while it was in flight, or the server's commit failed. Read what it wrote
 * before writing again.
 */
export class OutcomeUnknownError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('outcome_unknown', message, what)
	}
}

/**
 * A synchronous commit its home made that no copy it waits for held in time:
 * the write stands, and losing that home's disk would lose it.
 */
export class CommittedLocallyError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('committed_locally', message, what)
	}
}

/** The connection's capability does not allow it: a data token changing a schema. */
export class PermissionDeniedError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('permission', message, what)
	}
}

/** A method the server does not have. */
export class UnimplementedError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('unimplemented', message, what)
	}
}

/** The call was cancelled by its signal. */
export class CancelledError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('cancelled', message, what)
	}
}

/** The server is closing; the call was not run and may be sent on another connection. */
export class UnavailableError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('unavailable', message, what)
	}
}

/** A fault of the server's own. */
export class InternalError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('internal', message, what)
	}
}

/**
 * A lease that ran out or was revoked at its maxHold while its function ran:
 * another holder may have run beside it. The function's work stands, and a
 * system that compares fences refused what came late.
 */
export class LostError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('lost', message, what)
	}
}

/** A frame or a message that breaks the protocol; the connection ends with it. */
export class ProtocolError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('protocol', message, what)
	}
}

/** A remote connection whose token the server does not know. */
export class UnauthenticatedError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('unauthenticated', message, what)
	}
}

const classes: Record<Code, new (message: string, what?: What) => TinystoreError> = {
	invalid: InvalidError,
	not_found: NotFoundError,
	limit: LimitError,
	closed: ClosedError,
	in_use: InUseError,
	conflict: ConflictError,
	corrupt: CorruptError,
	too_old: TooOldError,
	too_new: TooNewError,
	suspended: SuspendedError,
	outcome_unknown: OutcomeUnknownError,
	committed_locally: CommittedLocallyError,
	lost: LostError,
	permission: PermissionDeniedError,
	unimplemented: UnimplementedError,
	cancelled: CancelledError,
	unavailable: UnavailableError,
	internal: InternalError,
	protocol: ProtocolError,
	unauthenticated: UnauthenticatedError,
}

/** The codes this SDK knows, as `messages.json` lists the server's. */
export const codes = Object.keys(classes) as Code[]

/**
 * The error a code names. A code this SDK does not know yet is an
 * `InternalError` keeping the code in its message.
 */
export function errorOf(code: string, message: string, what: What = {}): TinystoreError {
	const known = classes[code as Code]
	if (known === undefined) {
		return new InternalError(`${code}: ${message}`, what)
	}
	return new known(message, what)
}
