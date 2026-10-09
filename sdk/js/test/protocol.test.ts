// Every message of testdata/wire/protocol.json, which crates/protocol writes
// from the schema beside this SDK's codecs: each vector's fields write its
// bytes, and its bytes read back to what writes them again.

import { describe, expect, test } from 'bun:test'
import { resolve } from 'node:path'

import type { Codec, Message } from '../src/wire/codec.ts'
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
