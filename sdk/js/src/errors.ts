/**
 * What failed, as the server's error or the SDK's own says it. Each code is a
 * class of its own, so a caller catches `ConflictError` as a Go program asks
 * `errors.Is(err, tinystore.ErrConflict)`.
 */
export type Code =
	| 'invalid'
	| 'limit'
	| 'closed'
	| 'in_use'
	| 'conflict'
	| 'corrupt'
	| 'too_old'
	| 'too_new'
	| 'suspended'
	| 'outcome_unknown'
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

export class TinystoreError extends Error {
	readonly code: Code
	readonly what: What

	constructor(code: Code, message: string, what: What = {}) {
		super(message)
		this.name = new.target.name
		this.code = code
		this.what = what
	}
}

/** The request cannot be done as asked. */
export class InvalidError extends TinystoreError {
	constructor(message: string, what?: What) {
		super('invalid', message, what)
	}
}

/**
 * A bound: memory, size or count. Later, or smaller, it may go through. A
 * limit the store names says which, what the call would have taken of it,
 * and the bound: `decoded samples`, 120000, 100000.
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

/**
 * The names a LimitError's `limit` holds, as Go's constants and Python's
 * `tinystore.limits` spell them, to map a limit without matching its text.
 *
 * ```ts
 * if (err instanceof LimitError && err.limit === limits.decodedSamples) { … }
 * ```
 */
export const limits = Object.freeze({
	storeMemory: 'store memory',
	storeMemoryNow: 'store memory, now',
	matchedSeries: 'matched series',
	decodedBlocks: 'decoded blocks',
	fetchedBytes: 'fetched bytes',
	decodedSamples: 'decoded samples',
	outputSamples: 'output samples',
	outputBuckets: 'output buckets',
	recordBytes: "bytes of a record, a block's",
	appendBytes: "bytes of records in one Append, a segment's; split it",
	jobValueBytes: "bytes of a job's value",
	stepBytes: "bytes of a step's answer",
	objectBytes: "bytes of an object, the bucket's MaxSize",
} as const)

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
	limit: LimitError,
	closed: ClosedError,
	in_use: InUseError,
	conflict: ConflictError,
	corrupt: CorruptError,
	too_old: TooOldError,
	too_new: TooNewError,
	suspended: SuspendedError,
	outcome_unknown: OutcomeUnknownError,
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
