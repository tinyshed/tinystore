// A table that mirrors another system: it takes the source's list and says
// what changed. The server compares the list with what the table holds by the
// source's key and writes only the difference, in one commit; this file only
// says the call in TypeScript.

import { InvalidError } from './errors.ts'
import { type ColumnType, fieldOf, typedRow } from './query.ts'
import { cellOf, rowsOf } from './sql.ts'
import type { SqlArg, SqlValue } from './wire/codec.ts'
import { SqlDiff, SqlMirror } from './wire/protocol.ts'

/** How a mirror opens: what names a row at the source, what marks it gone, and what makes it changed. */
export interface MirrorOptions {
	/** the columns that name a row at the source: a unique index or the table's primary key covers them */
	key: string | readonly string[]
	/** a column that marks what the source dropped with the store's time, unix milliseconds, instead of deleting it */
	gone?: string | undefined
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

/**
 * A table that mirrors another system, from `db.mirror`: its `replace` takes
 * the source's list of a scope, its `upsert` a change of some rows, and each
 * answers what was added, changed and gone. A row the list gives as it is
 * held is not written at all.
 *
 *     const works = db.mirror<Work>('works', { key: ['course_id', 'id'], gone: 'gone_at', changeOn: ['grade'] })
 *     const diff = await works.replace({ course_id: 1 }, fetched)
 */
export class Mirror<T = Record<string, SqlValue>> {
	readonly table: string
	readonly #options: MirrorOptions
	readonly #call: MirrorCall

	/** Mirrors come from `db.mirror`. */
	constructor(table: string, options: MirrorOptions, call: MirrorCall) {
		this.table = table
		this.#options = options
		this.#call = call
	}

	/**
	 * Makes the scope, the rows whose columns hold its values, `{}` for the
	 * whole table, what `rows` lists: adds what the table lacks, writes what
	 * changed, and deletes, or marks gone, what the list leaves out. A row
	 * outside the scope is InvalidError, and so is one whose key holds a null.
	 * Values compare as the table keeps them: `4` given to a text column is
	 * `'4'`, and a key's text compares by its unique index's collation.
	 */
	async replace(
		scope: Record<string, unknown>,
		rows: readonly Partial<T>[],
		options: ReplaceOptions = {},
	): Promise<Diff<T>> {
		const columns = Object.keys(scope)
		const values = columns.map(column => cellOf(scope[column], `the scope's ${column}`))
		return this.#run(rows, {
			mode: 'replace',
			scope: columns,
			scopeValues: values,
			minKept: options.minKept,
		})
	}

	/** Writes what `rows` adds and changes, a delta of the source's; its `gone` is always empty. */
	async upsert(rows: readonly Partial<T>[]): Promise<Diff<T>> {
		return this.#run(rows, { mode: 'upsert' })
	}

	async #run(
		rows: readonly Partial<T>[],
		asked: { mode: string; scope?: string[]; scopeValues?: SqlArg[]; minKept?: number | undefined },
	): Promise<Diff<T>> {
		const types = this.#options.types ?? {}
		const listed = listOf(rows as readonly Record<string, unknown>[], types)
		const key = typeof this.#options.key === 'string' ? [this.#options.key] : [...this.#options.key]
		const answer = await this.#call(handle =>
			SqlMirror.encode({
				handle,
				table: this.table,
				key,
				gone: this.#options.gone,
				changeOn: this.#options.changeOn === undefined ? undefined : [...this.#options.changeOn],
				...asked,
				rows: listed,
			}),
		)
		const diff = SqlDiff.decode(answer)
		const columns = diff.columns ?? []
		// a row comes back as the table's handle with these types reads it
		const read = (rows: readonly (SqlValue | boolean)[][] | undefined) =>
			rowsOf<Record<string, SqlValue>>(columns, (rows ?? []) as SqlValue[][]).map(
				row => typedRow(types, row) as T,
			)
		const after = read(diff.after)
		return {
			added: read(diff.added),
			changed: read(diff.before).map((before, at) => ({ before, after: after[at] as T })),
			gone: read(diff.gone),
			same: diff.same ?? 0,
			suspect: diff.suspect ?? false,
		}
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
