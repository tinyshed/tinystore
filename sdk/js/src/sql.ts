// The application's own SQLite databases, sql/<name>.db, as Go's sqldb keeps
// them: the application writes every query, the server owns the file, its
// connections and its migrations, which the first open applies and every
// later one checks.

import { readdir, readFile } from 'node:fs/promises'
import { join } from 'node:path'

import type { Connection, Link } from './connection.ts'
import { InvalidError } from './errors.ts'
import { checkName, handleOn } from './handles.ts'
import { watch } from './session.ts'
import type { SqlArg, SqlValue } from './wire/codec.ts'
import {
	methods,
	SqlColumns,
	SqlDatabase,
	SqlDone,
	SqlResults,
	SqlRow,
	SqlStatement,
	SqlStatements,
} from './wire/messages.ts'

export type { SqlArg, SqlValue }

/** A row as SQLite returned it, by its columns' names. */
export type Row = Record<string, SqlValue>

/**
 * A database's migrations: a directory whose .sql files they are, at its root
 * or in its one directory, or the files themselves by name.
 */
export type Migrations = string | Record<string, string> | readonly { name: string; text: string }[]

export interface SqlOptions {
	/** what the first open applies, on an admin connection, and every later open checks */
	migrations?: Migrations
}

/** What a write changed: the rows, and the rowid of the last it inserted. */
export interface Done {
	changes: number
	lastId: number
}

/**
 * A statement's text and arguments, in any of the three ways a call takes
 * them: positional after the text, one object of named ones, or a template
 * whose values are its arguments.
 *
 *     db.all('select * from notes where id = ?', id)
 *     db.all('select * from notes where id = :id', { id })
 *     db.all`select * from notes where id = ${id}`
 */
export type Statement = [sql: string | TemplateStringsArray, ...args: unknown[]]

function statementOf(given: Statement): {
	sql: string
	args?: SqlArg[]
	named?: Record<string, SqlArg>
} {
	const [first, ...rest] = given
	if (typeof first !== 'string') {
		return { sql: first.join('?'), args: rest as SqlArg[] }
	}
	const only = rest[0]
	if (rest.length === 1 && isNamed(only)) {
		return { sql: first, named: only as Record<string, SqlArg> }
	}
	return { sql: first, args: rest as SqlArg[] }
}

function isNamed(value: unknown): boolean {
	return (
		typeof value === 'object' &&
		value !== null &&
		!(value instanceof Date) &&
		!(value instanceof Uint8Array) &&
		!Array.isArray(value)
	)
}

/** Opens a database by its name, applying or checking its migrations. */
export async function openDatabase(
	link: Link,
	name: string,
	options?: SqlOptions,
): Promise<Database> {
	checkName(name, 'database')
	const migrations =
		options?.migrations === undefined ? undefined : await migrationFiles(options.migrations)
	const open = SqlDatabase.encode({ name, migrations })
	const db = new Database(link, name, open)
	await link.run('write', connection => db.handle(connection))
	return db
}

async function migrationFiles(given: Migrations): Promise<{ name: string; text: string }[]> {
	if (typeof given !== 'string') {
		const files = Array.isArray(given)
			? [...given]
			: Object.entries(given).map(([name, text]) => ({ name, text }))
		return files.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
	}
	let dir = given
	let entries = await readdir(dir, { withFileTypes: true })
	if (!entries.some(e => e.isFile() && e.name.endsWith('.sql'))) {
		const dirs = entries.filter(e => e.isDirectory())
		if (dirs.length === 1) {
			dir = join(dir, dirs[0]!.name)
			entries = await readdir(dir, { withFileTypes: true })
		}
	}
	const names = entries
		.filter(e => e.isFile() && e.name.endsWith('.sql'))
		.map(e => e.name)
		.sort()
	if (names.length === 0) {
		throw new InvalidError(`no .sql migrations in ${given}`)
	}
	return Promise.all(
		names.map(async name => ({ name, text: await readFile(join(dir, name), 'utf8') })),
	)
}

/** Reads a query's rows as objects, refusing two columns of one name, which one object cannot hold. */
function rowsOf<T>(columns: string[], rows: SqlValue[][]): T[] {
	if (new Set(columns).size !== columns.length) {
		const twice = columns.find((c, i) => columns.indexOf(c) !== i)
		throw new InvalidError(`two columns named ${twice}: name them apart with as`)
	}
	return rows.map(values => {
		const row: Record<string, SqlValue> = {}
		columns.forEach((column, i) => {
			row[column] = values[i] ?? null
		})
		return row as T
	})
}

function one<T>(rows: T[]): T | undefined {
	if (rows.length > 1) {
		throw new InvalidError(`a query for one row answered ${rows.length}`)
	}
	return rows[0]
}

function scalar<T>(columns: string[], rows: SqlValue[][]): T {
	if (rows.length === 0) {
		throw new InvalidError('a scalar query answered no row; one always answers, as count(*) does')
	}
	if (columns.length !== 1 || rows.length > 1) {
		throw new InvalidError(
			`a scalar query answered ${rows.length} rows of ${columns.length} columns`,
		)
	}
	return rows[0]![0] as T
}

/**
 * A database. What a call's name starts with says where it runs: exec and
 * its kin may write and run on the file's one writer, grouped with the other
 * writes; all, one, scalar and each read, on readers that refuse to write.
 */
export class Database {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array

	constructor(link: Link, name: string, open: Uint8Array) {
		this.#link = link
		this.name = name
		this.#open = open
	}

	handle(connection: Connection): Promise<number> {
		return handleOn(connection, methods['sql.open'], this.#open)
	}

	async #query(given: Statement, write: boolean, signal?: AbortSignal) {
		const statement = statementOf(given)
		return this.#link.run(write ? 'write' : 'read', async connection => {
			const handle = await this.handle(connection)
			const stream = await connection.session.open(
				methods['sql.query'],
				SqlStatement.encode({ handle, ...statement, write: write || undefined }),
				true,
			)
			const unwatch = watch(signal, stream)
			try {
				const head = await stream.next()
				const columns = SqlColumns.decode(head.body).columns ?? []
				const rows: SqlValue[][] = []
				for (;;) {
					const event = await stream.next()
					stream.consumed(event.body.length)
					if (event.end) {
						return { columns, rows }
					}
					rows.push((SqlRow.decode(event.body).values ?? []) as SqlValue[])
				}
			} finally {
				unwatch()
			}
		})
	}

	/** Every row a query answers, held in memory: the server refuses past 64 MiB of them. */
	async all<T = Row>(...statement: Statement): Promise<T[]> {
		const { columns, rows } = await this.#query(statement, false)
		return rowsOf<T>(columns, rows)
	}

	/** The one row a query answers, undefined for none; two is InvalidError. */
	async one<T = Row>(...statement: Statement): Promise<T | undefined> {
		return one(await this.all<T>(...statement))
	}

	/** The one value of a query that always answers one row, as count(*) does. */
	async scalar<T = SqlValue>(...statement: Statement): Promise<T> {
		const { columns, rows } = await this.#query(statement, false)
		return scalar<T>(columns, rows)
	}

	/** The columns and rows of a query as SQLite returned them, arrays rather than objects. */
	query(...statement: Statement): Promise<{ columns: string[]; rows: SqlValue[][] }> {
		return this.#query(statement, false)
	}

	/**
	 * Reads a query's rows one at a time as they arrive, within the credit
	 * the client grants: the client holds a row and the frames in flight.
	 */
	async *each<T = Row>(...statement: Statement): AsyncGenerator<T> {
		const { sql, args, named } = statementOf(statement)
		const connection = await this.#link.connection()
		const handle = await this.handle(connection)
		const stream = await connection.session.open(
			methods['sql.query'],
			SqlStatement.encode({ handle, sql, args, named }),
			true,
		)
		let ended = false
		try {
			const columns = SqlColumns.decode((await stream.next()).body).columns ?? []
			rowsOf(columns, [])
			for (;;) {
				const event = await stream.next()
				stream.consumed(event.body.length)
				if (event.end) {
					ended = true
					return
				}
				yield rowsOf<T>(columns, [(SqlRow.decode(event.body).values ?? []) as SqlValue[]])[0]!
			}
		} finally {
			if (!ended) {
				stream.cancel()
			}
		}
	}

	/** Runs a statement on the writer and says what it changed. */
	async exec(...statement: Statement): Promise<Done> {
		const given = statementOf(statement)
		return this.#link.run('write', async connection => {
			const handle = await this.handle(connection)
			const body = await connection.session.call(
				methods['sql.exec'],
				SqlStatement.encode({ handle, ...given }),
			)
			const done = SqlDone.decode(body)
			return { changes: done.changes ?? 0, lastId: done.lastId ?? 0 }
		})
	}

	/** The rows a write's returning clause answers, run on the writer. */
	async execAll<T = Row>(...statement: Statement): Promise<T[]> {
		const { columns, rows } = await this.#query(statement, true)
		return rowsOf<T>(columns, rows)
	}

	async execOne<T = Row>(...statement: Statement): Promise<T | undefined> {
		return one(await this.execAll<T>(...statement))
	}

	async execScalar<T = SqlValue>(...statement: Statement): Promise<T> {
		const { columns, rows } = await this.#query(statement, true)
		return scalar<T>(columns, rows)
	}

	/**
	 * Runs the statements fn asks for in one transaction, all or none: a
	 * failure names its statement as `call`. fn returns before anything is
	 * sent, since no transaction is held across the network.
	 */
	batch(fn: (tx: SqlBatch) => void): Promise<void> {
		return this.#run(false, fn)
	}

	/** Runs the reads fn asks for from one snapshot. */
	view(fn: (tx: SqlBatch) => void): Promise<void> {
		return this.#run(true, fn)
	}

	async #run(read: boolean, fn: (tx: SqlBatch) => void): Promise<void> {
		const tx = new SqlBatch(read)
		const returned: unknown = fn(tx)
		tx.close()
		if (returned instanceof Promise) {
			throw new InvalidError(
				'a batch function returns before anything is sent; it cannot await inside',
			)
		}
		if (tx.statements.length === 0) {
			return
		}
		try {
			const results = await this.#link.run(read ? 'read' : 'write', async connection => {
				const handle = await this.handle(connection)
				const body = await connection.session.call(
					methods['sql.batch'],
					SqlStatements.encode({
						handle,
						statements: tx.statements.map(s => s.statement),
						read: read || undefined,
					}),
				)
				return SqlResults.decode(body).results ?? []
			})
			tx.statements.forEach((s, i) => {
				s.settle(results[i] ?? {})
			})
		} catch (err) {
			for (const s of tx.statements) {
				s.fail(err)
			}
			throw err
		}
	}
}

type Result = ReturnType<typeof SqlResults.decode>['results'] extends (infer R)[] | undefined
	? R
	: never

interface Batched {
	statement: Parameters<typeof SqlStatement.encode>[0]
	settle: (result: Result) => void
	fail: (err: unknown) => void
}

/** The statements of a batch or a view, whose promises settle with it. */
export class SqlBatch {
	readonly read: boolean
	readonly statements: Batched[] = []
	#done = false

	constructor(read: boolean) {
		this.read = read
	}

	close(): void {
		this.#done = true
	}

	#record<T>(given: Statement, rows: boolean, settle: (result: Result) => T): Promise<T> {
		if (this.#done) {
			throw new InvalidError(
				'a batch takes its statements while its function runs, before it is sent',
			)
		}
		const statement = statementOf(given)
		let resolve!: (v: T) => void
		let reject!: (e: unknown) => void
		const settled = new Promise<T>((res, rej) => {
			resolve = res
			reject = rej
		})
		settled.catch(() => {})
		this.statements.push({
			statement: { ...statement, rows: rows && !this.read ? true : undefined },
			settle: result => {
				try {
					resolve(settle(result))
				} catch (err) {
					reject(err)
				}
			},
			fail: reject,
		})
		return settled
	}

	exec(...statement: Statement): Promise<Done> {
		return this.#record(statement, false, r => ({ changes: r.changes ?? 0, lastId: r.lastId ?? 0 }))
	}

	all<T = Row>(...statement: Statement): Promise<T[]> {
		return this.#record(statement, true, r =>
			rowsOf<T>(r.columns ?? [], (r.rows ?? []) as SqlValue[][]),
		)
	}

	one<T = Row>(...statement: Statement): Promise<T | undefined> {
		return this.#record(statement, true, r =>
			one(rowsOf<T>(r.columns ?? [], (r.rows ?? []) as SqlValue[][])),
		)
	}

	scalar<T = SqlValue>(...statement: Statement): Promise<T> {
		return this.#record(statement, true, r =>
			scalar<T>(r.columns ?? [], (r.rows ?? []) as SqlValue[][]),
		)
	}
}
