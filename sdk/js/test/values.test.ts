// A kv value's row as every language writes it (testdata/kv/values.json): a
// bin's first byte the kind that wrote it, so that one read as another kind is
// corrupt rather than a plausible wrong value.

import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { jsonValues, valuesOf } from '../src/kv.ts'

interface Vectors {
	values: {
		kind: string
		text?: string
		bytes?: string
		json?: unknown
		float?: number
		row: string
	}[]
	refused: { read_as: string; row: string; says: string }[]
}

const vectors: Vectors = JSON.parse(
	readFileSync(
		join(import.meta.dirname, '..', '..', '..', 'testdata', 'kv', 'values.json'),
		'utf8',
	),
)

const hex = (bytes: Uint8Array) => Buffer.from(bytes).toString('hex')
const unhex = (text: string) => new Uint8Array(Buffer.from(text, 'hex'))

const codecs = {
	text: valuesOf('string'),
	bytes: valuesOf('bytes'),
	json: jsonValues,
	float: valuesOf('float'),
} as const

test('every language writes and reads the same rows', () => {
	for (const value of vectors.values) {
		const kind = value.kind as keyof typeof codecs
		const given = kind === 'bytes' ? unhex(value.bytes ?? '') : value[kind]
		const row = codecs[kind].encode(given)
		expect(hex(row as Uint8Array), value.kind).toBe(value.row)
		expect(codecs[kind].decode(unhex(value.row))).toEqual(given)
	}
})

test('a row read as another kind than wrote it is corrupt', () => {
	for (const refused of vectors.refused) {
		const codec = codecs[refused.read_as as keyof typeof codecs]
		expect(() => codec.decode(unhex(refused.row))).toThrow(refused.says)
	}
})
