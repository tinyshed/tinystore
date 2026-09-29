// The bytes every SDK is tested against: server/wire/testdata/vectors.json and
// messages.json, read and written through this SDK's own codec and messages.

import { describe, expect, test } from 'bun:test'
import { createHmac } from 'node:crypto'

import { codes } from '../src/errors.ts'
import type { Codec, Message } from '../src/wire/codec.ts'
import { checkHeader, frame, headerSize, parseHeader } from '../src/wire/frame.ts'
import { messages, methods } from '../src/wire/messages.ts'
import { Reader, Writer } from '../src/wire/msgpack.ts'

const testdata = `${import.meta.dir}/../../../server/wire/testdata`

type Notation =
	| null
	| boolean
	| { uint: string }
	| { int: string }
	| { float: string }
	| { str: string }
	| { bin: string }
	| { array: Notation[] }
	| { map: [Notation, Notation][] }
	| { ints: string[] }
	| { floats: string[] }
	| { fields: Record<string, Notation> }

interface Vectors {
	values: { name: string; hex: string; value: Notation; canonical?: string }[]
	refused: { name: string; hex: string; as: string }[]
	frames: {
		name: string
		hex: string
		header: { length: number; kind: number; flags: number; method: number; stream: number }
		body?: Notation
		raw?: string
	}[]
	'refused frames': { name: string; hex: string; 'max body'?: number }[]
	proofs: { name: string; secret: string; challenge: string; proof: string }[]
}

interface MessageVectors {
	methods: Record<string, number>
	codes: string[]
	messages: { name: string; message: string; hex: string; fields: Record<string, Notation> }[]
}

const vectors: Vectors = await Bun.file(`${testdata}/vectors.json`).json()
const messageVectors: MessageVectors = await Bun.file(`${testdata}/messages.json`).json()

const unhex = (s: string) => new Uint8Array(Buffer.from(s, 'hex'))
const hex = (b: Uint8Array) => Buffer.from(b).toString('hex')

/** Reads a value as the notation it is expected in says, and returns it in that notation. */
function read(r: Reader, want: Notation): Notation {
	if (want === null) {
		if (!r.nil()) {
			throw new Error('not nil')
		}
		return null
	}
	if (typeof want === 'boolean') {
		return r.bool()
	}
	if ('uint' in want) {
		return { uint: String(r.uint64()) }
	}
	if ('int' in want) {
		return { int: String(r.int64()) }
	}
	if ('float' in want) {
		return { float: bitsOf(r.float()) }
	}
	if ('str' in want) {
		return { str: r.str() }
	}
	if ('bin' in want) {
		return { bin: hex(r.bin()) }
	}
	if ('array' in want) {
		const items: Notation[] = []
		r.items(i => {
			items.push(read(r, want.array[i] ?? null))
		})
		return { array: items }
	}
	if ('map' in want) {
		const pairs: [Notation, Notation][] = []
		const value = (i: number) => read(r, want.map[i]?.[1] ?? null)
		const first = want.map[0]?.[0]
		if (first !== undefined && first !== null && typeof first === 'object' && 'str' in first) {
			r.names(name => {
				pairs.push([{ str: name }, value(pairs.length)])
			})
		} else {
			r.fields(key => {
				pairs.push([{ uint: String(key) }, value(pairs.length)])
				return true
			})
		}
		return { map: pairs }
	}
	throw new Error(`a vector of ${JSON.stringify(want)}`)
}

/** Writes a value of the notation canonically: a map's keys ascending. */
function write(w: Writer, value: Notation): void {
	if (value === null) {
		w.nil()
	} else if (typeof value === 'boolean') {
		w.bool(value)
	} else if ('uint' in value) {
		w.uint(BigInt(value.uint))
	} else if ('int' in value) {
		w.int(BigInt(value.int))
	} else if ('float' in value) {
		w.float(floatOf(value.float))
	} else if ('str' in value) {
		w.str(value.str)
	} else if ('bin' in value) {
		w.bin(unhex(value.bin))
	} else if ('array' in value) {
		w.array(value.array.length)
		for (const item of value.array) {
			write(w, item)
		}
	} else if ('map' in value) {
		const pairs = [...value.map].sort(([a], [b]) => compareKeys(a, b))
		w.map(pairs.length)
		for (const [k, v] of pairs) {
			write(w, k)
			write(w, v)
		}
	} else {
		throw new Error(`a vector of ${JSON.stringify(value)}`)
	}
}

function compareKeys(a: Notation, b: Notation): number {
	if (
		a !== null &&
		typeof a === 'object' &&
		'uint' in a &&
		b !== null &&
		typeof b === 'object' &&
		'uint' in b
	) {
		return Number(BigInt(a.uint) - BigInt(b.uint))
	}
	const x = (a as { str: string }).str
	const y = (b as { str: string }).str
	return x < y ? -1 : x > y ? 1 : 0
}

function bitsOf(f: number): string {
	const view = new DataView(new ArrayBuffer(8))
	view.setFloat64(0, f)
	return view.getBigUint64(0).toString(16).padStart(16, '0')
}

function floatOf(bits: string): number {
	const view = new DataView(new ArrayBuffer(8))
	view.setBigUint64(0, BigInt(`0x${bits}`))
	return view.getFloat64(0)
}

function readAs(r: Reader, as: string): void {
	switch (as) {
		case 'any':
			r.skip()
			return
		case 'uint':
			r.uint64()
			return
		case 'int':
			r.int64()
			return
		case 'float':
			r.float()
			return
		case 'str':
			r.str()
			return
		case 'bin':
			r.bin()
			return
		case 'map':
			r.fields(() => false)
			return
	}
	throw new Error(`a refused vector read as ${as}`)
}

/**
 * A NaN whose payload is not the one JavaScript makes: a number keeps no NaN's
 * payload, which a float's bits in a bin or a column carry instead.
 */
function isOddNaN(value: Notation): boolean {
	if (value === null || typeof value !== 'object' || !('float' in value)) {
		return false
	}
	return Number.isNaN(floatOf(value.float)) && value.float !== bitsOf(Number.NaN)
}

describe('values', () => {
	for (const v of vectors.values) {
		test(v.name, () => {
			const r = new Reader(unhex(v.hex))
			if (isOddNaN(v.value)) {
				expect(r.float()).toBeNaN()
				r.end()
				return
			}
			expect(read(r, v.value)).toEqual(v.value)
			r.end()
			const w = new Writer()
			write(w, v.value)
			expect(hex(w.bytes())).toBe(v.canonical ?? v.hex)
		})
	}
})

describe('refused', () => {
	for (const v of vectors.refused) {
		test(v.name, () => {
			expect(() => {
				const r = new Reader(unhex(v.hex))
				readAs(r, v.as)
				r.end()
			}).toThrow(/invalid message/)
		})
	}
})

describe('frames', () => {
	for (const v of vectors.frames) {
		test(v.name, () => {
			const bytes = unhex(v.hex)
			const h = parseHeader(bytes)
			checkHeader(h, 1 << 20)
			expect(h).toEqual(v.header)
			const body = bytes.subarray(headerSize, headerSize + h.length)
			if (v.raw !== undefined) {
				expect(hex(body)).toBe(v.raw)
				return
			}
			const r = new Reader(body)
			expect(read(r, v.body ?? null)).toEqual(v.body ?? null)
			r.end()
			const w = new Writer()
			write(w, v.body ?? null)
			expect(hex(frame(h.kind, h.flags, h.method, h.stream, w.bytes()))).toBe(v.hex)
		})
	}
})

describe('refused frames', () => {
	for (const v of vectors['refused frames']) {
		test(v.name, () => {
			expect(() => checkHeader(parseHeader(unhex(v.hex)), v['max body'] ?? 1 << 20)).toThrow()
		})
	}
})

describe('proofs', () => {
	for (const v of vectors.proofs) {
		test(v.name, () => {
			const proof = createHmac('sha256', unhex(v.secret)).update(unhex(v.challenge)).digest()
			expect(hex(proof)).toBe(v.proof)
		})
	}
})

const camel = (name: string) => name.replace(/ (\w)/g, (_, c: string) => c.toUpperCase())

type AnyCodec = Codec<unknown, unknown> & { item?: AnyCodec }

/** A value a codec read, in the notation messages.json spells it in. */
function noted(codec: AnyCodec, value: unknown): Notation {
	if (codec instanceof Object && 'fields' in codec && codec.kind === 'message') {
		const m = codec as unknown as Message<Record<string, [number, AnyCodec]>>
		const fields: Record<string, Notation> = {}
		for (const [name, [, c]] of Object.entries(m.fields)) {
			const v = (value as Record<string, unknown>)[name]
			if (v !== undefined) {
				fields[name] = noted(c, v)
			}
		}
		return { fields }
	}
	const kind = codec.kind
	if (kind.startsWith('[]')) {
		return { array: (value as unknown[]).map(v => noted(inner(codec), v)) }
	}
	if (kind.endsWith('?')) {
		return value === null ? null : noted(inner(codec), value)
	}
	return notedPlain(kind, value)
}

function notedPlain(kind: string, value: unknown): Notation {
	switch (kind) {
		case 'uint':
			return { uint: String(value) }
		case 'int':
			return { int: String(value) }
		case 'float':
			return { float: bitsOf(value as number) }
		case 'bool':
			return value as boolean
		case 'str':
			return { str: value as string }
		case 'bin':
			return { bin: hex(value as Uint8Array) }
		case 'text':
		case 'key':
			return typeof value === 'string' ? { str: value } : { bin: hex(value as Uint8Array) }
		case 'kv value':
		case 'sql value':
			return notedValue(value)
		case 'names':
		case 'sql names':
			return {
				map: Object.entries(value as Record<string, unknown>).map(([name, v]) => [
					{ str: name },
					kind === 'names' ? notedPlain('text', v) : notedValue(v),
				]),
			}
		case 'ints':
			return { ints: [...(value as BigInt64Array)].map(String) }
		case 'floats': {
			const column = value as Float64Array
			const view = new DataView(column.buffer, column.byteOffset, column.byteLength)
			return {
				floats: Array.from({ length: column.length }, (_, i) =>
					view
						.getBigUint64(8 * i, true)
						.toString(16)
						.padStart(16, '0'),
				),
			}
		}
	}
	throw new Error(`a field of kind ${kind}`)
}

function notedValue(value: unknown): Notation {
	if (value === null || typeof value === 'boolean') {
		return value
	}
	if (typeof value === 'bigint') {
		return { int: String(value) }
	}
	if (typeof value === 'number') {
		return Number.isInteger(value) ? { int: String(value) } : { float: bitsOf(value) }
	}
	if (typeof value === 'string') {
		return { str: value }
	}
	return { bin: hex(value as Uint8Array) }
}

/** The codec of a list's items or a nullable's value, which list and nullable keep. */
function inner(codec: AnyCodec): AnyCodec {
	if (codec.item === undefined) {
		throw new Error(`no item codec on ${codec.kind}`)
	}
	return codec.item
}

/** A value for a codec from the notation messages.json spells it in. */
function unnoted(codec: AnyCodec, n: Notation): unknown {
	if (codec.kind === 'message') {
		const m = codec as unknown as Message<Record<string, [number, AnyCodec]>>
		const value: Record<string, unknown> = {}
		for (const [name, field] of Object.entries(
			(n as { fields: Record<string, Notation> }).fields,
		)) {
			const spec = m.fields[camel(name)]
			if (spec === undefined) {
				throw new Error(`${m.name} has no field ${camel(name)}`)
			}
			value[camel(name)] = unnoted(spec[1], field)
		}
		return value
	}
	const kind = codec.kind
	if (kind.startsWith('[]')) {
		return (n as { array: Notation[] }).array.map(item => unnoted(inner(codec), item))
	}
	if (kind.endsWith('?')) {
		return n === null ? null : unnoted(inner(codec), n)
	}
	return unnotedPlain(kind, n)
}

function unnotedPlain(kind: string, n: Notation): unknown {
	if (n === null || typeof n === 'boolean') {
		return n
	}
	if ('uint' in n) {
		return kind === 'uint' ? Number(n.uint) : BigInt(n.uint)
	}
	if ('int' in n) {
		return kind === 'int' || kind === 'kv value' || kind === 'sql value'
			? BigInt(n.int)
			: Number(n.int)
	}
	if ('float' in n) {
		return floatOf(n.float)
	}
	if ('str' in n) {
		return n.str
	}
	if ('bin' in n) {
		return unhex(n.bin)
	}
	if ('map' in n) {
		const named: Record<string, unknown> = {}
		for (const [k, v] of n.map) {
			named[(k as { str: string }).str] = unnotedPlain(
				kind === 'sql names' ? 'sql value' : 'text',
				v,
			)
		}
		return named
	}
	if ('ints' in n) {
		return BigInt64Array.from(n.ints, BigInt)
	}
	if ('floats' in n) {
		const bytes = new Uint8Array(n.floats.length * 8)
		const view = new DataView(bytes.buffer)
		n.floats.forEach((bits, i) => {
			view.setBigUint64(8 * i, BigInt(`0x${bits}`), true)
		})
		return new Float64Array(bytes.buffer)
	}
	throw new Error(`a field of ${JSON.stringify(n)}`)
}

/** A notation whose field names are camelCase, as this SDK names them. */
function camelNoted(n: Notation): Notation {
	if (n === null || typeof n !== 'object') {
		return n
	}
	if ('fields' in n) {
		return {
			fields: Object.fromEntries(
				Object.entries(n.fields).map(([name, v]) => [camel(name), camelNoted(v)]),
			),
		}
	}
	if ('array' in n) {
		return { array: n.array.map(camelNoted) }
	}
	return n
}

describe('messages', () => {
	for (const v of messageVectors.messages) {
		test(`${v.message}: ${v.name}`, () => {
			const codec = messages[v.message as keyof typeof messages] as unknown as AnyCodec &
				Message<Record<string, [number, AnyCodec]>>
			expect(codec).toBeDefined()
			const r = new Reader(unhex(v.hex))
			const decoded = codec.read(r)
			r.end()
			expect(noted(codec, decoded)).toEqual(camelNoted({ fields: v.fields }))

			const w = new Writer()
			codec.write(w, unnoted(codec, { fields: v.fields }))
			expect(hex(w.bytes())).toBe(v.hex)
		})
	}
})

test('every method is the number the server gives it', () => {
	expect(methods as Record<string, number>).toEqual(messageVectors.methods)
})

test('every code the server sends is a class of its own', () => {
	expect([...codes].sort()).toEqual([...messageVectors.codes].sort() as typeof codes)
})
