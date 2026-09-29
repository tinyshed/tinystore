// A message is a map from small integer keys to values; each field's codec
// says how its value is written and read. A message's fields are declared by
// the names docs/wire.md gives them, in camelCase, so that the tests read
// every vector of messages.json through them without a table.

import { InvalidError } from '../errors.ts'
import { Reader, Writer } from './msgpack.ts'

/**
 * How a field's value is written and read. In is what a caller may give, Out
 * what a read returns: a key given as a number reads back as its decimal text.
 */
export interface Codec<Out, In = Out> {
	/** what the value is, which the tests that read vectors go by */
	readonly kind: string
	/** a list's items, or what a nullable holds when it holds something */
	readonly item?: Codec<unknown, never>
	write(w: Writer, value: In): void
	read(r: Reader): Out
}

export const uint: Codec<number> = {
	kind: 'uint',
	write: (w, v) => w.uint(v),
	read: r => r.uint(),
}

/** A signed integer a number holds exactly: unix milliseconds, a count. */
export const int: Codec<number> = {
	kind: 'int',
	write: (w, v) => w.int(v),
	read: r => r.int(),
}

/** A signed 64-bit integer, read as a bigint: a record's nanoseconds. */
export const int64: Codec<bigint, bigint | number> = {
	kind: 'int',
	write: (w, v) => w.int(v),
	read: r => r.int64(),
}

export const float: Codec<number> = {
	kind: 'float',
	write: (w, v) => w.float(v),
	read: r => r.float(),
}

export const bool: Codec<boolean> = {
	kind: 'bool',
	write: (w, v) => w.bool(v),
	read: r => r.bool(),
}

export const str: Codec<string> = {
	kind: 'str',
	write: (w, v) => w.str(v),
	read: r => r.str(),
}

/** Bytes, read as a copy that outlives the frame they came in. */
export const bin: Codec<Uint8Array> = {
	kind: 'bin',
	write: (w, v) => w.bin(v),
	read: r => r.bin().slice(),
}

const utf8 = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true })

/** The text bytes spell, or undefined when they are not UTF-8. */
export function textOf(bytes: Uint8Array): string | undefined {
	try {
		return utf8.decode(bytes)
	} catch {
		return undefined
	}
}

/**
 * Text: a str, or a bin whose bytes are text that is not UTF-8, as another
 * program's output may be. Bytes that are UTF-8 travel as the str they spell.
 */
export const text: Codec<string | Uint8Array> = {
	kind: 'text',
	write: (w, v) => writeText(w, v),
	read: r => (r.type() === 'bin' ? r.bin().slice() : r.str()),
}

function writeText(w: Writer, v: string | Uint8Array): void {
	if (typeof v === 'string') {
		w.str(v)
		return
	}
	const spelled = textOf(v)
	if (spelled === undefined) {
		w.bin(v)
		return
	}
	w.str(spelled)
}

/** A kv key's text: a str, a bin, or an integer, which is its decimal spelling. */
export type Key = string | Uint8Array | number | bigint

export const key: Codec<string | Uint8Array, Key> = {
	kind: 'key',
	write: (w, v) => {
		if (typeof v === 'number' || typeof v === 'bigint') {
			if (typeof v === 'number' && !Number.isSafeInteger(v)) {
				throw new InvalidError(`the key ${v} is not an integer; a key is text or an integer`)
			}
			w.str(String(v))
			return
		}
		writeText(w, v)
	},
	read: r => {
		switch (r.type()) {
			case 'bin':
				return r.bin().slice()
			case 'int':
				return String(r.int64())
		}
		return r.str()
	},
}

/** A kv value as its row keeps it: nothing, an integer or bytes. */
export type Raw = null | bigint | Uint8Array

export const kvValue: Codec<Raw, Raw | number> = {
	kind: 'kv value',
	write: (w, v) => {
		if (v === null) {
			w.nil()
		} else if (v instanceof Uint8Array) {
			w.bin(v)
		} else {
			w.int(v)
		}
	},
	read: r => {
		if (r.nil()) {
			return null
		}
		return r.type() === 'bin' ? r.bin().slice() : r.int64()
	},
}

/** A value SQLite returns: NULL, an integer, a float, text or bytes. */
export type SqlValue = null | number | bigint | string | Uint8Array

/**
 * A value an SQL statement takes: SQLite's five, a boolean, which it keeps as
 * 1 or 0, and a Date, which sqldb keeps as its unix milliseconds.
 */
export type SqlArg = SqlValue | boolean | Date | undefined

export const sqlValue: Codec<SqlValue | boolean, SqlArg> = {
	kind: 'sql value',
	write: (w, v) => writeSQL(w, v),
	read: r => {
		switch (r.type()) {
			case 'nil':
				r.nil()
				return null
			case 'bool':
				return r.bool()
			case 'int': {
				const n = r.int64()
				return n >= Number.MIN_SAFE_INTEGER && n <= Number.MAX_SAFE_INTEGER ? Number(n) : n
			}
			case 'float':
				return r.float()
			case 'bin':
				return r.bin().slice()
		}
		return r.str()
	},
}

function writeSQL(w: Writer, v: SqlArg): void {
	if (v === null || v === undefined) {
		w.nil()
	} else if (typeof v === 'boolean') {
		w.bool(v)
	} else if (typeof v === 'bigint') {
		w.int(v)
	} else if (typeof v === 'number') {
		if (Number.isNaN(v)) {
			throw new InvalidError('NaN as an SQL argument, which SQLite would keep as NULL')
		}
		if (Number.isInteger(v) && !Object.is(v, -0)) {
			w.int(v)
		} else {
			w.float(v)
		}
	} else if (typeof v === 'string') {
		w.str(v)
	} else if (v instanceof Date) {
		const ms = v.getTime()
		if (Number.isNaN(ms)) {
			throw new InvalidError('an invalid Date as an SQL argument')
		}
		w.int(ms)
	} else {
		w.bin(v)
	}
}

/** Names mapped to text, in the byte order of their UTF-8, which is their code points' order. */
export function names<V, In = V>(
	value: Codec<V, In>,
): Codec<Record<string, V>, Record<string, In>> {
	return {
		kind: value.kind === 'sql value' ? 'sql names' : 'names',
		write: (w, v) => {
			const sorted = Object.keys(v).sort(byCodePoint)
			w.map(sorted.length)
			for (const name of sorted) {
				w.str(name)
				value.write(w, v[name] as In)
			}
		},
		read: r => {
			const named: Record<string, V> = {}
			r.names(name => {
				named[name] = value.read(r)
			})
			return named
		},
	}
}

/** Orders strings as their UTF-8 bytes are ordered, which UTF-16's code units do not. */
export function byCodePoint(a: string, b: string): number {
	const x = a[Symbol.iterator]()
	const y = b[Symbol.iterator]()
	for (;;) {
		const p = x.next()
		const q = y.next()
		if (p.done || q.done) {
			return (p.done ? 0 : 1) - (q.done ? 0 : 1)
		}
		const d = p.value.codePointAt(0)! - q.value.codePointAt(0)!
		if (d !== 0) {
			return d
		}
	}
}

// a column's values are little-endian, as typed arrays hold them on every
// platform this SDK runs on; one that holds them otherwise copies value by value
const littleEndian = new Uint8Array(new Uint16Array([1]).buffer)[0] === 1

/** A column of int64s as a bin, eight bytes little-endian each. */
export const ints: Codec<BigInt64Array, BigInt64Array | readonly (number | bigint)[]> = {
	kind: 'ints',
	write: (w, v) => {
		const column = v instanceof BigInt64Array ? v : BigInt64Array.from(v, n => BigInt(n))
		w.bin(bytesOf(column))
	},
	read: r => {
		const bytes = column(r)
		const values = new BigInt64Array(bytes.length / 8)
		if (littleEndian) {
			new Uint8Array(values.buffer).set(bytes)
		} else {
			const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.length)
			values.forEach((_, i) => {
				values[i] = view.getBigInt64(8 * i, true)
			})
		}
		return values
	},
}

/**
 * A column of float64s as a bin, eight bytes of bits little-endian each: the
 * bytes are copied rather than the numbers, so a NaN's payload survives.
 */
export const floats: Codec<Float64Array, Float64Array | readonly number[]> = {
	kind: 'floats',
	write: (w, v) => {
		w.bin(bytesOf(v instanceof Float64Array ? v : Float64Array.from(v)))
	},
	read: r => {
		const bytes = column(r)
		const values = new Float64Array(bytes.length / 8)
		if (littleEndian) {
			new Uint8Array(values.buffer).set(bytes)
		} else {
			const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.length)
			values.forEach((_, i) => {
				values[i] = view.getFloat64(8 * i, true)
			})
		}
		return values
	},
}

function bytesOf(column: BigInt64Array | Float64Array): Uint8Array {
	if (littleEndian) {
		return new Uint8Array(column.buffer, column.byteOffset, column.byteLength)
	}
	const bytes = new Uint8Array(column.length * 8)
	const view = new DataView(bytes.buffer)
	column.forEach((n, i) => {
		if (typeof n === 'bigint') {
			view.setBigInt64(8 * i, n, true)
		} else {
			view.setFloat64(8 * i, n, true)
		}
	})
	return bytes
}

function column(r: Reader): Uint8Array {
	const bytes = r.bin()
	if (bytes.length % 8 !== 0) {
		throw new InvalidError(
			`a column of ${bytes.length} bytes, which holds no whole number of values`,
		)
	}
	return bytes
}

export function list<Out, In>(item: Codec<Out, In>): Codec<Out[], readonly In[]> {
	return {
		kind: `[]${item.kind}`,
		item,
		write: (w, v) => {
			w.array(v.length)
			for (const each of v) {
				item.write(w, each)
			}
		},
		read: r => {
			const items: Out[] = []
			r.items(() => {
				items.push(item.read(r))
			})
			return items
		},
	}
}

/** A value that may be nil in its place, as a settlement's error. */
export function nullable<Out, In>(value: Codec<Out, In>): Codec<Out | null, In | null> {
	return {
		kind: `${value.kind}?`,
		item: value,
		write: (w, v) => (v === null ? w.nil() : value.write(w, v)),
		read: r => (r.nil() ? null : value.read(r)),
	}
}

type Fields = Record<string, readonly [key: number, codec: Codec<unknown, never>]>

type OutOf<C> = C extends Codec<infer O, never> ? O : never
type InOf<C> = C extends Codec<unknown, infer I> ? I : never

/** What a message reads: every field it holds, a field it lacks undefined. */
export type Read<F extends Fields> = { [K in keyof F]?: OutOf<F[K][1]> }

/** What a message is written from: a field undefined is left out. */
export type Written<F extends Fields> = { [K in keyof F]?: InOf<F[K][1]> | undefined }

/**
 * A message: a map of the fields its declaration names, keys ascending. A
 * field is written when its value is not undefined, so an absent field and a
 * zero one stay what the caller said; a key the declaration lacks is skipped
 * as it reads, well formed and within bounds.
 */
export class Message<F extends Fields> implements Codec<Read<F>, Written<F>> {
	readonly kind = 'message'
	readonly name: string
	readonly fields: F
	readonly #order: { name: string; key: number; codec: Codec<unknown, unknown> }[]
	readonly #byKey: ({ name: string; codec: Codec<unknown, unknown> } | undefined)[] = []

	constructor(name: string, fields: F) {
		this.name = name
		this.fields = fields
		this.#order = Object.entries(fields)
			.map(([field, [key, codec]]) => ({
				name: field,
				key,
				codec: codec as Codec<unknown, unknown>,
			}))
			.sort((a, b) => a.key - b.key)
		for (const f of this.#order) {
			this.#byKey[f.key] = f
		}
	}

	write(w: Writer, value: Written<F>): void {
		const held = value as Record<string, unknown>
		let n = 0
		for (const f of this.#order) {
			if (held[f.name] !== undefined) {
				n++
			}
		}
		w.map(n)
		for (const f of this.#order) {
			const v = held[f.name]
			if (v !== undefined) {
				w.uint(f.key)
				f.codec.write(w, v)
			}
		}
	}

	/** The message's bytes. */
	encode(value: Written<F>): Uint8Array {
		const w = new Writer()
		this.write(w, value)
		return w.bytes()
	}

	/** Reads a body that is this message and nothing after it. */
	decode(body: Uint8Array): Read<F> {
		const r = new Reader(body)
		const value = this.read(r)
		r.end()
		return value
	}

	read(r: Reader): Read<F> {
		const read: Record<string, unknown> = {}
		r.fields(key => {
			const f = this.#byKey[key]
			if (f === undefined) {
				return false
			}
			read[f.name] = f.codec.read(r)
			return true
		})
		return read as Read<F>
	}
}

export function message<const F extends Fields>(name: string, fields: F): Message<F> {
	return new Message(name, fields)
}
