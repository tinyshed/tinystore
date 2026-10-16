// Search, as plan/api/search.md has it: documents of one type set by id in a
// named index and found again by their words, by vectors the program's own
// model gives them, or by both, kept to the documents a `where` passes. A
// search answers the exact best ten, or `limit`, of what the index holds; an
// index opened with `recall` answers its vectors from clusters. A table's
// index, `db.searchIndex`, finds a database's rows, each hit its row.

import type { Link } from './connection.ts'
import { InvalidError } from './errors.ts'
import { checkName, type Open, type Via, viaLink } from './handles.ts'
import { parseJson } from './json.ts'
import { keyText } from './kv.ts'
import { rowsOf } from './sql.ts'
import type { Key, SqlValue } from './wire/codec.ts'
import {
	Empty,
	Handle,
	methods,
	SearchDeleted,
	SearchDropped,
	SearchFound,
	SearchGot,
	SearchKey,
	SearchListed,
	SearchName,
	SearchOpen,
	SearchQuery,
	SearchSet,
	SearchSetMany,
	SearchStatus,
} from './wire/protocol.ts'

/** How an index reads its documents and combines its halves, given where it opens. */
export interface IndexOptions {
	/** the fields whose words are read, each with what a word found there weighs, or a list that weighs each 1 */
	fields?: Record<string, number> | readonly string[] | undefined
	/** the fields a search may keep its hits to */
	filters?: readonly string[] | undefined
	/** a misspelt word read as the index's nearest; on unless false */
	typoTolerance?: boolean | undefined
	/** a vector the program's model gives each document, and `recall` for searches from clusters */
	vectors?: { dimensions: number; recall?: number | undefined } | undefined
	/** how a search by words and a vector at once combines them unless it says */
	hybrid?: { vectorWeight?: number | undefined; rerank?: boolean | undefined } | undefined
	/** whether a change that needs every document read again waits for `store.reindex` */
	rebuild?: 'automatic' | 'manual' | undefined
}

/** A vector the program's own model gave, of the index's dimensions. */
export type Vector = readonly number[] | Float32Array

/** A value a `where` matches: text, a number, a boolean, one of a list, a range, or `null` for none. */
export type WhereValue =
	| string
	| number
	| bigint
	| boolean
	| null
	| readonly (string | number | bigint | boolean)[]
	| { gt?: number | bigint; gte?: number | bigint; lt?: number | bigint; lte?: number | bigint }

/**
 * What a search keeps its hits to, as sqldb's `where` reads an object: each
 * field equal to its value, one of a list, within a range, or `null` for
 * none, every field named passing; `search.or` and `search.and` group them.
 */
export type Where = Readonly<Record<string, WhereValue>> | Group

/** Conditions joined: `search.or(…)`, `search.and(…)`. */
export class Group {
	readonly join: 'or' | 'and'
	readonly parts: readonly Where[]

	/** A group comes from `search.or` and `search.and`. */
	constructor(join: 'or' | 'and', parts: readonly Where[]) {
		this.join = join
		this.parts = parts
	}
}

/** What a search asks: its words, its vector, or both, and what it keeps to. */
export interface Query {
	text?: string | undefined
	vector?: Vector | undefined
	where?: Where | undefined
	/** this search's way of combining its words and its vector, in place of the index's */
	hybrid?: { vectorWeight?: number | undefined; rerank?: boolean | undefined } | undefined
	/** 10 unless said; offset and limit together 1000 at most */
	limit?: number | undefined
	offset?: number | undefined
}

/** A document a search found, and the score that ordered it among this search's hits. */
export interface Hit<T> {
	id: string
	value: T
	score: number
}

/** What a search found, the best first, and the text it read in fact when a word was misspelt. */
export interface Found<T> {
	hits: Hit<T>[]
	corrected?: string | undefined
}

/** Where an index stands: serving what it was opened with, read again beside it, or a rebuild that failed. */
export type IndexStatus =
	| { state: 'ready' }
	| { state: 'rebuilding'; done: number }
	| { state: 'failed'; error: string }

/** An index as `store.indexes()` lists it. */
export interface IndexInfo {
	name: string
	documents: number
	vectors: number
	/** when a program last opened it */
	opened?: Date | undefined
	status: IndexStatus
}

/**
 * How a table's index reads a table of its database: its columns of text,
 * those a search may filter by, and a column of vectors the program fills.
 */
export interface TableIndexOptions {
	/** the table whose rows the index holds and follows */
	table: string
	/** the column of unique values a row is found by, the table's primary key unless said */
	key?: string | undefined
	/** the columns whose words are read, each with what a word found there weighs, or a list that weighs each 1 */
	fields: Record<string, number> | readonly string[]
	/** the columns a search may keep its hits to; a boolean is the 1 or 0 SQLite keeps */
	filters?: readonly string[] | undefined
	typoTolerance?: boolean | undefined
	/** a column of blobs of little-endian floats, one a row or null, and `recall` for searches from clusters */
	vectors?: { column: string; dimensions: number; recall?: number | undefined } | undefined
	hybrid?: { vectorWeight?: number | undefined; rerank?: boolean | undefined } | undefined
	rebuild?: 'automatic' | 'manual' | undefined
}

/** A document `setMany` sets: its id, its value, and its vector, or `null` for none. */
export interface IndexDocument<T> {
	id: Key
	value: T
	vector?: Vector | null | undefined
}

/** How often `ready` asks whether a rebuild switched in. */
const readyEvery = 100

/** The calls of an index that a call names. */
type Method =
	| 'search.set'
	| 'search.setMany'
	| 'search.delete'
	| 'search.get'
	| 'search.search'
	| 'search.status'

/** What every index answers: its searches, and where it stands. */
abstract class Searchable<T> {
	readonly name: string
	readonly #via: Via
	readonly #open: Open

	constructor(via: Via, name: string, open: Open) {
		this.name = name
		this.#via = via
		this.#open = open
	}

	/**
	 * The best ten, or `limit`, by `text`'s words, a string being a search
	 * by its words; by a vector; or by both, each half's best hundred scaled
	 * and summed at the index's `vectorWeight`; kept to what `where` passes.
	 * Of equal scores the one set first comes first.
	 */
	async search(query: string | Query): Promise<Found<T>> {
		const asked = typeof query === 'string' ? { text: query } : query
		const vector = asked.vector === undefined ? undefined : vectorBytes(asked.vector)
		const where = asked.where === undefined ? undefined : whereJson(asked.where)
		const body = (handle: number) =>
			SearchQuery.encode({
				handle,
				text: asked.text ?? '',
				vector,
				vectorWeight: asked.hybrid?.vectorWeight,
				rerank: asked.hybrid?.rerank,
				where,
				offset: asked.offset,
				limit: asked.limit,
			})
		const found = SearchFound.decode(await this.call('search.search', body, 'read'))
		const columns = found.columns
		const hits = (found.hits ?? []).map(hit => ({
			id: hit.id ?? '',
			value: (columns === undefined
				? parseJson(hit.value ?? 'null')
				: rowsOf<T>(columns, [(hit.row ?? []) as SqlValue[]])[0]) as T,
			score: hit.score ?? 0,
		}))
		return found.corrected === undefined ? { hits } : { hits, corrected: found.corrected }
	}

	/** Where the index stands; a search never waits, and answers by the settings it serves. */
	async status(): Promise<IndexStatus> {
		const answer = await this.call('search.status', handle => Handle.encode({ handle }), 'read')
		return statusOf(SearchStatus.decode(answer))
	}

	/** Waits until the index serves the settings it was opened with; a rebuild that failed throws why. */
	async ready(): Promise<void> {
		for (;;) {
			const status = await this.status()
			if (status.state === 'ready') {
				return
			}
			if (status.state === 'failed') {
				throw new InvalidError(`index ${this.name}: its rebuild failed: ${status.error}`)
			}
			await Bun.sleep(readyEvery)
		}
	}

	protected call(
		method: Method,
		body: (handle: number) => Uint8Array,
		idempotence: 'read' | 'write',
	): Promise<Uint8Array> {
		return this.#via.call(methods['search.open'], this.#open, methods[method], body, idempotence)
	}
}

/** Documents of one type by id, found again by their words, vectors or both: `store.index(name, options)`. */
export class SearchIndex<T> extends Searchable<T> {
	/** An index comes from `store.index` and `store.reindex`. */
	constructor(link: Link, name: string, open: Uint8Array) {
		super(viaLink(link), name, open)
	}

	/**
	 * Sets the document `id` to `value`, replacing it whole; the next search
	 * sees it. On an index with vectors a set gives its vector, or `null` for
	 * a document only its words find.
	 */
	async set(id: Key, value: T, options?: { vector?: Vector | null | undefined }): Promise<void> {
		const said = vectorSaid(options?.vector)
		const json = documentJson(this.name, value)
		await this.call(
			'search.set',
			handle => SearchSet.encode({ handle, id: keyText(id), value: json, ...said }),
			'write',
		)
	}

	/** Sets every document in one commit, all of them or none: 1,000 and 16 MiB at most. */
	async setMany(documents: readonly IndexDocument<T>[]): Promise<void> {
		const encoded = documents.map(document => ({
			id: keyText(document.id),
			value: documentJson(this.name, document.value),
			...vectorSaid(document.vector),
		}))
		await this.call(
			'search.setMany',
			handle => SearchSetMany.encode({ handle, documents: encoded }),
			'write',
		)
	}

	/** Deletes the document `id`; says whether there was one. */
	async delete(id: Key): Promise<boolean> {
		const answer = await this.call(
			'search.delete',
			handle => SearchKey.encode({ handle, id: keyText(id) }),
			'write',
		)
		return SearchDeleted.decode(answer).deleted ?? false
	}

	/** The document `id`, or undefined. */
	async get(id: Key): Promise<T | undefined> {
		const answer = await this.call(
			'search.get',
			handle => SearchKey.encode({ handle, id: keyText(id) }),
			'read',
		)
		const got = SearchGot.decode(answer)
		return got.found && got.value !== undefined ? (parseJson(got.value) as T) : undefined
	}
}

/**
 * A table's rows found again by their words, vectors or both, each hit's
 * value its row as `db.all` reads it: `db.searchIndex(name, options)`. The
 * table owns the rows, and every change of one reaches the index in the
 * commit that makes it.
 */
export class TableIndex<T> extends Searchable<T> {}

/** The open of a table's index: its name, its table and settings, and its database's handle. */
export function tableIndexOpen(
	name: string,
	options: TableIndexOptions,
	database?: number,
	reindex = false,
): Uint8Array {
	checkName(name, 'index')
	return SearchOpen.encode({
		name,
		fields: fieldsOf(options.fields),
		filters: [...(options.filters ?? [])],
		typoTolerance: options.typoTolerance,
		dimensions: options.vectors?.dimensions,
		recall: options.vectors?.recall,
		vectorWeight: options.hybrid?.vectorWeight,
		rerank: options.hybrid?.rerank,
		manual: options.rebuild === 'manual',
		reindex,
		database,
		table: options.table,
		key: options.key,
		vectorColumn: options.vectors?.column,
	})
}

function fieldsOf(fields: IndexOptions['fields']): { path: string; weight: number }[] {
	return Array.isArray(fields)
		? fields.map(path => ({ path, weight: 1 }))
		: Object.entries(fields ?? {}).map(([path, weight]) => ({ path, weight }))
}

/** The open of an index: its name, settings, and whether it reads the index again. */
export function indexOpen(name: string, options: IndexOptions = {}, reindex = false): Uint8Array {
	checkName(name, 'index')
	return SearchOpen.encode({
		name,
		fields: fieldsOf(options.fields),
		filters: [...(options.filters ?? [])],
		typoTolerance: options.typoTolerance,
		dimensions: options.vectors?.dimensions,
		recall: options.vectors?.recall,
		vectorWeight: options.hybrid?.vectorWeight,
		rerank: options.hybrid?.rerank,
		manual: options.rebuild === 'manual',
		reindex,
	})
}

/** Drops an index and every document it holds; says whether there was one. */
export async function dropIndex(link: Link, name: string): Promise<boolean> {
	checkName(name, 'index')
	const answer = await link.run('write', connection =>
		connection.session.call(methods['search.dropIndex'], SearchName.encode({ name })),
	)
	return SearchDropped.decode(answer).dropped ?? false
}

/** Every index the store holds, by name. */
export async function listIndexes(link: Link): Promise<IndexInfo[]> {
	const answer = await link.run('read', connection =>
		connection.session.call(methods['search.list'], Empty.encode({})),
	)
	return (SearchListed.decode(answer).indexes ?? []).map(index => {
		const info: IndexInfo = {
			name: index.name ?? '',
			documents: index.documents ?? 0,
			vectors: index.vectors ?? 0,
			status: statusOf(index),
		}
		if (index.opened !== undefined) {
			info.opened = new Date(index.opened)
		}
		return info
	})
}

function statusOf(status: {
	state?: string | undefined
	done?: number | undefined
	error?: string | undefined
}): IndexStatus {
	switch (status.state) {
		case 'rebuilding':
			return { state: 'rebuilding', done: status.done ?? 0 }
		case 'failed':
			return { state: 'failed', error: status.error ?? '' }
		default:
			return { state: 'ready' }
	}
}

function documentJson(name: string, value: unknown): string {
	const json = JSON.stringify(value)
	if (json === undefined) {
		throw new InvalidError(`index ${name}: a document that does not write as JSON`)
	}
	return json
}

/** What a set says of its vector: its bytes, none, or nothing. */
function vectorSaid(vector: Vector | null | undefined): {
	vector?: Uint8Array
	noVector?: boolean
} {
	if (vector === undefined) {
		return {}
	}
	return vector === null ? { noVector: true } : { vector: vectorBytes(vector) }
}

/** A vector's floats' bytes, little-endian, four a dimension. */
function vectorBytes(vector: Vector): Uint8Array {
	const floats = vector instanceof Float32Array ? vector : Float32Array.from(vector)
	const bytes = new Uint8Array(floats.length * 4)
	const view = new DataView(bytes.buffer)
	for (const [at, value] of floats.entries()) {
		view.setFloat32(at * 4, value, true)
	}
	return bytes
}

/**
 * A `where` as the wire takes it, a JSON list of conditions joined by and:
 * `{"eq":[path,value]}`, `{"in":[path,[values]]}`, `{"missing":path}`,
 * `{"range":[path,{"gt"|"gte"|"lt"|"lte":n}]}`, `{"or":[…]}`, `{"and":[…]}`.
 */
export function whereJson(where: Where): string {
	return `[${conditions(where).join(',')}]`
}

function conditions(where: Where): string[] {
	if (where instanceof Group) {
		const parts = where.parts.map(part => `{"and":[${conditions(part).join(',')}]}`)
		return [`{"${where.join}":[${parts.join(',')}]}`]
	}
	return Object.entries(where).map(([path, value]) => condition(path, value))
}

function condition(path: string, value: WhereValue | undefined): string {
	const at = JSON.stringify(path)
	if (value === undefined) {
		throw new InvalidError(
			`a where of ${path}: undefined; leave ${path} out to ask nothing of it, or null for no value`,
		)
	}
	if (value === null) {
		return `{"missing":${at}}`
	}
	if (Array.isArray(value)) {
		return `{"in":[${at},[${(value as readonly (string | number | bigint | boolean)[]).map(scalar).join(',')}]]}`
	}
	if (typeof value === 'object') {
		const bounds = Object.entries(value).map(([bound, n]) => {
			if (!['gt', 'gte', 'lt', 'lte'].includes(bound)) {
				throw new InvalidError(`a where of ${path}: ${bound}, not gt, gte, lt or lte`)
			}
			return `${JSON.stringify(bound)}:${scalar(n as number | bigint)}`
		})
		return `{"range":[${at},{${bounds.join(',')}}]}`
	}
	return `{"eq":[${at},${scalar(value)}]}`
}

/** A value as JSON, a bigint as its digits so that an id of 19 digits matches as it is. */
function scalar(value: string | number | bigint | boolean): string {
	if (typeof value === 'bigint') {
		return value.toString()
	}
	if (typeof value === 'number' && !Number.isFinite(value)) {
		throw new InvalidError(`a where's value ${value}, which is no number JSON writes`)
	}
	return JSON.stringify(value)
}

/** The conditions a `where` groups: `search.or({ category: 'boots' }, { tags: 'sale' })`. */
export const search = {
	or: (...parts: Where[]): Group => new Group('or', parts),
	and: (...parts: Where[]): Group => new Group('and', parts),
}
