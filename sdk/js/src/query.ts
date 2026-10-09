// A table's queries, built from pieces into the SQL a person would write: the
// builder adds the ands, the parentheses, the placeholders and the quotes
// around names, and nothing else. It runs in this process; the server sees
// the text and the values, as it would a statement written by hand.

import { InvalidError } from './errors.ts'
import { check, type StandardSchemaV1 } from './schema.ts'
import {
	type Condition,
	cellOf,
	conditionOf,
	type Done,
	type Embeddable,
	grouped,
	identOf,
	joinedWith,
	placeholders,
	type Row,
	type Runner,
	Sql,
	type SqlArg,
	type SqlValue,
} from './sql.ts'

/** How a column SQLite keeps otherwise reads in JavaScript. */
export type ColumnType = 'bool' | 'time' | 'json' | 'bigint'

export interface TableOptions {
	/** the columns SQLite keeps as 0 or 1, unix milliseconds, JSON text or an integer past 2^53 */
	types?: Record<string, ColumnType>
	/** a Standard Schema, zod's or valibot's, that checks each row read */
	schema?: StandardSchemaV1
}

/** What an upsert does when the row's key is taken. */
export interface UpsertOptions<T> {
	/** the columns whose value, taken, makes the insert an update: the key */
	conflict: string | readonly string[]
	/** the columns the update sets from the row; none other changes */
	update: readonly (keyof T & string)[]
	/** the condition the row that holds the key must meet, its owner's, or it stays as it is */
	where?: Condition | undefined
}

/** A page of a query's rows, and the cursor that reads the next; none after the last. */
export interface RowPage<T> {
	rows: T[]
	next?: string
}

type Direction = 'asc' | 'desc'

/** A condition of a query, and whether it may stand in an and without parentheses. */
interface Kept {
	piece: Sql
	bare: boolean
}

interface Parts {
	table: string
	alias?: string | undefined
	select?: Sql | undefined
	joins: Sql[]
	conditions: Kept[]
	groups: string[]
	having: Kept[]
	order: { piece: Sql; column?: string | undefined; direction: Direction }[]
	limit?: number | undefined
	offset?: number | undefined
}

/** How a table reads and writes: its runner, its name, and how its rows read. */
interface Source {
	runner: Runner
	types: Record<string, ColumnType>
	schema?: StandardSchemaV1 | undefined
}

/**
 * A query on a table: a value, each call returning a new one, so that the
 * first stays as it was. `all`, `one`, `scalar`, `count`, `each` and `list`
 * read; `update` and `delete` write.
 *
 *     const urgent = await orders
 *       .where({ status: 'pending', deleted: false })
 *       .where(sql.or(sql`amount >= ${1000}`, { priority: 'high' }))
 *       .orderBy('created_at', 'desc')
 *       .limit(50)
 *       .all()
 */
export class Query<T = Row> implements Embeddable {
	readonly #source: Source
	readonly #parts: Parts

	/** Queries come from `db.table`. */
	constructor(source: Source, parts: Parts) {
		this.#source = source
		this.#parts = parts
	}

	/**
	 * Adds a condition with and: equal values, `null` as is null and a list
	 * as in (…); a piece of SQL from `sql`; or text and the values of its ?s.
	 * `undefined` is InvalidError, never a filter left out.
	 */
	where(condition: Condition | string, ...values: unknown[]): Query<T> {
		return this.#with({ conditions: [...this.#parts.conditions, ...kept(condition, values)] })
	}

	/** The columns a row holds, as SQL text, and the shape of the row they make. */
	select<R = Row>(columns: string | Sql): Query<R> {
		const select = typeof columns === 'string' ? new Sql(columns, []) : columns
		return new Query<R>(this.#source, { ...this.#parts, select })
	}

	/** Joins a table, `'users as u'`, on a condition as SQL text or a piece of SQL. */
	join(table: string, on: string | Sql): Query<T> {
		return this.#joined('join', table, on)
	}

	/** Joins a table whose columns may be null where no row of it meets the condition. */
	leftJoin(table: string, on: string | Sql): Query<T> {
		return this.#joined('left join', table, on)
	}

	/** Groups the rows by columns, quoted as names. */
	groupBy(...columns: string[]): Query<T> {
		return this.#with({ groups: [...this.#parts.groups, ...columns.map(identOf)] })
	}

	/** A condition on the groups, as `where` takes one. */
	having(condition: Condition | string, ...values: unknown[]): Query<T> {
		return this.#with({ having: [...this.#parts.having, ...kept(condition, values)] })
	}

	/**
	 * Orders the rows by a column, quoted as a name, so that a sort a request
	 * chooses cannot be SQL, or by a piece of SQL: `` orderBy(sql`lower(title)`) ``.
	 */
	orderBy(column: string | Sql, direction: Direction = 'asc'): Query<T> {
		if (direction !== 'asc' && direction !== 'desc') {
			throw new InvalidError(`an order is asc or desc, not ${JSON.stringify(direction)}`)
		}
		const order =
			column instanceof Sql
				? { piece: column, direction }
				: { piece: new Sql(identOf(column), []), column, direction }
		return this.#with({ order: [...this.#parts.order, order] })
	}

	limit(rows: number): Query<T> {
		return this.#with({ limit: whole(rows, 'a limit') })
	}

	offset(rows: number): Query<T> {
		return this.#with({ offset: whole(rows, 'an offset') })
	}

	/** The text and the values the query sends, to read or to log. */
	toSQL(): { text: string; values: SqlArg[] } {
		const rendered = render(this.#parts)
		return { text: rendered.text, values: [...rendered.values] }
	}

	/** The query inside another piece of SQL: a subquery, in parentheses. */
	embedded(): Sql {
		const rendered = render(this.#parts)
		return new Sql(`(${rendered.text})`, rendered.values)
	}

	async all(): Promise<T[]> {
		const answered = await this.#source.runner.rows(render(this.#parts), 'all')
		return this.#rows(answered.columns, answered.rows)
	}

	async one(): Promise<T | undefined> {
		const answered = await this.#source.runner.rows(render(this.#parts), 'one')
		return (await this.#rows(answered.columns, answered.rows))[0]
	}

	async scalar<V = SqlValue>(): Promise<V> {
		const answered = await this.#source.runner.rows(render(this.#parts), 'scalar')
		return answered.rows[0]?.[0] as V
	}

	/** Every row the conditions match, without the order and the limit. */
	async count(): Promise<number> {
		const counted = {
			...this.#parts,
			select: new Sql('count(*)', []),
			order: [],
			limit: undefined,
			offset: undefined,
		}
		const statement =
			counted.groups.length === 0
				? render(counted)
				: new Sql(
						`select count(*) from (${render({ ...counted, select: new Sql('1', []) }).text})`,
						render(counted).values,
					)
		const answered = await this.#source.runner.rows(statement, 'scalar')
		return Number(answered.rows[0]?.[0] ?? 0)
	}

	/** The rows one at a time, as their parts arrive. */
	async *each(): AsyncGenerator<T> {
		const runner = this.#source.runner as Runner & {
			each?: (statement: Sql) => AsyncGenerator<Row>
		}
		if (runner.each === undefined) {
			yield* await this.all()
			return
		}
		for await (const row of runner.each(render(this.#parts))) {
			yield (await this.#rows(Object.keys(row), [Object.values(row)]))[0]!
		}
	}

	/**
	 * A page after the last row of the one before, by the query's order,
	 * which ends in a unique column so that no row is read twice or skipped.
	 * `next` reads the next page, and only of this query.
	 *
	 *     const first = await orders.orderBy('created_at', 'desc').orderBy('id', 'desc').list({ limit: 50 })
	 *     const second = await orders.orderBy('created_at', 'desc').orderBy('id', 'desc').list({ limit: 50, after: first.next })
	 */
	async list(
		options: { limit?: number | undefined; after?: string | undefined } = {},
	): Promise<RowPage<T>> {
		const order = this.#parts.order
		if (order.length === 0 || order.some(each => each.column === undefined)) {
			throw new InvalidError(
				'a page is read by the query order, of columns by their names: orderBy(…) first',
			)
		}
		const limit = whole(options.limit ?? 100, 'a page limit')
		const digest = digestOf(render({ ...this.#parts, limit: undefined, offset: undefined }))
		let parts: Parts = { ...this.#parts, limit, offset: undefined }
		if (options.after !== undefined) {
			const after = cursorOf(options.after, digest)
			parts = {
				...parts,
				conditions: [...parts.conditions, { piece: keyset(order, after), bare: true }],
			}
		}
		const answered = await this.#source.runner.rows(render(parts), 'all')
		const rows = await this.#rows(answered.columns, answered.rows)
		if (answered.rows.length < limit) {
			return { rows }
		}
		const last = answered.rows[answered.rows.length - 1]!
		const values = order.map(each => {
			const name = each.column!.split('.').pop()!
			const at = answered.columns.indexOf(name)
			if (at < 0) {
				throw new InvalidError(
					`the page is ordered by ${each.column}, which its rows lack: select it`,
				)
			}
			return last[at] ?? null
		})
		return { rows, next: cursorText(values, digest) }
	}

	/** SQLite's plan for the query, a line a step. */
	async explain(): Promise<string[]> {
		const rendered = render(this.#parts)
		const answered = await this.#source.runner.rows(
			new Sql(`explain query plan ${rendered.text}`, rendered.values),
			'all',
		)
		const detail = answered.columns.indexOf('detail')
		return answered.rows.map(row => String(row[detail] ?? ''))
	}

	/** Sets columns of the rows the conditions match, and says how many changed. */
	async update(values: { [K in keyof T]?: T[K] | Sql } & Record<string, unknown>): Promise<Done> {
		this.#refuseEveryRow('an update')
		const names = Object.keys(values)
		if (names.length === 0) {
			throw new InvalidError('an update sets at least one column')
		}
		const sets: string[] = []
		const out: SqlArg[] = []
		for (const name of names) {
			const value = values[name]
			if (value instanceof Sql) {
				sets.push(`${identOf(name)} = ${value.text}`)
				out.push(...value.values)
			} else {
				sets.push(`${identOf(name)} = ?`)
				out.push(fieldOf(value, name, this.#source.types))
			}
		}
		const where = conditionsText(this.#parts.conditions)
		return this.#source.runner.exec(
			new Sql(`update ${tableName(this.#parts)} set ${sets.join(', ')} where ${where.text}`, [
				...out,
				...where.values,
			]),
		)
	}

	/** Deletes the rows the conditions match, and says how many it deleted. */
	async delete(): Promise<Done> {
		this.#refuseEveryRow('a delete')
		const where = conditionsText(this.#parts.conditions)
		return this.#source.runner.exec(
			new Sql(`delete from ${tableName(this.#parts)} where ${where.text}`, where.values),
		)
	}

	/** A row as the table's types and schema read it. */
	protected async read(row: Row): Promise<T> {
		const typed: Record<string, unknown> = { ...row }
		for (const [column, type] of Object.entries(this.#source.types)) {
			if (column in typed) {
				typed[column] = typedOf(typed[column] as SqlValue, type, column)
			}
		}
		const schema = this.#source.schema
		if (schema === undefined) {
			return typed as T
		}
		const checked = await check(schema, typed)
		if ('issues' in checked) {
			throw new InvalidError(`a row does not meet the table's schema: ${checked.issues}`)
		}
		return checked.value as T
	}

	protected get source(): Source {
		return this.#source
	}

	protected get parts(): Parts {
		return this.#parts
	}

	async #rows(columns: readonly string[], rows: readonly SqlValue[][]): Promise<T[]> {
		const out: T[] = []
		for (const values of rows) {
			const row: Row = {}
			columns.forEach((column, at) => {
				row[column] = values[at] ?? null
			})
			out.push(await this.read(row))
		}
		return out
	}

	#with(changed: Partial<Parts>): Query<T> {
		return new Query<T>(this.#source, { ...this.#parts, ...changed })
	}

	#joined(kind: string, table: string, on: string | Sql): Query<T> {
		const condition = typeof on === 'string' ? new Sql(on, []) : on
		const ref = tableRef(table)
		const join = new Sql(`${kind} ${ref.text} on ${condition.text}`, condition.values)
		return this.#with({ joins: [...this.#parts.joins, join] })
	}

	/** Refuses an update or a delete of every row, which SQL says plainly. */
	#refuseEveryRow(what: string): void {
		const conditions = this.#parts.conditions.filter(each => each.piece.text !== '1')
		if (conditions.length === 0) {
			throw new InvalidError(`${what} without a condition: every row is said in SQL, delete from …`)
		}
		if (this.#parts.joins.length > 0) {
			throw new InvalidError(`${what} of a join: write it in SQL`)
		}
	}
}

/**
 * A table: its queries, and its rows' inserts and upserts.
 *
 *     const notes = db.table<Note>('notes', { types: { done: 'bool', tags: 'json', created_at: 'time' } })
 *     const note = await notes.insert({ id: uuidv7(), author_id: userId, title: 'Buy milk', done: false, tags: [], created_at: new Date() })
 */
export class Table<T = Row> extends Query<T> {
	readonly name: string

	/** Tables come from `db.table` and `tx.table`. */
	constructor(runner: Runner, name: string, options: TableOptions = {}) {
		const ref = tableRef(name)
		super(
			{ runner, types: options.types ?? {}, schema: options.schema },
			emptyParts(ref.name, ref.alias),
		)
		this.name = ref.name
	}

	/** Inserts a row and gives it back as the file keeps it; a list of rows is one write, all of them or none. */
	insert(row: Partial<T>): Promise<T>
	insert(rows: readonly Partial<T>[]): Promise<T[]>
	async insert(given: Partial<T> | readonly Partial<T>[]): Promise<T | T[]> {
		const rows = Array.isArray(given) ? (given as readonly Partial<T>[]) : [given as Partial<T>]
		if (rows.length === 0) {
			return []
		}
		const statement = insertOf(
			this.name,
			rows as readonly Record<string, unknown>[],
			this.source.types,
		)
		const answered = await this.source.runner.rows(
			new Sql(`${statement.text} returning *`, statement.values),
			'all',
		)
		const read: T[] = []
		for (const values of answered.rows) {
			const row: Row = {}
			answered.columns.forEach((column, at) => {
				row[column] = values[at] ?? null
			})
			read.push(await this.read(row))
		}
		return Array.isArray(given) ? read : read[0]!
	}

	/**
	 * Inserts a row, or, when its key is taken, sets the columns `update`
	 * names on the row that holds it, if that row meets `where`. It gives
	 * back the row as the file keeps it, or undefined when `where` kept it.
	 */
	async upsert(row: Partial<T>, options: UpsertOptions<T>): Promise<T | undefined> {
		const conflict = (
			typeof options.conflict === 'string' ? [options.conflict] : [...options.conflict]
		).map(identOf)
		if (options.update.length === 0) {
			throw new InvalidError('an upsert names the columns its update sets')
		}
		const insert = insertOf(this.name, [row as Record<string, unknown>], this.source.types)
		const sets = options.update
			.map(name => `${identOf(name)} = excluded.${identOf(name)}`)
			.join(', ')
		let text = `${insert.text} on conflict (${conflict.join(', ')}) do update set ${sets}`
		const values = [...insert.values]
		if (options.where !== undefined) {
			// in an upsert's update, a column is the row's that holds the key
			const where = conditionOf(options.where)
			text += ` where ${where.text}`
			values.push(...where.values)
		}
		const answered = await this.source.runner.rows(new Sql(`${text} returning *`, values), 'one')
		if (answered.rows.length === 0) {
			return undefined
		}
		const read: Row = {}
		answered.columns.forEach((column, at) => {
			read[column] = answered.rows[0]![at] ?? null
		})
		return this.read(read)
	}
}

function emptyParts(table: string, alias?: string): Parts {
	return { table, alias, joins: [], conditions: [], groups: [], having: [], order: [] }
}

/** `'orders'` or `'orders as o'`, quoted as names. */
function tableRef(text: string): { name: string; alias?: string | undefined; text: string } {
	const matched =
		/^\s*([A-Za-z_][\w$]*(?:\.[A-Za-z_][\w$]*)?)(?:\s+(?:as\s+)?([A-Za-z_][\w$]*))?\s*$/i.exec(text)
	if (matched === null) {
		throw new InvalidError(
			`the table ${JSON.stringify(text)}: a table is its name, and an alias after as`,
		)
	}
	const [, name, alias] = matched
	return {
		name: name!,
		alias,
		text: alias === undefined ? identOf(name!) : `${identOf(name!)} as ${identOf(alias)}`,
	}
}

function tableName(parts: Parts): string {
	return identOf(parts.table)
}

/** A condition `where` takes, as the pieces an and joins. */
function kept(condition: Condition | string, values: unknown[]): Kept[] {
	if (typeof condition === 'string') {
		const counted = placeholders(condition)
		if (counted.positional !== values.length || counted.named.length > 0 || counted.numbered) {
			throw new InvalidError(
				`${counted.positional} placeholders and ${values.length} values: each ? takes one value`,
			)
		}
		return [
			{
				piece: new Sql(
					condition,
					values.map((value, at) => cellOf(value, `value ${at + 1}`)),
				),
				bare: false,
			},
		]
	}
	if (values.length > 0) {
		throw new InvalidError('a condition that is not text carries its values: pass it alone')
	}
	if (condition instanceof Sql) {
		return [{ piece: condition, bare: condition.bare || grouped(condition.text) }]
	}
	return Object.keys(condition)
		.sort()
		.map(name => ({ piece: conditionOf({ [name]: condition[name] }), bare: true }))
}

function conditionsText(conditions: readonly Kept[]): Sql {
	return joinedWith(
		conditions.map(each =>
			each.bare ? new Sql(each.piece.text, each.piece.values, true) : each.piece,
		),
		'and',
	)
}

/** The query as one statement. */
function render(parts: Parts): Sql {
	const values: SqlArg[] = []
	const from =
		parts.alias === undefined
			? identOf(parts.table)
			: `${identOf(parts.table)} as ${identOf(parts.alias)}`
	let columns = '*'
	if (parts.select !== undefined) {
		columns = parts.select.text
		values.push(...parts.select.values)
	} else if (parts.joins.length > 0) {
		columns = `${identOf(parts.alias ?? parts.table)}.*`
	}
	let text = `select ${columns} from ${from}`
	for (const join of parts.joins) {
		text += ` ${join.text}`
		values.push(...join.values)
	}
	if (parts.conditions.length > 0) {
		const where = conditionsText(parts.conditions)
		text += ` where ${where.text}`
		values.push(...where.values)
	}
	if (parts.groups.length > 0) {
		text += ` group by ${parts.groups.join(', ')}`
	}
	if (parts.having.length > 0) {
		const having = conditionsText(parts.having)
		text += ` having ${having.text}`
		values.push(...having.values)
	}
	if (parts.order.length > 0) {
		text += ` order by ${parts.order.map(each => `${each.piece.text} ${each.direction}`).join(', ')}`
		values.push(...parts.order.flatMap(each => each.piece.values))
	}
	if (parts.limit !== undefined) {
		text += ' limit cast(? as integer)'
		values.push(parts.limit)
	}
	if (parts.offset !== undefined) {
		text += `${parts.limit === undefined ? ' limit -1' : ''} offset cast(? as integer)`
		values.push(parts.offset)
	}
	return new Sql(text, values)
}

/** The condition that starts a page after a row's order values. */
function keyset(order: Parts['order'], after: readonly SqlValue[]): Sql {
	const alternatives: string[] = []
	const values: SqlArg[] = []
	order.forEach((each, at) => {
		const equal = order.slice(0, at).map(earlier => `${earlier.piece.text} = ?`)
		values.push(...after.slice(0, at))
		const past = `${each.piece.text} ${each.direction === 'asc' ? '>' : '<'} ?`
		values.push(after[at] ?? null)
		alternatives.push(equal.length === 0 ? past : `(${[...equal, past].join(' and ')})`)
	})
	return new Sql(`(${alternatives.join(' or ')})`, values)
}

/** A short digest of a query, which a cursor carries so that another query refuses it. */
function digestOf(statement: Sql): string {
	let hash = 0x811c9dc5
	const text = `${statement.text}\u0000${JSON.stringify(statement.values, (_, v) => (typeof v === 'bigint' ? `${v}n` : v))}`
	for (let at = 0; at < text.length; at++) {
		hash ^= text.charCodeAt(at)
		hash = Math.imul(hash, 0x01000193) >>> 0
	}
	return hash.toString(36)
}

function cursorText(values: readonly SqlValue[], digest: string): string {
	const json = JSON.stringify({ q: digest, v: values }, (_, v) =>
		typeof v === 'bigint' ? { n: String(v) } : v,
	)
	return Buffer.from(json).toString('base64url')
}

function cursorOf(cursor: string, digest: string): SqlValue[] {
	let read: { q?: string; v?: unknown[] }
	try {
		read = JSON.parse(Buffer.from(cursor, 'base64url').toString())
	} catch {
		throw new InvalidError('a cursor that is not one: pass the next of a page')
	}
	if (read.q !== digest || !Array.isArray(read.v)) {
		throw new InvalidError(
			"a cursor of another query: a page's next reads only the query that made it",
		)
	}
	return read.v.map(v =>
		typeof v === 'object' && v !== null && 'n' in v
			? BigInt((v as { n: string }).n)
			: (v as SqlValue),
	)
}

/** An insert of rows of the same columns, the first row's. */
function insertOf(
	table: string,
	rows: readonly Record<string, unknown>[],
	types: Record<string, ColumnType>,
): Sql {
	const names = Object.keys(rows[0]!).filter(name => rows[0]![name] !== undefined)
	if (names.length === 0) {
		throw new InvalidError('a row to insert with no column')
	}
	const values: SqlArg[] = []
	const tuples = rows.map((row, at) => {
		const extra = Object.keys(row).filter(name => !names.includes(name) && row[name] !== undefined)
		if (extra.length > 0) {
			throw new InvalidError(
				`row ${at + 1} has ${extra.join(', ')}, which the first row has not: insert rows of one shape`,
			)
		}
		for (const name of names) {
			values.push(fieldOf(row[name], name, types))
		}
		return `(${names.map(() => '?').join(', ')})`
	})
	return new Sql(
		`insert into ${identOf(table)} (${names.map(identOf).join(', ')}) values ${tuples.join(', ')}`,
		values,
	)
}

/** A row's field as it is written: an array or an object as its JSON. */
function fieldOf(value: unknown, name: string, types: Record<string, ColumnType>): SqlArg {
	if (value === undefined) {
		throw new InvalidError(
			`${name} is undefined, which never sets a column to null: leave the field out, or say null`,
		)
	}
	if (
		types[name] === 'json' ||
		(typeof value === 'object' &&
			value !== null &&
			!(value instanceof Date) &&
			!(value instanceof Uint8Array))
	) {
		return JSON.stringify(value)
	}
	return cellOf(value, name)
}

/** A stored value as its column's type reads it. */
function typedOf(value: SqlValue, type: ColumnType, column: string): unknown {
	if (value === null) {
		return null
	}
	switch (type) {
		case 'bool':
			if (value === 0 || value === 1) {
				return value === 1
			}
			break
		case 'time':
			if (typeof value === 'number' || typeof value === 'bigint') {
				return new Date(Number(value))
			}
			break
		case 'json':
			if (typeof value === 'string') {
				return JSON.parse(value)
			}
			break
		case 'bigint':
			if (typeof value === 'number' || typeof value === 'bigint') {
				return BigInt(value)
			}
			break
	}
	throw new InvalidError(`column ${column}: ${String(value)} does not read as ${type}`)
}

function whole(n: number, what: string): number {
	if (!Number.isSafeInteger(n) || n < 0) {
		throw new InvalidError(`${what} of ${n}: a whole number`)
	}
	return n
}
