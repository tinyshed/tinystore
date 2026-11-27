// Search, as docs/guides/search/README.md has it: documents of one type set by id in a
// named index and found again by their words, by vectors the program's own
// model gives them, or by both, kept to the documents a `where` passes. A
// search answers the exact best ten, or `limit`, of what the index holds; an
// index opened with `recall` answers its vectors from clusters. A table's
// index, `db.searchIndex`, finds a database's rows, each hit its row.

import type { Link } from './connection.ts'
import { errorOf, InvalidError, ProtocolError, type TinystoreError } from './errors.ts'
import { refuseUnknown } from './fields.ts'
import { checkName, handleOn, type Open, type Via, viaLink } from './handles.ts'
import { parseJson } from './json.ts'
import { keyText } from './kv.ts'
import { rowsOf, Sql } from './sql.ts'
import type { Key, SqlArg, SqlValue } from './wire/codec.ts'
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
	SearchManyFound,
	SearchManyQuery,
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
	/**
	 * the keyboard layouts the index's readers type with, `['ru', 'en']`: a
	 * word that finds nothing as typed is read as the same keys type in the
	 * other layout, when it finds something there, and `corrected` says so
	 */
	layouts?: readonly KeyboardLayout[] | undefined
	/**
	 * each short form, one word, with the phrase of 1 to 8 words it stands
	 * for, `{ ml: 'machine learning' }`: a search for the short form finds a
	 * document that holds the phrase, its words adjacent and in order, and a
	 * search for the phrase's words one that holds the short form; read when
	 * a document is set, so a search pays nothing; other synonyms read an
	 * index that holds documents again, and none given leaves it none
	 */
	synonyms?: Readonly<Record<string, string>> | undefined
	/** a vector the program's model gives each document, and `recall` for searches from clusters */
	vectors?: { dimensions: number; recall?: number | undefined } | undefined
	/** how a search by words and a vector at once combines them unless it says */
	hybrid?: { vectorWeight?: number | undefined; rerank?: boolean | undefined } | undefined
	/** whether a change that needs every document read again waits for `store.reindex` */
	rebuild?: 'automatic' | 'manual' | undefined
}

/** A keyboard layout an index's readers type with: ЙЦУКЕН, or QWERTY. */
export type KeyboardLayout = 'ru' | 'en'

/** A vector the program's own model gave, of the index's dimensions. */
export type Vector = readonly number[] | Float32Array

/**
 * A value a `where` matches: text, a number, a boolean, one of a list, a
 * range, `null` for none, or, for a table's index, one of the values a
 * statement of its database gives: ``sql`select user_id from members where course_id = ${id}` ``.
 */
export type WhereValue =
	| Sql
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
 * A search's where holds 8 conditions and 100 values in its lists at most.
 * A statement's rows, read in the search's snapshot, count toward no bound
 * on values: 100,000 at most, and on the table's key no filter is needed.
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
	/**
	 * the least cosine, from -1 to 1, a document's vector has with the
	 * search's for the vector to find it: a search by its vector alone leaves
	 * out the hits under it, and one by words too adds nothing for them to
	 * the sum; invalid beside a rerank
	 */
	minSimilarity?: number | undefined
	/**
	 * the last word read as the start of a word too, as a search box types:
	 * `crypt` finds `cryptography` as well, at half its field's weight; a
	 * word a space follows is whole, and so is one of a single letter. With
	 * `'every'`, every word of two letters or more is read so, the first
	 * eight, as people shorten each word of a name: `лин алг` finds
	 * `Линейная алгебра`; a document that holds a word for each word of the
	 * text comes first, its score raised above the rest
	 */
	prefix?: boolean | 'every' | undefined
	/**
	 * each hit says where the search's words matched it, in `matches`, as a
	 * search box shows them in bold: a word found as written, by its stem or
	 * once corrected whole, and a word a prefix began by the letters typed
	 */
	matches?: boolean | undefined
	/**
	 * each hit's `snippet` of `field`, a path the index reads: its own text
	 * from `around` characters before the field's first match to `around`
	 * after it, 1 to 1000 and 80 unless said, cut where a word starts and
	 * ends and `…` where text was cut; a hit that matched elsewhere, or by
	 * its vector alone, gets the field's first `2 × around` characters
	 */
	snippet?: { field: string; around?: number | undefined } | undefined
	/**
	 * counts the documents it finds as well, in `total`, under its `where`
	 * and whatever its `limit` and `offset`: exactly up to 10,000, and past
	 * it 10,000 with `atLeast`; it reads every posting of its words, which a
	 * search alone skips most of; invalid beside a vector
	 */
	total?: boolean | undefined
	/** 10 unless said; offset and limit together 1000 at most */
	limit?: number | undefined
	offset?: number | undefined
}

/**
 * Where a search's word matched a hit: `start` to `end` of a field's text in
 * UTF-16 units, so that `text.slice(start, end)` is what matched.
 */
export interface Match {
	/** the field's path, or a table's column; `''` for a document that is its own text */
	field: string
	/** the string's place in a field that holds a list */
	item?: number
	start: number
	end: number
	/** the query's word, as its text wrote it */
	word: string
}

/**
 * The piece of a field around a hit's first match there: the field's own
 * text, `…` where it was cut, and every match inside it as `start` to `end`
 * in UTF-16 units of `text`. The store adds no markup and escapes nothing.
 */
export interface Snippet {
	text: string
	marks: { start: number; end: number }[]
	/** the string's place in a field that holds a list */
	item?: number
}

/** A document a search found, and the score that ordered it among this search's hits. */
export interface Hit<T> {
	id: string
	value: T
	score: number
	/** where the search's words matched it, when the search asked with `matches` */
	matches?: Match[]
	/** the piece of a field around its first match, when the search asked with `snippet` and it holds the field */
	snippet?: Snippet
	/** the score its words gave it, as a search by its words alone scores it, where they found it among their best */
	textScore?: number
	/** the cosine of its vector with the search's, from -1 to 1, where both have one */
	similarity?: number
}

/** What a search found, the best first, and the text it read in fact when a word was misspelt. */
export interface Found<T> {
	hits: Hit<T>[]
	corrected?: string | undefined
	/** how many it finds, when the search asked with `total` */
	total?: Total | undefined
}

/** How many documents a search finds: exactly, or past 10,000, 10,000 and `atLeast`, never an estimate. */
export interface Total {
	count: number
	/** whether counting stopped at 10,000, more being there */
	atLeast: boolean
}

/**
 * Where an index stands: serving what it was opened with, read again beside it, or a rebuild that failed.
 * `error` beside `ready` and `rebuilding` is the last maintenance pass's failure while the passes keep failing.
 */
export type IndexStatus =
	| { state: 'ready'; error?: string | undefined }
	| { state: 'rebuilding'; done: number; error?: string | undefined }
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
	/**
	 * the column of unique values a row is found by, or the columns whose
	 * values are unique together, `['space_id', 'n']`: the table's primary
	 * key unless said; a hit's `id` is a key of several columns' values as a
	 * JSON array, `'[1,6]'`
	 */
	key?: string | readonly string[] | undefined
	/** the columns whose words are read, each with what a word found there weighs, or a list that weighs each 1 */
	fields: Record<string, number> | readonly string[]
	/** the columns a search may keep its hits to; a boolean is the 1 or 0 SQLite keeps */
	filters?: readonly string[] | undefined
	typoTolerance?: boolean | undefined
	layouts?: readonly KeyboardLayout[] | undefined
	synonyms?: Readonly<Record<string, string>> | undefined
	/** a column of blobs of little-endian floats, one a row or null, and `recall` for searches from clusters */
	vectors?: { column: string; dimensions: number; recall?: number | undefined } | undefined
	hybrid?: { vectorWeight?: number | undefined; rerank?: boolean | undefined } | undefined
	rebuild?: 'automatic' | 'manual' | undefined
}

/** What a set says of its document besides its value: its vector, or `null` for none, and its aliases. */
export interface IndexSetOptions {
	vector?: Vector | null | undefined
	/**
	 * each alias, one word, with the path of the field whose whole text it
	 * stands for in this document alone: `{ кмзи: 'name' }`; a set without
	 * them leaves the document none
	 */
	aliases?: Readonly<Record<string, string>> | undefined
}

/** A document `setMany` sets: its id, its value, its vector, or `null` for none, and its aliases. */
export interface IndexDocument<T> extends IndexSetOptions {
	id: Key
	value: T
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

	constructor(link: Link, via: Via, name: string, open: Open) {
		this.name = name
		this.#via = via
		this.#open = open
		reached.set(this, { link, open, via })
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
		const statements: SearchStatementOf[] = []
		const where = asked.where === undefined ? undefined : whereJson(asked.where, statements)
		const body = (handle: number) =>
			SearchQuery.encode({
				handle,
				text: asked.text ?? '',
				vector,
				vectorWeight: asked.hybrid?.vectorWeight,
				rerank: asked.hybrid?.rerank,
				minSimilarity: asked.minSimilarity,
				prefix: asked.prefix === true || undefined,
				// none unless asked, so that a server before it reads the search
				prefixEvery: asked.prefix === 'every' || undefined,
				// JavaScript slices a string by its UTF-16 units
				matches: asked.matches === true ? 'utf16' : undefined,
				snippet:
					asked.snippet === undefined
						? undefined
						: { field: asked.snippet.field, around: asked.snippet.around, unit: 'utf16' },
				where,
				statements,
				total: asked.total,
				offset: asked.offset,
				limit: asked.limit,
			})
		const found = SearchFound.decode(await this.call('search.search', body, 'read'))
		const hits = hitsOf<T>(found.hits ?? [], found.columns)
		const read: Found<T> =
			found.corrected === undefined ? { hits } : { hits, corrected: found.corrected }
		if (found.total !== undefined) {
			read.total = { count: Number(found.total.count ?? 0), atLeast: found.total.atLeast ?? false }
		}
		return read
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
			await new Promise(resolve => setTimeout(resolve, readyEvery))
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
		super(link, viaLink(link), name, open)
	}

	/**
	 * Sets the document `id` to `value`, replacing it whole; the next search
	 * sees it. On an index with vectors a set gives its vector, or `null` for
	 * a document only its words find. Its aliases find it as a match of the
	 * field each stands for, and mark that field's text.
	 */
	async set(id: Key, value: T, options?: IndexSetOptions): Promise<void> {
		const said = { ...vectorSaid(options?.vector), ...aliasesSaid(options?.aliases) }
		const json = documentJson(this.name, value)
		await this.call(
			'search.set',
			handle => SearchSet.encode({ handle, id: keyText(id), value: json, ...said }),
			'write',
		)
	}

	/**
	 * Sets every document in one commit, all of them or none: 1,000 at most,
	 * `limits.setManyDocuments`, and no more than one request body holds,
	 * 2 MiB, `limits.requestBytes`. Past either it fails with a LimitError
	 * that names the bound.
	 */
	async setMany(documents: readonly IndexDocument<T>[]): Promise<void> {
		const encoded = documents.map(document => ({
			id: keyText(document.id),
			value: documentJson(this.name, document.value),
			...vectorSaid(document.vector),
			...aliasesSaid(document.aliases),
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

/** Each index's link, open and via, which a search among several opens on one connection. */
const reached = new WeakMap<object, { link: Link; open: Open; via: Via }>()

/** An index a search among several asks, and its search as a search of it alone says it. */
export interface Among {
	index: SearchIndex<unknown> | TableIndex<unknown>
	/** what its hits weigh in `merged`, a positive number, 1 unless said */
	boost?: number | undefined
	where?: Where | undefined
	/** 10 unless said, offset and limit together 1000 at most; `more` says whether the index has more */
	limit?: number | undefined
	/** the hits passed before the first answered: the next page is `offset + limit` */
	offset?: number | undefined
	prefix?: boolean | 'every' | undefined
	matches?: boolean | undefined
	snippet?: { field: string; around?: number | undefined } | undefined
}

/** The fields an `Among` takes, so that one it does not, as a typo, is refused rather than dropped. */
const amongFields = ['index', 'boost', 'where', 'limit', 'offset', 'prefix', 'matches', 'snippet']

/** One question of several indexes: the text every one reads, and 1 to 32 indexes in order. */
export interface ManyQuery {
	text: string
	in: readonly Among[]
}

/** An index's hits, the best by its own scores first and whether it has more, or why its search failed. */
export type ManyGroup =
	| { index: string; hits: Hit<unknown>[]; more: boolean; corrected?: string | undefined }
	| { index: string; error: TinystoreError }

/** A hit of `merged`: its group's index, the group's place among the groups, and the hit its group holds. */
export interface Merged {
	index: string
	group: number
	hit: Hit<unknown>
}

/** What a search among several indexes found: a group an index, in the order asked, and every hit in one order. */
export interface ManyFound {
	groups: ManyGroup[]
	/**
	 * every group's hits by reciprocal rank fusion, never by score: the hit
	 * at rank `r` of its group, from 1, weighs `boost / (60 + r)`; of equal
	 * weights the group asked first comes first, then the higher rank
	 */
	merged: Merged[]
}

/**
 * Asks `text` of every index of `query.in` in one call over one connection,
 * each index's handle opened there first: an index of another store is
 * refused.
 */
export async function searchMany(link: Link, query: ManyQuery): Promise<ManyFound> {
	refuseUnknown('a search among indexes', query, ['text', 'in'])
	const asked = query.in.map(among => amongOf(link, among))
	const answer = await link.run('read', async connection => {
		const handles = await Promise.all(
			asked.map(({ open }) => handleOn(connection, methods['search.open'], open)),
		)
		const among = asked.map(({ body }, at) => body(handles[at] ?? 0))
		return connection.session.call(
			methods['search.searchMany'],
			SearchManyQuery.encode({ text: query.text, among }),
		)
	})
	const found = SearchManyFound.decode(answer)
	const groups = (found.groups ?? []).map(groupOf)
	return { groups, merged: mergedOf(groups, found.merged ?? []) }
}

/**
 * Each hit `merged` names, the one its group holds; a hit the groups do not
 * hold is the server's fault, and breaks the protocol.
 */
export function mergedOf(
	groups: ManyGroup[],
	named: NonNullable<ReturnType<typeof SearchManyFound.decode>['merged']>,
): Merged[] {
	return named.map(({ group, hit }) => {
		const at = Number(group ?? 0)
		const held = groups[at]
		if (held === undefined || 'error' in held) {
			throw new ProtocolError(
				`a search among indexes: merged names group ${at}, which holds no hit`,
			)
		}
		const kept = held.hits[Number(hit ?? 0)]
		if (kept === undefined) {
			throw new ProtocolError(
				`a search among indexes: merged names hit ${hit} of group ${at}, past its hits`,
			)
		}
		return { index: held.index, group: at, hit: kept }
	})
}

/** An index a search among several asks: its open, and its part of the request once its handle is known. */
function amongOf(link: Link, among: Among) {
	refuseUnknown('a search among indexes', among, amongFields)
	const reach = reached.get(among.index)
	if (reach === undefined) {
		throw new InvalidError(
			'a search among indexes: each `index` is one `store.index` or `db.searchIndex` opened',
		)
	}
	if (reach.link !== link) {
		throw new InvalidError(
			`index ${among.index.name}: an index of another store, in a search among this one's`,
		)
	}
	reach.via.refuse?.()
	const statements: SearchStatementOf[] = []
	const where = among.where === undefined ? undefined : whereJson(among.where, statements)
	const body = (handle: number) => ({
		query: {
			handle,
			where,
			statements,
			offset: among.offset,
			limit: among.limit,
			prefix: among.prefix === true || undefined,
			prefixEvery: among.prefix === 'every' || undefined,
			matches: among.matches === true ? 'utf16' : undefined,
			snippet:
				among.snippet === undefined
					? undefined
					: { field: among.snippet.field, around: among.snippet.around, unit: 'utf16' },
		},
		boost: among.boost,
	})
	return { open: reach.open, body }
}

function groupOf(group: {
	index?: string | undefined
	hits?: Parameters<typeof hitsOf>[0] | undefined
	corrected?: string | undefined
	columns?: string[] | undefined
	more?: boolean | undefined
	failure?:
		| {
				code?: string | undefined
				message?: string | undefined
				what?: Record<string, string> | undefined
		  }
		| undefined
}): ManyGroup {
	const index = group.index ?? ''
	if (group.failure !== undefined) {
		const { code, message, what } = group.failure
		return { index, error: errorOf(code ?? 'internal', message ?? '', what ?? {}) }
	}
	const hits = hitsOf<unknown>(group.hits ?? [], group.columns)
	const found: ManyGroup = { index, hits, more: group.more ?? false }
	return group.corrected === undefined ? found : { ...found, corrected: group.corrected }
}

/** A search's hits as the wire sends them: a document's JSON, or a table's row by the answer's columns. */
function hitsOf<T>(
	hits: {
		id?: string | undefined
		value?: string | undefined
		score?: number | undefined
		row?: unknown[] | undefined
		matches?: Parameters<typeof matchOf>[0][] | undefined
		snippet?: Parameters<typeof snippetOf>[0] | undefined
		textScore?: number | undefined
		similarity?: number | undefined
	}[],
	columns: string[] | undefined,
): Hit<T>[] {
	return hits.map(hit => {
		const value = (
			columns === undefined
				? parseJson(hit.value ?? 'null')
				: rowsOf<T>(columns, [(hit.row ?? []) as SqlValue[]])[0]
		) as T
		const read: Hit<T> = { id: hit.id ?? '', value, score: hit.score ?? 0 }
		if (hit.matches !== undefined) {
			read.matches = hit.matches.map(matchOf)
		}
		if (hit.snippet !== undefined) {
			read.snippet = snippetOf(hit.snippet)
		}
		if (hit.textScore !== undefined) {
			read.textScore = hit.textScore
		}
		if (hit.similarity !== undefined) {
			read.similarity = hit.similarity
		}
		return read
	})
}

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
		layouts: layoutsOf(options.layouts),
		synonyms: { ...options.synonyms },
		dimensions: options.vectors?.dimensions,
		recall: options.vectors?.recall,
		vectorWeight: options.hybrid?.vectorWeight,
		rerank: options.hybrid?.rerank,
		manual: options.rebuild === 'manual',
		reindex,
		database,
		table: options.table,
		...keyOf(options.key),
		vectorColumn: options.vectors?.column,
	})
}

/** A table's key as the open sends it: one column as `key`, several as `keys`, which a server before them ignores. */
function keyOf(key: TableIndexOptions['key']): {
	key?: string | undefined
	keys?: string[] | undefined
} {
	if (typeof key === 'string' || key === undefined) {
		return { key }
	}
	return key.length === 1 ? { key: key[0] } : { keys: [...key] }
}

function fieldsOf(fields: IndexOptions['fields']): { path: string; weight: number }[] {
	return Array.isArray(fields)
		? fields.map(path => ({ path, weight: 1 }))
		: Object.entries(fields ?? {}).map(([path, weight]) => ({ path, weight }))
}

/** The layouts asked for; none left out, so that a server before them reads the open. */
function layoutsOf(layouts: readonly KeyboardLayout[] | undefined): KeyboardLayout[] | undefined {
	return layouts?.length ? [...layouts] : undefined
}

/** The open of an index: its name, settings, and whether it reads the index again. */
export function indexOpen(name: string, options: IndexOptions = {}, reindex = false): Uint8Array {
	checkName(name, 'index')
	return SearchOpen.encode({
		name,
		fields: fieldsOf(options.fields),
		filters: [...(options.filters ?? [])],
		typoTolerance: options.typoTolerance,
		layouts: layoutsOf(options.layouts),
		synonyms: { ...options.synonyms },
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
	// an empty error is none: the wire's field is a string
	const error = status.error ? { error: status.error } : {}
	switch (status.state) {
		case 'rebuilding':
			return { state: 'rebuilding', done: status.done ?? 0, ...error }
		case 'failed':
			return { state: 'failed', error: status.error ?? '' }
		default:
			return { state: 'ready', ...error }
	}
}

function matchOf(found: {
	field?: string | undefined
	item?: number | undefined
	start?: number | undefined
	end?: number | undefined
	word?: string | undefined
}): Match {
	const match: Match = {
		field: found.field ?? '',
		start: found.start ?? 0,
		end: found.end ?? 0,
		word: found.word ?? '',
	}
	if (found.item !== undefined) {
		match.item = found.item
	}
	return match
}

function snippetOf(cut: {
	text?: string | undefined
	marks?: { start?: number | undefined; end?: number | undefined }[] | undefined
	item?: number | undefined
}): Snippet {
	const marks = (cut.marks ?? []).map(mark => ({ start: mark.start ?? 0, end: mark.end ?? 0 }))
	const snippet: Snippet = { text: cut.text ?? '', marks }
	if (cut.item !== undefined) {
		snippet.item = cut.item
	}
	return snippet
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

/** A set's aliases as the wire takes them: none left out. */
function aliasesSaid(aliases: Readonly<Record<string, string>> | undefined): {
	aliases?: Record<string, string>
} {
	return aliases === undefined || Object.keys(aliases).length === 0
		? {}
		: { aliases: { ...aliases } }
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

/** A statement of a `where`, as the query carries it beside the conditions. */
type SearchStatementOf = { text: string; values: SqlArg[] }

/**
 * A `where` as the wire takes it, a JSON list of conditions joined by and:
 * `{"eq":[path,value]}`, `{"in":[path,[values]]}`, `{"missing":path}`,
 * `{"range":[path,{"gt"|"gte"|"lt"|"lte":n}]}`, `{"or":[…]}`, `{"and":[…]}`;
 * a statement is `{"in":[path,{"statement":n}]}`, the `n`th of `statements`.
 */
export function whereJson(where: Where, statements: SearchStatementOf[] = []): string {
	return `[${conditions(where, statements).join(',')}]`
}

function conditions(where: Where, statements: SearchStatementOf[]): string[] {
	if (where instanceof Group) {
		const parts = where.parts.map(part => `{"and":[${conditions(part, statements).join(',')}]}`)
		return [`{"${where.join}":[${parts.join(',')}]}`]
	}
	return Object.entries(where).map(([path, value]) => condition(path, value, statements))
}

function condition(
	path: string,
	value: WhereValue | undefined,
	statements: SearchStatementOf[],
): string {
	const at = JSON.stringify(path)
	if (value instanceof Sql) {
		statements.push({ text: value.text, values: [...value.values] })
		return `{"in":[${at},{"statement":${statements.length - 1}}]}`
	}
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
