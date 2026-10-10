// A table's queries, built from pieces into the SQL a person would write: the
// builder adds the ands, the parentheses, the placeholders and the quotes
// around names, and nothing else. It runs in this process; the server sees
// the text and the values, as it would a statement written by hand.

import { CorruptError, InvalidError } from './errors.ts'
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
	includes: Included[]
}

/** A query whose rows each row of another holds as a list under a name. */
interface Included {
	name: string
	parts: Parts
	/** how the included rows read: their table's types and schema */
	source: Source
	/** the names of the columns its select gives, in their order */
	columns: string[]
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

	/**
	 * Gives each row the rows of another query as a list under `name`, from
	 * the same statement: the query names the row it belongs to in its
	 * `where`, by this table's alias, names its columns with `select`, and has
	 * a `limit`, which counts for each row. Every value comes back as it is
	 * kept, bytes and integers past 2^53 among them.
	 *
	 *     const latest = db.table<Post>('posts as p').select<{ id: string; title: string }>('p.id, p.title')
	 *       .where('p.author_id = u.id').orderBy('p.id', 'desc').limit(3)
	 *     const authors = await db.table<User>('users as u').include('posts', latest).all()
	 */
	include<N extends string, C>(name: N, query: Query<C>): Query<T & Record<N, C[]>> {
		const describe = `the included ${JSON.stringify(name)}`
		const parts = query.parts
		if (parts.select === undefined) {
			throw new InvalidError(`${describe}: it names its columns with select('p.id, p.title')`)
		}
		if (parts.limit === undefined) {
			throw new InvalidError(`${describe}: it needs a limit, which counts for each row`)
		}
		if (parts.includes.length > 0) {
			throw new InvalidError(`${describe}: an included query includes nothing itself`)
		}
		const included = {
			name,
			parts,
			source: query.source,
			columns: namesOf(parts.select.text, describe),
		}
		const taken = this.#parts.includes.some(each => each.name === name)
		if (taken || name.length === 0) {
			throw new InvalidError(`${describe}: an included list has a name of its own`)
		}
		return new Query<T & Record<N, C[]>>(this.#source, {
			...this.#parts,
			includes: [...this.#parts.includes, included],
		})
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
			includes: [],
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
		return (await readAs(this.#source, row)) as T
	}

	protected get source(): Source {
		return this.#source
	}

	protected get parts(): Parts {
		return this.#parts
	}

	async #rows(columns: readonly string[], rows: readonly SqlValue[][]): Promise<T[]> {
		const includes = this.#parts.includes
		const out: T[] = []
		for (const values of rows) {
			const row: Row = {}
			const lists: Record<string, unknown[]> = {}
			for (const [at, column] of columns.entries()) {
				const included = includes.find(each => each.name === column)
				if (included === undefined) {
					row[column] = values[at] ?? null
				} else {
					lists[column] = await includedRows(included, values[at] ?? null)
				}
			}
			out.push({ ...((await this.read(row)) as object), ...lists } as T)
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
	return {
		table,
		alias,
		joins: [],
		conditions: [],
		groups: [],
		having: [],
		order: [],
		includes: [],
	}
}

/** A row as a table's types and schema read it. */
async function readAs(source: Source, row: Row): Promise<unknown> {
	const typed: Record<string, unknown> = { ...row }
	for (const [column, type] of Object.entries(source.types)) {
		if (column in typed) {
			typed[column] = typedOf(typed[column] as SqlValue, type, column)
		}
	}
	if (source.schema === undefined) {
		return typed
	}
	const checked = await check(source.schema, typed)
	if ('issues' in checked) {
		throw new InvalidError(`a row does not meet the table's schema: ${checked.issues}`)
	}
	return checked.value
}

/** The rows a row's included query gave, read as that query's table reads them. */
async function includedRows(included: Included, kept: SqlValue): Promise<unknown[]> {
	if (kept === null) {
		return []
	}
	if (typeof kept !== 'string') {
		throw new CorruptError(`the included ${JSON.stringify(included.name)} came back as no text`)
	}
	const rows: unknown[] = []
	for (const values of literalRows(kept)) {
		const row: Row = {}
		included.columns.forEach((column, at) => {
			row[column] = values[at] ?? null
		})
		rows.push(await readAs(included.source, row))
	}
	return rows
}

/**
 * The names of the columns a select's text gives, in their order: a column's
 * own name, or what follows its `as`. An expression without a name is
 * refused, since its rows could not be given their fields.
 */
export function namesOf(select: string, describe: string): string[] {
	const name = `("(?:[^"]|"")+"|[A-Za-z_][\\w$]*)`
	const aliased = new RegExp(`\\s+as\\s+${name}\\s*$`, 'i')
	const column = new RegExp(`^(?:${name}\\.)?${name}$`)
	const bare = (quoted: string) =>
		quoted.startsWith('"') ? quoted.slice(1, -1).replaceAll('""', '"') : quoted
	const names: string[] = []
	for (const item of itemsOf(select)) {
		const found = aliased.exec(item)?.[1] ?? column.exec(item)?.[2]
		if (found === undefined) {
			throw new InvalidError(
				`${describe}: the column ${JSON.stringify(item)} needs a name: write it as … as name`,
			)
		}
		if (names.includes(bare(found))) {
			throw new InvalidError(`${describe}: two columns named ${bare(found)}`)
		}
		names.push(bare(found))
	}
	return names
}

/** A select's items: its text cut at the commas outside quotes and parentheses. */
function itemsOf(select: string): string[] {
	const items: string[] = []
	let [depth, from, at] = [0, 0, 0]
	while (at < select.length) {
		const c = select[at] as string
		if (c === "'" || c === '"' || c === '`') {
			at = select.indexOf(c, at + 1)
			if (at < 0) {
				break
			}
		} else if (c === '(') {
			depth++
		} else if (c === ')') {
			depth--
		} else if (c === ',' && depth === 0) {
			items.push(select.slice(from, at).trim())
			from = at + 1
		}
		at++
	}
	items.push(select.slice(from).trim())
	return items
}

/**
 * An included query as one value of its row: its rows as SQL literals,
 * `(1,'a'),(2,NULL)`, in the query's order. SQLite's quote() writes an
 * integer, a float, a text and bytes so that each reads back as it is kept,
 * where JSON would lose bytes and the last digits of a float.
 */
function includedText(included: Included): Sql {
	const child = included.parts
	const order = child.order.map((each, at) => ({ ...each, alias: `"$o${at + 1}"` }))
	const select = child.select as Sql
	const inner = render({
		...child,
		select: new Sql(
			[select.text, ...order.map(each => `${each.piece.text} as ${each.alias}`)].join(', '),
			[...select.values, ...order.flatMap(each => each.piece.values)],
		),
	})
	const row = included.columns.map(literalOf).join(" || ',' || ")
	const ordered =
		order.length === 0
			? ''
			: ` order by ${order.map(each => `${each.alias} ${each.direction}`).join(', ')}`
	return new Sql(
		`(select group_concat('(' || ${row} || ')', ','${ordered}) from (${inner.text}))`,
		inner.values,
	)
}

/** A column as an SQL literal that keeps it whole: quote() cuts a text at a zero byte, so such a text goes as its bytes. */
function literalOf(column: string): string {
	const c = `"${column.replaceAll('"', '""')}"`
	return `case when typeof(${c}) = 'text' and instr(cast(${c} as blob), x'00') > 0 then 'T' || quote(cast(${c} as blob)) else quote(${c}) end`
}

/** The rows an included query gave, read from their literals: `(1,'it''s',NULL),(2,X'00ff',1.5)`. */
export function literalRows(text: string): SqlValue[][] {
	const reader = new Literals(text)
	const rows: SqlValue[][] = []
	while (!reader.done) {
		reader.expect('(')
		const row = [reader.literal()]
		while (reader.next === ',') {
			reader.expect(',')
			row.push(reader.literal())
		}
		reader.expect(')')
		rows.push(row)
		if (!reader.done) {
			reader.expect(',')
		}
	}
	return rows
}

/** A reader of SQL literals as SQLite's quote() writes them. */
class Literals {
	readonly #text: string
	#at = 0

	constructor(text: string) {
		this.#text = text
	}

	get done(): boolean {
		return this.#at >= this.#text.length
	}

	get next(): string | undefined {
		return this.#text[this.#at]
	}

	expect(wanted: string): void {
		if (this.next !== wanted) {
			throw this.#broken()
		}
		this.#at++
	}

	literal(): SqlValue {
		switch (this.next) {
			case "'":
				return this.#quoted()
			case 'X':
				return this.#hex()
			case 'T':
				this.#at++
				return new TextDecoder().decode(this.#hex())
		}
		if (this.#text.startsWith('NULL', this.#at)) {
			this.#at += 4
			return null
		}
		return this.#number()
	}

	/** A text in single quotes, a quote inside it doubled. */
	#quoted(): string {
		let value = ''
		for (this.#at++; ; this.#at++) {
			const end = this.#text.indexOf("'", this.#at)
			if (end < 0) {
				throw this.#broken()
			}
			value += this.#text.slice(this.#at, end)
			this.#at = end + 1
			if (this.next !== "'") {
				return value
			}
			value += "'"
		}
	}

	/** Bytes as `X'00ff'`. */
	#hex(): Uint8Array {
		const end = this.#text.indexOf("'", this.#at + 2)
		const digits = this.#text.slice(this.#at + 2, Math.max(end, 0))
		if (
			!this.#text.startsWith("X'", this.#at) ||
			end < 0 ||
			!/^(?:[0-9A-Fa-f]{2})*$/.test(digits)
		) {
			throw this.#broken()
		}
		this.#at = end + 1
		return new Uint8Array(Buffer.from(digits, 'hex'))
	}

	/** An integer, a bigint past 2^53, or a float, which quote() writes with a point or an exponent. */
	#number(): number | bigint {
		let end = this.#at
		while (end < this.#text.length && this.#text[end] !== ',' && this.#text[end] !== ')') {
			end++
		}
		const word = this.#text.slice(this.#at, end)
		if (/^-?\d+$/.test(word)) {
			this.#at = end
			const whole = BigInt(word)
			return whole >= Number.MIN_SAFE_INTEGER && whole <= Number.MAX_SAFE_INTEGER
				? Number(whole)
				: whole
		}
		if (word.length === 0 || Number.isNaN(Number(word))) {
			throw this.#broken()
		}
		this.#at = end
		return Number(word)
	}

	#broken(): CorruptError {
		const shown = this.#text.slice(this.#at, this.#at + 20)
		return new CorruptError(`included rows that do not read at ${this.#at}: ${shown}`)
	}
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
	} else if (parts.joins.length > 0 || parts.includes.length > 0) {
		columns = `${identOf(parts.alias ?? parts.table)}.*`
	}
	for (const included of parts.includes) {
		const list = includedText(included)
		columns += `, ${list.text} as ${identOf(included.name)}`
		values.push(...list.values)
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
