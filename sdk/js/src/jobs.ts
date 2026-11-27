// Work an application must do later, or now but outside the request that
// asked for it, as docs/guides/jobs/README.md has it: queues of JSON values run in the
// order of their time, ids that find a job again, repeats, schedules and
// limits. A worker's handlers run here while the server's loop claims the
// queue's jobs and hands each over on one stream, the answers going back on
// it. A handle opens with its first call on a connection.

import { FailureLog, messageOf } from './background.ts'
import type { Connection, Idempotence, Link } from './connection.ts'
import { CancelledError, CorruptError, errorOf, InvalidError, UnavailableError } from './errors.ts'
import { checkName, type Home, handleOn, type Open, type Via, viaLink } from './handles.ts'
import { type Rate, rateOf } from './limits.ts'
import { sayOwn } from './logger.ts'
import { check, type StandardSchemaV1 } from './schema.ts'
import { ConnectionLostError, type Stream } from './session.ts'
import { type Calendar, type Duration, ms, type Time, unixMs } from './time.ts'
import type { Read, Written } from './wire/codec.ts'
import {
	Handle,
	JobsAnswer,
	JobsCall,
	JobsChanged,
	JobsCounts,
	JobsGather,
	JobsGathered,
	JobsHeld,
	JobsId,
	JobsJob,
	JobsKept,
	JobsList,
	JobsPage,
	JobsQueueOpen,
	JobsScheduleOpen,
	JobsStep,
	JobsTx,
	JobsWork,
	methods,
} from './wire/protocol.ts'

/** How many jobs of a queue run at once: in all, across every worker of the store, and in each group. */
export interface Concurrency {
	total?: number | undefined
	/** at most this many jobs of one group at once: one customer's backlog holds back no other */
	group?: number | undefined
}

/** The wait before a retry: initial, doubling each time up to max, a tenth longer or shorter at random. */
export interface Backoff {
	initial: Duration
	max: Duration
}

export interface QueueOptions {
	/** runs a job gets before it fails for good, the first counted: 10 */
	attempts?: number | undefined
	/** 1s doubling up to 1h */
	backoff?: Backoff | undefined
	/** how long one run may take before its signal aborts: a minute */
	timeout?: Duration | undefined
	/** jobs running at once across every worker of the store: 8, or { total: 8, group: 2 }; one a worker unless given */
	concurrency?: number | Concurrency | undefined
	/** jobs started in any span: '30/s' */
	rate?: Rate | undefined
	/** jobs of each group started in any span, beside rate: '2/s', each account's own limit; it lives in memory */
	groupRate?: Rate | undefined
	/** how long a done job's id stays taken, so that an add of it adds nothing: '10m', or { calendar: 'day', timeZone } */
	dedupe?: Dedupe | undefined
	/** how long a failed job stays, to be found and started again: a week; '0s' not at all */
	keep?: Duration | undefined
	/** jobs that may wait; an add past it is LimitError: ten million */
	maxWaiting?: number | undefined
	/** hours in which the queue's jobs do not run: a time inside them, a retry's or a repeat's too, becomes their end */
	quiet?: Quiet | undefined
}

/**
 * Hours in which a job does not run, two wall-clock times of a day in a time
 * zone: `{ from: '23:00', to: '07:00', timeZone: 'Europe/Moscow' }`. A from
 * after to wraps midnight; the same two, or a zone no one has, is InvalidError.
 */
export interface Quiet {
	from: string
	to: string
	timeZone: string
}

/**
 * How long a done job holds its id: a span from when it was done, '10m', or
 * until the local day, week or month it was done in ends, in its zone:
 * `{ calendar: 'day', timeZone: 'Europe/Moscow' }`.
 */
export type Dedupe = Duration | { calendar: Calendar; timeZone: string }

/** A job's time, group, repeat and options of its own, as an add, a set and an update take them. */
export interface JobOptions {
	/** runs at this time, one past now; not beside delay */
	at?: Time | undefined
	/** runs this long from now */
	delay?: Duration | undefined
	/** the group whose running jobs the queue's concurrency.group bounds */
	group?: string | undefined
	/** repeats every span, each id at a phase of its own within it; needs an id */
	every?: Duration | undefined
	/** repeats by five cron fields, or @daily and its kind, on timeZone's wall clock; needs an id */
	cron?: string | undefined
	/** an IANA name, 'UTC' among them, which a cron needs: one given as undefined is InvalidError, never UTC */
	timeZone?: string | undefined
	/** how long this job holds its id once done, in the place of its queue's dedupe */
	dedupe?: Dedupe | undefined
	/** how long this job is kept once it failed for good, in the place of its queue's: '0s' not at all */
	keep?: Duration | undefined
	/** it leaves its queue this long after its last write unless written again: an upsert, a set or an update renews it */
	ttl?: Duration | undefined
	/** it leaves its queue at this time; not beside ttl */
	expiresAt?: Time | undefined
	/** this job's own hours in which it does not run, in the place of its queue's; false: no hours hold it */
	quiet?: Quiet | false | undefined
}

export interface AddOptions extends JobOptions {
	/** the job's id, 1 to 1024 bytes: what finds it again, and adds it once */
	id?: string | undefined
}

/** How a gather waits, and the options of its own a job it adds or changes takes. */
export interface GatherOptions {
	/** the job runs this long after the last gather under its id */
	settle: Duration
	/** and never later than this long after the gather that started its list */
	max?: Duration | undefined
	/** the group whose running jobs the queue's concurrency.group bounds */
	group?: string | undefined
	/** how long this job holds its id once done, in the place of its queue's dedupe */
	dedupe?: Dedupe | undefined
	/** how long this job is kept once it failed for good, in the place of its queue's: '0s' not at all */
	keep?: Duration | undefined
	/** it leaves its queue this long after its last write unless written again */
	ttl?: Duration | undefined
	/** it leaves its queue at this time; not beside ttl */
	expiresAt?: Time | undefined
	/** this job's own hours in which it does not run, in the place of its queue's; false: no hours hold it */
	quiet?: Quiet | false | undefined
}

/** What a gather did: whether it started the job, the items it holds now, and when it is due. */
export interface Gathered {
	added: boolean
	/** 0 when a done job its dedupe holds keeps the id, and nothing was gathered */
	count: number
	at: Date
}

/** done while dedupe keeps its id; cancelled only a watcher sees */
export type JobState = 'scheduled' | 'waiting' | 'running' | 'done' | 'failed' | 'cancelled'

/** A job as its queue holds it. */
export interface Job<T> {
	id: string | undefined
	value: T
	state: JobState
	/** when it runs next; for one done or failed, when its last run was for */
	at: Date
	/** its attempts, a running one included */
	attempt: number
	/** the jobs that run before a waiting one, exactly up to 10,000 */
	ahead: number
	/** counting `ahead` stopped at 10,000, more being there: past it `ahead` is 10,000, never an estimate */
	aheadAtLeast?: boolean
	/** what its handler last reported of a running job */
	progress: unknown
	/** why its last run failed */
	error: string | undefined
	group: string | undefined
	/** a repeating job's repeat as it keeps it: '@every 30s +6178ms', '10 3 * * * Europe/Berlin' */
	repeat: string | undefined
	/** when the last run a handler finished started and ended */
	lastRun: { startedAt: Date; endedAt: Date } | undefined
	/** when it leaves its queue unless written again: its ttl's end, or its expiresAt */
	expiresAt: Date | undefined
}

export interface ListOptions {
	/** ids that start with it, in their byte order: end it with a separator, 'chat:4:' */
	prefix?: string | undefined
	/** the jobs in this state alone; 'failed' with no prefix lists the last failed first */
	state?: 'scheduled' | 'waiting' | 'running' | 'failed' | undefined
	/** where the page before ended: its next */
	after?: string | undefined
	/** jobs a page holds at most: 100, 1000 at most */
	limit?: number | undefined
}

/** How many jobs a queue holds in each state, from one snapshot; a done job has left it. */
export interface JobCounts {
	/** due later */
	scheduled: number
	/** due, and no handler runs it yet */
	waiting: number
	running: number
	/** kept for the queue's keep */
	failed: number
}

/**
 * Where a job without an id lies, which a cancel names it by: kept on the job
 * a page gave, out of sight, and not on a copy of it.
 */
const ROW = Symbol('tinystore.job.row')

/** What a cancel sends of a job: its id, or for one without, its row. */
function namedBy(job: Job<unknown> | string, describe: string): { id?: string; row?: string } {
	if (typeof job === 'string') {
		return { id: job }
	}
	if (job.id !== undefined) {
		return { id: job.id }
	}
	const row = (job as { [ROW]?: string })[ROW]
	if (row === undefined) {
		throw new InvalidError(
			`${describe}: a job without an id names no row: cancel the job a list gave, not a copy`,
		)
	}
	return { row }
}

/** A page of a queue's jobs, and where the next starts when there is one. */
export interface JobPage<T> {
	jobs: Job<T>[]
	next: string | undefined
}

export interface WorkOptions {
	/** the handlers this worker runs at once, under the queue's bound: its total, or one */
	concurrency?: number | undefined
}

/**
 * What runs a job: it gets the job's value and its run. Returning ends the
 * job done, throwing runs it again after its backoff, and returning what
 * run.retry, run.snooze or run.fail made ends it so.
 */
export type Handler<T> = (value: T, run: Run) => unknown

/** When a schedule runs: every span, or by a cron on a time zone's wall clock. */
export type ScheduleWhen = { every: Duration } | { cron: string; timeZone: string }

export interface ScheduleOptions {
	/** runs a time gets before it fails, the first counted: 10; the schedule goes on */
	attempts?: number | undefined
	backoff?: Backoff | undefined
	/** how long one run may take before its signal aborts: a minute */
	timeout?: Duration | undefined
}

/** A run's answer as the stream carries it. */
type AnswerFields = Omit<Written<typeof JobsAnswer.fields>, 'run'>

/** The handlers a client's worker runs at once at most, as the server bounds them. */
const mostHandlers = 1024

/** The pause before a worker dials again after a failure, doubling up to the last. */
const firstPause = 100
const lastPause = 30_000

/** How a run ends otherwise than done, made by run.retry, run.snooze or run.fail: its handler returns it. */
export class Answer {
	readonly #how: string

	/** Answers come from a run. */
	constructor(how: string) {
		this.#how = how
	}

	toString(): string {
		return `run.${this.#how}(…)`
	}
}

/** What a run is made of, which its worker gives it. */
interface RunParts {
	held: Read<typeof JobsHeld.fields>
	signal: AbortSignal
	steps: Steps
	reporter: Reporter
	/** the answers the run made, which the worker reads what the handler returned against */
	made: Map<Answer, AnswerFields>
}

/** This call on a job: what it is, its signal, its steps and progress, and the answers its handler returns. */
export class Run {
	/** the job's id, undefined for one added without */
	readonly id: string | undefined
	/** when the run was due, which "thirty days ago" counts from even when it starts late */
	readonly at: Date
	/** this run's number among the job's attempts, the first being 1 */
	readonly attempt: number
	readonly group: string | undefined
	/**
	 * aborts when the run passes its timeout, or when a cancel takes the job,
	 * whose reason is then a CancelledError: the handler should stop too
	 */
	readonly signal: AbortSignal
	readonly #steps: Steps
	readonly #reporter: Reporter
	readonly #made: Map<Answer, AnswerFields>

	/** Runs come to a handler from its worker. */
	constructor(parts: RunParts) {
		this.id = parts.held.id
		this.at = new Date(parts.held.at ?? 0)
		this.attempt = parts.held.attempt ?? 0
		this.group = parts.held.group
		this.signal = parts.signal
		this.#steps = parts.steps
		this.#reporter = parts.reporter
		this.#made = parts.made
	}

	/**
	 * Runs fn once in the job's run and keeps its answer, as JSON, across the
	 * job's attempts: the attempt after a retry, a lost worker or a restart
	 * gets the answer back without running fn again. A step whose attempt
	 * ends before its answer is kept runs again, so what fn does outside the
	 * store should bear doing twice. A name is the step's within its job,
	 * numbered in a loop: 'model:1', 'tool:1'.
	 *
	 *     const found = await run.step('search', () => search(question.text))
	 */
	step<R>(name: string, fn: () => R | Promise<R>): Promise<R> {
		return this.#steps.run(name, fn)
	}

	/**
	 * Shows how far the job got, any JSON within 4 KiB, to get: the latest
	 * goes, ten a second at most, and nothing waits for the server.
	 */
	setProgress(progress: unknown): void {
		this.#reporter.report(progressText(progress))
	}

	/** Runs the job again after its backoff, or at the time given, the run counted as an attempt: return it. */
	retry(when?: Duration | Date): Answer {
		return this.#make('retry', when === undefined ? {} : timing(when))
	}

	/** Runs the job again at the time given, the run not counted: return it. */
	snooze(when: Duration | Date): Answer {
		return this.#make('snooze', timing(when))
	}

	/** Fails the job for good now, its reason kept as its error: return it. */
	fail(reason: string | Error): Answer {
		return this.#make('fail', { error: messageOf(reason) })
	}

	#make(how: 'retry' | 'snooze' | 'fail', fields: AnswerFields): Answer {
		const answer = new Answer(how)
		this.#made.set(answer, { ...fields, how })
		return answer
	}
}

/** A retry's or a snooze's time: a Date at it, a span from now on the server's clock. */
function timing(when: Duration | Date): AnswerFields {
	return when instanceof Date ? { at: unixMs(when) } : { delay: ms(when) }
}

/** How a handler's return ends its run: an answer it made, done for anything else, and a failed run for an answer made and not returned. */
function answerOf(returned: unknown, made: Map<Answer, AnswerFields>): AnswerFields {
	if (returned instanceof Answer) {
		return (
			made.get(returned) ?? {
				how: 'retry',
				error: 'the handler returned an answer another run made',
			}
		)
	}
	const [unreturned] = made.keys()
	if (unreturned !== undefined) {
		return {
			how: 'retry',
			error: `the handler made ${unreturned} and returned something else: return the answer that ends the run`,
		}
	}
	return { how: 'done' }
}

/** The JSON a handler reports of its job, 4 KiB of it at most. */
function progressText(progress: unknown): string {
	const text = jsonText(progress, "a job's progress")
	if (new TextEncoder().encode(text).length > 4096) {
		throw new InvalidError("a job's progress past 4 KiB of JSON")
	}
	return text
}

/**
 * Sends what a handler reports of its job: the latest, one send at a time
 * and at most one each 100 ms, so that a handler reporting each chunk it
 * reads costs the connection ten frames a second.
 */
class Reporter {
	readonly #send: (text: string) => Promise<void>
	#latest: string | undefined
	#sending: Promise<void> | undefined
	#timer: ReturnType<typeof setTimeout> | undefined
	#sent = 0
	#stopped = false

	constructor(send: (text: string) => Promise<void>) {
		this.#send = send
	}

	report(text: string): void {
		this.#latest = text
		this.#flush()
	}

	#flush(): void {
		if (this.#stopped || this.#sending !== undefined || this.#latest === undefined) {
			return
		}
		const wait = this.#sent + 100 - Date.now()
		if (wait > 0) {
			this.#timer ??= setTimeout(() => {
				this.#timer = undefined
				this.#flush()
			}, wait)
			return
		}
		const text = this.#latest
		this.#latest = undefined
		this.#sent = Date.now()
		this.#sending = this.#send(text)
			.catch(() => {})
			.finally(() => {
				this.#sending = undefined
				this.#flush()
			})
	}

	/** Drops what waits and lets what is being sent finish, before the run's answer follows it. */
	async stop(): Promise<void> {
		this.#stopped = true
		clearTimeout(this.#timer)
		await this.#sending
	}
}

/** Calls a method on the connection a job is held on, answering its body. */
type Call = (method: number, body: Uint8Array) => Promise<Uint8Array>

/** The steps of a held job's run, which the server keeps under the run's number on its connection. */
class Steps {
	readonly #call: Call
	readonly #run: number
	readonly #cancelled: AbortSignal

	constructor(call: Call, run: number, cancelled: AbortSignal) {
		this.#call = call
		this.#run = run
		this.#cancelled = cancelled
	}

	async run<R>(name: string, fn: () => R | Promise<R>): Promise<R> {
		this.#cancelled.throwIfAborted()
		const asked = JobsStep.encode({ run: this.#run, name })
		const kept = JobsKept.decode(await this.#call(methods['jobs.step'], asked))
		if (kept.found === true) {
			return JSON.parse(kept.answer ?? 'null') as R
		}
		const answer = await fn()
		const text = jsonText(answer, `step ${JSON.stringify(name)}'s answer`)
		await this.#call(methods['jobs.keep'], JobsStep.encode({ run: this.#run, name, answer: text }))
		return answer
	}
}

/** A value's JSON, the text every language reads; undefined writes as null, as a step's answer may be nothing. */
function jsonText(value: unknown, what: string): string {
	let text: string | undefined
	try {
		text = JSON.stringify(value)
	} catch (err) {
		throw new InvalidError(`${what} JSON cannot write: ${messageOf(err)}`)
	}
	if (text === undefined) {
		if (value === undefined) {
			return 'null'
		}
		throw new InvalidError(`${what} JSON cannot write: a function or a symbol`)
	}
	return text
}

/** A queue's values: JSON, checked by its schema when it has one. */
interface Values<T> {
	encode(value: T): string
	decode(text: string): Promise<T>
}

function valuesOf<T>(schema: StandardSchemaV1 | undefined): Values<T> {
	return {
		encode: value => {
			if (value === undefined) {
				throw new InvalidError('a value of undefined, which JSON cannot write: give null')
			}
			return jsonText(value, 'a value')
		},
		decode: async text => {
			let value: unknown
			try {
				value = JSON.parse(text)
			} catch (err) {
				throw new CorruptError(`a value that is not JSON any more: ${messageOf(err)}`)
			}
			if (schema === undefined) {
				return value as T
			}
			const checked = await check(schema, value)
			if ('issues' in checked) {
				throw new CorruptError(`a value that no longer meets the queue's schema: ${checked.issues}`)
			}
			return checked.value as T
		},
	}
}

/** What a queue's or a schedule's calls and workers need of it. */
interface Source<T> {
	link: Link
	/** 'jobs queue emails', which an error names */
	describe: string
	openMethod: number
	open: Open
	/** the database whose file keeps the queue; the store's jobs.db when undefined */
	home?: Home | undefined
	/** how its calls reach the server: the link's connection, or a database's transaction */
	via: Via
	values: Values<T>
	/** a run's timeout, in milliseconds */
	timeout: number
	/** the store's running workers, which its close stops */
	workers: Set<Worker>
}

function callOf<T>(
	source: Source<T>,
	method: number,
	body: (handle: number) => Uint8Array,
	idempotence: Idempotence,
): Promise<Uint8Array> {
	return source.via.call(source.openMethod, source.open, method, body, idempotence)
}

async function gatherInto<T>(
	source: Source<T>,
	id: string,
	items: T,
	options: GatherOptions,
): Promise<Gathered> {
	const { settle, max, ...own } = options
	const call = { ...jobFields(own), id, value: source.values.encode(items) }
	const body = await callOf(
		source,
		methods['jobs.gather'],
		handle => JobsGather.encode({ call: { ...call, handle }, settle: ms(settle), max: msOf(max) }),
		'write',
	)
	const { added, count, at } = JobsGathered.decode(body)
	return { added: added === true, count: count ?? 0, at: new Date(at ?? 0) }
}

async function getJob<T>(source: Source<T>, id: string): Promise<Job<T> | undefined> {
	const body = await callOf(
		source,
		methods['jobs.get'],
		handle => JobsId.encode({ handle, id }),
		'read',
	)
	const found = JobsJob.decode(body)
	return found.found === true ? jobOf(source, found) : undefined
}

async function cancelJob<T>(source: Source<T>, job: Job<T> | string): Promise<boolean> {
	const named = namedBy(job as Job<unknown> | string, source.describe)
	const body = await callOf(
		source,
		methods['jobs.cancel'],
		handle => JobsId.encode({ handle, ...named }),
		'write',
	)
	return JobsChanged.decode(body).changed === true
}

/**
 * A page of the queue's jobs. A walk's page has a job whose value no longer
 * reads as its error in its place; any other page fails on it.
 */
async function pageOf<T>(
	source: Source<T>,
	options: ListOptions,
	walk: boolean,
): Promise<{ jobs: (Job<T> | Error)[]; next: string | undefined }> {
	const body = await callOf(
		source,
		methods['jobs.list'],
		handle => JobsList.encode({ handle, ...options, walk }),
		'read',
	)
	const page = JobsPage.decode(body)
	const jobs: (Job<T> | Error)[] = []
	for (const found of page.jobs ?? []) {
		if (found.failure !== undefined) {
			const { code, message, what } = found.failure
			jobs.push(errorOf(code ?? 'internal', message ?? '', what ?? {}))
			continue
		}
		const job = jobOf(source, found)
		jobs.push(walk ? await job.catch((err: Error) => err) : await job)
	}
	return { jobs, next: page.next }
}

/**
 * Every job a filter names, a page at a time. A job whose value no longer
 * reads is its error in its place: `next()` rejects with it, and the call
 * after goes on past it, as a `for await` that catches it and iterates the
 * same walk again does.
 */
class Walk<T> implements AsyncGenerator<Job<T>, void, unknown> {
	readonly #source: Source<T>
	readonly #options: Omit<ListOptions, 'after'>
	#jobs: (Job<T> | Error)[] = []
	#after: string | undefined
	#ended = false

	constructor(source: Source<T>, options: Omit<ListOptions, 'after'>) {
		this.#source = source
		this.#options = options
	}

	async next(): Promise<IteratorResult<Job<T>, void>> {
		while (this.#jobs.length === 0 && !this.#ended) {
			const page = await pageOf(this.#source, { ...this.#options, after: this.#after }, true)
			this.#jobs = page.jobs
			this.#after = page.next
			this.#ended = page.next === undefined
		}
		const job = this.#jobs.shift()
		if (job === undefined) {
			return { done: true, value: undefined }
		}
		if (job instanceof Error) {
			throw job
		}
		return { done: false, value: job }
	}

	async return(): Promise<IteratorResult<Job<T>, void>> {
		this.#end()
		return { done: true, value: undefined }
	}

	async throw(err: unknown): Promise<IteratorResult<Job<T>, void>> {
		this.#end()
		throw err
	}

	[Symbol.asyncIterator](): this {
		return this
	}

	async [Symbol.asyncDispose](): Promise<void> {
		this.#end()
	}

	#end(): void {
		this.#jobs = []
		this.#ended = true
	}
}

/** A job as the server sent it, its value read as the queue reads one. */
async function jobOf<T>(source: Source<T>, found: Read<typeof JobsJob.fields>): Promise<Job<T>> {
	let value: T
	try {
		value = await source.values.decode(found.value ?? 'null')
	} catch (err) {
		const named = found.id === undefined ? 'a job without an id' : `id ${JSON.stringify(found.id)}`
		throw new CorruptError(`${source.describe}: ${named}: ${messageOf(err)}`)
	}
	const job: Job<T> = {
		id: found.id,
		value,
		state: (found.state ?? 'waiting') as JobState,
		at: new Date(found.at ?? 0),
		attempt: found.attempt ?? 0,
		ahead: found.ahead ?? 0,
		aheadAtLeast: found.aheadAtLeast ?? false,
		progress: found.progress === undefined ? undefined : JSON.parse(found.progress),
		error: found.error,
		group: found.group,
		repeat: found.repeat,
		lastRun:
			found.startedAt === undefined
				? undefined
				: {
						startedAt: new Date(found.startedAt),
						endedAt: new Date(found.endedAt ?? found.startedAt),
					},
		expiresAt: found.expiresAt === undefined ? undefined : new Date(found.expiresAt),
	}
	if (found.row !== undefined) {
		Object.defineProperty(job, ROW, { value: found.row })
	}
	return job
}

/**
 * Follows the job under id over the server's watch, until the stream's last
 * DATA. A lost connection or a server going away watches again, and the
 * first report of the new watch is left out when it shows what was seen.
 */
async function* watchJob<T>(source: Source<T>, id: string): AsyncGenerator<Job<T>> {
	let last: Read<typeof JobsJob.fields> | undefined
	for (;;) {
		const stream = await source.link.run('read', connection => openWatch(connection, source, id))
		const unfollow = source.link.follow(stream)
		try {
			for (;;) {
				const event = await stream.next()
				stream.consumed(event.body.length)
				if (event.end) {
					return
				}
				const found = JobsJob.decode(event.body)
				if (last === undefined || !showsTheSame(last, found)) {
					last = found
					yield await jobOf(source, found)
				}
			}
		} catch (err) {
			if (!(err instanceof ConnectionLostError || err instanceof UnavailableError)) {
				throw err
			}
		} finally {
			stream.cancel()
			unfollow()
		}
	}
}

/** Opens a watch of the job under id, once its RESPONSE came. */
async function openWatch<T>(
	connection: Connection,
	source: Source<T>,
	id: string,
): Promise<Stream> {
	const handle = await handleOn(connection, source.openMethod, source.open)
	const stream = await connection.session.open(
		methods['jobs.watch'],
		JobsId.encode({ handle, id }),
		true,
	)
	await stream.next()
	return stream
}

/** Whether two reports show a watcher the same: the job's state, place, attempt, time, progress and error. */
function showsTheSame(a: Read<typeof JobsJob.fields>, b: Read<typeof JobsJob.fields>): boolean {
	return (
		a.state === b.state &&
		a.ahead === b.ahead &&
		a.aheadAtLeast === b.aheadAtLeast &&
		a.attempt === b.attempt &&
		a.at === b.at &&
		a.progress === b.progress &&
		a.error === b.error
	)
}

// a queue's source and name, kept out of its public shape, for a database's transaction
const queues = new WeakMap<object, QueueParts>()

/** What a database's transaction needs of a queue it takes in. */
export interface QueueParts {
	readonly name: string
	readonly source: Source<unknown>
}

/** A queue of jobs of one type, run in the order of their time: open it with `store.queue(name)`. */
export class Queue<T> {
	readonly name: string
	readonly #source: Source<T>

	/** Queues come from `store.queue` and `db.queue`. */
	constructor(source: Source<T>, name: string) {
		this.#source = source
		this.name = name
		queues.set(this, { name, source: source as Source<unknown> })
	}

	/**
	 * Adds a job unless its id is taken: by a job scheduled, waiting or
	 * running, or done while the queue's dedupe keeps it. A job failed for
	 * good holds no id: the new job takes its place. It returns once the job
	 * is on disk, and says whether it added one; a job without an id is
	 * always added.
	 *
	 *     await reminders.add({ userId: 42, text: 'Call mom' }, { delay: '1h' })
	 */
	async add(value: T, options: AddOptions = {}): Promise<boolean> {
		const fields = {
			...jobFields(options),
			id: options.id,
			value: this.#source.values.encode(value),
		}
		return (await this.#changed(methods['jobs.add'], fields)) === true
	}

	/**
	 * A job's add for a batch of the database the queue was opened from:
	 * `db.batch` writes it with the batch's statements, so the job commits
	 * with the rows or not at all, and shares the commit with every other
	 * write of the file, where `db.tx` holds the writer and pays a sync of
	 * its own. It adds as `add` does, unless its id is taken: its `Done` in
	 * the batch's answer changed 1 job, or 0.
	 *
	 *     await db.batch([sql`update notes set body = ${body} where id = ${id}`, indexing.adding({ noteId: id })])
	 */
	adding(value: T, options: AddOptions = {}): Adding {
		const call = { ...jobFields(options), id: options.id, value: this.#source.values.encode(value) }
		return new Adding({ name: this.name, source: this.#source as Source<unknown>, call })
	}

	/**
	 * Makes the id's job this value at this time, whatever it was: one waiting
	 * is made new, a failed one starts again, and a running one runs once more
	 * after this run, with this value.
	 *
	 *     await checks.set(`check:${check.id}`, check, { delay: '25h' })
	 */
	async set(id: string, value: T, options: JobOptions = {}): Promise<void> {
		const fields = { ...jobFields(options), id, value: this.#source.values.encode(value) }
		await callOf(
			this.#source,
			methods['jobs.set'],
			handle => JobsCall.encode({ ...fields, handle }),
			'write',
		)
	}

	/**
	 * Changes the id's job when it is not running: its value, the steps kept
	 * for the old one dropped with it, and its time, group or repeat when the
	 * options give them. It says whether it did: a job that runs, is done or
	 * failed, or never was answers false.
	 */
	async update(id: string, value: T, options: JobOptions = {}): Promise<boolean> {
		const fields = { ...jobFields(options), id, value: this.#source.values.encode(value) }
		return (await this.#changed(methods['jobs.update'], fields)) === true
	}

	/**
	 * Adds the job under `id` when it is free, as `add` does, or brings the
	 * one there to this call: its value, group, repeat and options of its own
	 * where they differ, its time, attempts and steps as they were. A changed
	 * value drops the steps kept for the old one; a changed repeat moves a job
	 * not yet due to the repeat's next time. It says whether it added or
	 * changed anything: a job that already was so is not written, so it is
	 * cheap to call on every request. `at` and `delay` count only when it adds.
	 *
	 *     const changed = await polls.upsert(`group:${g}`, { group: g }, { every: '3m' })
	 */
	async upsert(id: string, value: T, options: JobOptions = {}): Promise<boolean> {
		const fields = { ...jobFields(options), id, value: this.#source.values.encode(value) }
		return (await this.#changed(methods['jobs.upsert'], fields)) === true
	}

	/**
	 * Gathers items into the job under id, whose value is a list: a job
	 * scheduled or waiting takes them after its own, in the order they came,
	 * and is due settle after this gather, never later than max after the
	 * gather that started it. With no job there it adds one of these items;
	 * a running one runs on with what it read, and its next run takes only
	 * what is gathered meanwhile, at its settle time, once it ends well; a
	 * retry takes both, and a failure for good leaves the gathered ones a job
	 * of their own. A done job its dedupe holds keeps the id, count 0; a
	 * failed one gives it up, as to add. A
	 * value that is no list, there or here, or a job that repeats, is
	 * InvalidError. It compiles only for a queue whose value is a list.
	 *
	 *     const batch = store.queue<Notice[]>('notify-batch')
	 *     const { added, count, at } = await batch.gather('grades:7', [{ course: 1 }], { settle: '5m', max: '1h' })
	 */
	async gather<I>(
		this: Queue<I[]>,
		id: string,
		items: I[],
		options: GatherOptions,
	): Promise<Gathered> {
		return gatherInto(this.#source, id, items, options)
	}

	/**
	 * Takes a job, whatever its state, and says whether it was there: the job
	 * under an id, or a job a list gave, by its id or, for one without, by the
	 * row it was listed from. A job retried, run again for its repeat or
	 * cancelled since is in no row, and its cancel takes nothing, never another
	 * job. A running one's handler sees its signal abort, and what it returns
	 * then settles nothing; a repeating one stops.
	 *
	 *     for await (const job of emails.all({ state: 'scheduled' })) if (job.value.to === bounced) await emails.cancel(job)
	 */
	cancel(job: Job<T> | string): Promise<boolean> {
		return cancelJob(this.#source, job)
	}

	/** How many jobs the queue holds in each state, from one snapshot; it reads no value. */
	async counts(): Promise<JobCounts> {
		const body = await callOf(
			this.#source,
			methods['jobs.counts'],
			handle => Handle.encode({ handle }),
			'read',
		)
		const { scheduled, waiting, running, failed } = JobsCounts.decode(body)
		return {
			scheduled: scheduled ?? 0,
			waiting: waiting ?? 0,
			running: running ?? 0,
			failed: failed ?? 0,
		}
	}

	/**
	 * Where the job under id is: scheduled, waiting and how many jobs are
	 * ahead of it, running and its progress, failed and why, or done while
	 * the queue's dedupe keeps its id; undefined for an id with no job.
	 */
	get(id: string): Promise<Job<T> | undefined> {
		return getJob(this.#source, id)
	}

	/**
	 * The job under id as it is, then again each time its state, place,
	 * attempt, time, progress or error changes; it ends with the job: done,
	 * failed, or cancelled, a state only a watcher sees. An id with no job
	 * yields nothing. A watch whose connection is lost watches again on the
	 * next; a job that ended meanwhile ends it without its last state.
	 *
	 *     for await (const job of videos.watch(id)) send(job.state, job.ahead, job.progress)
	 */
	watch(id: string): AsyncGenerator<Job<T>> {
		return watchJob(this.#source, id)
	}

	/**
	 * A page of the queue's jobs: with no prefix every job, those with ids by id,
	 * then those without as they run, then those without that failed.
	 */
	async list(options: ListOptions = {}): Promise<JobPage<T>> {
		const page = await pageOf(this.#source, options, false)
		return { jobs: page.jobs as Job<T>[], next: page.next }
	}

	/**
	 * Every job the options name, a page at a time, holding nothing between
	 * pages. A job whose value no longer reads is a CorruptError in its place,
	 * naming it, and iterating the same walk again goes on past it.
	 */
	all(options: Omit<ListOptions, 'after'> = {}): AsyncGenerator<Job<T>> {
		return new Walk(this.#source, options)
	}

	/**
	 * Runs handler on each job as it falls due, as many at once as the
	 * queue's concurrency lets, one when it sets none, and returns the running
	 * worker at once: stop it at shutdown, as store.close() does. A worker
	 * whose connection is lost reaches the store again, the jobs it held
	 * running again as a dead worker's would.
	 *
	 *     const worker = reminders.work(async ({ userId, text }) => push(userId, text))
	 */
	work(handler: Handler<T>, options: WorkOptions = {}): Worker {
		const concurrency = handlersOf(options, this.#source.describe)
		return new Worker(
			working => keepWorking({ source: this.#source, handler, concurrency, working }),
			this.#source.workers,
		)
	}

	/**
	 * Runs what is due when it starts and what falls due meanwhile, then says
	 * how many jobs ran; it never waits for a later job. Tests, scripts and
	 * commands use it.
	 */
	runDue(handler: Handler<T>, options: WorkOptions = {}): Promise<number> {
		return runDue(this.#source, handler, handlersOf(options, this.#source.describe))
	}

	async #changed(
		method: number,
		fields: Omit<Written<typeof JobsCall.fields>, 'handle'>,
	): Promise<boolean | undefined> {
		const body = await callOf(
			this.#source,
			method,
			handle => JobsCall.encode({ ...fields, handle }),
			'write',
		)
		return JobsChanged.decode(body).changed
	}
}

/**
 * A queue's calls inside a database's transaction, from `tx.with(queue)`: a
 * job it adds is there only if the transaction commits, and the queue's
 * workers find it once it has. A worker, a watch and a page work outside.
 */
export class TxQueue<T> {
	readonly name: string
	readonly #queue: Queue<T>

	/** Comes from `tx.with(queue)`. */
	constructor(queue: Queue<T>) {
		this.#queue = queue
		this.name = queue.name
	}

	/** Adds a job unless its id is taken, as `queue.add` does, with the transaction. */
	add(value: T, options: AddOptions = {}): Promise<boolean> {
		return this.#queue.add(value, options)
	}

	set(id: string, value: T, options: JobOptions = {}): Promise<void> {
		return this.#queue.set(id, value, options)
	}

	update(id: string, value: T, options: JobOptions = {}): Promise<boolean> {
		return this.#queue.update(id, value, options)
	}

	upsert(id: string, value: T, options: JobOptions = {}): Promise<boolean> {
		return this.#queue.upsert(id, value, options)
	}

	/** Gathers items into the job under id, as `queue.gather` does, with the transaction. */
	gather<I>(this: TxQueue<I[]>, id: string, items: I[], options: GatherOptions): Promise<Gathered> {
		return this.#queue.gather(id, items, options)
	}

	cancel(job: Job<T> | string): Promise<boolean> {
		return this.#queue.cancel(job)
	}

	/** The job under id as the transaction sees it, its own writes among it. */
	get(id: string): Promise<Job<T> | undefined> {
		return this.#queue.get(id)
	}
}

/** A queue's add as a database's batch carries it: the queue, and the add's call but its handle. */
interface AddingParts {
	name: string
	source: Source<unknown>
	call: Omit<Written<typeof JobsCall.fields>, 'handle'>
}

// reads an add's parts, which only this module sees
let partsOf: (adding: Adding) => AddingParts

/** A job's add that a database's batch carries, from `queue.adding`: it commits with the batch's statements, or not at all. */
export class Adding {
	readonly #parts: AddingParts

	/** Comes from `queue.adding`. */
	constructor(parts: AddingParts) {
		this.#parts = parts
	}

	static {
		partsOf = adding => adding.#parts
	}
}

/**
 * What a batch of `home` sends of an add, on the connection it goes on: the
 * add's call, its queue's handle opened there first. A queue kept outside
 * `home` is refused, since its job would not commit with the rows.
 */
export function addingIn(
	adding: Adding,
	home: Home,
): (connection: Connection) => Promise<{ method: number; body: Uint8Array }> {
	const { name, source, call } = partsOf(adding)
	if (source.home !== home) {
		const kept = source.home === undefined ? 'jobs.db' : source.home.label
		throw new InvalidError(
			`${home.label}: jobs queue ${name}: kept in ${kept}, outside this batch of ${home.label}`,
		)
	}
	return async connection => {
		const handle = await handleOn(connection, source.openMethod, source.open)
		return { method: methods['jobs.add'], body: JobsCall.encode({ ...call, handle }) }
	}
}

/** A write the store's transaction keeps for its commit: its method, and its call or the job it cancels. */
export interface JobOp {
	source: Source<unknown>
	method: number
	call?: Omit<Written<typeof JobsCall.fields>, 'handle'>
	id?: { id?: string; row?: string }
}

/**
 * A queue's writes inside the store's transaction, from `tx.with(queue)` in
 * `store.tx`: each is kept for the commit, which makes them all or none, so
 * what one did is known once the transaction has committed.
 *
 *     await store.tx(async tx => {
 *       for (const member of members) await tx.with(pushes).add({ member, message }, { id: `${message}:${member}` })
 *     })
 */
export class StoreTxQueue<T> {
	readonly name: string
	readonly #source: Source<T>
	readonly #keep: (op: JobOp) => void

	/** Comes from `tx.with(queue)` in `store.tx`. */
	constructor(parts: QueueParts, keep: (op: JobOp) => void) {
		this.name = parts.name
		this.#source = parts.source as Source<T>
		this.#keep = keep
	}

	/** Adds a job unless its id is taken, with the commit. */
	async add(value: T, options: AddOptions = {}): Promise<void> {
		this.#write('jobs.add', { ...jobFields(options), id: options.id }, value)
	}

	/** Makes the id's job this value at this time, whatever it was, with the commit. */
	async set(id: string, value: T, options: JobOptions = {}): Promise<void> {
		this.#write('jobs.set', { ...jobFields(options), id }, value)
	}

	/** Changes the id's job when it is not running, its old value's steps dropped, with the commit. */
	async update(id: string, value: T, options: JobOptions = {}): Promise<void> {
		this.#write('jobs.update', { ...jobFields(options), id }, value)
	}

	/** Adds the job under id, or brings the one there to this call, its time kept, with the commit. */
	async upsert(id: string, value: T, options: JobOptions = {}): Promise<void> {
		this.#write('jobs.upsert', { ...jobFields(options), id }, value)
	}

	/** Takes the job under id, or a job a list gave, whatever its state, with the commit. */
	async cancel(job: Job<T> | string): Promise<void> {
		const id = namedBy(job as Job<unknown> | string, this.#source.describe)
		this.#keep({ source: this.#source as Source<unknown>, method: methods['jobs.cancel'], id })
	}

	#write(
		method: 'jobs.add' | 'jobs.set' | 'jobs.update' | 'jobs.upsert',
		fields: Omit<Written<typeof JobsCall.fields>, 'handle' | 'value'>,
		value: T,
	): void {
		const call = { ...fields, value: this.#source.values.encode(value) }
		this.#keep({ source: this.#source as Source<unknown>, method: methods[method], call })
	}
}

/** Sends a transaction's writes of jobs as one jobs.tx: all of them are made, or none. */
export async function commitJobs(link: Link, ops: readonly JobOp[]): Promise<void> {
	await link.run('write', async connection => {
		const writes = []
		for (const op of ops) {
			const handle = await handleOn(connection, op.source.openMethod, op.source.open)
			writes.push({
				method: op.method,
				call: op.call === undefined ? undefined : { ...op.call, handle },
				id: op.id === undefined ? undefined : { handle, ...op.id },
			})
		}
		await connection.session.call(methods['jobs.tx'], JobsTx.encode({ writes }))
	})
}

/** A queue's parts, for a database's transaction that takes it in; undefined for anything else. */
export function queueParts(handle: object): QueueParts | undefined {
	return queues.get(handle)
}

/** The queue's calls inside a transaction, going through its stream. */
export function queueVia(parts: QueueParts, via: Via): TxQueue<unknown> {
	return new TxQueue(new Queue({ ...parts.source, via }, parts.name))
}

/** A repeat the code owns: one repeating job under its name, and the worker that runs it. */
export class Schedule {
	readonly name: string
	readonly #source: Source<null>
	readonly #handler: Handler<null>
	readonly #worker: Worker

	/** Schedules come from `store.schedule`, started. */
	constructor(source: Source<null>, name: string, handler: (run: Run) => unknown) {
		this.#source = source
		this.name = name
		this.#handler = (_, run) => handler(run)
		const worked = { source, handler: this.#handler, concurrency: undefined }
		this.#worker = new Worker(working => keepWorking({ ...worked, working }), source.workers)
	}

	/** When it runs next and how its last run went: what a page of cron jobs shows. */
	get(): Promise<Job<null> | undefined> {
		return getJob(this.#source, this.name)
	}

	/** Runs the schedule's job when it is due, as a queue's runDue does. */
	runDue(): Promise<number> {
		return runDue(this.#source, this.#handler, undefined)
	}

	/** Stops the schedule's worker, as a worker's stop does. */
	stop(): Promise<void> {
		return this.#worker.stop()
	}

	/** Removes the schedule's job, until the program opens it again. */
	cancel(): Promise<boolean> {
		return cancelJob(this.#source, this.name)
	}

	/**
	 * Runs the schedule once as soon as a worker is free, under its own
	 * rules, and leaves its later times as they were: run at 03:29, a
	 * schedule of 03:30 runs again at 03:30. A run under way is followed by
	 * one more right after it, never beside it. A schedule whose job a cancel
	 * took is `NotFoundError` until the program opens it again.
	 *
	 *     if (!(await night.get())?.lastRun) await night.runNow() // the first deploy: run once if it never ran
	 */
	async runNow(): Promise<void> {
		await callOf(this.#source, methods['jobs.runNow'], handle => Handle.encode({ handle }), 'write')
	}
}

/** What a queue's open sends, checked before anything leaves. */
/**
 * A queue, its open checked before anything leaves; `home` is the database
 * whose file keeps it, its open naming the database's handle on each
 * connection.
 */
export function openQueue<T>(
	link: Link,
	workers: Set<Worker>,
	name: string,
	options: QueueOptions & { schema?: StandardSchemaV1 | undefined },
	home?: Home,
): Queue<T> {
	checkName(name, 'queue')
	const describe = home === undefined ? `jobs queue ${name}` : `${home.label}: jobs queue ${name}`
	const concurrency =
		typeof options.concurrency === 'number' ? { total: options.concurrency } : options.concurrency
	const fields = {
		name,
		attempts: whole(options.attempts, describe, 'attempts'),
		backoff: backoffOf(options.backoff),
		timeout: msOf(options.timeout),
		concurrency:
			concurrency === undefined
				? undefined
				: {
						total: whole(concurrency.total, describe, 'a concurrency'),
						group: whole(concurrency.group, describe, "a group's concurrency"),
					},
		rate: options.rate === undefined ? undefined : rateOf(options.rate),
		groupRate: options.groupRate === undefined ? undefined : rateOf(options.groupRate),
		...dedupeOf(options.dedupe),
		keep: msOf(options.keep),
		maxWaiting: whole(options.maxWaiting, describe, 'a maxWaiting'),
		quiet: options.quiet,
	}
	const bytes = JobsQueueOpen.encode(fields)
	const open: Open =
		home === undefined
			? bytes
			: home.keep(methods['jobs.queue.open'], bytes, async connection =>
					JobsQueueOpen.encode({ ...fields, database: await home.handle(connection) }),
				)
	const source: Source<T> = {
		link,
		describe,
		openMethod: methods['jobs.queue.open'],
		open,
		home,
		via: home === undefined ? viaLink(link) : home.via(`jobs queue ${name}`),
		values: valuesOf<T>(options.schema),
		timeout: options.timeout === undefined ? 60_000 : ms(options.timeout),
		workers,
	}
	return new Queue<T>(source, name)
}

/** A schedule, its open checked before anything leaves, and its worker started. */
export function openSchedule(
	link: Link,
	workers: Set<Worker>,
	name: string,
	when: ScheduleWhen,
	handler: (run: Run) => unknown,
	options: ScheduleOptions = {},
): Schedule {
	checkName(name, 'schedule')
	const describe = `jobs schedule ${name}`
	const repeat =
		'cron' in when
			? { cron: when.cron, timeZone: zoneOf(when.timeZone, describe) }
			: { every: ms(when.every) }
	const open = JobsScheduleOpen.encode({
		name,
		...repeat,
		attempts: whole(options.attempts, describe, 'attempts'),
		backoff: backoffOf(options.backoff),
		timeout: msOf(options.timeout),
	})
	const source: Source<null> = {
		link,
		describe,
		openMethod: methods['jobs.schedule.open'],
		open,
		via: viaLink(link),
		values: valuesOf<null>(undefined),
		timeout: options.timeout === undefined ? 60_000 : ms(options.timeout),
		workers,
	}
	return new Schedule(source, name, handler)
}

/** A cron's zone, which an hour means nothing without: undefined, a user's never set, is refused. */
function zoneOf(zone: string | undefined, describe: string): string {
	if (typeof zone !== 'string' || zone === '') {
		throw new InvalidError(`${describe}: a cron needs a timeZone, 'UTC' among them`)
	}
	return zone
}

function jobFields(
	options: JobOptions,
): Omit<Written<typeof JobsCall.fields>, 'handle' | 'id' | 'value'> {
	return {
		at: options.at === undefined ? undefined : unixMs(options.at),
		delay: msOf(options.delay),
		group: options.group,
		every: msOf(options.every),
		cron: options.cron,
		timeZone: options.timeZone,
		...dedupeOf(options.dedupe),
		keep: msOf(options.keep),
		ttl: msOf(options.ttl),
		expiresAt: options.expiresAt === undefined ? undefined : unixMs(options.expiresAt),
		quiet: options.quiet === false ? undefined : options.quiet,
		noQuiet: options.quiet === false ? true : undefined,
	}
}

/** A dedupe as a queue's open and a job's call carry it: a span, or a calendar in its place. */
function dedupeOf(dedupe: Dedupe | undefined): {
	dedupe: number | undefined
	dedupeCalendar: { calendar: string; timeZone: string } | undefined
} {
	if (typeof dedupe !== 'object') {
		return { dedupe: msOf(dedupe), dedupeCalendar: undefined }
	}
	return {
		dedupe: undefined,
		dedupeCalendar: { calendar: dedupe.calendar, timeZone: dedupe.timeZone },
	}
}

function msOf(d: Duration | undefined): number | undefined {
	return d === undefined ? undefined : ms(d)
}

function backoffOf(backoff: Backoff | undefined): { initial: number; max: number } | undefined {
	return backoff === undefined ? undefined : { initial: ms(backoff.initial), max: ms(backoff.max) }
}

/** A count the server takes as a whole number; one that is none is refused here, naming what it was. */
function whole(n: number | undefined, describe: string, what: string): number | undefined {
	if (n !== undefined && (!Number.isSafeInteger(n) || n < 0)) {
		throw new InvalidError(`${describe}: ${what} of ${n}, which is no whole number`)
	}
	return n
}

function handlersOf(options: WorkOptions, describe: string): number | undefined {
	const n = options.concurrency
	if (n !== undefined && (!Number.isSafeInteger(n) || n < 1 || n > mostHandlers)) {
		throw new InvalidError(`${describe}: a worker of ${n} handlers, not 1 to ${mostHandlers}`)
	}
	return n
}

/** A worker's state, which its loop and its stop share. */
interface Working {
	stopping: boolean
	/** the stream the worker runs on now, which a stop is sent on */
	stream: Stream | undefined
	/** wakes a loop pausing before it dials again */
	wake: (() => void) | undefined
}

/** A worker running a queue's handlers here while the server's loop claims their jobs. */
export class Worker implements AsyncDisposable {
	readonly #working: Working = { stopping: false, stream: undefined, wake: undefined }
	readonly #done: Promise<void>

	/** Workers come from a queue's work and from store.schedule, running. */
	constructor(run: (working: Working) => Promise<void>, workers: Set<Worker>) {
		workers.add(this)
		this.#done = run(this.#working).finally(() => workers.delete(this))
	}

	/**
	 * Takes no job more and waits for the handlers under way, each bounded by
	 * its timeout: the server gives back the jobs it held for them, and ends
	 * the worker once their answers are written.
	 */
	stop(): Promise<void> {
		const working = this.#working
		if (!working.stopping) {
			working.stopping = true
			working.stream?.send(JobsAnswer.encode({ how: 'stop' }), false).catch(() => {})
			working.wake?.()
		}
		return this.#done
	}

	[Symbol.asyncDispose](): Promise<void> {
		return this.stop()
	}
}

/** One worker's parts: what it runs, how many at once, and the state its stop shares. */
interface Worked<T> {
	source: Source<T>
	handler: Handler<T>
	concurrency: number | undefined
	working: Working
}

/**
 * Runs a worker until it stops: a stream on the link's connection, and
 * another once a connection is lost or a server going away has ended one.
 * What fails between is said in TinyStore's own lines, as background work's
 * failures are.
 */
async function keepWorking<T>(worked: Worked<T>): Promise<void> {
	const { source, working } = worked
	const failures = new FailureLog(`${source.describe} worker`, sayOwn)
	let pause = firstPause
	while (!working.stopping) {
		try {
			await source.link.run('read', connection => WorkStream.serve(connection, worked, false))
			failures.succeeded()
			pause = firstPause
		} catch (err) {
			if (working.stopping) {
				return
			}
			failures.failed(err)
			pause = Math.min(pause * 2, lastPause)
		}
		await pausing(pause, working)
	}
}

/** Runs a stream that ends once nothing is due, and says how many jobs its handlers finished. */
async function runDue<T>(
	source: Source<T>,
	handler: Handler<T>,
	concurrency: number | undefined,
): Promise<number> {
	const working: Working = { stopping: false, stream: undefined, wake: undefined }
	let finished = 0
	await source.link.run('read', async connection => {
		finished += await WorkStream.serve(connection, { source, handler, concurrency, working }, true)
	})
	return finished
}

/** Waits ms, or until the worker stops. */
function pausing(ms: number, working: Working): Promise<void> {
	if (working.stopping) {
		return Promise.resolve()
	}
	return new Promise(resolve => {
		const done = () => {
			clearTimeout(timer)
			working.wake = undefined
			resolve()
		}
		const timer = setTimeout(done, ms)
		working.wake = done
	})
}

/** A worker's stream on one connection: the jobs the server hands over, and the handlers they run here. */
class WorkStream<T> {
	readonly #worked: Worked<T>
	readonly #connection: Connection
	readonly #stream: Stream
	/** the runs under way, by number, and what cancels each */
	readonly #runs = new Map<number, AbortController>()
	#finished = 0

	constructor(worked: Worked<T>, connection: Connection, stream: Stream) {
		this.#worked = worked
		this.#connection = connection
		this.#stream = stream
	}

	/**
	 * Opens a worker's stream on a connection and runs it to its end, which
	 * the server sends: after a stop or a GOAWAY once the jobs in hand are
	 * answered, or once nothing is due when it ends when idle. It says how
	 * many jobs its handlers finished.
	 */
	static async serve<T>(
		connection: Connection,
		worked: Worked<T>,
		untilIdle: boolean,
	): Promise<number> {
		const handle = await handleOn(connection, worked.source.openMethod, worked.source.open)
		const asked = JobsWork.encode({
			handle,
			concurrency: worked.concurrency,
			untilIdle: untilIdle || undefined,
		})
		const stream = await connection.session.open(methods['jobs.work'], asked, false)
		const { working } = worked
		working.stream = stream
		if (working.stopping) {
			stream.send(JobsAnswer.encode({ how: 'stop' }), false).catch(() => {})
		}
		try {
			return await new WorkStream(worked, connection, stream).#serve()
		} finally {
			if (working.stream === stream) {
				working.stream = undefined
			}
		}
	}

	async #serve(): Promise<number> {
		try {
			const started = await this.#stream.next()
			while (!started.end) {
				const event = await this.#stream.next()
				this.#stream.consumed(event.body.length)
				if (event.end) {
					break
				}
				this.#take(JobsHeld.decode(event.body))
			}
			return this.#finished
		} catch (err) {
			for (const cancel of this.#runs.values()) {
				cancel.abort(err)
			}
			throw err
		}
	}

	/** A job the server handed over, run at once, since it sends no more than the handlers; or a cancel of one. */
	#take(held: Read<typeof JobsHeld.fields>): void {
		const run = held.run ?? 0
		if (held.cancelled === true) {
			this.#runs.get(run)?.abort(new CancelledError('a cancel took the job while it ran'))
			this.#runs.delete(run)
			return
		}
		const cancel = new AbortController()
		this.#runs.set(run, cancel)
		void this.#run(held, cancel).finally(() => this.#runs.delete(run))
	}

	/** Runs one job's handler and sends the answer it ended with; a job a cancel took is the server's, and gets none. */
	async #run(held: Read<typeof JobsHeld.fields>, cancel: AbortController): Promise<void> {
		const { source, handler } = this.#worked
		const run = held.run ?? 0
		const signal = AbortSignal.any([cancel.signal, AbortSignal.timeout(source.timeout)])
		const reporter = new Reporter(progress =>
			this.#stream.send(JobsAnswer.encode({ run, how: 'progress', progress }), false),
		)
		let answer: AnswerFields
		try {
			const value = await source.values.decode(held.value ?? 'null')
			const made = new Map<Answer, AnswerFields>()
			const steps = new Steps(
				(method, body) => this.#connection.session.call(method, body),
				run,
				cancel.signal,
			)
			try {
				answer = answerOf(
					await handler(value, new Run({ held, signal, steps, reporter, made })),
					made,
				)
			} catch (err) {
				answer = { how: 'retry', error: messageOf(err) }
			}
		} catch (err) {
			answer = { how: 'fail', error: `its value no longer reads: ${messageOf(err)}` }
		}
		await reporter.stop()
		if (cancel.signal.aborted) {
			return
		}
		this.#finished++
		await this.#stream.send(JobsAnswer.encode({ run, ...answer }), false).catch(() => {})
	}
}
