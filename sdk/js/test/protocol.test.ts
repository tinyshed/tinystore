// Every message of testdata/wire/protocol.json, which crates/protocol writes
// from the schema beside this SDK's codecs: each vector's fields write its
// bytes, and its bytes read back to what writes them again. And every
// message's own reader and writer, which crates/protocol writes out a field
// at a time, against its table of fields walked: the same bytes for the same
// values, the same values or the same refusal for the same bytes.

import { describe, expect, test } from 'bun:test'
import { resolve } from 'node:path'

import { type Codec, Message, message } from '../src/wire/codec.ts'
import * as protocol from '../src/wire/protocol.ts'

type Typed =
	| { bool: boolean }
	| { uint: string }
	| { int: string }
	| { float: string }
	| { str: string }
	| { bin: string }
	| { list: Typed[] }
	| { names: Record<string, Typed> }
	| { fields: Record<string, Typed> }

interface Vector {
	name: string
	message: string
	hex: string
	fields: Record<string, Typed>
}

// biome-ignore lint/suspicious/noExplicitAny: a vector names any message of the schema
type AnyMessage = Message<any>

const vectors: { methods: Record<string, number>; messages: Vector[] } = await Bun.file(
	resolve(import.meta.dir, '../../../testdata/wire/protocol.json'),
).json()

/** A message's codec by its name in the schema: kv.Call is KvCall. */
function codecOf(name: string): AnyMessage {
	const exported = name
		.split('.')
		.map(part => part.charAt(0).toUpperCase() + part.slice(1))
		.join('')
	return (protocol as unknown as Record<string, AnyMessage>)[exported]!
}

/** A field's value from the vectors' notation, as a caller would give it. */
function given(codec: Codec<unknown, never>, typed: Typed): unknown {
	if ('bool' in typed) return typed.bool
	if ('uint' in typed) return Number(typed.uint)
	if ('int' in typed) {
		const n = BigInt(typed.int)
		const exact = n >= BigInt(Number.MIN_SAFE_INTEGER) && n <= BigInt(Number.MAX_SAFE_INTEGER)
		return exact ? Number(n) : n
	}
	if ('float' in typed) return new DataView(bytesOf(typed.float).buffer).getFloat64(0)
	if ('str' in typed) return typed.str
	if ('bin' in typed) return bytesOf(typed.bin)
	if ('list' in typed) return typed.list.map(item => given(codec.item!, item))
	if ('names' in typed) {
		return Object.fromEntries(
			Object.entries(typed.names).map(([name, v]) => [name, given(codec, v)]),
		)
	}
	return fieldsOf(codec as AnyMessage, typed.fields)
}

function fieldsOf(message: AnyMessage, fields: Record<string, Typed>): Record<string, unknown> {
	const pairs = Object.entries(fields).map(([name, typed]) => {
		const field = message.fields[name]
		if (field === undefined) {
			throw new Error(`${message.name} has no field ${name}`)
		}
		return [name, given(field[1], typed)]
	})
	return Object.fromEntries(pairs)
}

function bytesOf(hex: string): Uint8Array {
	return Uint8Array.from(hex.match(/../g) ?? [], pair => Number.parseInt(pair, 16))
}

function hexOf(bytes: Uint8Array): string {
	return Buffer.from(bytes).toString('hex')
}

describe('protocol 2', () => {
	test('the methods are the schema’s, by name and number', () => {
		expect(protocol.methods as Record<string, number>).toEqual(vectors.methods)
	})

	for (const vector of vectors.messages) {
		test(vector.name, () => {
			const codec = codecOf(vector.message)
			expect(hexOf(codec.encode(fieldsOf(codec, vector.fields)))).toBe(vector.hex)
			expect(hexOf(codec.encode(codec.decode(bytesOf(vector.hex))))).toBe(vector.hex)
		})
	}
})

/** Numbers in [0, 1) from a seed, the same each run, so that a failure comes back as it went. */
function numbers(seed: number): () => number {
	let state = seed >>> 0
	return () => {
		state = (state + 0x6d2b79f5) >>> 0
		let t = state
		t = Math.imul(t ^ (t >>> 15), t | 1)
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296
	}
}

/** A value a caller may give a field of the codec, from the edges of each type. */
function anyOf(codec: Codec<unknown, never>, next: () => number): unknown {
	const pick = <T>(values: readonly T[]): T => values[Math.floor(next() * values.length)]!
	switch (codec.kind) {
		case 'bool':
			return next() < 0.5
		case 'uint':
			return pick([0, 1, 127, 128, 255, 256, 65_535, 65_536, 2 ** 32, Number.MAX_SAFE_INTEGER])
		case 'int':
			return pick([0, -1, -32, -33, -128, -129, 2 ** 31, -(2 ** 40), Number.MIN_SAFE_INTEGER])
		case 'float':
			return pick([0, -0, 1.5, -3, Number.NaN, Number.POSITIVE_INFINITY, 1e300])
		case 'str':
			return pick([
				'',
				'a',
				'é',
				'\u{1d11e} music',
				'x'.repeat(31),
				'y'.repeat(32),
				'z'.repeat(300),
			])
		case 'bin':
			return Uint8Array.from({ length: pick([0, 1, 255, 256]) }, (_, i) => i % 251)
		case 'key':
			return pick(['k', 'ключ', '', 7, 9_007_199_254_740_993n, Uint8Array.of(0xff, 0)])
		case 'kv value':
			return pick([null, 0, -5, 5n, 2n ** 63n - 1n, Uint8Array.of(1, 2), new Uint8Array(0)])
		case 'sql value':
			return pick([null, 0, -0, 1.5, -2n, 'text', '', Uint8Array.of(3), true, false])
		case 'names':
			return Object.fromEntries(
				pick([
					[],
					[['b', 'x']],
					[
						['b', 'x'],
						['a', ''],
						['é', 'z'],
					],
				]),
			)
		case 'message':
			return valuesOf(codec as AnyMessage, next)
	}
	if (codec.kind.startsWith('[]')) {
		return Array.from({ length: pick([0, 1, 3, 16]) }, () => anyOf(codec.item!, next))
	}
	throw new Error(`no values for a field of kind ${codec.kind}`)
}

/** A message's fields given, each with an even chance of being left out. */
function valuesOf(message: AnyMessage, next: () => number): Record<string, unknown> {
	const values: Record<string, unknown> = {}
	const fields = message.fields as Record<string, readonly [number, Codec<unknown, never>]>
	for (const [name, [, codec]] of Object.entries(fields)) {
		if (next() < 0.5) {
			values[name] = anyOf(codec, next)
		}
	}
	return values
}

/** What reading bytes came to: the message, or the refusal's words. */
function outcome(read: () => unknown): unknown {
	try {
		return { read: read() }
	} catch (error) {
		return { refused: (error as Error).message }
	}
}

describe("a message's own reader and writer", () => {
	const made = Object.values(protocol).filter((each): each is AnyMessage => each instanceof Message)

	test('every message of the schema has them', () => {
		expect(made.length).toBe(new Set(vectors.messages.map(vector => vector.message)).size)
	})

	for (const own of made) {
		test(`${own.name} writes and reads what its table of fields does`, () => {
			const table = message(own.name, own.fields)
			const next = numbers([...own.name].reduce((hash, c) => hash * 31 + c.charCodeAt(0), 7))
			for (let round = 0; round < 300; round++) {
				const values = valuesOf(own, next)
				const bytes = own.encode(values)
				expect(hexOf(bytes)).toBe(hexOf(table.encode(values)))
				expect(own.decode(bytes)).toEqual(table.decode(bytes))

				const cut = bytes.slice(0, Math.floor(next() * bytes.length))
				expect(outcome(() => own.decode(cut))).toEqual(outcome(() => table.decode(cut)))
				const changed = bytes.slice()
				changed[Math.floor(next() * changed.length)] = Math.floor(next() * 256)
				expect(outcome(() => own.decode(changed))).toEqual(outcome(() => table.decode(changed)))
			}
		})
	}
})
