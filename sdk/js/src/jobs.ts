// Work an application must do later, or now but outside the request that
// asked for it, as plan/api/jobs.md has it: queues of JSON values run in the
// order of their time, ids that find a job again, repeats, schedules and
// limits. A worker's handlers run here while the server's loop claims the
// queue's jobs and hands each over on one stream, the answers going back on
// it. A handle opens with its first call on a connection.

import { FailureLog, messageOf } from './background.ts'
import type { Connection, Idempotence, Link } from './connection.ts'
import { CancelledError, CorruptError, InvalidError, UnavailableError } from './errors.ts'
import { checkName, type Home, handleOn, type Open, type Via, viaLink } from './handles.ts'
import { type Rate, rateOf } from './limits.ts'
import { sayOwn } from './logger.ts'
import { check, type StandardSchemaV1 } from './schema.ts'
import { LostError, type Stream } from './session.ts'
import { type Duration, ms, type Time, unixMs } from './time.ts'
import type { Read, Written } from './wire/codec.ts'
import {
	JobsAnswer,
	JobsCall,
	JobsChanged,
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
	/** how long a done job's id stays taken, so that an add of it adds nothing */
	dedupe?: Duration | undefined
	/** how long a failed job stays, to be found and started again: a week */
	keep?: Duration | undefined
	/** jobs that may wait; an add past it is LimitError: ten million */
	maxWaiting?: number | undefined
}

/** A job's time, group and repeat, as an add, a set and an update take them. */
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
}

export interface AddOptions extends JobOptions {
	/** the job's id, 1 to 1024 bytes: what finds it again, and adds it once */
	id?: string | undefined
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
	/** the jobs that run before a waiting one, up to 10,000 */
	ahead: number
	/** what its handler last reported of a running job */
	progress: unknown
	/** why its last run failed */
	error: string | undefined
	group: string | undefined
	/** a repeating job's repeat as it keeps it: '@every 30s +6178ms', '10 3 * * * Europe/Berlin' */
	repeat: string | undefined
	/** when the last run a handler finished started and ended */
	lastRun: { startedAt: Date; endedAt: Date } | undefined
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

async function cancelJob<T>(source: Source<T>, id: string): Promise<boolean> {
	const body = await callOf(
		source,
		methods['jobs.cancel'],
		handle => JobsId.encode({ handle, id }),
		'write',
	)
	return JobsChanged.decode(body).changed === true
}

/** A job as the server sent it, its value read as the queue reads one. */
async function jobOf<T>(source: Source<T>, found: Read<typeof JobsJob.fields>): Promise<Job<T>> {
	let value: T
	try {
		value = await source.values.decode(found.value ?? 'null')
	} catch (err) {
		throw new CorruptError(
			`${source.describe}: id ${JSON.stringify(found.id ?? '')}: ${messageOf(err)}`,
		)
	}
	return {
		id: found.id,
		value,
		state: (found.state ?? 'waiting') as JobState,
		at: new Date(found.at ?? 0),
		attempt: found.attempt ?? 0,
		ahead: found.ahead ?? 0,
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
	}
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
			if (!(err instanceof LostError || err instanceof UnavailableError)) {
				throw err
			}
		} finally {
			stream.cancel()
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
	 * Adds a job unless its id is taken: by a job scheduled, waiting, running
	 * or failed, or done while the queue's dedupe keeps it. It returns once
	 * the job is on disk, and says whether it added one; a job without an id
	 * is always added.
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
	 * Changes the id's job when it has not started: its value, and its time,
	 * group or repeat when the options give them. It says whether it did: a
	 * job that runs, ran or never was answers false.
	 */
	async update(id: string, value: T, options: JobOptions = {}): Promise<boolean> {
		const fields = { ...jobFields(options), id, value: this.#source.values.encode(value) }
		return (await this.#changed(methods['jobs.update'], fields)) === true
	}

	/**
	 * Takes the job under id, whatever its state, and says whether there was
	 * one. A running one's handler sees its signal abort, and what it returns
	 * then settles nothing; a repeating one stops.
	 */
	cancel(id: string): Promise<boolean> {
		return cancelJob(this.#source, id)
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

	/** A page of the queue's jobs whose ids start with a prefix, or of its failed ones. */
	async list(options: ListOptions = {}): Promise<JobPage<T>> {
		const body = await callOf(
			this.#source,
			methods['jobs.list'],
			handle => JobsList.encode({ handle, ...options }),
			'read',
		)
		const page = JobsPage.decode(body)
		const jobs: Job<T>[] = []
		for (const found of page.jobs ?? []) {
			jobs.push(await jobOf(this.#source, found))
		}
		return { jobs, next: page.next }
	}

	/** Every job the options name, a page at a time, holding nothing between pages. */
	async *all(options: Omit<ListOptions, 'after'> = {}): AsyncGenerator<Job<T>> {
		let after: string | undefined
		for (;;) {
			const page = await this.list({ ...options, after })
			yield* page.jobs
			if (page.next === undefined) {
				return
			}
			after = page.next
		}
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

	cancel(id: string): Promise<boolean> {
		return this.#queue.cancel(id)
	}

	/** The job under id as the transaction sees it, its own writes among it. */
	get(id: string): Promise<Job<T> | undefined> {
		return this.#queue.get(id)
	}
}

/** A write the store's transaction keeps for its commit: its method, and its call or the id it cancels. */
export interface JobOp {
	source: Source<unknown>
	method: number
	call?: Omit<Written<typeof JobsCall.fields>, 'handle'>
	id?: string
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

	/** Changes the id's job when it has not started, with the commit. */
	async update(id: string, value: T, options: JobOptions = {}): Promise<void> {
		this.#write('jobs.update', { ...jobFields(options), id }, value)
	}

	/** Takes the job under id, whatever its state, with the commit. */
	async cancel(id: string): Promise<void> {
		this.#keep({ source: this.#source as Source<unknown>, method: methods['jobs.cancel'], id })
	}

	#write(
		method: 'jobs.add' | 'jobs.set' | 'jobs.update',
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
				id: op.id === undefined ? undefined : { handle, id: op.id },
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
	const describe =
		home === undefined ? `jobs queue ${name}` : `${home.describe}: jobs queue ${name}`
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
		dedupe: msOf(options.dedupe),
		keep: msOf(options.keep),
		maxWaiting: whole(options.maxWaiting, describe, 'a maxWaiting'),
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
		via: viaLink(link),
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
