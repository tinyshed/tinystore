import { describe, expect, test } from 'bun:test'

import { alignTables, unalignedFiles } from './tables'

describe('a table', () => {
	test('gets its cells padded so that its pipes line up', () => {
		const written = ['| a | bb |', '|---|---|', '| ccc | d |'].join('\n')
		expect(alignTables(written)).toBe(['| a   | bb |', '|-----|----|', '| ccc | d  |'].join('\n'))
	})

	test('keeps an escaped pipe in its cell, an empty header and its indent', () => {
		const written = ['  | | |', '  |---|---|', "  | format | `'pretty' \\| 'json'` |"].join('\n')
		expect(alignTables(written)).toBe(
			[
				'  |        |                      |',
				'  |--------|----------------------|',
				"  | format | `'pretty' \\| 'json'` |",
			].join('\n'),
		)
	})

	test('in a fence stays as it is written', () => {
		const written = ['````md', '| a | bb |', '|---|---|', '````', 'text'].join('\n')
		expect(alignTables(written)).toBe(written)
	})

	test('that is aligned already is left alone', () => {
		const written = ['| a   | bb |', '|-----|----|', '| ccc | d  |', '', 'after'].join('\n')
		expect(alignTables(written)).toBe(written)
	})
})

test("every table of the repository's markdown is aligned: task tables aligns them", () => {
	expect(unalignedFiles()).toEqual([])
})
