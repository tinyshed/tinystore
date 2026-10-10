// The application's own SQLite databases, sql/<name>.db, as plan/api/sqldb.md
// has them: the application writes its SQL, or builds it with a table's
// queries, and the server owns the file, its connections and its migrations,
// which every open applies and checks. A statement leaves as its text and the
// values of its ?s, each as SQLite keeps it.

import { AsyncLocalStorage } from 'node:async_hooks'
import { readdir, readFile } from 'node:fs/promises'
import { join } from 'node:path'

import type { Connection, Durability, Idempotence, Link } from './connection.ts'
import { checkDurability, download } from './connection.ts'
import { errorOf, InvalidError } from './errors.ts'
import {
	askByCall,
	checkName,
	type Home,
	handleOn,
	type Open,
	type Via,
	viaLink,
} from './handles.ts'
import {
	openQueue,
	type Queue,
	type QueueOptions,
	queueParts,
	queueVia,
	type TxQueue,
	type Worker,
} from './jobs.ts'
import { Bucket, type BucketOptions, bucketOpen, partsOf, type Values, valuesOf } from './kv.ts'
import { Table, type TableOptions } from './query.ts'
import type { StandardSchemaV1 } from './schema.ts'
import type { Stream } from './session.ts'
import type { SqlArg, SqlValue } from './wire/codec.ts'
import {
	methods,
	SqlBatch,
	SqlBatched,
	SqlDone,
	SqlOpen,
	SqlQuery,
	SqlRows,
	SqlStatement,
	SqlTxAnswer,
	SqlTxCall,
	SqlTxOpen,
} from './wire/protocol.ts'

export type { SqlArg, SqlValue }

/** A row as SQLite gave it, by its columns' names. */
export type Row = Record<string, SqlValue>

/**
 * A database's migrations: a directory of .sql files named 0001_notes.sql,
 * 0002_tags.sql, or the files themselves by name, for a program bundled into
 * one file. A relative directory is the working directory's.
 */
export type Migrations = string | Record<string, string> | readonly { name: string; sql: string }[]

export interface DatabaseOptions {
	/** applied when the file has not, and checked at every open; without them the file opens as it is */
	migrations?: Migrations
	/**
	 * How far this database's commits go before they return, where the
	 * store's own is not this file's: `'os'` for what may be lost to a power
	 * loss, beside a store of what may not. Its buckets and queues commit
	 * with it. A database open already keeps what it was opened with.
	 */
	durability?: Durability
}

/** What a write changed: the rows, and SQLite's rowid of the row it inserted, 0 when none. */
export interface Done {
	changes: number
	lastInsertRowid: number | bigint
}

/**
 * A statement in any of the ways a call takes it: a template whose values are
 * its values, the text and the values of its ?s, the text and one object of
 * named values, or a piece of SQL from `sql`.
 *
 *     db.all`select * from notes where id = ${id}`
 *     db.all('select * from notes where id = ?', id)
 *     db.all('select * from notes where id = :id', { id })
 */
export type Statement = [text: string | TemplateStringsArray | Sql, ...values: unknown[]]

/**
 * A piece of SQL and the values of its ?s, which are always values and never
 * SQL. `sql` makes one; a call, a batch and a table's query take it.
 */
export class Sql {
	readonly text: string
	readonly values: readonly SqlArg[]
	/** whether it stands beside others in an and or an or without parentheses: one column's comparison */
	readonly bare: boolean

	/** Pieces come from `sql`. */
	constructor(text: string, values: readonly SqlArg[], bare = false) {
		this.text = text
		this.values = values
		this.bare = bare
	}
}

/** Whether text is one group in parentheses, or a bare 0 or 1, which may stand beside others as it is. */
export function grouped(text: string): boolean {
	if (!text.startsWith('(') || !text.endsWith(')')) {
		return /^[01]$/.test(text)
	}
	let depth = 0
	for (let at = 0; at < text.length; at++) {
		if (text[at] === '(') depth++
		if (text[at] === ')') depth--
		if (depth === 0 && at < text.length - 1) {
			return false
		}
	}
	return true
}

/** Pieces joined with and or or, each in parentheses unless it may stand as it is. */
export function joinedWith(pieces: readonly Sql[], joiner: string): Sql {
	const several = pieces.length > 1
	return new Sql(
		pieces
			.map(each =>
				several && !each.bare && !standsAlone(each.text) ? `(${each.text})` : each.text,
			)
			.join(` ${joiner} `),
		pieces.flatMap(each => each.values),
	)
}

/**
 * Whether a piece of SQL keeps its meaning beside others in an and or an or:
 * one group, or text with no and or or of its own outside quotes, comments
 * and parentheses, which would bind with its neighbours'.
 */
export function standsAlone(text: string): boolean {
	if (grouped(text)) {
		return true
	}
	let depth = 0
	for (let at = 0; at < text.length; at++) {
		const c = text[at]!
		if (c === "'" || c === '"' || c === '`') {
			at = closing(text, at, c)
		} else if (c === '-' && text[at + 1] === '-') {
			at = text.indexOf('\n', at)
			if (at < 0) break
		} else if (c === '/' && text[at + 1] === '*') {
			at = text.indexOf('*/', at + 2)
			if (at < 0) break
			at++
		} else if (c === '(') {
			depth++
		} else if (c === ')') {
			depth--
		} else if (depth === 0 && /[a-z]/i.test(c) && !/[a-z0-9_$]/i.test(text[at - 1] ?? '')) {
			const word = /^[a-z_][a-z0-9_$]*/i.exec(text.slice(at))![0]
			if (/^(and|or)$/i.test(word)) {
				return false
			}
			at += word.length - 1
		}
	}
	return true
}

/** Something that writes itself as SQL inside another piece: a table's query. */
export interface Embeddable {
	/** its text and values, wrapped as a subquery is */
	embedded(): Sql
}

function isEmbeddable(value: unknown): value is Embeddable {
	return (
		typeof value === 'object' &&
		value !== null &&
		typeof (value as Embeddable).embedded === 'function'
	)
}

/** A piece of SQL from a template, or from text and the values of its ?s. */
function piece(first: TemplateStringsArray | string, values: unknown[]): Sql {
	if (typeof first === 'string') {
		const counted = placeholders(first)
		if (counted.named.length > 0 || counted.numbered) {
			throw new InvalidError(`a piece of SQL takes its values at ? alone: ${first}`)
		}
		if (counted.positional !== values.length) {
			throw new InvalidError(
				`${counted.positional} placeholders and ${values.length} values: each ? takes one value`,
			)
		}
		return new Sql(
			first,
			values.map((value, at) => cellOf(value, `value ${at + 1}`)),
		)
	}
	let text = first[0] ?? ''
	const out: SqlArg[] = []
	values.forEach((value, at) => {
		if (value instanceof Sql) {
			text += value.text
			out.push(...value.values)
		} else if (isEmbeddable(value)) {
			const inner = value.embedded()
			text += inner.text
			out.push(...inner.values)
		} else {
			text += '?'
			out.push(cellOf(value, `value ${at + 1}`))
		}
		text += first[at + 1] ?? ''
	})
	return new Sql(text, out)
}

/** A condition: equal values in an object, or a piece of SQL. */
export type Condition = Sql | Record<string, unknown>

/**
 * A piece of SQL: `` sql`total >= ${min}` ``, or `sql('total >= ?', min)`.
 * Its helpers build the pieces a query's conditions are made of.
 */
export interface SqlTag {
	(strings: TemplateStringsArray, ...values: unknown[]): Sql
	(text: string, ...values: unknown[]): Sql
	/** conditions joined with or, in parentheses; an empty or finds nothing */
	or(...conditions: Condition[]): Sql
	/** conditions joined with and, in parentheses; an empty and finds everything */
	and(...conditions: Condition[]): Sql
	/** a list for in (…), as one value SQLite reads with json_each: any length, the same statement */
	list(values: readonly unknown[]): Sql
	/** a value kept as its JSON */
	json(value: unknown): Sql
	/** a name, quoted, never SQL: a column a request chooses */
	ident(name: string): Sql
	/** a JSON list in column `name` holding `value` */
	has(name: string, value: unknown): Sql
}

export const sql: SqlTag = Object.assign(
	(first: TemplateStringsArray | string, ...values: unknown[]) => piece(first, values),
	{
		or: (...conditions: Condition[]) => group(conditions, 'or', '0'),
		and: (...conditions: Condition[]) => group(conditions, 'and', '1'),
		list: (values: readonly unknown[]) =>
			new Sql('(select value from json_each(?))', [JSON.stringify(values.map(listed))]),
		json: (value: unknown) => new Sql('?', [JSON.stringify(value)]),
		ident: (name: string) => new Sql(identOf(name), []),
		has: (name: string, value: unknown) =>
			new Sql(`exists (select 1 from json_each(${identOf(name)}) where value = ?)`, [
				cellOf(value, `${name}'s value`),
			]),
	},
)

/** A list's item as JSON keeps it: a Date as its milliseconds, bytes refused. */
function listed(value: unknown): unknown {
	if (value instanceof Date) {
		return value.getTime()
	}
	if (typeof value === 'bigint') {
		return Number(value)
	}
	if (value === undefined || value instanceof Uint8Array) {
		throw new InvalidError('a list for in (…) holds text, numbers, booleans and null')
	}
	return value
}

function group(conditions: Condition[], joiner: 'or' | 'and', empty: string): Sql {
	const pieces = conditions.map(conditionOf)
	if (pieces.length === 0) {
		return new Sql(empty, [])
	}
	const joined = joinedWith(pieces, joiner)
	return new Sql(`(${joined.text})`, joined.values)
}

/** A condition as a piece of SQL: equal values written in the order of their columns' names. */
export function conditionOf(condition: Condition | string): Sql {
	if (condition instanceof Sql) {
		return condition
	}
	if (typeof condition === 'string') {
		return piece(condition, [])
	}
	const names = Object.keys(condition).sort()
	const pieces = names.map(name => equal(name, condition[name]))
	if (pieces.length === 1) {
		return pieces[0]!
	}
	return group(pieces, 'and', '1')
}

function equal(name: string, value: unknown): Sql {
	const column = identOf(name)
	if (value === undefined) {
		throw new InvalidError(
			`${name} is undefined, which a filter never leaves out: a missing value is a bug, or an if`,
		)
	}
	if (value === null) {
		return new Sql(`${column} is null`, [], true)
	}
	if (Array.isArray(value)) {
		const list = sql.list(value)
		return new Sql(`${column} in ${list.text}`, list.values, true)
	}
	return new Sql(`${column} = ?`, [cellOf(value, name)], true)
}

/** A name as SQL quotes it: "u"."created_at", each part on its own. */
export function identOf(name: string): string {
	if (name.length === 0 || name.includes('\0')) {
		throw new InvalidError(`the name ${JSON.stringify(name)}: a name is text without NUL`)
	}
	return name
		.split('.')
		.map(part => `"${part.replaceAll('"', '""')}"`)
		.join('.')
}

/**
 * A value as it leaves: SQLite's five, a boolean, kept as 1 or 0, and a Date,
 * kept as its unix milliseconds. Anything that would arrive changed is
 * refused here, before anything is sent.
 */
export function cellOf(value: unknown, where: string): SqlArg {
	if (value === undefined) {
		throw new InvalidError(`${where} is undefined: a value that is missing is null, said so`)
	}
	if (typeof value === 'number' && Number.isInteger(value) && !Number.isSafeInteger(value)) {
		throw new InvalidError(
			`${where}, ${value}, is past 2^53, where a number has lost digits: pass a bigint`,
		)
	}
	if (
		value === null ||
		typeof value === 'string' ||
		typeof value === 'number' ||
		typeof value === 'boolean' ||
		typeof value === 'bigint' ||
		value instanceof Uint8Array ||
		value instanceof Date
	) {
		return value
	}
	if (Array.isArray(value)) {
		throw new InvalidError(
			`${where} is an array: a list for in (…) is sql.list(…), a JSON value sql.json(…)`,
		)
	}
	throw new InvalidError(`${where} is an object: a JSON value is sql.json(…)`)
}

/** What a text's placeholders are, counted outside quotes and comments. */
export function placeholders(text: string): {
	positional: number
	named: string[]
	numbered: boolean
} {
	let positional = 0
	let numbered = false
	const named: string[] = []
	for (let at = 0; at < text.length; at++) {
		const c = text[at]!
		if (c === "'" || c === '"' || c === '`') {
			at = closing(text, at, c)
		} else if (c === '[') {
			at = text.indexOf(']', at + 1)
			if (at < 0) break
		} else if (c === '-' && text[at + 1] === '-') {
			at = text.indexOf('\n', at)
			if (at < 0) break
		} else if (c === '/' && text[at + 1] === '*') {
			at = text.indexOf('*/', at + 2)
			if (at < 0) break
			at++
		} else if (c === '?') {
			if (/[0-9]/.test(text[at + 1] ?? '')) {
				numbered = true
			} else {
				positional++
			}
		} else if ((c === ':' || c === '@' || c === '$') && /[A-Za-z_]/.test(text[at + 1] ?? '')) {
			const name = /^[A-Za-z_][A-Za-z0-9_]*/.exec(text.slice(at + 1))![0]
			if (!named.includes(name)) {
				named.push(name)
			}
			at += name.length
		}
	}
	return { positional, named, numbered }
}

/** The index past a quoted run that starts at `at`, a doubled quote inside it kept. */
function closing(text: string, at: number, quote: string): number {
	for (let next = at + 1; next < text.length; next++) {
		if (text[next] === quote) {
			if (text[next + 1] === quote) {
				next++
				continue
			}
			return next
		}
	}
	return text.length
}

/** A statement as a piece of SQL, in whichever way it was given. */
export function statementOf(given: Statement): Sql {
	const [first, ...rest] = given
	if (first instanceof Sql) {
		if (rest.length > 0) {
			throw new InvalidError('a piece of SQL carries its values: pass it alone')
		}
		return first
	}
	if (typeof first !== 'string') {
		return piece(first, rest)
	}
	const only = rest[0]
	if (rest.length === 1 && isNamed(only)) {
		return namedPiece(first, only as Record<string, unknown>)
	}
	return piece(first, rest)
}

function isNamed(value: unknown): boolean {
	return (
		typeof value === 'object' &&
		value !== null &&
		!(value instanceof Date) &&
		!(value instanceof Uint8Array) &&
		!(value instanceof Sql) &&
		!Array.isArray(value)
	)
}

/** Text with :name values, which SQLite numbers in the order the names first appear. */
function namedPiece(text: string, values: Record<string, unknown>): Sql {
	const counted = placeholders(text)
	if (counted.positional > 0 || counted.numbered) {
		throw new InvalidError('named values and ? in one statement: give the values one way')
	}
	return new Sql(
		text,
		counted.named.map(name => cellOf(values[name], `:${name}`)),
	)
}

/** Whether a statement may write, which says whether a lost connection may send it again. */
function mayWrite(text: string): boolean {
	return /\b(insert|update|delete|replace|returning|create|drop|alter|vacuum|reindex|analyze)\b/i.test(
		text,
	)
}

async function migrationFiles(given: Migrations): Promise<{ name: string; sql: string }[]> {
	if (typeof given !== 'string') {
		const files = Array.isArray(given)
			? [...(given as readonly { name: string; sql: string }[])]
			: Object.entries(given as Record<string, string>).map(([name, sql]) => ({ name, sql }))
		return files.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
	}
	const entries = await readdir(given, { withFileTypes: true })
	const names = entries
		.filter(entry => entry.isFile() && entry.name.endsWith('.sql'))
		.map(entry => entry.name)
		.sort()
	return Promise.all(
		names.map(async name => ({ name, sql: await readFile(join(given, name), 'utf8') })),
	)
}

/** Opens a database by its name, its migrations applied and checked before it resolves. */
export async function openDatabase(
	link: Link,
	workers: Set<Worker>,
	name: string,
	options: DatabaseOptions = {},
): Promise<Database> {
	checkName(name, 'database')
	checkDurability(options.durability, `sql ${name}`)
	const migrations =
		options.migrations === undefined ? undefined : await migrationFiles(options.migrations)
	const open = SqlOpen.encode({ name, migrations, durability: options.durability })
	const db = new Database(link, workers, name, open)
	await link.run('read', connection => db.handle(connection))
	return db
}

/** A query's rows as objects, refusing two columns of one name, which one object cannot hold. */
function rowsOf<T>(columns: readonly string[], rows: readonly SqlValue[][]): T[] {
	if (new Set(columns).size !== columns.length) {
		const twice = columns.find((column, at) => columns.indexOf(column) !== at)
		throw new InvalidError(`two columns named ${twice}: name them apart with as`)
	}
	return rows.map(values => {
		const row: Record<string, SqlValue> = {}
		columns.forEach((column, at) => {
			row[column] = values[at] ?? null
		})
		return row as T
	})
}

/** What one query answered: its columns, and its rows' values. */
interface Answered {
	columns: string[]
	rows: SqlValue[][]
}

function joined(parts: Uint8Array[]): Answered {
	const answered: Answered = { columns: [], rows: [] }
	for (const body of parts) {
		const part = SqlRows.decode(body)
		if (answered.columns.length === 0) {
			answered.columns = [...(part.columns ?? [])]
		}
		for (const row of part.rows ?? []) {
			answered.rows.push(row as SqlValue[])
		}
	}
	return answered
}

function doneOf(done: { changes?: number; lastInsertRowid?: bigint }): Done {
	const rowid = done.lastInsertRowid ?? 0n
	const exact = rowid >= BigInt(Number.MIN_SAFE_INTEGER) && rowid <= BigInt(Number.MAX_SAFE_INTEGER)
	return { changes: done.changes ?? 0, lastInsertRowid: exact ? Number(rowid) : rowid }
}

type Want = 'all' | 'one' | 'scalar'

/** What reads and writes a database: the database itself, or a transaction of it. */
export interface Runner {
	readonly describe: string
	rows(statement: Sql, want: Want): Promise<Answered>
	exec(statement: Sql): Promise<Done>
}

/** The reads every runner shares: all, one and scalar over its rows. */
async function all<T>(runner: Runner, statement: Sql): Promise<T[]> {
	const answered = await runner.rows(statement, 'all')
	return rowsOf<T>(answered.columns, answered.rows)
}

async function one<T>(runner: Runner, statement: Sql): Promise<T | undefined> {
	const answered = await runner.rows(statement, 'one')
	return rowsOf<T>(answered.columns, answered.rows)[0]
}

async function scalar<T>(runner: Runner, statement: Sql): Promise<T> {
	const answered = await runner.rows(statement, 'scalar')
	return answered.rows[0]?.[0] as T
}

/** A transaction whose function is running. */
interface Running {
	database: Database
	/** set once the function returned or threw: what it left running is outside the transaction */
	ended: boolean
}

// The transactions whose functions the caller runs inside, as its async
// context carries them: a thread's own stack, for a runtime of one thread.
const running = new AsyncLocalStorage<readonly Running[]>()

/**
 * An application's database: `await store.database('app', { migrations })`.
 * The verb says what comes back: all the rows, one or none, one value, or
 * what a write changed. A write with `returning` goes through a read's verb
 * and runs on the writer, its rows given once it is durable.
 */
export class Database implements Runner, Home {
	readonly name: string
	readonly describe: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #workers: Set<Worker>
	/** the buckets and queues kept in the file, by their open: a transaction opens them first */
	readonly #guests = new Map<string, { method: number; open: Open }>()

	/** Databases come from `store.database`. */
	constructor(link: Link, workers: Set<Worker>, name: string, open: Uint8Array) {
		this.#link = link
		this.#workers = workers
		this.name = name
		this.describe = `sql ${name}`
		this.#open = open
	}

	handle(connection: Connection): Promise<number> {
		return handleOn(connection, methods['sql.open'], this.#open)
	}

	keep(
		method: number,
		bytes: Uint8Array,
		make: (connection: Connection) => Promise<Uint8Array>,
	): Open {
		const key = `${method}:${Buffer.from(bytes).toString('base64')}`
		const kept = this.#guests.get(key)
		if (kept !== undefined) {
			return kept.open
		}
		this.#guests.set(key, { method, open: make })
		return make
	}

	via(what: string): Via {
		const link = viaLink(this.#link)
		return {
			call: async (openMethod, open, method, body, idempotence) => {
				this.#refuseAround(what)
				return link.call(openMethod, open, method, body, idempotence)
			},
			ask: async (openMethod, open, method, write, read, idempotence) => {
				this.#refuseAround(what)
				return link.ask(openMethod, open, method, write, read, idempotence)
			},
		}
	}

	/**
	 * Refuses a call made around the database's transaction from inside its
	 * function. It would wait for the writer the transaction holds until the
	 * server rolled the transaction back, and then commit alone.
	 */
	#refuseAround(what: string): void {
		if (running.getStore()?.some(each => each.database === this && !each.ended)) {
			throw new InvalidError(
				`${this.describe}: ${what}: made around the database's transaction from inside it, where it would wait for the writer the transaction holds: make it through the transaction, as tx.exec or tx.with(queue).add`,
			)
		}
	}

	/**
	 * A bucket of values by key kept in this database's file, beside its
	 * tables, as `store.bucket` keeps one in kv.db: the same calls, and its
	 * writes commit with the rows in `db.tx`, through `tx.with(bucket)`.
	 *
	 *     const sessions = db.bucket<Session>('sessions', { idle: '30d' })
	 */
	bucket<T = unknown>(name: string, options?: BucketOptions): Bucket<T> {
		const bytes = bucketOpen(name, options)
		const open = this.keep(methods['kv.bucket.open'], bytes, async connection =>
			bucketOpen(name, options, await this.handle(connection)),
		)
		return new Bucket<T>(this.#link, name, open, valuesOf(options?.type) as Values<T>, [], this)
	}

	/**
	 * A queue of jobs kept in this database's file, as `store.queue` keeps one
	 * in jobs.db: the same calls and workers, and its jobs commit with the
	 * rows in `db.tx`, through `tx.with(queue)`. It shares the database's
	 * writer, which is the point and its cost.
	 *
	 *     const emails = db.queue<Email>('emails', { attempts: 5 })
	 */
	queue<S extends StandardSchemaV1>(
		name: string,
		options: QueueOptions & { schema: S },
	): Queue<StandardSchemaV1.InferOutput<S>>
	queue<T = unknown>(name: string, options?: QueueOptions): Queue<T>
	queue(name: string, options: QueueOptions & { schema?: StandardSchemaV1 } = {}): Queue<unknown> {
		return openQueue(this.#link, this.#workers, name, options, this)
	}

	/** Every row a statement gives: `db.all<Note>\`select * from notes where author_id = ${id}\`` */
	async all<T = Row>(...statement: Statement): Promise<T[]> {
		return all<T>(this, statementOf(statement))
	}

	/** The one row a statement gives, or undefined; two are InvalidError, and a write that gave them rolls back. */
	async one<T = Row>(...statement: Statement): Promise<T | undefined> {
		return one<T>(this, statementOf(statement))
	}

	/** The one value of the one row a statement gives, as count(*) does; null as SQL answers it. */
	async scalar<T = SqlValue>(...statement: Statement): Promise<T> {
		return scalar<T>(this, statementOf(statement))
	}

	/**
	 * The rows a statement gives, one at a time as their parts arrive: an
	 * export that holds one part, not every row. The server reads them from
	 * one snapshot, at most 64 MiB.
	 */
	async *each<T = Row>(...statement: Statement): AsyncGenerator<T> {
		this.#refuseAround('a read')
		const piece = statementOf(statement)
		const connection = await this.#link.connection()
		const handle = await this.handle(connection)
		const body = SqlQuery.encode({
			handle,
			text: piece.text,
			values: [...piece.values],
			want: 'all',
		})
		const stream = await connection.session.open(methods['sql.query'], body, true)
		try {
			const head = await stream.next()
			if (head.end) {
				const whole = joined([head.body])
				yield* rowsOf<T>(whole.columns, whole.rows)
				return
			}
			let columns: string[] = []
			for (;;) {
				const event = await stream.next()
				stream.consumed(event.body.length)
				const part = joined([event.body])
				if (columns.length === 0) {
					columns = part.columns
				}
				yield* rowsOf<T>(columns, part.rows)
				if (event.end) {
					return
				}
			}
		} finally {
			stream.cancel()
		}
	}

	/**
	 * A table of the database, typed where it opens: its rows' inserts and
	 * upserts, and queries built without SQL text. It costs nothing until its
	 * first call; open each once, in the module that owns it.
	 *
	 *     const notes = db.table<Note>('notes', { types: { done: 'bool', created_at: 'time' } })
	 */
	table<S extends StandardSchemaV1>(
		name: string,
		options: TableOptions & { schema: S },
	): Table<StandardSchemaV1.InferOutput<S>>
	table<T = Row>(name: string, options?: TableOptions): Table<T>
	table(name: string, options?: TableOptions): Table<unknown> {
		return new Table(this, name, options)
	}

	/** Statements known before they run, written as one: all of them or none, in a shared commit. */
	async batch(statements: readonly Sql[]): Promise<Done[]> {
		const texts = statements.map(each => {
			if (!(each instanceof Sql)) {
				throw new InvalidError('a batch takes pieces of SQL: sql`…`')
			}
			return { text: each.text, values: [...each.values] }
		})
		this.#refuseAround('a batch')
		const answered = await this.#link.run('write', async connection => {
			const handle = await this.handle(connection)
			return connection.session.call(
				methods['sql.batch'],
				SqlBatch.encode({ handle, statements: texts }),
			)
		})
		return (SqlBatched.decode(answered).done ?? []).map(doneOf)
	}

	/**
	 * Runs fn in a transaction that holds the writer, for writes that depend on
	 * what its reads found: returning commits, a throw rolls back. It runs
	 * once, five seconds at most, past which the server rolls it back; inside
	 * it, call nothing that waits on the world.
	 *
	 *     await db.tx(async tx => {
	 *       const left = await tx.scalar<number>`select stock from items where sku = ${sku}`
	 *       if (left < 1) throw new SoldOut()
	 *       await tx.exec`update items set stock = stock - 1 where sku = ${sku}`
	 *     })
	 */
	async tx<T>(fn: (tx: SqlTx) => T | Promise<T>): Promise<T> {
		this.#refuseAround('a transaction')
		const connection = await this.#link.connection()
		const handle = await this.handle(connection)
		const guests = await this.#openGuests(connection)
		const stream = await connection.session.open(
			methods['sql.tx'],
			SqlTxOpen.encode({ handle }),
			false,
		)
		await stream.next()
		const tx = new SqlTx(stream, this, connection, guests)
		const inside: Running = { database: this, ended: false }
		let value: T
		try {
			value = await running.run([...(running.getStore() ?? []), inside], () => fn(tx))
		} catch (err) {
			inside.ended = true
			await tx.end(false).catch(() => {})
			throw err
		}
		inside.ended = true
		await tx.end(true)
		return value
	}

	/**
	 * Opens on `connection` the buckets and queues kept in the file, before a
	 * transaction holds its writer: a first open writes the file, and would
	 * wait for the writer the transaction holds. One that fails is told when
	 * the transaction takes it in.
	 */
	async #openGuests(connection: Connection): Promise<Guests> {
		const guests: Guests = new Map()
		const kept = [...this.#guests.values()]
		const settled = await Promise.allSettled(
			kept.map(guest => handleOn(connection, guest.method, guest.open)),
		)
		settled.forEach((outcome, at) => {
			const open = kept[at]?.open
			if (open !== undefined) {
				guests.set(open, outcome.status === 'fulfilled' ? undefined : outcome.reason)
			}
		})
		return guests
	}

	async rows(statement: Sql, want: Want): Promise<Answered> {
		this.#refuseAround('a statement')
		const idempotence: Idempotence = mayWrite(statement.text) ? 'write' : 'read'
		return this.#link.run(idempotence, async connection => {
			const handle = await this.handle(connection)
			const body = SqlQuery.encode({
				handle,
				text: statement.text,
				values: [...statement.values],
				want,
			})
			const { items, trailer } = await download(connection, methods['sql.query'], body)
			return joined([...items, trailer])
		})
	}

	/** Runs a write once it is durable; writes from many callers share a commit, each failing alone. */
	async exec(...statement: Statement): Promise<Done> {
		this.#refuseAround('a write')
		const piece = statementOf(statement)
		const answered = await this.#link.run('write', async connection => {
			const handle = await this.handle(connection)
			const body = SqlStatement.encode({ handle, text: piece.text, values: [...piece.values] })
			return connection.session.call(methods['sql.exec'], body)
		})
		return doneOf(SqlDone.decode(answered))
	}
}

/** The buckets and queues a transaction opened before it began, each with why its open failed, if it did. */
type Guests = Map<Open, unknown>

/**
 * A database's transaction, from `db.tx`: its reads see its writes, and a
 * call that fails leaves it as it was before the call. Its calls go one at a
 * time, in order.
 */
export class SqlTx implements Runner {
	readonly describe: string
	readonly #stream: Stream
	readonly #database: Database
	readonly #connection: Connection
	readonly #guests: Guests
	readonly #via: Via
	#last: Promise<unknown> = Promise.resolve()

	/** Transactions come from `db.tx`. */
	constructor(stream: Stream, database: Database, connection: Connection, guests: Guests) {
		this.#stream = stream
		this.#database = database
		this.#connection = connection
		this.#guests = guests
		this.describe = database.describe
		const call: Via['call'] = async (openMethod, open, method, body) => {
			const handle = await handleOn(this.#connection, openMethod, open)
			return this.#nested(method, body(handle))
		}
		this.#via = { call, ask: (...asked) => askByCall(call, ...asked) }
	}

	/**
	 * A bucket or a queue opened from this database, inside the transaction:
	 * what it writes commits with the rows or not at all, and what it reads
	 * sees what the transaction wrote.
	 *
	 *     await db.tx(async tx => {
	 *       const order = await tx.table<Order>('orders').insert({ user_id, total })
	 *       await tx.with(emails).add({ order: order.id, to })
	 *     })
	 */
	with<T>(bucket: Bucket<T>): Bucket<T>
	with<T>(queue: Queue<T>): TxQueue<T>
	with(handle: Bucket<unknown> | Queue<unknown>): Bucket<unknown> | TxQueue<unknown> {
		const bucket = partsOf(handle)
		if (bucket?.values !== undefined && bucket.openMethod === 'kv.bucket.open') {
			this.#takes(bucket.home, bucket.open, `kv bucket ${bucket.name}`)
			const { link, name, open, values, under, home } = bucket
			return new Bucket(link, name, open, values, under, home, this.#via)
		}
		const queue = queueParts(handle)
		if (queue !== undefined) {
			this.#takes(queue.source.home, queue.source.open, `jobs queue ${queue.name}`)
			return queueVia(queue, this.#via)
		}
		throw new InvalidError(
			`${this.describe}: a transaction takes in a bucket or a queue opened from its database`,
		)
	}

	async all<T = Row>(...statement: Statement): Promise<T[]> {
		return all<T>(this, statementOf(statement))
	}

	async one<T = Row>(...statement: Statement): Promise<T | undefined> {
		return one<T>(this, statementOf(statement))
	}

	async scalar<T = SqlValue>(...statement: Statement): Promise<T> {
		return scalar<T>(this, statementOf(statement))
	}

	async exec(...statement: Statement): Promise<Done> {
		return this.#call(statementOf(statement), 'exec').then(answer => doneOf(answer.done ?? {}))
	}

	/** A table inside the transaction: its calls are the transaction's. */
	table<T = Row>(name: string, options?: TableOptions): Table<T> {
		return new Table<T>(this, name, options)
	}

	async rows(statement: Sql, want: Want): Promise<Answered> {
		const answer = await this.#call(statement, want)
		return {
			columns: [...(answer.rows?.columns ?? [])],
			rows: (answer.rows?.rows ?? []) as SqlValue[][],
		}
	}

	/** Sends the last call: commit, or roll back, and waits for the server's word. */
	async end(commit: boolean): Promise<void> {
		await this.#last.catch(() => {})
		await this.#stream.send(SqlTxCall.encode({ commit }), true)
		await this.#stream.next()
	}

	/**
	 * Refuses a handle kept elsewhere, whose writes would not commit with the
	 * rows, and one made after the transaction began, whose first open would
	 * wait for the writer the transaction holds.
	 */
	#takes(home: Home | undefined, open: Open, what: string): void {
		if (home !== this.#database) {
			const kept = home === undefined ? 'the store' : home.describe
			throw new InvalidError(
				`${what}: kept in ${kept}, outside this transaction of ${this.describe}`,
			)
		}
		if (!this.#guests.has(open)) {
			throw new InvalidError(
				`${this.describe}: ${what}: made after the transaction began; make it before db.tx, which opens it first`,
			)
		}
		const failed = this.#guests.get(open)
		if (failed !== undefined) {
			throw failed
		}
	}

	/** A call of kv or jobs in the transaction: its method and its request, and the method's answer. */
	async #nested(method: number, body: Uint8Array): Promise<Uint8Array> {
		const answer = await this.#send(SqlTxCall.encode({ want: 'call', method, body }))
		return answer.body ?? new Uint8Array(0)
	}

	#call(statement: Sql, want: Want | 'exec') {
		return this.#send(
			SqlTxCall.encode({ text: statement.text, values: [...statement.values], want }),
		)
	}

	#send(body: Uint8Array) {
		const call = async () => {
			await this.#stream.send(body, false)
			const event = await this.#stream.next()
			this.#stream.consumed(event.body.length)
			const answer = SqlTxAnswer.decode(event.body)
			if (answer.failure !== undefined) {
				throw errorOf(
					answer.failure.code ?? 'internal',
					answer.failure.message ?? '',
					answer.failure.what ?? {},
				)
			}
			return answer
		}
		const next = this.#last.catch(() => {}).then(call)
		this.#last = next
		return next
	}
}
