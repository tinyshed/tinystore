// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.

import {
	bin,
	bool,
	float,
	int,
	int64,
	key,
	kvValue,
	list,
	message,
	names,
	str,
	uint,
} from './codec.ts'

export const methods = {
	'kv.bucket.open': 0x0101,
	'kv.get': 0x0102,
	'kv.has': 0x0103,
	'kv.set': 0x0104,
	'kv.create': 0x0105,
	'kv.take': 0x0106,
	'kv.delete': 0x0107,
	'kv.expire': 0x0108,
	'kv.clear': 0x0109,
	'kv.list': 0x010a,
	'kv.counters.open': 0x0110,
	'kv.counters.add': 0x0111,
	'kv.counters.get': 0x0112,
	'kv.counters.delete': 0x0113,
	'kv.counters.clear': 0x0114,
	'kv.rateLimit.open': 0x0120,
	'kv.quota.open': 0x0121,
	'kv.allow': 0x0122,
	'kv.peek': 0x0123,
	'kv.reset': 0x0124,
	'kv.refund': 0x0125,
	'kv.once.open': 0x0130,
	'kv.once.run': 0x0131,
	'kv.once.get': 0x0132,
	'kv.once.delete': 0x0133,
	'kv.tx': 0x0140,
} as const

export type Method = keyof typeof methods

/**
 * What a client says first.
 */
export const Hello = message('Hello', {
	/** the newest the client speaks */
	protocol: [1, uint],
	/** a name and version, for logs */
	client: [2, str],
	/** required on TCP */
	token: [3, str],
	/** the largest body the client takes; the server's when absent */
	maxBody: [4, uint],
	/** the DATA the server may send a stream before the client grants more; 2 MiB when absent */
	streamCredit: [5, uint],
	/** 16 random bytes, when the client found the server through SERVE */
	challenge: [6, bin],
})

/**
 * What a server answers HELLO with: what the connection agrees.
 */
export const Welcome = message('Welcome', {
	/** the one this connection speaks */
	protocol: [1, uint],
	/** its version */
	server: [2, str],
	/** 16 random bytes a start */
	instance: [3, bin],
	/** admin or data */
	capability: [4, str],
	/** the largest body either side sends */
	maxBody: [5, uint],
	/** the streams a client may have open at once */
	inFlight: [6, uint],
	/** the REQUEST and DATA bytes a client may send before credit comes back */
	connectionCredit: [7, uint],
	/** the DATA bytes a client may send a stream before credit comes back */
	streamCredit: [8, uint],
	/** what this server serves */
	engines: [9, list(str)],
	/** the store's clock */
	now: [10, int],
	/** the HMAC-SHA256 of HELLO's challenge, keyed with SERVE's secret */
	proof: [11, bin],
})

/**
 * The last frame of a connection.
 */
export const GoAway = message('GoAway', {
	code: [1, str],
	message: [2, str],
})

/**
 * A stream's last frame when it failed, with ERROR set: its kind's code, what
 * failed, and the item it names. A message refused is one too.
 */
export const Failure = message('Failure', {
	code: [1, str],
	message: [2, str],
	what: [3, names(str)],
})

/**
 * What a call that opens something answers: its handle, which later calls name.
 */
export const Handle = message('Handle', {
	handle: [1, uint],
})

/**
 * A message with nothing to say.
 */
export const Empty = message('Empty', {})

/**
 * Opens a bucket of values by key. Its keys expire ttl after they are written,
 * or idle after they were last read or written; not both.
 */
export const KvBucketOpen = message('kv.BucketOpen', {
	/** [a-z0-9][a-z0-9_-]{0,63} */
	name: [1, str],
	ttl: [2, uint],
	idle: [3, uint],
})

/**
 * A call on one key of a handle's branch.
 */
export const KvCall = message('kv.Call', {
	handle: [1, uint],
	/** the branch's owners, outermost first; the bucket's root when empty */
	under: [2, list(key)],
	key: [3, key],
	/** what a set or a create writes */
	value: [4, kvValue],
	/** the expiry a write gives, from now */
	ttl: [5, uint],
	/** the expiry a write gives, as a time */
	expiresAt: [6, int],
	/** the version the key must still be at */
	ifVersion: [7, bin],
	/** what a counter adds; the requests or uses a limit is asked for, 1 when absent */
	n: [8, int64],
})

/**
 * A key's value with what a conditional write needs.
 */
export const KvEntry = message('kv.Entry', {
	found: [1, bool],
	value: [2, kvValue],
	/** compared only for equality */
	version: [3, bin],
	/** absent for a key that never expires */
	expiresAt: [4, int],
	/** a page's entry alone */
	key: [5, key],
})

/**
 * What a write left: whether it wrote, and the version and expiry the key has
 * now, the live key's own when a create found one.
 */
export const KvWritten = message('kv.Written', {
	written: [1, bool],
	version: [2, bin],
	expiresAt: [3, int],
})

export const KvFound = message('kv.Found', {
	found: [1, bool],
})

/**
 * A branch of a handle, for a clear.
 */
export const KvBranch = message('kv.Branch', {
	handle: [1, uint],
	under: [2, list(key)],
})

/**
 * A page of a branch's own keys, in the byte order of their text.
 */
export const KvList = message('kv.List', {
	handle: [1, uint],
	under: [2, list(key)],
	/** the key the page starts after */
	after: [3, key],
	/** 100 when absent, 1000 at most */
	limit: [4, uint],
})

/**
 * A page of entries, as many as the limit asks and the body holds, and where
 * the next page starts, when there is one.
 */
export const KvPage = message('kv.Page', {
	entries: [1, list(KvEntry)],
	next: [2, key],
})

/**
 * Opens counters: numbers by key that only add up.
 */
export const KvCountersOpen = message('kv.CountersOpen', {
	name: [1, str],
	/** a counter lasts this long from its first add */
	ttl: [2, uint],
	/** kept in memory and written every interval; each add written when absent */
	durability: [3, uint],
})

export const KvCount = message('kv.Count', {
	value: [1, int64],
})

/**
 * Opens a rate limit: rate requests a key every per, burst at once.
 */
export const KvRateLimitOpen = message('kv.RateLimitOpen', {
	name: [1, str],
	rate: [2, uint],
	per: [3, uint],
	/** the rate when absent */
	burst: [4, uint],
})

/**
 * One window of a quota: a key may use up to limit every per from its first use.
 */
export const KvWindow = message('kv.Window', {
	/** [a-z][a-z0-9_]{0,31} */
	name: [1, str],
	limit: [2, uint],
	per: [3, uint],
})

export const KvQuotaOpen = message('kv.QuotaOpen', {
	name: [1, str],
	/** one to eight */
	windows: [2, list(KvWindow)],
})

/**
 * One window of a quota as a key stands in it.
 */
export const KvWindowUse = message('kv.WindowUse', {
	name: [1, str],
	used: [2, uint],
	limit: [3, uint],
	left: [4, uint],
	/** absent for a window not started */
	resetsAt: [5, int],
})

/**
 * What a rate limit or a quota answers a request.
 */
export const KvAllowance = message('kv.Allowance', {
	ok: [1, bool],
	/** how many more would pass now */
	left: [2, uint],
	/** when the request would pass; absent when it did */
	retryAt: [3, int],
	/** a quota's, in the order it names them */
	windows: [4, list(KvWindowUse)],
})

/**
 * Opens once's answers: a function run once a key, its answer kept.
 */
export const KvOnceOpen = message('kv.OnceOpen', {
	name: [1, str],
	/** a day when absent */
	keep: [2, uint],
})

/**
 * An answer of once: in the server's RESPONSE, found when one was kept, and
 * otherwise the run is the client's; in the client's last DATA, found with the
 * answer to keep, or not found when the function failed.
 */
export const KvAnswer = message('kv.Answer', {
	found: [1, bool],
	value: [2, kvValue],
})

/**
 * A read a transaction made, which its commit checks: the key still at the
 * version it was found at, or still absent.
 */
export const KvCheck = message('kv.Check', {
	handle: [1, uint],
	under: [2, list(key)],
	key: [3, key],
	/** absent: the read found nothing */
	version: [4, bin],
})

/**
 * One write of a transaction: the method it is, and its call.
 */
export const KvOp = message('kv.Op', {
	/** kv.set, kv.create, kv.take, kv.delete, kv.expire, kv.clear or kv.counters.add */
	method: [1, uint],
	call: [2, KvCall],
})

/**
 * A transaction across the wire: its reads' checks, then its writes, all
 * applied or none. A failed check or write names its place in what.
 */
export const KvTx = message('kv.Tx', {
	checks: [1, list(KvCheck)],
	writes: [2, list(KvOp)],
})

/**
 * What one write of a transaction did.
 */
export const KvOutcome = message('kv.Outcome', {
	/** a take, a delete or an expire found a live key */
	found: [1, bool],
	/** a set or a create wrote */
	written: [2, bool],
	/** what a take took */
	value: [3, kvValue],
	version: [4, bin],
	expiresAt: [5, int],
	/** a counter's value after its add */
	count: [6, int64],
})

export const KvTxResults = message('kv.TxResults', {
	outcomes: [1, list(KvOutcome)],
})
