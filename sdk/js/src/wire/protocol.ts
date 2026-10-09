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
	'jobs.queue.open': 0x0201,
	'jobs.schedule.open': 0x0202,
	'jobs.add': 0x0203,
	'jobs.set': 0x0204,
	'jobs.update': 0x0205,
	'jobs.cancel': 0x0206,
	'jobs.get': 0x0207,
	'jobs.list': 0x0208,
	'jobs.work': 0x0209,
	'jobs.step': 0x020a,
	'jobs.keep': 0x020b,
	'jobs.watch': 0x020c,
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
	'server.stop': 0x0001,
	'server.clock': 0x0002,
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
 * How many jobs of a queue run at once: in all, across every worker of the
 * store, and in each group.
 */
export const JobsConcurrency = message('jobs.Concurrency', {
	total: [1, uint],
	group: [2, uint],
})

/**
 * The wait before a retry: initial, doubling each time up to max.
 */
export const JobsBackoff = message('jobs.Backoff', {
	initial: [1, uint],
	max: [2, uint],
})

/**
 * Jobs started in any span of per.
 */
export const JobsRate = message('jobs.Rate', {
	count: [1, uint],
	per: [2, uint],
})

/**
 * Opens a queue; an option left out takes its default.
 */
export const JobsQueueOpen = message('jobs.QueueOpen', {
	/** [a-z0-9][a-z0-9_-]{0,63} */
	name: [1, str],
	/** 10 when absent, the first run counted */
	attempts: [2, uint],
	/** 1 s doubling to 1 h when absent */
	backoff: [3, JobsBackoff],
	/** how long one run may take: a minute when absent */
	timeout: [4, uint],
	/** one at a time in each worker when absent */
	concurrency: [5, JobsConcurrency],
	rate: [6, JobsRate],
	/** how long a done job's id stays taken */
	dedupe: [7, uint],
	/** how long a failed job stays: a week when absent */
	keep: [8, uint],
	/** ten million when absent */
	maxWaiting: [9, uint],
})

/**
 * Opens a schedule: one repeating job the code owns, under its name, whose
 * repeat replaces the one kept.
 */
export const JobsScheduleOpen = message('jobs.ScheduleOpen', {
	name: [1, str],
	every: [2, uint],
	/** five fields, with timeZone */
	cron: [3, str],
	/** an IANA name, UTC among them */
	timeZone: [4, str],
	attempts: [5, uint],
	backoff: [6, JobsBackoff],
	timeout: [7, uint],
})

/**
 * A call on one job: an add, a set or an update.
 */
export const JobsCall = message('jobs.Call', {
	handle: [1, uint],
	/** 1 to 1024 bytes; a set and an update need one */
	id: [2, str],
	value: [3, str],
	at: [4, int],
	/** from now; not beside at */
	delay: [5, uint],
	group: [6, str],
	/** a repeat, which needs an id */
	every: [7, uint],
	cron: [8, str],
	timeZone: [9, str],
})

/**
 * The job under an id.
 */
export const JobsId = message('jobs.Id', {
	handle: [1, uint],
	id: [2, str],
})

/**
 * Whether a call did what it asked: an add added, an update changed, a cancel
 * found a job.
 */
export const JobsChanged = message('jobs.Changed', {
	changed: [1, bool],
})

/**
 * A job as its queue holds it.
 */
export const JobsJob = message('jobs.Job', {
	found: [1, bool],
	id: [2, str],
	value: [3, str],
	/** scheduled, waiting, running, done, failed or cancelled */
	state: [4, str],
	/** when it runs next; for one done or failed, when its last run was for */
	at: [5, int],
	attempt: [6, uint],
	/** the jobs that run before a waiting one, up to 10,000 */
	ahead: [7, uint],
	progress: [8, str],
	error: [9, str],
	group: [10, str],
	/** as the job keeps it: @every 30s +6178ms, 10 3 * * * Europe/Berlin */
	repeat: [11, str],
	/** the last run a handler finished */
	startedAt: [12, int],
	endedAt: [13, int],
})

/**
 * A page of a queue's jobs: those whose ids start with a prefix, in the byte
 * order of their ids, or with no prefix and the failed state, the last failed
 * first.
 */
export const JobsList = message('jobs.List', {
	handle: [1, uint],
	prefix: [2, str],
	/** scheduled, waiting, running or failed */
	state: [3, str],
	/** the page before's next */
	after: [4, str],
	/** 100 when absent, 1000 at most */
	limit: [5, uint],
})

export const JobsPage = message('jobs.Page', {
	jobs: [1, list(JobsJob)],
	next: [2, str],
})

/**
 * Starts the queue's worker for the client: the server claims its jobs as they
 * fall due and hands each over as a held job, no more at once than the
 * client's handlers, and the client answers each. A stop, or the server's
 * GOAWAY, hands no job more, and the server ends the stream with DATA·END once
 * the jobs the client holds are answered and written. The client's DATA·END
 * ends the worker at once, the attempt of a job it still holds failing.
 */
export const JobsWork = message('jobs.Work', {
	handle: [1, uint],
	/** the handlers the client runs at once, 1024 at most: the queue's total, or one */
	concurrency: [2, uint],
	/** runDue: the server ends the stream once no job is due and none is held */
	untilIdle: [3, bool],
})

/**
 * A job the server hands the client's worker. Its run's number names it to the
 * answer, a step and a keep; a cancel sends it again, cancelled, so that its
 * handler stops.
 */
export const JobsHeld = message('jobs.Held', {
	run: [1, uint],
	id: [2, str],
	value: [3, str],
	/** when the run was due */
	at: [4, int],
	/** the first being 1 */
	attempt: [5, uint],
	group: [6, str],
	cancelled: [7, bool],
})

/**
 * The client's answer for a held job: how its run ended, or how far it got,
 * which settles nothing; or a stop, which names no run. An answer for a job a
 * cancel took settles nothing.
 */
export const JobsAnswer = message('jobs.Answer', {
	run: [1, uint],
	/** done, retry, snooze, fail, back, progress or stop */
	how: [2, str],
	/** when a retry or a snooze runs again; a retry without one waits its backoff */
	at: [3, int],
	/** why a retry or a failure */
	error: [4, str],
	/** 4 KiB at most */
	progress: [5, str],
	/** from now on the server's clock; not beside at */
	delay: [6, uint],
})

/**
 * A step of a held job's run: jobs.step asks for its kept answer, jobs.keep
 * keeps one.
 */
export const JobsStep = message('jobs.Step', {
	run: [1, uint],
	/** 1 to 256 bytes */
	name: [2, str],
	/** jobs.keep's: 1 MiB at most */
	answer: [3, str],
})

export const JobsKept = message('jobs.Kept', {
	found: [1, bool],
	answer: [2, str],
})

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
	/** kept in memory and written every span; each add written when absent */
	flushEvery: [3, uint],
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

/**
 * A private server's clock: a time to set it to, a span to move it forward by,
 * or neither to read it. The answer is the time it reads once moved.
 */
export const ServerClock = message('server.Clock', {
	at: [1, int],
	advance: [2, uint],
})
