// Every query of testdata/sql/queries.json, built with this SDK's builder from
// its steps: the text and the values it sends, which every SDK's builder must
// write alike. A runner that records what it is given stands for a database.

import { describe, expect, test } from 'bun:test'
import { resolve } from 'node:path'

import { type Query, Table } from '../src/query.ts'
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
