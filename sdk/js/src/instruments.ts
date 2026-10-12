// A program's instruments, as plan/api/metrics.md has them: counters, gauges,
// a gauge read from a function, timers and histograms, each set of labels a
// series of its own. A call changes a number here and never waits. What
// changed goes to the store's totals on a stream of its own, a batch a
// second, each numbered in this run's session, so that the store counts it
// once: a batch whose answer a lost connection took goes again on the next,
// and a store that started again is sent nothing it may have counted.

import { randomBytes } from 'node:crypto'

import { FailureLog, messageOf } from './background.ts'
import type { Link } from './connection.ts'
import { InvalidError, UnavailableError } from './errors.ts'
import { sayOwn } from './logger.ts'
import { LostError, type Stream } from './session.ts'
import { type Duration, ms } from './time.ts'
import { MetricsAck, MetricsBatch, MetricsSession, methods } from './wire/protocol.ts'

/** A label's value as an instrument takes it: a number or a boolean is kept as its text. */
export type LabelValue = string | number | boolean

/** What an instrument is told when it is made. */
export interface InstrumentOptions {
	/** the unit of its values, 32 bytes at most: 'bytes', '%' */
	unit?: string | undefined
	/** a line on what it means, 1024 bytes at most */
	help?: string | undefined
	/** how long its name's samples stay, as ingest takes it */
	keep?: Duration | undefined
}

/** A timer's or a histogram's upper bounds, ascending, or the store's grid. */
export type Buckets = readonly number[] | 'exponential'

export interface TimerOptions {
	help?: string | undefined
	keep?: Duration | undefined
	/** the upper bounds of its spread, to which the store adds +Inf, or 'exponential' */
	buckets?: Buckets | undefined
}

export interface HistogramOptions extends InstrumentOptions {
	buckets?: Buckets | undefined
}

/**
 * The labels `with` takes: any, or, when the instrument was made with their
 * names, `counter<'route' | 'status'>(…)`, those alone, checked as they are written.
 */
export type InstrumentLabels<L extends string = string> = { readonly [K in L]?: LabelValue }

type Form = 'counter' | 'gauge' | 'timer' | 'histogram'

/** The sets of labels an instrument holds; past them it counts into the series of __overflow. */
const mostSets = 2000
const sendEvery = 1000
/** How often a gauge's function is read. */
const readEvery = 15_000
/** Batches sent and not yet answered, past which changes gather for the next. */
const mostUnanswered = 16
/** The changes a batch carries at most. */
const mostChanges = 1000
/** How long a counter's or a timer's set of labels stays without a call. */
const idle = 3_600_000
/** The bounds a timer or a histogram takes at most. */
const mostBounds = 128

/** An instrument as the stream declares it, and its sets of labels. */
interface Declared {
	id: number
	name: string
	form: Form
	unit: string | undefined
	help: string | undefined
	keep: number | undefined
	bounds: number[] | undefined
	exponential: boolean
	sets: Map<string, LabelSet>
	overflow: LabelSet | undefined
	/** the store refused it: its calls change nothing, said once */
	refused: boolean
	read: (() => unknown) | undefined
	/** a call of its function runs: it is not called again until it ends */
	reading: boolean
	/** the error its function last gave, said once while it repeats */
	readFailed: string | undefined
}

/** A set of labels of an instrument: its id on the stream, and a gauge's value. */
interface LabelSet {
	id: number
	instrument: Declared
	labels: Record<string, string>
	overflow: boolean
	/** when a call last changed it */
	touched: number
	value: number | undefined
}

/** What a set of labels changed since the batch before. */
interface Change {
	add: number | undefined
	value: number | undefined
	count: number
	sum: number
	longest: number
	buckets: number[] | undefined
	bins: Map<string, number> | undefined
}

interface Waiter {
	resolve: () => void
	reject: (err: Error) => void
}

/** A batch: its number in the session, its changes, and who waits for its answer. */
interface Batch {
	number: number
	changes: Map<LabelSet, Change>
	flush: boolean
	waiters: Waiter[]
	/** the stream it went on, since it goes again on the next */
	sentOn: Opened | undefined
}

/** A stream of the instruments, and what it declared. */
interface Opened {
	stream: Stream
	instruments: Set<number>
	sets: Set<number>
}

/**
 * The instruments of a store's handle, and what sends their changes. A store
 * has one; `store.metrics` makes its instruments through it.
 */
export class Instruments {
	readonly #link: Link
	readonly #session = randomBytes(16)
	readonly #declared = new Map<string, Declared>()
	readonly #byId = new Map<number, Declared>()
	#pending = new Map<LabelSet, Change>()
	#unanswered: Batch[] = []
	#answered = 0
	#next = 1
	#ids = 0
	#opened: Opened | undefined
	#opening: Promise<Opened> | undefined
	/** the sends in turn, so that batches leave in their order */
	#pumping: Promise<void> = Promise.resolve()
	#sending: ReturnType<typeof setInterval> | undefined
	#reading: ReturnType<typeof setInterval> | undefined
	#stopped = false
	readonly #failures = new FailureLog('metrics instruments', sayOwn)

	constructor(link: Link) {
		this.#link = link
	}

	/** An instrument of name, made the first time; another form or other options than it has are refused. */
	declare(name: string, form: Form, options: HistogramOptions, read?: () => unknown): Declared {
		const told = toldOf(name, form, options)
		const held = this.#declared.get(name)
		if (held !== undefined && read !== undefined) {
			throw new InvalidError(
				`metrics ${name}: a gauge's function is made once while the store is open`,
			)
		}
		if (held !== undefined) {
			if (
				held.form !== form ||
				!sameOptions(held, told) ||
				(read !== undefined) !== (held.read !== undefined)
			) {
				throw new InvalidError(
					`metrics ${name}: another instrument or other options than it has while the store is open`,
				)
			}
			return held
		}
		const declared: Declared = {
			...told,
			id: ++this.#ids,
			name,
			form,
			sets: new Map(),
			overflow: undefined,
			refused: false,
			read,
			reading: false,
			readFailed: undefined,
		}
		this.#declared.set(name, declared)
		this.#byId.set(declared.id, declared)
		this.#start()
		return declared
	}

	/** The set of labels of an instrument, made the first time; past the sets it holds, its __overflow. */
	setOf(instrument: Declared, labels: Record<string, string>): LabelSet {
		const key = keyOf(labels)
		const held = instrument.sets.get(key)
		if (held !== undefined) {
			return held
		}
		if (instrument.sets.size >= mostSets) {
			instrument.overflow ??= this.#labelled(instrument, {}, true)
			return instrument.overflow
		}
		const set = this.#labelled(instrument, labels, false)
		instrument.sets.set(key, set)
		return set
	}

	#labelled(instrument: Declared, labels: Record<string, string>, overflow: boolean): LabelSet {
		return { id: ++this.#ids, instrument, labels, overflow, touched: Date.now(), value: undefined }
	}

	/** What a call changes of a set, kept for the next batch. */
	change(set: LabelSet): Change {
		set.touched = Date.now()
		let change = this.#pending.get(set)
		if (change === undefined) {
			change = blank(set.instrument)
			this.#pending.set(set, change)
		}
		return change
	}

	/**
	 * Sends what changed now and has the store write its totals; resolves
	 * once it has, and says whether there was anything to send it with. One
	 * the store cannot be reached for fails as a call does, and what it
	 * carried goes once the store can be.
	 */
	async flush(): Promise<boolean> {
		if (this.#declared.size === 0) {
			return false
		}
		await this.#readGauges()
		const done = new Promise<void>((resolve, reject) => {
			const batches = this.#batches(true)
			batches.at(-1)?.waiters.push({ resolve, reject })
		})
		void this.#pump()
		await done
		return true
	}

	/** Stops the timers, sends what is left, and ends the stream; the store's close calls it. */
	async stop(): Promise<void> {
		if (this.#stopped) {
			return
		}
		this.#stopped = true
		clearInterval(this.#sending)
		clearInterval(this.#reading)
		if (this.#declared.size === 0) {
			return
		}
		try {
			await this.flush()
		} catch (err) {
			this.#failures.failed(err)
		}
		await this.#opened?.stream.send(new Uint8Array(0), true).catch(() => {})
	}

	#start(): void {
		if (this.#sending !== undefined || this.#stopped) {
			return
		}
		this.#sending = setInterval(() => this.#tick(), sendEvery)
		this.#sending.unref?.()
		this.#reading = setInterval(() => void this.#readGauges(), readEvery)
		this.#reading.unref?.()
	}

	#tick(): void {
		const now = Date.now()
		for (const instrument of this.#declared.values()) {
			if (instrument.form === 'gauge') {
				continue
			}
			for (const [key, set] of instrument.sets) {
				if (now - set.touched >= idle && !this.#pending.has(set)) {
					instrument.sets.delete(key)
				}
			}
		}
		if (this.#pending.size > 0 && this.#unanswered.length < mostUnanswered) {
			this.#batches(false)
		}
		void this.#pump()
	}

	/** What waits, as batches of mostChanges changes each; with flush, the last asks the store to write. */
	#batches(flush: boolean): Batch[] {
		const made: Batch[] = []
		const pending = [...this.#pending]
		this.#pending = new Map()
		for (let from = 0; from < pending.length || (flush && made.length === 0); from += mostChanges) {
			const changes = new Map(pending.slice(from, from + mostChanges))
			const last = from + mostChanges >= pending.length
			made.push({
				number: this.#next++,
				changes,
				flush: flush && last,
				waiters: [],
				sentOn: undefined,
			})
		}
		this.#unanswered.push(...made)
		return made
	}

	/** Sends every unanswered batch that has not gone on the stream open now, in order, a send at a time. */
	#pump(): Promise<void> {
		this.#pumping = this.#pumping.then(() => this.#sendWaiting())
		return this.#pumping
	}

	// a connection lost on the way is dialled again, as a call's is; a store
	// that cannot be dialled is tried again at the next tick, not at once
	async #sendWaiting(): Promise<void> {
		for (let tries = 0; !this.#sentAll(); tries++) {
			try {
				await this.#sendOn(await this.#stream())
				this.#failures.succeeded()
				return
			} catch (err) {
				if (tries < 2 && err instanceof LostError) {
					continue
				}
				this.#failures.failed(err)
				this.#fail(err)
				return
			}
		}
	}

	#sentAll(): boolean {
		return this.#unanswered.every(
			batch => batch.sentOn !== undefined && batch.sentOn === this.#opened,
		)
	}

	async #sendOn(opened: Opened): Promise<void> {
		for (const batch of this.#unanswered) {
			if (batch.sentOn !== opened) {
				batch.sentOn = opened
				await opened.stream.send(encode(batch, opened), false)
			}
		}
	}

	/** Every flush waiting fails with err; the batches wait for the next send. */
	#fail(err: unknown): void {
		for (const batch of this.#unanswered) {
			for (const waiter of batch.waiters.splice(0)) {
				waiter.reject(errorOf(err))
			}
		}
	}

	/**
	 * The store refused the batch it was to apply next and would refuse it
	 * again: it is dropped, said once, and those after it are numbered on.
	 */
	#drop(err: unknown): void {
		const dropped = this.#unanswered.shift()
		if (dropped === undefined) {
			return
		}
		const error = messageOf(err)
		sayOwn('warn', 'metrics instruments dropped a batch the store refused', {
			changes: dropped.changes.size,
			error,
		})
		for (const waiter of dropped.waiters) {
			waiter.reject(errorOf(err))
		}
		this.#renumber()
	}

	/** The stream open now, or one opened and resumed: the store says what it applied of the session. */
	#stream(): Promise<Opened> {
		if (this.#opened !== undefined && !this.#opened.stream.finished) {
			return Promise.resolve(this.#opened)
		}
		this.#opening ??= this.#open().finally(() => {
			this.#opening = undefined
		})
		return this.#opening
	}

	async #open(): Promise<Opened> {
		const connection = await this.#link.connection()
		const body = MetricsSession.encode({ id: this.#session, after: this.#answered })
		const stream = await connection.session.open(methods['metrics.instruments'], body, false)
		const head = await stream.next()
		const opened: Opened = { stream, instruments: new Set(), sets: new Set() }
		this.#resume(MetricsAck.decode(head.body))
		this.#opened = opened
		void this.#listen(opened)
		return opened
	}

	/**
	 * Takes up where the store is: a batch it applied is done; one sent that
	 * it may have applied, when it knows no session, is dropped rather than
	 * counted twice, and those never sent are numbered on. Every gauge's value
	 * goes again, since a gauge left with the last connection.
	 */
	#resume(ack: ReturnType<typeof MetricsAck.decode>): void {
		const applied = ack.applied ?? 0
		if (ack.known === true) {
			this.#answered = Math.max(this.#answered, applied)
			this.#settle(applied)
		} else {
			const lost = this.#unanswered.filter(batch => batch.sentOn !== undefined)
			this.#unanswered = this.#unanswered.filter(batch => batch.sentOn === undefined)
			if (lost.length > 0) {
				const changes = lost.reduce((n, batch) => n + batch.changes.size, 0)
				sayOwn(
					'warn',
					'metrics instruments dropped what a store that started again may have counted',
					{
						batches: lost.length,
						changes,
					},
				)
			}
			for (const batch of lost) {
				for (const waiter of batch.waiters) {
					waiter.reject(
						new UnavailableError('the store started again; what this flush sent may be lost'),
					)
				}
			}
			this.#answered = applied
			this.#renumber()
		}
		for (const instrument of this.#declared.values()) {
			for (const set of instrument.form === 'gauge' ? instrument.sets.values() : []) {
				if (set.value !== undefined) {
					this.change(set).value = set.value
				}
			}
		}
	}

	/** Numbers what waits after the last batch the store answered. */
	#renumber(): void {
		this.#next = this.#answered + 1
		for (const batch of this.#unanswered) {
			batch.number = this.#next++
		}
	}

	/**
	 * Reads the store's answers on a stream until it ends. What it had not
	 * answered goes again at once on another, but for a batch it refused.
	 */
	async #listen(opened: Opened): Promise<void> {
		try {
			for (;;) {
				const event = await opened.stream.next()
				opened.stream.consumed(event.body.length)
				this.#answer(MetricsAck.decode(event.body))
				if (event.end) {
					return
				}
			}
		} catch (err) {
			if (!this.#stopped) {
				this.#failures.failed(err)
			}
			if (err instanceof InvalidError && !this.#stopped) {
				this.#drop(err)
			}
		} finally {
			if (this.#opened === opened) {
				this.#opened = undefined
			}
			if (!this.#stopped && this.#unanswered.length > 0) {
				void this.#pump()
			}
		}
	}

	#answer(ack: ReturnType<typeof MetricsAck.decode>): void {
		const applied = ack.applied ?? 0
		this.#answered = Math.max(this.#answered, applied)
		this.#settle(applied)
		for (const refused of ack.refused ?? []) {
			const instrument = this.#byId.get(refused.instrument ?? 0)
			if (instrument !== undefined && !instrument.refused) {
				instrument.refused = true
				sayOwn('warn', 'metrics instrument refused', {
					instrument: instrument.name,
					error: refused.error ?? '',
				})
			}
		}
	}

	/** The batches the store applied, each done. */
	#settle(applied: number): void {
		while ((this.#unanswered[0]?.number ?? Infinity) <= applied) {
			const done = this.#unanswered.shift()
			for (const waiter of done?.waiters ?? []) {
				waiter.resolve()
			}
		}
	}

	/** Calls each gauge's function whose last call ended; a value it gives is sent with the next batch. */
	async #readGauges(): Promise<void> {
		const reads: Promise<void>[] = []
		for (const instrument of this.#declared.values()) {
			if (instrument.read !== undefined && !instrument.reading && !instrument.refused) {
				reads.push(this.#readGauge(instrument, instrument.read))
			}
		}
		await Promise.all(reads)
	}

	async #readGauge(instrument: Declared, read: () => unknown): Promise<void> {
		instrument.reading = true
		try {
			const value = await read()
			if (typeof value !== 'number' || !Number.isFinite(value)) {
				throw new InvalidError(`a gauge's function gave ${String(value)}, where a finite number is`)
			}
			const set = this.setOf(instrument, {})
			set.value = value
			this.change(set).value = value
			instrument.readFailed = undefined
		} catch (err) {
			const error = messageOf(err)
			if (error !== instrument.readFailed) {
				sayOwn('warn', 'metrics gauge read failed', { instrument: instrument.name, error })
				instrument.readFailed = error
			}
		} finally {
			instrument.reading = false
		}
	}
}

function errorOf(err: unknown): Error {
	return err instanceof Error ? err : new UnavailableError(String(err))
}

/** A change of nothing yet, with room for an instrument's buckets. */
function blank(instrument: Declared): Change {
	const buckets =
		instrument.bounds === undefined ? undefined : new Array(instrument.bounds.length + 1).fill(0)
	const bins = instrument.exponential ? new Map<string, number>() : undefined
	return { add: undefined, value: undefined, count: 0, sum: 0, longest: 0, buckets, bins }
}

/** A batch's bytes for a stream: the instruments and sets it names that the stream has not declared. */
function encode(batch: Batch, opened: Opened): Uint8Array {
	const instruments = []
	const sets = []
	const changes = []
	for (const [set, change] of batch.changes) {
		const instrument = set.instrument
		if (instrument.refused) {
			continue
		}
		if (!opened.instruments.has(instrument.id)) {
			opened.instruments.add(instrument.id)
			instruments.push({
				number: instrument.id,
				name: instrument.name,
				form: instrument.form,
				unit: instrument.unit,
				help: instrument.help,
				keep: instrument.keep,
				bounds: instrument.bounds,
				exponential: instrument.exponential,
			})
		}
		if (!opened.sets.has(set.id)) {
			opened.sets.add(set.id)
			sets.push({
				number: set.id,
				instrument: instrument.id,
				labels: set.labels,
				overflow: set.overflow,
			})
		}
		changes.push({
			set: set.id,
			add: change.add,
			value: change.value,
			count: change.count,
			sum: change.sum,
			longest: change.longest,
			buckets: change.buckets,
			bins: change.bins === undefined ? undefined : Object.fromEntries(change.bins),
		})
	}
	return MetricsBatch.encode({
		number: batch.number,
		instruments,
		sets,
		changes,
		flush: batch.flush,
	})
}

/** What an instrument was told, checked. */
function toldOf(
	name: string,
	form: Form,
	options: HistogramOptions,
): Pick<Declared, 'unit' | 'help' | 'keep' | 'bounds' | 'exponential'> {
	checkName(name)
	if (options.unit !== undefined && bytes(options.unit) > 32) {
		throw new InvalidError(`metrics ${name}: a unit of 32 bytes at most`)
	}
	if (options.help !== undefined && bytes(options.help) > 1024) {
		throw new InvalidError(`metrics ${name}: a help of 1024 bytes at most`)
	}
	const keep = options.keep === undefined ? undefined : ms(options.keep)
	if (keep === 0) {
		throw new InvalidError(`metrics ${name}: keep 0, where a name keeps its samples 1ms at least`)
	}
	const buckets = options.buckets
	if (buckets !== undefined && form !== 'timer' && form !== 'histogram') {
		throw new InvalidError(`metrics ${name}: buckets are a timer's or a histogram's`)
	}
	if (buckets === 'exponential') {
		return { unit: options.unit, help: options.help, keep, bounds: undefined, exponential: true }
	}
	return {
		unit: options.unit,
		help: options.help,
		keep,
		bounds: buckets === undefined ? undefined : checkedBounds(name, buckets),
		exponential: false,
	}
}

function checkedBounds(name: string, bounds: readonly number[]): number[] {
	if (bounds.length === 0 || bounds.length > mostBounds) {
		throw new InvalidError(`metrics ${name}: 1 to ${mostBounds} bounds`)
	}
	bounds.forEach((bound, i) => {
		if (!(Number.isFinite(bound) && bound >= 0) || (i > 0 && !(bound > bounds[i - 1]!))) {
			throw new InvalidError(`metrics ${name}: bounds of 0 or more, each above the one before`)
		}
	})
	return [...bounds]
}

function sameOptions(held: Declared, told: ReturnType<typeof toldOf>): boolean {
	return (
		held.unit === told.unit &&
		held.help === told.help &&
		held.keep === told.keep &&
		held.exponential === told.exponential &&
		JSON.stringify(held.bounds) === JSON.stringify(told.bounds)
	)
}

function checkName(name: string): void {
	if (name === '' || !name.isWellFormed() || bytes(name) > 4096) {
		throw new InvalidError(`metrics name ${JSON.stringify(name)}: 1 to 4096 bytes of UTF-8`)
	}
}

const encoder = new TextEncoder()

function bytes(text: string): number {
	return encoder.encode(text).length
}

/** Labels as an instrument keeps them: text, a number or a boolean as its text, none of the store's own. */
export function labelsOf(given: InstrumentLabels, timed: boolean): Record<string, string> {
	const labels: Record<string, string> = {}
	for (const [label, value] of Object.entries(given)) {
		if (label === '' || bytes(label) > 256) {
			throw new InvalidError(`label ${JSON.stringify(label)}: a name of 1 to 256 bytes`)
		}
		if (label.startsWith('__')) {
			throw new InvalidError(`label ${label}: a name beginning with __ is the store's own`)
		}
		if (timed && label === 'le') {
			throw new InvalidError(`label le: a timer's _bucket series give it`)
		}
		if (typeof value !== 'string' && typeof value !== 'number' && typeof value !== 'boolean') {
			throw new InvalidError(
				`label ${label}: a value of text, a number or a boolean, where ${String(value)} is`,
			)
		}
		const text = String(value)
		if (bytes(text) > 4096) {
			throw new InvalidError(`label ${label}: a value of 4096 bytes at most`)
		}
		labels[label] = text
	}
	return labels
}

function keyOf(labels: Record<string, string>): string {
	return JSON.stringify(Object.entries(labels).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)))
}

/** A finite number an instrument takes, or invalid at once where the program made it. */
function finite(name: string, what: string, value: number): number {
	if (typeof value !== 'number' || !Number.isFinite(value)) {
		throw new InvalidError(`metrics ${name}: ${what} ${String(value)}, where a finite number is`)
	}
	return value
}

/** A total that only goes up, which the store keeps: processes counting into one series count into one total. */
export class Counter<L extends string = string> {
	readonly #instruments: Instruments
	readonly #declared: Declared
	readonly #labels: Record<string, string>

	constructor(instruments: Instruments, declared: Declared, labels: Record<string, string>) {
		this.#instruments = instruments
		this.#declared = declared
		this.#labels = labels
	}

	/** The same counter with more labels, a series of its own; the same labels in any order are one. */
	with(labels: InstrumentLabels<L>): Counter<L> {
		return new Counter(this.#instruments, this.#declared, {
			...this.#labels,
			...labelsOf(labels, false),
		})
	}

	inc(): void {
		this.add(1)
	}

	/** Adds n, a finite number of 0 or more; add(0) makes the set of labels known with a total of zero. */
	add(n: number): void {
		if (finite(this.#declared.name, 'a counter adds', n) < 0) {
			throw new InvalidError(`metrics ${this.#declared.name}: a counter adds ${n}; it only grows`)
		}
		if (this.#declared.refused) {
			return
		}
		const change = this.#instruments.change(this.#instruments.setOf(this.#declared, this.#labels))
		change.add = (change.add ?? 0) + n
	}
}

/** A value that goes up and down, written while this program is connected. */
export class Gauge<L extends string = string> {
	readonly #instruments: Instruments
	readonly #declared: Declared
	readonly #labels: Record<string, string>

	constructor(instruments: Instruments, declared: Declared, labels: Record<string, string>) {
		this.#instruments = instruments
		this.#declared = declared
		this.#labels = labels
	}

	with(labels: InstrumentLabels<L>): Gauge<L> {
		return new Gauge(this.#instruments, this.#declared, {
			...this.#labels,
			...labelsOf(labels, false),
		})
	}

	set(value: number): void {
		this.#to(finite(this.#declared.name, 'a gauge set to', value))
	}

	/** Moves it by n, from zero when it was never set. */
	add(n: number): void {
		finite(this.#declared.name, 'a gauge moved by', n)
		const set = this.#instruments.setOf(this.#declared, this.#labels)
		this.#to(finite(this.#declared.name, 'a gauge moved to', (set.value ?? 0) + n))
	}

	inc(): void {
		this.add(1)
	}

	dec(): void {
		this.add(-1)
	}

	#to(value: number): void {
		if (this.#declared.refused) {
			return
		}
		const set = this.#instruments.setOf(this.#declared, this.#labels)
		set.value = value
		this.#instruments.change(set).value = value
	}
}

/** Counts a duration or a value into a timer's or a histogram's change: its count, sum, longest and bucket. */
function measured(
	instruments: Instruments,
	declared: Declared,
	labels: Record<string, string>,
	value: number,
): void {
	if (declared.refused) {
		return
	}
	const change = instruments.change(instruments.setOf(declared, labels))
	change.count++
	change.sum += value
	change.longest = change.count === 1 ? value : Math.max(change.longest, value)
	if (change.buckets !== undefined && declared.bounds !== undefined) {
		const bucket = declared.bounds.findIndex(bound => value <= bound)
		change.buckets[bucket < 0 ? declared.bounds.length : bucket]! += 1
	}
	if (change.bins !== undefined) {
		const bin = binOf(value)
		change.bins.set(bin, (change.bins.get(bin) ?? 0) + 1)
	}
}

/** How many things took how long: their count, sum and longest in milliseconds, and their spread over its buckets. */
export class Timer<L extends string = string> {
	readonly #instruments: Instruments
	readonly #declared: Declared
	readonly #labels: Record<string, string>

	constructor(instruments: Instruments, declared: Declared, labels: Record<string, string>) {
		this.#instruments = instruments
		this.#declared = declared
		this.#labels = labels
	}

	with(labels: InstrumentLabels<L>): Timer<L> {
		return new Timer(this.#instruments, this.#declared, {
			...this.#labels,
			...labelsOf(labels, true),
		})
	}

	/** Records a duration measured elsewhere: milliseconds, as performance.now() counts them, or text such as '250ms'. */
	record(duration: Duration): void {
		const millis =
			typeof duration === 'number'
				? finite(this.#declared.name, 'a duration of', duration)
				: ms(duration)
		if (millis < 0) {
			throw new InvalidError(
				`metrics ${this.#declared.name}: a duration of ${millis}; a duration is never negative`,
			)
		}
		measured(this.#instruments, this.#declared, this.#labels, millis)
	}

	/**
	 * Runs fn, awaits what it returns, returns or throws what it does, and
	 * records how long it took either way.
	 *
	 *     const user = await latency.with({ route: '/users' }).measure(() => users.get(id))
	 */
	async measure<R>(fn: () => R | Promise<R>): Promise<R> {
		const started = performance.now()
		try {
			return await fn()
		} finally {
			this.record(performance.now() - started)
		}
	}

	/** A timing that records when it stops, for a span that is no one function. */
	start(): Timing {
		return new Timing(this)
	}
}

/** A span a timer measures from its start; stop records it, once. */
export class Timing {
	readonly #timer: Timer
	readonly #started = performance.now()
	#stopped = false

	constructor(timer: Timer) {
		this.#timer = timer
	}

	/** Records how long since the timing started, the first time it is called; its milliseconds. */
	stop(): number {
		const took = performance.now() - this.#started
		if (!this.#stopped) {
			this.#stopped = true
			this.#timer.record(took)
		}
		return took
	}
}

/** A timer of numbers that are not durations: a response's size in bytes. */
export class Histogram<L extends string = string> {
	readonly #instruments: Instruments
	readonly #declared: Declared
	readonly #labels: Record<string, string>

	constructor(instruments: Instruments, declared: Declared, labels: Record<string, string>) {
		this.#instruments = instruments
		this.#declared = declared
		this.#labels = labels
	}

	with(labels: InstrumentLabels<L>): Histogram<L> {
		return new Histogram(this.#instruments, this.#declared, {
			...this.#labels,
			...labelsOf(labels, true),
		})
	}

	/** Records a value of 0 or more. */
	record(value: number): void {
		if (finite(this.#declared.name, 'a value of', value) < 0) {
			throw new InvalidError(
				`metrics ${this.#declared.name}: a value of ${value}; the lowest bucket starts at 0`,
			)
		}
		measured(this.#instruments, this.#declared, this.#labels, value)
	}
}

/** Upper bounds from start, each factor times the one before: exponentialBuckets(1, 2, 14) is 1, 2, 4 … 8192. */
export function exponentialBuckets(start: number, factor: number, count: number): number[] {
	if (!(start > 0 && factor > 1 && Number.isInteger(count) && count >= 1 && count <= mostBounds)) {
		throw new InvalidError(
			`exponentialBuckets(${start}, ${factor}, ${count}): a start above 0, a factor above 1, 1 to ${mostBounds} bounds`,
		)
	}
	const bounds = [start]
	while (bounds.length < count) {
		bounds.push(bounds.at(-1)! * factor)
	}
	return bounds
}

/**
 * The fractions of Prometheus' native grid for schema 3, 2^(j/8 − 1), as its
 * table writes them: three a unit in the last place below the nearest float,
 * so that a value on a bound falls where Prometheus and the store put it.
 */
const fractions = [
	0.5, 0.5452538663326288, 0.5946035575013605, 0.6484197773255048, 0.7071067811865475,
	0.7711054127039704, 0.8408964152537144, 0.9170040432046711,
]

const halves = new DataView(new ArrayBuffer(8))

/** The interval of the store's grid a value of 0 or more falls in, by its label: '3:13', or 'zero'. */
export function binOf(value: number): string {
	if (value === 0) {
		return 'zero'
	}
	// a subnormal falls in the lowest interval, whose bound is the least normal float
	halves.setFloat64(0, Math.max(value, 2.2250738585072014e-308))
	const high = halves.getUint32(0)
	const exponent = ((high >>> 20) & 0x7ff) - 1022
	halves.setUint32(0, (high & ~(0x7ff << 20)) | (1022 << 20))
	const fraction = halves.getFloat64(0)
	const within = fractions.findIndex(bound => bound >= fraction)
	return `3:${(within < 0 ? 8 : within) + (exponent - 1) * 8}`
}
