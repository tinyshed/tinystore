import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { apiPages, escapeProse } from './reference'
import { checkout } from './repo'

describe('the API pages', () => {
	// go run and the TypeScript compiler take seconds, not the default five
	test('are what task reference writes from the source', () => {
		const root = checkout()
		for (const page of apiPages(root)) {
			const written = readFileSync(join(root, page.file), 'utf8')
			if (written !== page.markdown) {
				throw new Error(`${page.file} is not what the source says: run task reference`)
			}
		}
	}, 60_000)

	test('show a doc comment’s < as text, outside code', () => {
		expect(escapeProse('a Setting<string> and `Setting<string>`')).toBe(
			'a Setting&lt;string> and `Setting<string>`',
		)
		expect(escapeProse('Example:\n\n    const a: Array<number> = []')).toBe(
			'Example:\n\n    const a: Array<number> = []',
		)
	})
})
