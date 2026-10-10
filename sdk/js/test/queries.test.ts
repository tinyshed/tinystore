// Every query of testdata/sql/queries.json, built with this SDK's builder from
// its steps: the text and the values it sends, which every SDK's builder must
// write alike. A runner that records what it is given stands for a database.

import { describe, expect, test } from 'bun:test'
import { resolve } from 'node:path'

import { CorruptError, InvalidError } from '../src/errors.ts'
import { literalRows, namesOf, type Query, Table } from '../src/query.ts'
import { type Condition, type Done, type Runner, type Sql, type SqlArg, sql } from '../src/sql.ts'

type Step = Record<string, unknown>

interface Case {
	name: string
	table: string
	steps?: Step[]
	update?: [string, unknown][]
	delete?: true
	insert?: [string, unknown][][]
	upsert?: [[string, unknown][], { conflict: string; update: string[]; where?: unknown }]
	count?: true
	text: string
	values: unknown[]
}

const vectors: { queries: Case[] } = await Bun.file(
	resolve(import.meta.dir, '../../../testdata/sql/queries.json'),
).json()

/** A runner that keeps the last statement it was given and answers nothing. */
class Recorder implements Runner {
	readonly describe = 'sql vectors'
	last: Sql | undefined

	async rows(statement: Sql) {
		this.last = statement
		return { columns: [], rows: [] }
	}

	async exec(statement: Sql): Promise<Done> {
		this.last = statement
		return { changes: 0, lastInsertRowid: 0 }
	}
}

function conditionOf(given: unknown): Condition {
	const condition = given as Record<string, unknown>
	if ('equal' in condition) return condition.equal as Record<string, unknown>
	if ('sql' in condition) {
		const [text, ...values] = condition.sql as [string, ...unknown[]]
		return sql(text, ...values)
	}
	if ('or' in condition) return sql.or(...(condition.or as unknown[]).map(conditionOf))
	if ('and' in condition) return sql.and(...(condition.and as unknown[]).map(conditionOf))
	if ('has' in condition) {
		const [name, value] = condition.has as [string, unknown]
		return sql.has(name, value)
	}
	throw new Error(`no condition ${JSON.stringify(given)}`)
}

function apply(query: Query<Record<string, unknown>>, step: Step): Query<Record<string, unknown>> {
	const [verb, argument] = Object.entries(step)[0]!
	switch (verb) {
		case 'where':
			return query.where(conditionOf(argument))
		case 'having':
			return query.having(conditionOf(argument))
		case 'select':
			return query.select(argument as string)
		case 'join': {
			const [table, on] = argument as [string, unknown]
			return query.join(table, typeof on === 'string' ? on : (conditionOf(on) as Sql))
		}
		case 'leftJoin': {
			const [table, on] = argument as [string, unknown]
			return query.leftJoin(table, typeof on === 'string' ? on : (conditionOf(on) as Sql))
		}
		case 'groupBy':
			return query.groupBy(...(argument as string[]))
		case 'orderBy': {
			const [column, direction] = argument as [string, 'asc' | 'desc']
			return query.orderBy(column, direction)
		}
		case 'limit':
			return query.limit(argument as number)
		case 'offset':
			return query.offset(argument as number)
		case 'include': {
			const [name, included] = argument as [string, { table: string; steps: Step[] }]
			const table: Query<Record<string, unknown>> = new Table(new Recorder(), included.table)
			return query.include(name, included.steps.reduce(apply, table))
		}
	}
	throw new Error(`no step ${verb}`)
}

/** A value as SQLite keeps it, which is what the vectors say. */
function kept(value: SqlArg): unknown {
	if (typeof value === 'boolean') return value ? 1 : 0
	if (value instanceof Date) return value.getTime()
	return value
}

/** What a vector's write is given, its pieces of SQL made. */
function written(values: Record<string, unknown>): Record<string, unknown> {
	return Object.fromEntries(
		Object.entries(values).map(([name, value]) => [
			name,
			typeof value === 'object' && value !== null && 'sql' in value ? conditionOf(value) : value,
		]),
	)
}

describe('the query vectors', () => {
	for (const vector of vectors.queries) {
		test(vector.name, async () => {
			const recorder = new Recorder()
			const table = new Table<Record<string, unknown>>(recorder, vector.table)
			const query = (vector.steps ?? []).reduce(apply, table as Query<Record<string, unknown>>)
			let statement: { text: string; values: readonly SqlArg[] }
			if (vector.update !== undefined) {
				await query.update(written(Object.fromEntries(vector.update)))
				statement = recorder.last!
			} else if (vector.delete) {
				await query.delete()
				statement = recorder.last!
			} else if (vector.insert !== undefined) {
				await table.insert(vector.insert.map(row => Object.fromEntries(row)))
				statement = recorder.last!
			} else if (vector.upsert !== undefined) {
				const [pairs, options] = vector.upsert
				const row = Object.fromEntries(pairs)
				await table.upsert(row, {
					...options,
					where: options.where === undefined ? undefined : conditionOf(options.where),
				})
				statement = recorder.last!
			} else if (vector.count) {
				await query.count()
				statement = recorder.last!
			} else {
				statement = query.toSQL()
			}
			expect(statement.text).toBe(vector.text)
			expect(statement.values.map(kept)).toEqual(vector.values)
		})
	}
})

// The same cases as the core's own tests of sql/included.rs: both read a
// select's names and an included query's literals alike.
describe('an included query', () => {
	test("a select's columns are named by themselves or by their as", () => {
		const select = `p.id, "p"."the title", upper(p.status) as status, count(*) AS "n", 'a, b' as c`
		expect(namesOf(select, 'posts')).toEqual(['id', 'the title', 'status', 'n', 'c'])
		expect(namesOf('p.id  AS\n"a ""b""", x\tas y', 'posts')).toEqual(['a "b"', 'y'])
		for (const unnamed of [
			'p.id, count(*)',
			'p.id, o.id',
			'cast(p.id as text)',
			'p.title t',
			'p.*',
		]) {
			expect(() => namesOf(unnamed, 'posts')).toThrow(InvalidError)
		}
	})

	test('rows read back from their literals', () => {
		const text =
			"(1,'it''s, (odd)',NULL),(-9223372036854775808,X'00FF27',1.5),(2,TX'610062',-9.0e+999)"
		expect(literalRows(text)).toEqual([
			[1, "it's, (odd)", null],
			[-9223372036854775808n, new Uint8Array([0, 255, 39]), 1.5],
			[2, 'a\u0000b', Number.NEGATIVE_INFINITY],
		])
		expect(literalRows('')).toEqual([])
		for (const broken of ["(1,'open", '(1)(2)', "(X'0g')", '(1,)']) {
			expect(() => literalRows(broken)).toThrow(CorruptError)
		}
	})
})
