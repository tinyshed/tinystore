// Work that runs at its time, in jobs.db, as Go's jobs package keeps it:
// queues ordered by time, leases, retries and repeats. A handle opens with its
// first call; a work loop is the server's own Work, its jobs handed over the
// connection and settled by what the handler returns.

import { currentSignal } from './cancel.ts'
import { type Connection, download, type Link } from './connection.ts'
import { CancelledError, CorruptError, errorOf, InvalidError } from './errors.ts'
import { checkName, handleOn, type Page } from './handles.ts'
import { check, isSchema, type StandardSchemaV1 } from './schema.ts'
import { LostError, watch } from './session.ts'
import { batchBelongsTo, type Database, databaseBelongsTo, type SqlBatch } from './sql.ts'
import { type Duration, dateOf, ms, type Time, unixMs } from './time.ts'
import {
	JobsAnswer,
	JobsBatch,
	JobsChange,
	JobsEntry,
	JobsHeld,
	JobsKept,
	JobsKey,
	JobsLease,
	JobsOutcome,
	JobsOutcomes,
	JobsPage,
	JobsQuery,
	JobsQueue,
	JobsSettled,
	JobsWorkers,
	methods,
} from './wire/messages.ts'

export interface QueueOptions {
	/** how long a claim holds a job before it is given back: 30 s */
	lease?: Duration
	/** the attempts after which a job fails for good: 10 */
	maxAttempts?: number
	/** a retry's wait, doubling from first to most: a second to an hour */
	backoff?: { first?: Duration; most?: Duration }
	/** the jobs a queue holds before it refuses the next with LimitError: ten million */
	maxWaiting?: number
	/** how long a failed job is kept for Get and Scan: seven days */
	keepFailed?: Duration
	/** remembers a done job's key this long, so that enqueuing it again adds nothing */
	keepDone?: Duration
	/** the jobs that may run at once, across every worker of the store: no bound */
	maxRunning?: number
	/**
	 * keeps the queue in this database's file instead of jobs.db, so that a
	 * batch of the database commits a job with its rows: `queue.withTx(tx)`
	 */
	in?: Database
}

/** When a job runs again: cron text in a zone by its name, a daily time, or every so often. */
export type Repeat =
	| { cron: string; zone: string }
	| { daily: string; zone: string }
	| { every: Duration }

export interface EnqueueOptions {
	/** the job's time; one past runs now */
	at?: Time
	after?: Duration
	/** names one job: enqueuing it again adds nothing and can bring it forward, never back */
	key?: string
	/** needs a key, since only a key stops it */
	repeat?: Repeat
}

/** done while keepDone keeps its key; cancelled only a watcher sees */
export type JobState = 'waiting' | 'running' | 'failed' | 'done' | 'cancelled'

export interface JobEntry<T> {
	key: string
	value: T
	/** the time it runs for */
	at: Date
	/** its attempts, a running one included */
	attempt: number
	state: JobState
	/** the jobs that run before a waiting one, up to 10,000; get and watch count it, scan leaves it 0 */
	ahead: number
	/** what a running job's handler last reported with job.progress */
	progress: unknown
	/** when the last run a handler finished began: acknowledged, retried, failed or snoozed */
	ran: Date | undefined
	/** how many milliseconds that run took */
	took: number | undefined
	/** its last failure */
	error: string | undefined
	/** a repeating job's cron text and zone */
	repeat: string | undefined
}

export interface WorkOptions {
	/** the jobs in this process's hands at once: 1, the one order a queue promises */
	workers?: number
	/** how long a job may be in the handler's hands, after which its attempt fails: a minute */
	timeout?: Duration
	/** stops taking jobs: the ones in hand finish first, and work returns */
	signal?: AbortSignal
	/** returns once no job is due and none runs, as a test wants it */
	untilIdle?: boolean
}

type Settlement =
	| { how: 'ack' }
	| { how: 'retry'; error: string; at?: Time; after?: Duration }
	| { how: 'fail'; error: string }
	| { how: 'snooze'; at?: Time; after?: Duration }

const how = { ack: 1, retry: 2, fail: 3, snooze: 4, extend: 5, progress: 6 } as const

/** the JSON a handler reports of its job, at most 4 KiB of it */
function progressText(progress: unknown): string {
	let text: string | undefined
	try {
		text = JSON.stringify(progress)
	} catch (err) {
		throw new InvalidError(`a job's progress JSON cannot write: ${(err as Error).message}`)
	}
	if (text === undefined) {
		throw new InvalidError("a job's progress JSON cannot write: undefined, a function or a symbol")
	}
	if (new TextEncoder().encode(text).length > 4096) {
		throw new InvalidError("a job's progress is past 4 KiB of JSON")
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

	/** Drops what waits and lets what is being sent finish, before the job's outcome follows it. */
	async stop(): Promise<void> {
		this.#stopped = true
		clearTimeout(this.#timer)
		await this.#sending
	}
}

/** Calls a method on the connection a job is held on, answering its body. */
type Call = (method: number, body: Uint8Array) => Promise<Uint8Array>

/**
 * The steps of a held job's run, which the server keeps under the number the
 * job was held by: a step's kept answer, or the step run and its answer kept.
 */
class Steps {
	readonly #call: Call
	readonly #job: number

	constructor(call: Call, job: number) {
		this.#call = call
		this.#job = job
	}

	async run<R>(name: string, fn: () => R | Promise<R>): Promise<R> {
		const kept = JobsKept.decode(
			await this.#call(methods['jobs.step'], JobsAnswer.encode({ job: this.#job, name })),
		)
		if (kept.found === true) {
			return JSON.parse(kept.answer ?? 'null') as R
		}
		const answer = await fn()
		let text: string
		try {
			text = JSON.stringify(answer) ?? 'null'
		} catch (err) {
			throw new InvalidError(
				`step ${name} answered what JSON cannot write: ${(err as Error).message}`,
			)
		}
		await this.#call(
			methods['jobs.keep'],
			JobsAnswer.encode({ job: this.#job, name, answer: text }),
		)
		return answer
	}
}

/**
 * A job in a work loop's handler. Returning acknowledges it and throwing
 * retries it, unless the handler said otherwise with retry, fail or snooze;
 * the last of those it calls is how the job settles.
 */
export class Job<T> {
	readonly key: string
	readonly value: T
	/** the time it ran for */
	readonly at: Date
	/** its attempts, this one included */
	readonly attempt: number
	/**
	 * aborts when the job's timeout passes, its loop stops, or cancel takes it,
	 * whose reason is then a CancelledError: the handler should stop too
	 */
	readonly signal: AbortSignal
	settlement: Settlement | undefined
	readonly #reporter: Reporter | undefined
	readonly #steps: Steps | undefined

	constructor(
		key: string,
		value: T,
		at: Date,
		attempt: number,
		signal: AbortSignal,
		reporter?: Reporter,
		steps?: Steps,
	) {
		this.key = key
		this.value = value
		this.at = at
		this.attempt = attempt
		this.signal = signal
		this.#reporter = reporter
		this.#steps = steps
	}

	/**
	 * Runs fn once in the job's run and keeps what it answers, as JSON: an
	 * attempt after a retry, a lost lease or a restart gets the kept answer
	 * back without running fn again. A step whose attempt ends before its
	 * answer is kept runs again, so what fn does outside the store should bear
	 * doing twice. A name is the step's within the run, which a loop numbers:
	 * 'model:1', 'tool:1', 'model:2'. An answer is at most 1 MiB of JSON.
	 *
	 *     const hits = await job.step('search', () => search(job.value.query))
	 */
	step<R>(name: string, fn: () => R | Promise<R>): Promise<R> {
		if (this.#steps === undefined) {
			return Promise.reject(new InvalidError('a job no work loop handed over keeps no steps'))
		}
		return this.#steps.run(name, fn)
	}

	/**
	 * Reports how far the job got, any JSON within 4 KiB: get and watch show
	 * the latest until the job is settled. It does not wait for the server.
	 *
	 *     await transcode({ signal: job.signal, onProgress: p => job.progress(p) })
	 */
	progress(progress: unknown): void {
		this.#reporter?.report(progressText(progress))
	}

	/** Fails this attempt: the job runs again after its backoff, or at the time given. */
	retry(error?: unknown, when?: { at?: Time; after?: Duration }): void {
		this.settlement = { how: 'retry', error: reasonOf(error), ...when }
	}

	/** Fails the job for good, keeping it for keepFailed with its reason. */
	fail(error?: unknown): void {
		this.settlement = { how: 'fail', error: reasonOf(error) }
	}

	/** Moves the job to another time without counting the attempt. */
	snooze(when: { at?: Time; after?: Duration }): void {
		this.settlement = { how: 'snooze', ...when }
	}
}

/** A job a claim leased, which its caller settles itself before the lease ends. */
export class ClaimedJob<T> {
	readonly key: string
	readonly value: T
	readonly at: Date
	readonly attempt: number
	readonly #settle: (outcome: Parameters<typeof JobsOutcome.encode>[0]) => Promise<void>
	readonly #job: number
	readonly #steps: Steps

	constructor(
		held: ReturnType<typeof JobsHeld.decode>,
		value: T,
		settle: (outcome: Parameters<typeof JobsOutcome.encode>[0]) => Promise<void>,
		call: Call,
	) {
		this.key = held.key ?? ''
		this.value = value
		this.at = new Date(held.at ?? 0)
		this.attempt = held.attempt ?? 0
		this.#job = held.job ?? 0
		this.#settle = settle
		this.#steps = new Steps(call, this.#job)
	}

	/** Runs fn once in the job's run and keeps its answer, as a work loop's job.step does. */
	step<R>(name: string, fn: () => R | Promise<R>): Promise<R> {
		return this.#steps.run(name, fn)
	}

	/** The job is done. */
	ack(): Promise<void> {
		return this.#settle({ job: this.#job, how: how.ack })
	}

	retry(error?: unknown, when?: { at?: Time; after?: Duration }): Promise<void> {
		return this.#settle({ job: this.#job, how: how.retry, err: reasonOf(error), ...timing(when) })
	}

	fail(error?: unknown): Promise<void> {
		return this.#settle({ job: this.#job, how: how.fail, err: reasonOf(error) })
	}

	snooze(when: { at?: Time; after?: Duration }): Promise<void> {
		return this.#settle({ job: this.#job, how: how.snooze, ...timing(when) })
	}

	/** Holds the job this much longer from now. */
	extend(d: Duration): Promise<void> {
		return this.#settle({ job: this.#job, how: how.extend, after: ms(d) })
	}

	/** Reports how far the job got, any JSON within 4 KiB, which get and watch show until it is settled. */
	progress(progress: unknown): Promise<void> {
		return this.#settle({ job: this.#job, how: how.progress, progress: progressText(progress) })
	}
}

function reasonOf(error: unknown): string {
	if (error === undefined) {
		return 'the handler gave no reason'
	}
	return error instanceof Error ? error.message : String(error)
}

function timing(when: { at?: Time; after?: Duration } | undefined): {
	at?: number
	after?: number
} {
	if (when?.at !== undefined) {
		return { at: unixMs(when.at) }
	}
	if (when?.after !== undefined) {
		return { after: ms(when.after) }
	}
	return {}
}

/** Encodes a queue's values as JSON, the text every language reads, and back. */
interface Values<T> {
	encode(value: T): string
	decode(text: string): T | Promise<T>
}

const jsonValues: Values<unknown> = {
	encode: value => {
		let text: string | undefined
		try {
			text = JSON.stringify(value)
		} catch (err) {
			throw new InvalidError(`a job's value JSON cannot write: ${(err as Error).message}`)
		}
		if (text === undefined) {
			throw new InvalidError("a job's value JSON cannot write: undefined, a function or a symbol")
		}
		return text
	},
	decode: text => {
		try {
			return JSON.parse(text)
		} catch (err) {
			throw new CorruptError(`a job's value is not JSON any more: ${(err as Error).message}`)
		}
	},
}

function schemaValues<S extends StandardSchemaV1>(schema: S): Values<unknown> {
	return {
		encode: jsonValues.encode,
		decode: async text => {
			const checked = await check(schema, await jsonValues.decode(text))
			if ('issues' in checked) {
				throw new CorruptError(`a job's value does not meet the queue's schema: ${checked.issues}`)
			}
			return checked.value
		},
	}
}

function repeatOf(r: Repeat): Parameters<typeof JobsQueue.encode>[0]['schedule'] {
	if ('every' in r) {
		return { every: ms(r.every) }
	}
	if ('daily' in r) {
		const time = /^(\d{1,2}):(\d{2})$/.exec(r.daily)
		if (time === null || Number(time[1]) > 23 || Number(time[2]) > 59) {
			throw new InvalidError(`the daily time ${JSON.stringify(r.daily)}; write it as 03:10`)
		}
		return { cron: `${Number(time[2])} ${Number(time[1])} * * *`, zone: r.zone }
	}
	return { cron: r.cron, zone: r.zone }
}

export class Jobs {
	readonly #link: Link

	constructor(link: Link) {
		this.#link = link
	}

	/** A queue of JSON values of type T. */
	queue<T = unknown>(name: string, options?: QueueOptions): Queue<T>
	/** A queue whose values each claim checks, with zod's, valibot's or another Standard Schema. */
	queue<S extends StandardSchemaV1>(
		name: string,
		schema: S,
		options?: QueueOptions,
	): Queue<StandardSchemaV1.InferOutput<S>>
	queue(
		name: string,
		of?: StandardSchemaV1 | QueueOptions,
		options?: QueueOptions,
	): Queue<unknown> {
		checkName(name, 'queue')
		let values = jsonValues
		if (isSchema(of)) {
			values = schemaValues(of)
		} else if (of !== undefined) {
			options = of
		}
		const open = JobsQueue.encode(queueFields(name, options))
		return new Queue(this.#link, name, open, values, options?.in)
	}

	/**
	 * A schedule: a queue of one job under the schedule's name, which repeats
	 * as it says. work runs it; cancel stops it.
	 */
	schedule(name: string, repeat: Repeat, options?: QueueOptions): Queue<null> {
		checkName(name, 'schedule')
		const open = JobsQueue.encode({ ...queueFields(name, options), schedule: repeatOf(repeat) })
		const nothing = { encode: () => '{}', decode: () => null }
		return new Queue(this.#link, name, open, nothing, options?.in)
	}
}

function queueFields(
	name: string,
	options: QueueOptions | undefined,
): Parameters<typeof JobsQueue.encode>[0] {
	return {
		name,
		lease: options?.lease === undefined ? undefined : ms(options.lease),
		maxAttempts: options?.maxAttempts,
		backoffFirst: options?.backoff?.first === undefined ? undefined : ms(options.backoff.first),
		backoffMost: options?.backoff?.most === undefined ? undefined : ms(options.backoff.most),
		maxWaiting: options?.maxWaiting,
		keepFailed: options?.keepFailed === undefined ? undefined : ms(options.keepFailed),
		keepDone: options?.keepDone === undefined ? undefined : ms(options.keepDone),
		maxRunning: options?.maxRunning,
		in: options?.in?.name,
	}
}

/** A job's fields as jobs.enqueue and a batch carry them. */
function jobFields<T>(values: Values<T>, value: T, options: EnqueueOptions | undefined) {
	return {
		value: values.encode(value),
		key: options?.key,
		at: options?.at === undefined ? undefined : unixMs(options.at),
		after: options?.after === undefined ? undefined : ms(options.after),
		repeat: options?.repeat === undefined ? undefined : repeatOf(options.repeat),
	}
}

/** A queue's enqueue inside a batch of the database it lives in. */
export class QueueTx<T> {
	readonly #tx: SqlBatch
	readonly #open: Uint8Array
	readonly #values: Values<T>

	constructor(tx: SqlBatch, open: Uint8Array, values: Values<T>) {
		this.#tx = tx
		this.#open = open
		this.#values = values
	}

	/** Adds a job to the batch; it settles once the batch has committed. */
	enqueue(value: T, options?: EnqueueOptions): Promise<void> {
		return this.#tx.enqueue(this.#open, jobFields(this.#values, value, options))
	}
}

const states: Record<number, JobState> = {
	1: 'waiting',
	2: 'running',
	3: 'failed',
	4: 'done',
	5: 'cancelled',
}

export class Queue<T> {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #values: Values<T>
	readonly #in: Database | undefined

	constructor(
		link: Link,
		name: string,
		open: Uint8Array,
		values: Values<T>,
		inDatabase: Database | undefined,
	) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#values = values as Values<T>
		this.#in = inDatabase
		if (inDatabase !== undefined && !databaseBelongsTo(inDatabase, link)) {
			throw new InvalidError(`queue ${name}: its SQL database belongs to another store`)
		}
	}

	/** The queue's handle, its database opened first when it lives in one, as the server needs. */
	async #handle(connection: Connection): Promise<number> {
		await this.#in?.handle(connection)
		return handleOn(connection, methods['jobs.open'], this.#open)
	}

	/**
	 * The queue's enqueue inside a batch of the database it lives in, which
	 * it was opened `in`: the job commits with the batch's rows or not at all.
	 *
	 *     await db.batch(tx => {
	 *       tx.exec`update notes set body = ${body} where id = ${id}`
	 *       index.withTx(tx).enqueue({ id })
	 *     })
	 */
	withTx(tx: SqlBatch): QueueTx<T> {
		if (
			this.#in === undefined ||
			this.#in.name !== tx.database ||
			!batchBelongsTo(tx, this.#link)
		) {
			const lives = this.#in === undefined ? 'jobs.db' : `sql ${this.#in.name}`
			throw new InvalidError(
				`the queue ${this.name} lives in ${lives}, not in sql ${tx.database}: ` +
					"open it with { in: db } to enqueue in that database's batches",
			)
		}
		return new QueueTx(tx, this.#open, this.#values)
	}

	/** Adds a job; it returns once the job is in the file. */
	enqueue(value: T, options?: EnqueueOptions): Promise<void> {
		return this.enqueueAll([{ value, ...options }])
	}

	/** Adds jobs in one transaction, all or none: a refused one names itself as `call`. */
	async enqueueAll(jobs: readonly ({ value: T } & EnqueueOptions)[]): Promise<void> {
		const encoded = jobs.map(job => jobFields(this.#values, job.value, job))
		await this.#link.run('write', async connection => {
			const handle = await this.#handle(connection)
			await connection.session.call(
				methods['jobs.enqueue'],
				JobsBatch.encode({ handle, jobs: encoded }),
			)
		})
	}

	/**
	 * Changes a job that waits or failed: its value and, when the options
	 * say, its time or repeat. A job a worker holds, done or absent is
	 * ConflictError.
	 */
	async update(key: string, value: T, options?: Omit<EnqueueOptions, 'key'>): Promise<void> {
		const change = {
			value: this.#values.encode(value),
			key,
			at: options?.at === undefined ? undefined : unixMs(options.at),
			after: options?.after === undefined ? undefined : ms(options.after),
			repeat: options?.repeat === undefined ? undefined : repeatOf(options.repeat),
		}
		await this.#link.run('write', async connection => {
			const handle = await this.#handle(connection)
			await connection.session.call(
				methods['jobs.update'],
				JobsChange.encode({ ...change, handle }),
			)
		})
	}

	/**
	 * Removes the job under key, whether it waits, runs or failed, and says
	 * whether there was one. A running job's handler sees job.signal abort,
	 * and what it returns settles nothing.
	 */
	cancel(key: string): Promise<boolean> {
		return this.#link.run('write', async connection => {
			const handle = await this.#handle(connection)
			const body = await connection.session.call(
				methods['jobs.cancel'],
				JobsKey.encode({ handle, key }),
			)
			return JobsEntry.decode(body).found === true
		})
	}

	/**
	 * The job a key names, waiting, running or failed, or done while keepDone
	 * keeps its key; undefined when none is.
	 */
	async get(key: string): Promise<JobEntry<T> | undefined> {
		const entry = await this.#link.run('read', async connection => {
			const handle = await this.#handle(connection)
			return JobsEntry.decode(
				await connection.session.call(methods['jobs.get'], JobsKey.encode({ handle, key })),
			)
		})
		return entry.found === true ? this.#entryOf(entry) : undefined
	}

	async #entryOf(entry: ReturnType<typeof JobsEntry.decode>): Promise<JobEntry<T>> {
		const state = states[entry.state ?? 1] ?? 'waiting'
		return {
			key: entry.key ?? '',
			value:
				state === 'done' && entry.value === undefined
					? (undefined as T)
					: await this.#values.decode(entry.value ?? 'null'),
			at: new Date(entry.at ?? 0),
			attempt: entry.attempt ?? 0,
			state,
			ahead: entry.ahead ?? 0,
			progress: entry.progress === undefined ? undefined : JSON.parse(entry.progress),
			ran: dateOf(entry.ran),
			took: entry.ran === undefined ? undefined : (entry.took ?? 0),
			error: entry.err,
			repeat: entry.repeat,
		}
	}

	/**
	 * Yields the job under key as it is, then again each time its state,
	 * place, attempt, time, progress or error changes, until it ends: done,
	 * failed or cancelled, the last entry it yields. A key that names no job
	 * yields nothing. Leaving the loop ends the watch; a connection lost is
	 * connected again, and the watch goes on from the job as it is then.
	 *
	 *     for await (const s of videos.watch(id)) send(s.state, s.ahead, s.progress)
	 */
	async *watch(key: string): AsyncGenerator<JobEntry<T>> {
		let last: string | undefined
		for (;;) {
			const stream = await this.#link.run('read', async connection => {
				const handle = await this.#handle(connection)
				const opened = await connection.session.open(
					methods['jobs.watch'],
					JobsKey.encode({ handle, key }),
					true,
				)
				await opened.next()
				return opened
			})
			const unwatch = watch(currentSignal(), stream)
			try {
				for (;;) {
					const event = await stream.next()
					stream.consumed(event.body.length)
					if (event.end) {
						return
					}
					const entry = await this.#entryOf(JobsEntry.decode(event.body))
					const seen = JSON.stringify([
						entry.state,
						entry.ahead,
						entry.attempt,
						entry.at,
						entry.progress,
						entry.error,
					])
					if (seen !== last) {
						last = seen
						yield entry
					}
				}
			} catch (err) {
				if (!(err instanceof LostError)) {
					throw err
				}
			} finally {
				unwatch()
				stream.cancel()
			}
		}
	}

	/**
	 * A page of the jobs under a prefix, in the byte order of their keys; with
	 * state 'failed' and no prefix, the failed jobs, the last failed first.
	 */
	async scan(query?: { prefix?: string; state?: 'failed'; after?: string; limit?: number }) {
		const got = await this.#link.run('read', async connection => {
			const handle = await this.#handle(connection)
			const body = JobsQuery.encode({
				handle,
				prefix: query?.prefix,
				state: query?.state === 'failed' ? 3 : undefined,
				after: query?.after,
				limit: query?.limit,
			})
			return download(connection, methods['jobs.scan'], body)
		})
		const items: JobEntry<T>[] = []
		for (const item of got.items) {
			items.push(await this.#entryOf(JobsEntry.decode(item)))
		}
		const page = JobsPage.decode(got.trailer)
		return { items, next: page.more === true ? (page.after ?? '') : undefined } satisfies Page<
			JobEntry<T>,
			string
		>
	}

	/** Walks scan's pages, holding no snapshot between them. */
	async *all(query?: {
		prefix?: string
		state?: 'failed'
		limit?: number
	}): AsyncGenerator<JobEntry<T>> {
		let after: string | undefined
		for (;;) {
			const page = await this.scan({ ...query, ...(after === undefined ? {} : { after }) })
			yield* page.items
			if (page.next === undefined) {
				return
			}
			after = page.next
		}
	}

	/**
	 * Leases the next due job, or says there is none, without waiting. The
	 * claim lives on this connection: settle it before its lease ends.
	 */
	async claim(options?: { lease?: Duration }): Promise<ClaimedJob<T> | undefined> {
		return this.#link.run('write', async connection => {
			const handle = await this.#handle(connection)
			const lease = options?.lease === undefined ? undefined : ms(options.lease)
			const held = JobsHeld.decode(
				await connection.session.call(methods['jobs.claim'], JobsLease.encode({ handle, lease })),
			)
			if (held.found !== true) {
				return undefined
			}
			const value = await this.#values.decode(held.value ?? 'null')
			return new ClaimedJob(
				held,
				value,
				async outcome => {
					const body = await connection.session.call(
						methods['jobs.settle'],
						JobsOutcomes.encode({ outcomes: [outcome] }),
					)
					const refused = JobsSettled.decode(body).settled?.[0]
					if (refused !== undefined && refused !== null) {
						throw errorOf(refused.code ?? 'internal', refused.message ?? '', refused.what ?? {})
					}
				},
				(method, body) => connection.session.call(method, body),
			)
		})
	}

	/**
	 * Runs the queue's jobs as they come due, workers at once, until the
	 * signal aborts: the server's own Work loop claims ahead and extends
	 * leases, and nothing polls. A handler's return acknowledges its job and a
	 * throw retries it. A connection lost takes the jobs in hand with it, as a
	 * process that died would, and the loop connects again.
	 */
	async work(
		handler: (job: Job<T>) => void | Promise<void>,
		options: WorkOptions = {},
	): Promise<void> {
		for (;;) {
			options.signal?.throwIfAborted()
			try {
				await this.#link.run(
					'read',
					connection => this.#workOn(connection, handler, options),
					options.signal,
				)
				if (options.untilIdle === true || options.signal?.aborted === true) {
					return
				}
			} catch (err) {
				if (!(err instanceof LostError) || options.signal?.aborted === true) {
					throw err
				}
			}
		}
	}

	async #workOn(
		connection: Connection,
		handler: (job: Job<T>) => void | Promise<void>,
		options: WorkOptions,
	) {
		const handle = await this.#handle(connection)
		const timeout = options.timeout === undefined ? 60_000 : ms(options.timeout)
		const stream = await connection.session.open(
			methods['jobs.work'],
			JobsWorkers.encode({
				handle,
				workers: options.workers,
				timeout,
				untilIdle: options.untilIdle || undefined,
				cancels: true,
			}),
			false,
		)
		const stopping = new AbortController()
		const stop = () => stopping.abort()
		options.signal?.addEventListener('abort', stop, { once: true })
		const inHand = new Set<Promise<void>>()
		const cancels = new Map<number, AbortController>()
		let ended = false
		const endOurSide = async () => {
			if (!ended) {
				ended = true
				await Promise.allSettled(inHand)
				await stream.send(new Uint8Array(0), true).catch(() => {})
			}
		}
		stopping.signal.addEventListener('abort', () => void endOurSide(), { once: true })
		try {
			await stream.next()
			for (;;) {
				const event = await stream.next()
				stream.consumed(event.body.length)
				if (event.end) {
					return
				}
				const held = JobsHeld.decode(event.body)
				if (held.cancelled === true) {
					cancels.get(held.job ?? 0)?.abort(new CancelledError('cancel took the job while it ran'))
					continue
				}
				const cancel = new AbortController()
				cancels.set(held.job ?? 0, cancel)
				const steps = new Steps(
					(method, body) => connection.session.call(method, body),
					held.job ?? 0,
				)
				const running = this.#run(
					stream,
					held,
					steps,
					handler,
					timeout,
					stopping.signal,
					cancel.signal,
				).finally(() => cancels.delete(held.job ?? 0))
				inHand.add(running)
				running.finally(() => inHand.delete(running))
			}
		} finally {
			options.signal?.removeEventListener('abort', stop)
			stopping.abort()
		}
	}

	// runs one job's handler and sends the outcome it settled by; a handler
	// stopped by the loop's end gives its job back without counting the
	// attempt, and one cancel took settles nothing, so it sends nothing
	async #run(
		stream: Awaited<ReturnType<Connection['session']['open']>>,
		held: ReturnType<typeof JobsHeld.decode>,
		steps: Steps,
		handler: (job: Job<T>) => void | Promise<void>,
		timeout: number,
		stopping: AbortSignal,
		cancelled: AbortSignal,
	): Promise<void> {
		const signal = AbortSignal.any([stopping, AbortSignal.timeout(timeout), cancelled])
		const number = held.job ?? 0
		const reporter = new Reporter(progress =>
			stream.send(JobsOutcome.encode({ job: number, how: how.progress, progress }), false),
		)
		let outcome: Parameters<typeof JobsOutcome.encode>[0]
		try {
			const job = new Job(
				held.key ?? '',
				await this.#values.decode(held.value ?? 'null'),
				new Date(held.at ?? 0),
				held.attempt ?? 0,
				signal,
				reporter,
				steps,
			)
			try {
				await handler(job)
				outcome = outcomeOf(number, job.settlement ?? { how: 'ack' })
			} catch (err) {
				outcome =
					stopping.aborted && signal.aborted
						? { job: number, how: how.snooze, at: held.at ?? Date.now() }
						: outcomeOf(number, job.settlement ?? { how: 'retry', error: reasonOf(err) })
			}
		} catch (err) {
			outcome = { job: number, how: how.fail, err: `the value no longer reads: ${reasonOf(err)}` }
		}
		await reporter.stop()
		if (!cancelled.aborted) {
			await stream.send(JobsOutcome.encode(outcome), false).catch(() => {})
		}
	}
}

function outcomeOf(job: number, s: Settlement): Parameters<typeof JobsOutcome.encode>[0] {
	switch (s.how) {
		case 'ack':
			return { job, how: how.ack }
		case 'retry':
			return { job, how: how.retry, err: s.error, ...timing(s) }
		case 'fail':
			return { job, how: how.fail, err: s.error }
		case 'snooze':
			return { job, how: how.snooze, ...timing(s) }
	}
}
