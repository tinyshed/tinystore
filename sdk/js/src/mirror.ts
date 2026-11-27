// A table that mirrors another system: it takes the source's list and says
// what changed. The server compares the list with what the table holds by the
// source's key and writes only the difference, in one commit; this file only
// says the call in TypeScript, and cuts a list past one message into the
// parts of a run.

import { InvalidError, LimitError, limits } from './errors.ts'
import { type ColumnType, fieldOf, type Mark, markOption, typedRow } from './query.ts'
import { cellOf, rowsOf } from './sql.ts'
import type { SqlArg, SqlValue } from './wire/codec.ts'
import { methods, SqlAdd, SqlBegin, SqlDiff, SqlMirror, SqlRun } from './wire/protocol.ts'

/** How a mirror opens: what names a row at the source, what marks it gone, and what makes it changed. */
export interface MirrorOptions {
	/** the columns that name a row at the source: a unique index or the table's primary key covers them */
	key: string | readonly string[]
	/** a column that marks what the source dropped with the store's time instead of deleting it: unix milliseconds, or seconds with `{ column, unit: 's' }` */
	gone?: string | Mark | undefined
	/** the columns whose change makes a row changed: every column the rows carry but the key's when absent */
	changeOn?: readonly string[] | undefined
	/** the columns SQLite keeps as 0 or 1, unix milliseconds, JSON text or an integer past 2^53, as a table's */
	types?: Record<string, ColumnType> | undefined
}

/** How a replace guards against a list that cannot be right. */
export interface ReplaceOptions {
	/** writes nothing, and answers `suspect`, when fewer than this fraction of the scope's live rows would stay live */
	minKept?: number | undefined
}

/** What a mirror's call changed. A suspect answer wrote nothing. */
export interface Diff<T> {
	added: T[]
	changed: { before: T; after: T }[]
	gone: T[]
	same: number
	suspect: boolean
}

/** What sends a mirror's request on the database's connection and gives back its answer. */
export type MirrorCall = (asked: (handle: number) => Uint8Array) => Promise<Uint8Array>

/** The connection a run began on: every call of the run goes on it, and a run dies with it. */
export interface RunLink {
	call(method: number, body: Uint8Array): Promise<Uint8Array>
	/** a call whose answer comes in parts: every part's body */
	download(method: number, body: Uint8Array): Promise<Uint8Array[]>
}

/** Sends a run's begin with the database's handle on a connection, and gives the run's handle and that connection. */
export type MirrorBegin = (
	asked: (handle: number) => Uint8Array,
) => Promise<{ run: number; link: RunLink }>

/**
 * A table that mirrors another system, from `db.mirror`: its `replace` takes
 * the source's list of a scope, its `upsert` a change of some rows, and each
 * answers what was added, changed and gone. A row the list gives as it is
 * held is not written at all.
 *
 *     const works = db.mirror<Work>('works', { key: ['course_id', 'id'], gone: { column: 'gone_at', unit: 's' } })
 *     const diff = await works.replace({ course_id: 1 }, fetched)
 */
export class Mirror<T = Record<string, SqlValue>> {
	readonly table: string
	readonly #options: MirrorOptions
	readonly #call: MirrorCall
	readonly #begin: MirrorBegin

	/** Mirrors come from `db.mirror`. */
	constructor(table: string, options: MirrorOptions, call: MirrorCall, begin: MirrorBegin) {
		this.table = table
		this.#options = options
		this.#call = call
		this.#begin = begin
	}

	/**
	 * Makes the scope, the rows whose columns hold its values, `{}` for the
	 * whole table, what `rows` lists: adds what the table lacks, writes what
	 * changed, and deletes, or marks gone, what the list leaves out. A row
	 * outside the scope is InvalidError, and so is one whose key holds a null.
	 * Values compare as the table keeps them: `4` given to a text column is
	 * `'4'`, and a key's text compares by its unique index's collation. A
	 * list, or an answer, past one message goes in the parts of a run, as
	 * `begin` makes one, and writes in one commit all the same.
	 */
	async replace(
		scope: Record<string, unknown>,
		rows: readonly Partial<T>[],
		options: ReplaceOptions = {},
	): Promise<Diff<T>> {
		try {
			return await this.#run(rows, { mode: 'replace', ...scopeOf(scope), minKept: options.minKept })
		} catch (err) {
			if (!pastOneMessage(err)) {
				throw err
			}
		}
		// refused before it was written: the same list, in parts over one run
		const run = this.begin(scope, options)
		try {
			await run.add(rows)
			return await run.commit()
		} finally {
			await run.close()
		}
	}

	/** Writes what `rows` adds and changes, a delta of the source's; its `gone` is always empty. */
	async upsert(rows: readonly Partial<T>[]): Promise<Diff<T>> {
		return this.#run(rows, { mode: 'upsert' })
	}

	/**
	 * Begins a `replace` of the scope whose list comes in parts: each `add`
	 * gives a part, and `commit` makes the scope what every part listed, in
	 * one write, so that only what no part named is gone. A run never
	 * committed writes nothing: closed, dropped, idle for five minutes, its
	 * connection lost or the store closed.
	 *
	 *     await using run = works.begin({ course_id: 1 })
	 *     for await (const page of pages) await run.add(page)
	 *     const diff = await run.commit()
	 */
	begin(scope: Record<string, unknown>, options: ReplaceOptions = {}): MirrorRun<T> {
		const begun = () =>
			this.#begin(handle =>
				SqlBegin.encode({ handle, ...this.#shape(), ...scopeOf(scope), minKept: options.minKept }),
			)
		return new MirrorRun<T>(begun, this.#options.types ?? {})
	}

	async #run(
		rows: readonly Partial<T>[],
		asked: { mode: string; scope?: string[]; scopeValues?: SqlArg[]; minKept?: number | undefined },
	): Promise<Diff<T>> {
		const types = this.#options.types ?? {}
		const listed = listOf(rows as readonly Record<string, unknown>[], types)
		const answer = await this.#call(handle =>
			SqlMirror.encode({ handle, ...this.#shape(), ...asked, rows: listed }),
		)
		return diffOf<T>([answer], types)
	}

	/** The mirror as every call names it: its table, key, mark and compared columns. */
	#shape() {
		const key = typeof this.#options.key === 'string' ? [this.#options.key] : [...this.#options.key]
		const gone = markOption(this.#options.gone, "a mirror's gone option")
		return {
			table: this.table,
			key,
			gone: gone?.column,
			// milliseconds go unnamed, which a server older than the unit reads too
			goneUnit: gone?.unit === 's' ? 's' : undefined,
			changeOn: this.#options.changeOn === undefined ? undefined : [...this.#options.changeOn],
		}
	}
}

/**
 * A mirror's `replace` whose list comes in parts, from `mirror.begin`. Its
 * parts wait on the server, outside the table, until `commit`; one never
 * committed writes nothing, and `close` lets go of it at once.
 */
export class MirrorRun<T = Record<string, SqlValue>> implements AsyncDisposable {
	readonly #begin: () => Promise<{ run: number; link: RunLink }>
	readonly #types: Record<string, ColumnType>
	#begun: Promise<{ run: number; link: RunLink }> | undefined
	#ended = false

	/** Runs come from `mirror.begin`. */
	constructor(
		begin: () => Promise<{ run: number; link: RunLink }>,
		types: Record<string, ColumnType>,
	) {
		this.#begin = begin
		this.#types = types
	}

	/**
	 * Gives a part of the list: rows of the first part's shape, inside the
	 * scope, each key once in the whole run. A part past one message goes as
	 * several. A part that fails ends the run.
	 */
	async add(rows: readonly Partial<T>[]): Promise<void> {
		this.#refuseEnded()
		const begun = await this.#started()
		await this.#send(begun, rows as readonly Record<string, unknown>[])
	}

	/** Makes the scope what every part listed, in one write, and answers what changed. */
	async commit(): Promise<Diff<T>> {
		this.#refuseEnded()
		this.#ended = true
		const { run, link } = await this.#started()
		return diffOf<T>(
			await link.download(methods['sql.commit'], SqlRun.encode({ run })),
			this.#types,
		)
	}

	/** Lets go of a run not committed, which writes nothing; nothing after a commit. */
	async close(): Promise<void> {
		if (this.#ended) {
			return
		}
		this.#ended = true
		const begun = await this.#begun?.catch(() => undefined)
		// a connection lost let go of the run already
		await begun?.link.call(methods['sql.drop'], SqlRun.encode({ run: begun.run })).catch(() => {})
	}

	async [Symbol.asyncDispose](): Promise<void> {
		await this.close()
	}

	#started(): Promise<{ run: number; link: RunLink }> {
		this.#begun ??= this.#begin()
		return this.#begun
	}

	/** Sends a part, halved until each half fits one message. */
	async #send(
		begun: { run: number; link: RunLink },
		rows: readonly Record<string, unknown>[],
	): Promise<void> {
		const body = SqlAdd.encode({ run: begun.run, rows: listOf(rows, this.#types) })
		try {
			await begun.link.call(methods['sql.add'], body)
		} catch (err) {
			if (
				rows.length < 2 ||
				!(err instanceof LimitError && err.limit === limits.requestBytes.name)
			) {
				throw err
			}
			const half = Math.ceil(rows.length / 2)
			await this.#send(begun, rows.slice(0, half))
			await this.#send(begun, rows.slice(half))
		}
	}

	#refuseEnded(): void {
		if (this.#ended) {
			throw new InvalidError('the run is committed or closed: begin another')
		}
	}
}

/** Whether a call was refused for a request or an answer past one message, which wrote nothing. */
function pastOneMessage(err: unknown): boolean {
	const limit = err instanceof LimitError ? err.limit : undefined
	return limit === limits.requestBytes.name || limit === limits.answerBytes.name
}

function scopeOf(scope: Record<string, unknown>): { scope: string[]; scopeValues: SqlArg[] } {
	const columns = Object.keys(scope)
	return {
		scope: columns,
		scopeValues: columns.map(column => cellOf(scope[column], `the scope's ${column}`)),
	}
}

/** A difference from its parts, each list joined in their order, a row as the table's handle with these types reads it. */
function diffOf<T>(parts: readonly Uint8Array[], types: Record<string, ColumnType>): Diff<T> {
	const diffs = parts.map(part => SqlDiff.decode(part))
	const columns = diffs.find(diff => (diff.columns ?? []).length > 0)?.columns ?? []
	const read = (
		list: (diff: (typeof diffs)[number]) => readonly (SqlValue | boolean)[][] | undefined,
	) =>
		diffs
			.flatMap(diff =>
				rowsOf<Record<string, SqlValue>>(columns, (list(diff) ?? []) as SqlValue[][]),
			)
			.map(row => typedRow(types, row) as T)
	const after = read(diff => diff.after)
	return {
		added: read(diff => diff.added),
		changed: read(diff => diff.before).map((before, at) => ({ before, after: after[at] as T })),
		gone: read(diff => diff.gone),
		same: diffs.reduce((same, diff) => same + (diff.same ?? 0), 0),
		suspect: diffs.some(diff => diff.suspect ?? false),
	}
}

/** The rows as cells, the first row's columns once: rows of another shape are InvalidError. */
function listOf(
	rows: readonly Record<string, unknown>[],
	types: Record<string, ColumnType>,
): { columns: string[]; rows: SqlArg[][] } {
	const first = rows[0]
	if (first === undefined) {
		return { columns: [], rows: [] }
	}
	const columns = Object.keys(first).filter(name => first[name] !== undefined)
	const cells = rows.map((row, at) => {
		const named = Object.keys(row).filter(name => row[name] !== undefined)
		if (named.length !== columns.length || named.some(name => !columns.includes(name))) {
			throw new InvalidError(`row ${at + 1}: not the first row's columns: give rows of one shape`)
		}
		return columns.map(column => fieldOf(row[column], column, types))
	})
	return { columns, rows: cells }
}
